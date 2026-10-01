// Runs the cases in cases.json through the wasm32 build of this crate in
// node and checks each output line by line against the case's
// expectations (`*` stands for any text), as the native test does.
//
//   node run.js MODULE.wasm            check every case
//   node run.js MODULE.wasm --depths   also find the deepest function and
//                                      module recursion that still works
//   --frames=N                         run the depth search with frame
//                                      budget N (to calibrate the default:
//                                      a huge N finds where V8 traps)
//
// Each case gets a fresh instance, so one that traps cannot affect the
// next. A trap (V8's "Maximum call stack size exceeded", an out-of-bounds
// access) fails the case: the point of the check is that deep recursion
// ends in OpenSCAD's error text instead.
'use strict';
const fs = require('fs');
const path = require('path');

const [wasmPath, ...flags] = process.argv.slice(2);
const module_ = new WebAssembly.Module(fs.readFileSync(wasmPath));
const here = __dirname;

function instance(files) {
  const e = new WebAssembly.Instance(module_, {}).exports;
  // `input` may grow the memory, which detaches the old buffer: take the
  // buffer after the call.
  const put = (bytes) => {
    const ptr = e.input(bytes.length);
    new Uint8Array(e.memory.buffer, ptr, bytes.length).set(bytes);
  };
  for (const f of files) {
    const name = Buffer.from(f.path);
    put(Buffer.concat([name, f.data]));
    e.add_file(name.length);
  }
  return {
    run(src, seed, frames, preview) {
      put(Buffer.from(src));
      try {
        e.run_input(seed >>> 0, (frames || 0) >>> 0,
          preview === 'stop' ? 7 : preview === 'lsp' ? 6 : preview === 'test' ? 5 : preview === 'fmt' ? 4 : preview === 'check' ? 3
            : preview === 'session' ? 2 : preview ? 1 : 0);
      } catch (x) {
        // A Rust panic leaves its message in the output before trapping.
        let why = '';
        try {
          why = Buffer.from(new Uint8Array(e.memory.buffer, e.output_ptr(), e.output_len())).toString();
        } catch (_) {}
        throw new Error(`${x}${why.startsWith('PANIC') ? ` (${why.trim()})` : ''}`);
      }
      return Buffer.from(new Uint8Array(e.memory.buffer, e.output_ptr(), e.output_len())).toString();
    },
  };
}

function matches(line, pattern) {
  const parts = pattern.split('*');
  if (parts.length === 1) return line === pattern;
  const first = parts[0], last = parts[parts.length - 1];
  if (!line.startsWith(first) || !line.slice(first.length).endsWith(last)) return false;
  let rest = line.slice(first.length, line.length - last.length);
  for (const p of parts.slice(1, -1)) {
    const i = rest.indexOf(p);
    if (i < 0) return false;
    rest = rest.slice(i + p.length);
  }
  return true;
}

function runCase(c) {
  const files = [];
  for (const f of c.files || []) {
    if (f.text !== undefined) files.push({ path: f.path, data: Buffer.from(f.text) });
    else {
      const p = path.join(here, f.from);
      if (!fs.existsSync(p)) return { skipped: `no ${f.from}` };
      files.push({ path: f.path, data: fs.readFileSync(p) });
    }
  }
  const t0 = Date.now();
  let out;
  try {
    const mode = ['check', 'fmt', 'test', 'lsp'].includes(c.session) ? c.session
      : c.session ? 'session' : c.preview;
    out = instance(files).run(c.src, c.seed || 0, 0, mode);
  } catch (x) {
    return { error: String(x), ms: Date.now() - t0 };
  }
  const got = out.split('\n');
  if (got[got.length - 1] === '') got.pop();
  const ok = got.length === c.expect.length && got.every((l, i) => matches(l, c.expect[i]));
  return { ok, got, ms: Date.now() - t0 };
}

let failed = 0;
const cases = JSON.parse(fs.readFileSync(path.join(here, 'cases.json')));
for (const c of cases) {
  const r = runCase(c);
  if (r.skipped) {
    console.log(`skip ${c.name}: ${r.skipped}`);
  } else if (r.ok) {
    console.log(`ok   ${c.name} (${r.ms} ms)`);
  } else {
    failed++;
    console.log(`FAIL ${c.name}: ${r.error || 'output differs'}`);
    if (r.got) {
      console.log('  got:');
      for (const l of r.got.slice(0, 20)) console.log(`    ${l}`);
      console.log('  expected:');
      for (const l of c.expect.slice(0, 20)) console.log(`    ${l}`);
    }
  }
}

// The deepest recursion that evaluates (and renders) without an error,
// by bisection; and whether one level more gives the recursion error
// rather than a trap.
const PROGRAMS = {
  function: (n) => `function f(n) = n == 0 ? 0 : 1 + f(n - 1);\necho(f(${n}));`,
  module: (n) => `module m(n) { if (n > 0) m(n - 1); else cube(1); }\nm(${n});`,
  // Heavier levels, for calibration: a list comprehension between calls,
  // and four tree levels per module level.
  'function-lc': (n) => `function g(n) = n == 0 ? [] : [for (i = [0:0]) let (x = i) each g(n - 1)];\necho(len(g(${n})));`,
  'module-transforms': (n) => `module k(n) { if (n > 0) translate([0, 0, 1]) rotate(1) k(n - 1); else cube(1); }\nk(${n});`,
  'module-children': (n) => `module c(n) { if (n > 0) c(n - 1) children(); else children(); }\nc(${n}) cube(1);`,
  'function-nested': (n) => `function h(n) = n == 0 ? 0 : 1 + (1 + (1 + (1 + (1 + h(n - 1)))));\necho(h(${n}));`,
  'function-args': (n) => `function a(n, v) = n == 0 ? v : max(0, a(n - 1, [v[0] + 1, norm([1, 2, 3])]));\necho(a(${n}, [0, 0]));`,
};

function depthOf(kind) {
  const src = PROGRAMS[kind];
  const works = (n) => {
    try {
      const out = instance([]).run(src(n), 0, frameLimit);
      return { ok: !/ERROR/.test(out), trapped: false };
    } catch (x) {
      return { ok: false, trapped: true, error: String(x) };
    }
  };
  let lo = 1, hi = 1 << 20;
  while (hi - lo > 1) {
    const mid = (lo + hi) >> 1;
    if (works(mid).ok) lo = mid; else hi = mid;
  }
  const next = works(hi);
  return `${kind}: ${lo} (at ${hi}: ${next.trapped ? 'TRAP ' + next.error : 'recursion error'})`;
}

const frameFlag = flags.find((f) => f.startsWith('--frames='));
const frameLimit = frameFlag ? Number(frameFlag.slice('--frames='.length)) : 0;
if (flags.includes('--depths')) {
  const kinds = flags.includes('--all-programs') ? Object.keys(PROGRAMS) : ['function', 'module'];
  for (const k of kinds) {
    const line = depthOf(k);
    console.log(`depth ${line}`);
    if (/TRAP/.test(line)) failed++;
  }
}

console.log(failed ? `${failed} failed` : 'all passed');
process.exit(failed ? 1 : 0);
