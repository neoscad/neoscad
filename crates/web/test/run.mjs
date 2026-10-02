// The built web core in node, through the reference worker's glue
// (js/worker.js): the protocol end to end on real examples, with their
// console summaries and a time budget each.
//
//   node crates/web/test/run.mjs [DIR]     DIR: scripts/web/build-core.sh's
//                                           output (default dist/web-core)
//
// Needs the reference checkouts (.reference/openscad for CSG.scad and
// sign.scad, .reference/BOSL2 for the gear); a case whose input is
// missing is skipped with a note. Exits 1 on any failure.
//
// Memory: every model here is small, but a regression could make one
// balloon, and wasm memory never shrinks. The engine runs under the
// protocol's 1 GiB memory limit, and after each case the process's RSS
// and the wasm memory are checked against a 2 GB guard; above it the run
// stops at once.

import assert from 'node:assert/strict';
import { readFileSync, readdirSync, existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '../../..');
const dir = resolve(process.argv[2] || join(root, 'dist/web-core'));
const GUARD = 2 * 1024 ** 3;

const worker = await import(pathToFileURL(join(dir, 'worker.js')).href);
const { start, handle } = worker;
await start(readFileSync(join(dir, 'neoscad_web_bg.wasm')));

let id = 0;
function call(type, fields = {}) {
    const { reply, transfer } = handle({ id: ++id, type, ...fields });
    assert.equal(reply.id, id);
    return { reply, transfer };
}
function ok(type, fields) {
    const { reply } = call(type, fields);
    assert.equal(reply.ok, true, `${type}: ${JSON.stringify(reply.error)}`);
    return reply.result;
}
function guard(name) {
    const rss = process.memoryUsage().rss;
    const wasm = ok('stats').memoryBytes;
    if (rss > GUARD || wasm > GUARD) {
        console.error(`FAIL memory guard after ${name}: rss ${rss}, wasm ${wasm}`);
        process.exit(1);
    }
    return wasm;
}

// A ustar archive of `files` ([name, Uint8Array]): what the page hands
// over after gunzipping bosl2.tar.gz.
function tar(files) {
    const blocks = [];
    const enc = new TextEncoder();
    for (const [name, body] of files) {
        const h = new Uint8Array(512);
        h.set(enc.encode(name).subarray(0, 100), 0);
        h.set(enc.encode(body.length.toString(8).padStart(11, '0') + '\0'), 124);
        h[156] = '0'.charCodeAt(0);
        h.set(enc.encode('ustar\0'), 257);
        blocks.push(h, body, new Uint8Array((512 - (body.length % 512)) % 512));
    }
    blocks.push(new Uint8Array(1024));
    const out = new Uint8Array(blocks.reduce((n, b) => n + b.length, 0));
    let at = 0;
    for (const b of blocks) {
        out.set(b, at);
        at += b.length;
    }
    return out;
}

const results = [];
let failures = 0;
async function test(name, budgetMs, f) {
    const t0 = performance.now();
    try {
        const skipped = await f();
        const ms = performance.now() - t0;
        if (skipped) {
            console.log(`skip ${name}: ${skipped}`);
            return;
        }
        const wasm = guard(name);
        const over = ms > budgetMs ? ` (over its ${budgetMs} ms budget)` : '';
        console.log(`ok   ${name}: ${ms.toFixed(0)} ms, wasm memory ${(wasm / 2 ** 20).toFixed(0)} MiB${over}`);
        if (over) failures++;
        results.push({ name, ms });
    } catch (e) {
        failures++;
        console.log(`FAIL ${name}: ${e.stack || e}`);
    }
}

const init = ok('init', { seed: 42 });
assert.deepEqual(init.libraryDirs, ['/neoscad/libraries']);
assert.equal(init.limits.memoryBytes, 2 ** 30);

const example = (p) => join(root, '.reference/openscad/examples', p);

