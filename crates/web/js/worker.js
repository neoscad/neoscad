// The reference worker for NeoSCAD's web core: the envelope of
// docs/web-protocol.md over the wasm-bindgen exports of crates/web.
// scripts/web/build-core.sh copies it beside neoscad_web.js; the front
// end's web/src/worker.js may copy or adapt it.
//
// Start it as a module worker:
//
//   const worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' });
//
// It posts { type: "ready" } once the module is loaded, then answers each
// request { id, type, ... } with { id, ok, result | error }, moving the
// reply's buffers (scenes, exports) to the page as transferables. The
// node tests (crates/web/test/run.mjs) import `start` and `handle` and
// drive it without a worker.

import init, {
    Engine, lastPanic, frameLimit, setFrameLimit, frameWeights, setFrameWeights, heapStatements,
} from './neoscad_web.js';

let engine = null;
let memory = null;
let crashed = false;

// What a trap with no panic message was, for the page to show. A panic
// records its message before it aborts, so an `unreachable` trap without
// one is Rust aborting on a failed allocation: the model needed more
// memory than the instance could grow to. Kernel working memory is not
// counted against the memory limit, so this is how such a model ends.
// "engine restarted: crashed: unreachable" told the user nothing.
function trapMessage(e) {
    const raw = String(e && e.message ? e.message : e);
    if (isStackOverflow(e)) {
        return `the engine's stack overflowed (${raw})`;
    }
    if (/unreachable/i.test(raw)) {
        let size = '';
        try {
            size = ` at ${Math.round(memory.buffer.byteLength / (1 << 20))} MiB`;
        } catch (_) {
            // No memory to read.
        }
        return `the engine ran out of memory${size}; the model needs more than the browser gives it`;
    }
    return raw;
}

// Every `ArrayBuffer` or typed array inside `value`, replaced by
// { $buffer: n } with the buffer at `buffers[n]`: how buffers reach Rust
// (addFiles' data and tar). Strings and plain values pass unchanged.
function extractBuffers(value, buffers) {
    if (value instanceof ArrayBuffer || ArrayBuffer.isView(value)) {
        buffers.push(value);
        return { $buffer: buffers.length - 1 };
    }
    if (Array.isArray(value)) return value.map((v) => extractBuffers(v, buffers));
    if (value && typeof value === 'object') {
        const out = {};
        for (const [k, v] of Object.entries(value)) out[k] = extractBuffers(v, buffers);
        return out;
    }
    return value;
}

// The reverse: each { $buffer: n } in the reply becomes its ArrayBuffer.
function insertBuffers(value, buffers) {
    if (Array.isArray(value)) return value.map((v) => insertBuffers(v, buffers));
    if (value && typeof value === 'object') {
        const keys = Object.keys(value);
        if (keys.length === 1 && keys[0] === '$buffer') return buffers[value.$buffer];
        for (const k of keys) value[k] = insertBuffers(value[k], buffers);
    }
    return value;
}

// Whether `e` is the engine's stack running out: V8 and JavaScriptCore
// throw a RangeError ("Maximum call stack size exceeded"), SpiderMonkey an
// InternalError ("too much recursion").
function isStackOverflow(e) {
    return /call stack|too much recursion/i.test(String(e && e.message ? e.message : e));
}

