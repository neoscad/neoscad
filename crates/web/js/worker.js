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

import init, { Engine, lastPanic } from './neoscad_web.js';

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
// passes its bytes) and make the engine.
export async function start(module = new URL('./neoscad_web_bg.wasm', import.meta.url)) {
    const compiled = await compile(module);
    const exports = await init({ module_or_path: compiled });
    memory = exports.memory;
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
            globalThis.postMessage({ type: 'ready', version: '0.1.0' });
            while (queue.length) answer(queue.shift());
        })
        .catch((e) => globalThis.postMessage({ type: 'crashed', message: String(e) }));
}