// A recursion through a range's bounds once recursed natively, each level
// starting a heap loop with large native frames, and the frame budget
// stopped it after a few dozen levels (with a trap here, cold, when the
// heap loop was charged as a call). It now runs on the heap evaluator, as
// `is_undef()`'s argument, callees that are expressions and methods'
// arguments do, and reaches the counted limit like any function
// recursion. It runs first, while the module is cold: V8's baseline
// frames are the larger.
await test("a recursion through a range's bounds reaches the counted limit", 5000, () => {
    const text = (n) => `function f(n) = n == 0 ? 0 : [0 : 1 : f(n - 1)][2] + 1;\necho(f(${n}));\n`;
    ok('open', { path: '/doc/range.scad', text: text(99999) });
    const at = ok('run', { path: '/doc/range.scad', mode: 'preview' }).render;
    assert.match(at.console, /ECHO: 99999/, at.console);
    ok('open', { path: '/doc/range.scad', text: text(100000) });
    const r = ok('run', { path: '/doc/range.scad', mode: 'preview' }).render;
    assert.match(r.console, /ERROR: Recursion detected calling function 'f'/, r.console);
    console.log(`     ${r.console.trim().split('\n')[0]}`);
});

await test('CSG.scad (render)', 2000, () => {
    if (!existsSync(example('Basics/CSG.scad'))) return 'no .reference/openscad';
    const path = '/doc/CSG.scad';
    ok('open', { path, text: readFileSync(example('Basics/CSG.scad'), 'utf8') });
    const r = ok('run', { path, mode: 'render' });
    assert.equal(r.render.exitCode, 0, r.render.console);
    assert.match(r.render.echo[0], /^ECHO: version = \[/);
    const g = r.render.geometry;
    assert.equal(g.dimensions, 3);
    assert.equal(g.manifold, true);
    assert.deepEqual(g.bboxMax.map(Math.round), [32, 10, 10]);
    // render::packed's wire form, as crates/web-view's Viewer.setModel takes it.
    assert.ok(r.scene.faces instanceof ArrayBuffer && r.scene.edges instanceof ArrayBuffer);
    assert.ok(r.scene.faces.byteLength > 0 && r.scene.faces.byteLength % 44 === 0);
    const meta = JSON.parse(r.scene.meta);
    assert.ok(meta.draws.length > 0 && meta.edge_color.length === 4);
    assert.deepEqual(meta.bbox[1].map(Math.round), [32, 10, 10]);
    assert.ok(r.render.timings.totalMs > 0, 'timings come from performance.now()');
    console.log(`     ${r.render.console.trim().split('\n').slice(-3).join(' | ')}`);
    console.log(`     timings ${JSON.stringify(r.render.timings)}`);
});

await test('CSG.scad (preview)', 2000, () => {
    if (!existsSync(example('Basics/CSG.scad'))) return 'no .reference/openscad';
    const r = ok('run', { path: '/doc/CSG.scad', mode: 'preview' });
    assert.equal(r.render.exitCode, 0);
    const meta = JSON.parse(r.scene.meta);
    assert.ok(meta.draws.length > 0);
    assert.ok(Array.isArray(meta.image_csg));
});

await test('sign.scad (customizer + text)', 3000, () => {
    if (!existsSync(example('Parametric/sign.scad'))) return 'no .reference/openscad';
    const path = '/doc/sign.scad';
    ok('open', { path, text: readFileSync(example('Parametric/sign.scad'), 'utf8') });
    const { groups } = ok('parameters', { path });
    const names = groups.flatMap((g) => g.parameters.map((p) => p.name));
    assert.ok(names.includes('Message') && names.includes('radius'), names.join());
    const radius = groups.flatMap((g) => g.parameters).find((p) => p.name === 'radius');
    assert.equal(radius.control.kind, 'slider');
    const message = groups.flatMap((g) => g.parameters).find((p) => p.name === 'Message');
    assert.equal(message.control.kind, 'dropdown');
    const r = ok('run', {
        path,
        mode: 'render',
        overrides: [{ name: 'radius', value: { kind: 'number', value: 100 } }],
    });
    assert.equal(r.render.exitCode, 0, r.render.console);
    const g = r.render.geometry;
    assert.equal(g.dimensions, 3);
    assert.ok(Math.abs(g.bboxMax[0] - 100) < 1, JSON.stringify(g));
    console.log(`     ${g.triangles} triangles, volume ${g.volume.toFixed(1)}`);
});

await test('BOSL2 helical spur gear (addFiles tar)', 20000, () => {
    const bosl = join(root, '.reference/BOSL2');
    if (!existsSync(join(bosl, 'std.scad'))) return 'no .reference/BOSL2';
    const files = readdirSync(bosl)
        .filter((f) => f.endsWith('.scad'))
        .map((f) => [`BOSL2/${f}`, new Uint8Array(readFileSync(join(bosl, f)))]);
    const t0 = performance.now();
    const { added } = ok('addFiles', { tar: tar(files).buffer });
    assert.equal(added, files.length);
    const addMs = performance.now() - t0;
    const path = '/doc/gear.scad';
    ok('open', {
        path,
        text: 'include <BOSL2/std.scad>\ninclude <BOSL2/gears.scad>\n' +
            'spur_gear(circ_pitch=5, teeth=20, thickness=8, helical=20, shaft_diam=5);\n',
    });
    const t1 = performance.now();
    const r = ok('run', { path, mode: 'render' });
    const runMs = performance.now() - t1;
    assert.equal(r.render.exitCode, 0, r.render.console);
    const g = r.render.geometry;
    assert.equal(g.dimensions, 3);
    assert.equal(g.manifold, true);
    assert.ok(r.files.includes('/neoscad/libraries/BOSL2/gears.scad'));
    const again = performance.now();
    ok('run', { path, mode: 'render' });
    const warmMs = performance.now() - again;
    console.log(`     ${files.length} files added in ${addMs.toFixed(0)} ms; ` +
        `render ${runMs.toFixed(0)} ms cold, ${warmMs.toFixed(0)} ms warm; ` +
        `${g.triangles} triangles`);
    const src = ok('readFile', { path: '/neoscad/libraries/BOSL2/gears.scad' });
    assert.match(src.text, /module spur_gear/);
});

await test('export binary STL', 2000, () => {
    ok('open', { path: '/doc/cube.scad', text: 'cube(1);' });
    const { reply, transfer } = call('export', { path: '/doc/cube.scad', format: 'binstl' });
    assert.equal(reply.ok, true);
    assert.equal(reply.result.mime, 'model/stl');
    assert.ok(reply.result.data instanceof ArrayBuffer);
    assert.equal(reply.result.data.byteLength, 684);
    assert.equal(transfer.length, 1);
});

// Deep recursion ends in OpenSCAD's error, not a trap: the module is
// linked with the 8 MiB stack the evaluator's budget assumes.
await test('runaway recursion is an error, not a crash', 5000, () => {
    ok('open', { path: '/doc/deep.scad', text: 'function f(n) = f(n + 1);\necho(f(0));\n' });
    const r = ok('run', { path: '/doc/deep.scad', mode: 'render' });
    assert.match(r.render.console, /ERROR: Recursion detected calling function 'f'/, r.render.console);
    console.log(`     ${r.render.console.trim().split('\n')[0]}`);
});

// Recursion runs on the evaluator's heap stack, so function, module and
// comprehension recursion reach the counted depth limit (99,999 levels;
// the 100,000th call is the error) in the web core as natively.
await test('deep recursion reaches the counted limit', 20000, () => {
    const deep = {
        function: (n) => `function f(n) = n == 0 ? 0 : 1 + f(n - 1);\necho(f(${n}));\n`,
        module: (n) => `module m(n) { if (n > 0) m(n - 1); else cube(1); }\nm(${n});\n`,
        comprehension: (n) =>
            `function g(n) = n == 0 ? [] : [for (i = [0:0]) each g(n - 1)];\necho(len(g(${n})));\n`,
    };
    const run = (text) => {
        ok('open', { path: '/doc/deep.scad', text });
        return ok('run', { path: '/doc/deep.scad', mode: 'preview' }).render;
    };
    for (const [kind, text] of Object.entries(deep)) {
        const t0 = performance.now();
        const at = run(text(99999));
        assert.equal(at.exitCode, 0, `${kind} at 99,999: ${at.console}`);
        assert.doesNotMatch(at.console, /Recursion detected/, `${kind} at 99,999`);
        const past = run(text(100000));
        assert.match(past.console, /ERROR: Recursion detected calling (function|module) '[fgm]'/,
            `${kind} at 100,000: ${past.console}`);
        console.log(`     ${kind}: 99,999 ran, 100,000 stopped (${(performance.now() - t0).toFixed(0)} ms)`);
    }
});

// The memory limit measures (the wasm build's counting allocator, heap.rs)
// as well as estimates: the heavy example's BOSL2 evaluation and kernel
// working memory are mostly outside the estimate, so under a 256 MiB
// limit it is the measurement that stops it, with a resource-limit error
// (unmeasured, it ran on to about 860 MiB). The engine is still usable
// afterwards.
await test('the measured memory limit is a resource-limit error', 30000, () => {
    if (!existsSync(join(root, '.reference/BOSL2/std.scad'))) return 'no .reference/BOSL2';
    const path = '/doc/gearbox.scad';
    ok('open', { path, text: readFileSync(join(root, 'web/examples/gearbox.scad'), 'utf8') });
    ok('setLimits', { limits: { ...init.limits, memoryBytes: 256 * 2 ** 20 } });
    const r = ok('run', { path, mode: 'render' });
    assert.equal(r.render.exitCode, 1);
    assert.match(r.render.console, /ERROR: Resource limit exceeded: .* over the memory limit of 256 MiB \(measured\)/, r.render.console);
    const stats = ok('stats');
    assert.ok(stats.heapBytes > 0 && stats.heapBytes < stats.memoryBytes, JSON.stringify(stats));
    ok('setLimits', { limits: init.limits });
    ok('open', { path: '/doc/after.scad', text: 'cube(1);' });
    assert.equal(ok('run', { path: '/doc/after.scad', mode: 'render' }).render.exitCode, 0);
    console.log(`     ${r.render.console.split('\n').find((l) => l.includes('measured'))}`);
});

// The Menger example at depth 5: its normalised difference is a chain of
// 14,044 holes, whose recursive walk overflowed V8's stack ("Maximum call
// stack size exceeded") within 100 ms; and its one product's boolean is the
// depth-5 sponge itself, past what a preview computes, so it is drawn
// thrown together with a warning instead of running out of memory.
await test('menger depth 5 preview is thrown together, not a crash', 5000, () => {
    const menger = join(root, 'web/examples/example024.scad');
    if (!existsSync(menger)) return 'no web/examples/example024.scad';
    const text = readFileSync(menger, 'utf8').replace(/^n\s*=\s*\d+;/m, 'n=5;');
    assert.match(text, /^n=5;/m);
    ok('open', { path: '/doc/menger.scad', text });
    const r = ok('run', { path: '/doc/menger.scad', mode: 'preview' });
    assert.match(r.render.console, /WARNING: The CSG products have 14045 elements to combine/, r.render.console);
    assert.ok(JSON.parse(r.scene.meta).draws.length > 0);
});

// The Menger example at depth 4 previews in about 30 s here, all of it in
// the product booleans, which once ran on past the time limit. Under a
// 3 s limit the run must stop at the next kernel operation after it,
// with the limit as its error.
await test('a preview past its time limit stops with the limit', 10000, () => {
    const menger = join(root, 'web/examples/example024.scad');
    if (!existsSync(menger)) return 'no web/examples/example024.scad';
    const text = readFileSync(menger, 'utf8').replace(/^n\s*=\s*\d+;/m, 'n=4;');
    ok('open', { path: '/doc/menger4.scad', text });
    ok('setLimits', { limits: { ...init.limits, timeSeconds: 3 } });
    try {
        const { reply } = call('run', { path: '/doc/menger4.scad', mode: 'preview' });
        assert.equal(reply.ok, false, 'the preview should stop');
        assert.match(JSON.stringify(reply.error), /time limit of 3 s/);
    } finally {
        ok('setLimits', { limits: init.limits });
    }
});

console.log(failures ? `web core: ${failures} failed` : 'web core: all passed');
process.exit(failures ? 1 : 0);