// --- The stack probe ---------------------------------------------------
//
// The evaluator stops a deep recursion with OpenSCAD's "Recursion
// detected" error when its frame budget runs out (crates/eval/src/
// recursion.rs). The budget counts weighted frames, not bytes, and how
// many bytes each kind of frame costs is the browser's business: the
// default weights and budget were calibrated in V8, and in WebKit, whose
// baseline wasm tier gives each frame about a kilobyte on a worker stack
// of about 512 KiB, `m(40)` of a recursive module overflowed the stack
// instead, which kills the instance. Engines also disagree about which
// kind is expensive, so one budget scaled down for the worst kind cut
// module chains that fit (the BOSL2 gearbox) to the depth of functions.
//
// So before the engine starts, each probe program below recurses without
// a budget in a throwaway instance of the same module until the stack
// overflows. The frames it held at its last check are one static away
// (`framesAtLastCheck`) even though the instance is dead. Each run counts
// one kind of frame only (the others weigh 0), so a probe yields how many
// statement, expression (with calls) and comprehension frames its kind of
// recursion held when this stack ran out. From those, weights are chosen
// under one large budget (PROBE_BUDGET) so that every probed kind stops at
// PROBE_SHARE of the depth that overflowed: expressions from plain
// function recursion, then comprehensions and statements from what is
// left of the budget in their probes. No weight goes below the default's
// share of the budget, so no kind recurses deeper than the defaults allow.
//
// A probe runs while the module is cold, mostly in the engines' first
// tiers; the optimised tiers that take over later have smaller frames,
// except JavaScriptCore's BBQ, whose frames are larger than its
// interpreter's: with only BBQ, a recursion reaches two thirds to three
// quarters of the depth it does cold (measured in the jsc shell at
// 512 KiB). PROBE_SHARE leaves room for that and for kinds of recursion
// the probes do not cover.
const PROBES = {
    fn: 'function f(n) = n == 0 ? 0 : 1 + f(n - 1);\necho(f(1000000));\n',
    lc: 'function g(n) = n == 0 ? [] : [for (i = [0:0]) each g(n - 1)];\necho(len(g(1000000)));\n',
    children: 'module c(n) { if (n > 0) c(n - 1) children(); else children(); }\nc(1000000) cube(1);\n',
    transform: 'module m(n) { if (n > 0) translate([0, 0, 1]) m(n - 1); else cube(1); }\nm(1000000);\n',
};
// The runs: which probe, and the weights it counts with (statement,
// expression, call, comprehension, geometry); calls weigh two expressions,
// as in the defaults.
const RUNS = [
    ['fn', 'expr', [0, 1, 2, 0, 0]],
    ['lc', 'expr', [0, 1, 2, 0, 0]],
    ['lc', 'lc', [0, 0, 0, 1, 0]],
    ['children', 'expr', [0, 1, 2, 0, 0]],
    ['children', 'stmt', [1, 0, 0, 0, 0]],
    ['transform', 'expr', [0, 1, 2, 0, 0]],
    ['transform', 'stmt', [1, 0, 0, 0, 0]],
    ['transform', 'geometry', [0, 0, 0, 0, 1]],
];
const PROBE_SHARE = 0.5;
const PROBE_BUDGET = 1000000;
// The default budget and weights (crates/eval/src/recursion.rs): the
// floors, as weights per unit of PROBE_BUDGET.
const DEFAULT_LIMIT = 2000;
const DEFAULT_WEIGHTS = { stmt: 4, expr: 1, lc: 4, geometry: 0 };

// What the last probe found: { frames: {probe: {kind: n | null}}, weights,
// limit, ms }.
export let probeResult = null;

// Run one probe in a fresh instance: the frames of the counted kind at the
// last check before the stack overflowed, or null when something else
// stopped it (the linear-memory stack's own limit, a resource limit).
async function probeRun(compiled, n, text, weights) {
    // A fresh copy of the glue per run: wasm-bindgen's glue holds one
    // instance, and a run's instance dies when its stack overflows.
    const glue = await import(`./neoscad_web.js?probe=${n}`);
    glue.initSync({ module: compiled });
    glue.setFrameLimit(0xffffffff);
    glue.setFrameWeights(...weights);
    const probe = new glue.Engine();
    const call = (request) => probe.handle(JSON.stringify(request), []);
    try {
        call({ id: 1, type: 'init', seed: 0 });
        call({ id: 2, type: 'open', path: '/probe.scad', text });
        call({ id: 3, type: 'run', path: '/probe.scad', mode: 'preview' });
        return null;
    } catch (e) {
        return isStackOverflow(e) ? glue.framesAtLastCheck() : null;
    }
}

