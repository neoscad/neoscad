// Runs the cases in cases.json through the wasm32 build of this crate in
// node and checks each output line by line against the case's
// expectations (`*` stands for any text), as the native test does.
//
//   node run.js MODULE.wasm            check every case
//   node run.js MODULE.wasm --depths   also find the deepest function and
//                                      module recursion, and source
//                                      nesting, that still works
//   --all-programs                     with --depths: the calibration
//                                      programs too
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
          preview === 'brep' ? 9 : preview === 'sketch' ? 8 : preview === 'stop' ? 7 : preview === 'lsp' ? 6 : preview === 'test' ? 5 : preview === 'fmt' ? 4 : preview === 'check' ? 3
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
    const mode = ['check', 'fmt', 'test', 'lsp', 'sketch', 'brep'].includes(c.session) ? c.session
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
  // Shapes that recursed natively, a heap loop a level, until they moved
  // to the heap: they reach the counted limit as `function` does (the
  // callee two counted calls a level: 49,999).
  'function-range': (n) => `function r(n) = n == 0 ? 0 : [0 : 1 : r(n - 1)][2] + 1;\necho(r(${n}));`,
  'function-is-undef': (n) => `function u(n) = n == 0 ? 0 : is_undef(u(n - 1)) ? -1 : n;\necho(u(${n}));`,
  'function-callee': (n) => `function c(n) = n == 0 ? function (x) x : c(n - 1)(0) == 0 ? function (x) x : undef;\necho(c(${n})(7));`,
  'function-cfor': (n) => `function cf(n) = n == 0 ? 0 : [for (i = cf(n - 1); i < n; i = n) i][0] + 1;\necho(cf(${n}));`,
  'function-default': (n) => `function d(x = $k > 0 ? let ($k = $k - 1) d() + 1 : 0) = x;\necho(let ($k = ${n}) d());`,
  // Values nested as deep as a recursion can build them: printing stops
  // at its counted depth (46,918 levels, as natively) with OpenSCAD's
  // "Stack exhausted"; the operators, and freeing a chain of closures,
  // go on to the tail-call limit (999,999).
  'value-print': (n) => `function nest(n, acc = 0) = n == 0 ? acc : nest(n - 1, [acc]);\necho(len(str(nest(${n}))));`,
  'value-ops': (n) => `function nest(n, acc = 0) = n == 0 ? acc : nest(n - 1, [acc]);\nv = nest(${n});\necho(len(-v) + len(v + v) + len(v * 2), v == v);`,
  'value-closures': (n) => `function nf(n, acc) = n == 0 ? acc : nf(n - 1, function () acc);\necho(is_function(nf(${n}, 0)));`,
  // Nesting in the source rather than in a recursion: the parser, the
  // lowering and everything after walk it recursively, and past the
  // parser's nesting limit it must end in OpenSCAD's "memory exhausted"
  // error rather than a trap.
  'source-transforms': (n) => `${'translate([0, 0, 1]) '.repeat(n)}cube(1);`,
  'source-blocks': (n) => `${'{'.repeat(n)}cube(1);${'}'.repeat(n)}`,
  'source-else-if': (n) => `x = 1;\n${'if (x == 0) cube(1); else '.repeat(n)}sphere(1);`,
  'source-parens': (n) => `echo(${'('.repeat(n)}1${')'.repeat(n)});`,
  'source-lists': (n) => `echo(len(${'['.repeat(n)}1${']'.repeat(n)}));`,
  'source-sum': (n) => `echo(${'1 + '.repeat(n)}1);`,
  'source-calls': (n) => `echo(${'max('.repeat(n)}1${')'.repeat(n)});`,
  'source-chain': (n) => `echo(${'let (a = 1) assert(true) '.repeat(n)}1);`,
};

// Run by `--depths` alone; `--all-programs` runs every program above.
const DEFAULT_PROGRAMS = [
  'function', 'module', 'function-range', 'function-is-undef', 'function-callee',
  'function-cfor', 'function-default', 'value-print', 'value-ops', 'value-closures',
  ...Object.keys(PROGRAMS).filter((k) => k.startsWith('source-')),
];

function depthOf(kind) {
  const src = PROGRAMS[kind];
  const works = (n) => {
    try {
      const out = instance([]).run(src(n), 0, frameLimit);
      const error = out.split('\n').find((l) => /ERROR/.test(l));
      return { ok: !error, trapped: false, message: error ? error.replace(/ in file .*/, '') : 'no error' };
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
  return `${kind}: ${lo} (at ${hi}: ${next.trapped ? 'TRAP ' + next.error : next.message})`;
}

const frameFlag = flags.find((f) => f.startsWith('--frames='));
const frameLimit = frameFlag ? Number(frameFlag.slice('--frames='.length)) : 0;
if (flags.includes('--depths')) {
  const kinds = flags.includes('--all-programs') ? Object.keys(PROGRAMS) : DEFAULT_PROGRAMS;
  for (const k of kinds) {
    const line = depthOf(k);
    console.log(`depth ${line}`);
    if (/TRAP/.test(line)) failed++;
  }
}

console.log(failed ? `${failed} failed` : 'all passed');
process.exit(failed ? 1 : 0);