// The weights and budget for this thread's stack, or null when no probe
// overflowed it (the defaults stand).
async function probeWeights(compiled) {
    const t0 = typeof performance !== 'undefined' ? performance.now() : 0;
    const frames = {};
    for (const [i, [probe, kind, weights]] of RUNS.entries()) {
        frames[probe] ??= {};
        // A core that runs statements on the heap has no module frames to
        // measure: those probes would only recurse to its depth limit.
        if (heapStatements() && (probe === 'children' || probe === 'transform')) {
            frames[probe][kind] = null;
            continue;
        }
        frames[probe][kind] = await probeRun(compiled, i, PROBES[probe], weights);
    }
    const ms = typeof performance !== 'undefined' ? Math.round(performance.now() - t0) : 0;
    probeResult = { frames, ms };
    const unit = PROBE_BUDGET / DEFAULT_LIMIT;
    const target = PROBE_BUDGET / PROBE_SHARE; // a probe's weighted frames at overflow
    const floor = (k) => Math.ceil(unit * DEFAULT_WEIGHTS[k]);
    const overflowed = Object.values(frames).some((f) => Object.values(f).some((n) => n));
    if (!overflowed) return null;
    // Expressions (and calls) from function recursion.
    const e = frames.fn.expr;
    const expr = Math.max(floor('expr'), e ? Math.ceil(target / e) : 0);
    // What each kind's own frames must carry once its expressions are paid.
    const rest = (f, kind) => {
        if (!f[kind]) return 0;
        const left = target - expr * (f.expr || 0);
        // Expressions alone already stop it: the kind's weight is the floor.
        return left > 0 ? left / f[kind] : 0;
    };
    const lc = Math.max(floor('lc'), Math.ceil(rest(frames.lc, 'lc')));
    // Statements from the `children()` chain, whose statements are all
    // cheap ones (a user module, `if`, `children()`); then a geometry
    // module's extra from what the transform chain's statements leave.
    const stmt = Math.max(floor('stmt'), Math.ceil(rest(frames.children, 'stmt')));
    const t = frames.transform;
    const geometry = t.geometry
        ? Math.max(0, Math.ceil((target - expr * (t.expr || 0) - stmt * (t.stmt || 0)) / t.geometry))
        : 0;
    probeResult.weights = { stmt, expr, call: 2 * expr, lc, geometry };
    probeResult.limit = PROBE_BUDGET;
    return probeResult;
}

async function compile(module) {
    if (module instanceof WebAssembly.Module) return module;
    if ((typeof URL !== 'undefined' && module instanceof URL) || typeof module === 'string') {
        try {
            return await WebAssembly.compileStreaming(fetch(module));
        } catch (_) {
            // A server that does not send `application/wasm` (the
            // streaming response is spent, so fetch it again).
            return WebAssembly.compile(await (await fetch(module)).arrayBuffer());
        }
    }
    return WebAssembly.compile(module);
}

// Load the module (`module` defaults to the .wasm beside this file; node
// passes its bytes), probe this thread's stack (`probe: false` skips it),
// and make the engine.
export async function start(module = new URL('./neoscad_web_bg.wasm', import.meta.url),
    { probe = true } = {}) {
    const compiled = await compile(module);
    const exports = await init({ module_or_path: compiled });
    memory = exports.memory;
    if (probe) {
        const found = await probeWeights(compiled);
        if (found) {
            const w = found.weights;
            setFrameWeights(w.stmt, w.expr, w.call, w.lc, w.geometry);
            setFrameLimit(found.limit);
        }
    }
    engine = new Engine();
}

// One request, synchronously: `{ reply, transfer }`, the reply envelope
// and the buffers in it to transfer. The request that traps gets a
// `crashed` error with `justCrashed` set; every later one a `crashed`
// error too, since the instance is unusable after a trap.
export function handle(request) {
    if (crashed) {
        return { reply: { id: request.id, ok: false,
            error: { kind: 'crashed', message: 'the engine crashed; respawn the worker' } },
            transfer: [] };
    }
    try {
        const inputs = [];
        const plain = extractBuffers(request, inputs);
        const out = engine.handle(JSON.stringify(plain), inputs);
        const buffers = [];
        for (let i = 1; i < out.length; i++) buffers.push(out[i].buffer);
        return { reply: insertBuffers(JSON.parse(out[0]), buffers), transfer: buffers };
    } catch (e) {
        // A trap (a panic, out of memory, the engine's stack exhausted).
        crashed = true;
        let message = trapMessage(e);
        try {
            message = lastPanic() || message;
        } catch (_) {
            // The instance could not even answer that.
        }
        return { reply: { id: request.id, ok: false, error: { kind: 'crashed', message } },
            transfer: [], justCrashed: true };
    }
}

// In a worker: answer the page's messages.
if (typeof WorkerGlobalScope !== 'undefined' && globalThis instanceof WorkerGlobalScope) {
    const queue = [];
    let ready = false;
    const answer = (request) => {
        const { reply, transfer, justCrashed } = handle(request);
        globalThis.postMessage(reply, transfer);
        if (justCrashed) globalThis.postMessage({ type: 'crashed', message: reply.error.message });
    };
    // Requests that arrive while the module loads wait for it.
    globalThis.onmessage = (event) => (ready ? answer(event.data) : queue.push(event.data));
    start()
        .then(() => {
            ready = true;
            globalThis.postMessage({ type: 'ready', version: '0.1.0', frameLimit: frameLimit(),
                frameWeights: Array.from(frameWeights()), probe: probeResult });
            while (queue.length) answer(queue.shift());
        })
        .catch((e) => globalThis.postMessage({ type: 'crashed', message: String(e) }));
}
