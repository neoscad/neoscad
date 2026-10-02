//! The wasm-bindgen layer the worker's JavaScript calls (wasm32 only): an
//! [`Engine`] around a [`Worker`] whose clock is `performance.now()`, and
//! the panic message of a crashed instance.
//!
//! A panic aborts on wasm32 (the `web` profile builds with
//! `panic = "abort"`, and the target cannot unwind anyway), which traps the
//! instance: the JavaScript call throws a `RuntimeError` and the instance
//! is unusable after it. The panic hook runs before the trap, so it keeps
//! the message for [`last_panic`], which the worker reads (a small call
//! that touches no state the panic could have broken) to tell the page
//! why the engine restarted.

use std::sync::{Arc, Mutex};

use wasm_bindgen::prelude::*;

use crate::Worker;

#[wasm_bindgen]
extern "C" {
    /// `performance.now()`: a monotonic clock in milliseconds, in workers
    /// and in node alike.
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn performance_now() -> f64;

    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(message: &str);
}

static LAST_PANIC: Mutex<Option<String>> = Mutex::new(None);

/// Installs the panic hook once the module is instantiated.
#[wasm_bindgen(start)]
fn start() {
    std::panic::set_hook(Box::new(|info| {
        let message = info.to_string();
        console_error(&format!("neoscad: {message}"));
        if let Ok(mut last) = LAST_PANIC.lock() {
            *last = Some(message);
        }
    }));
}

/// The message of the panic that crashed the instance, if one did.
#[wasm_bindgen(js_name = lastPanic)]
pub fn last_panic() -> Option<String> {
    LAST_PANIC.lock().ok().and_then(|l| l.clone())
}

/// Sets the evaluator's frame budget for this instance (0: the default,
/// `eval::recursion::DEFAULT_FRAME_LIMIT`), as the worker's stack probe
/// measured it; see `eval::recursion::set_default_frame_limit`.
#[wasm_bindgen(js_name = setFrameLimit)]
pub fn set_frame_limit(limit: u32) {
    eval::recursion::set_default_frame_limit(limit);
}

/// Sets the frame weights (statement, expression, call, comprehension,
/// geometry;
/// 0 counts nothing for a kind, 4294967295 keeps its default) the worker's
/// stack probe calibrated; see
/// `eval::recursion::set_frame_weights`.
#[wasm_bindgen(js_name = setFrameWeights)]
pub fn set_frame_weights(
    statement: u32,
    expression: u32,
    call: u32,
    comprehension: u32,
    geometry: u32,
) {
    eval::recursion::set_frame_weights(eval::recursion::FrameWeights {
        statement,
        expression,
        call,
        comprehension,
        geometry,
    });
}

/// The frame weights in effect: `[statement, expression, call,
/// comprehension, geometry]`.
#[wasm_bindgen(js_name = frameWeights)]
pub fn frame_weights() -> Vec<u32> {
    let w = eval::recursion::frame_weights();
    vec![
        w.statement,
        w.expression,
        w.call,
        w.comprehension,
        w.geometry,
    ]
}

/// The frame budget evaluations in this instance start with.
#[wasm_bindgen(js_name = frameLimit)]
pub fn frame_limit() -> u32 {
    eval::recursion::default_frame_limit()
}

/// The frames in use at the evaluator's last recursion check. Read from an
/// instance whose stack overflowed: it is a load of one static, so it
/// touches nothing the trap could have left half-written.
#[wasm_bindgen(js_name = framesAtLastCheck)]
pub fn frames_at_last_check() -> u32 {
    eval::recursion::frames_at_last_check()
}

/// Whether this core runs statements and calls on the heap (the
/// `heap-eval` feature): a recursion then takes no stack past a few native
/// call levels, and every one of the worker's stack probes would only run
/// to the counted depth limit, which in WebKit's cold tiers took 40 s at
/// start-up, so the worker skips them.
#[wasm_bindgen(js_name = heapStatements)]
pub fn heap_statements() -> bool {
    cfg!(feature = "heap-eval")
}

/// The worker's engine: `handle` one request at a time.
#[wasm_bindgen]
#[derive(Debug)]
pub struct Engine {
    worker: Worker,
}

#[wasm_bindgen]
impl Engine {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Engine {
        let clock: crate::Clock = Arc::new(performance_now);
        let probe: session::MemoryProbe = Arc::new(crate::heap::peak);
        Engine {
            worker: Worker::new(Some(clock)).with_probe(Some(probe)),
        }
    }

    /// Handle one request: `request` is its JSON text and `buffers` the
    /// `ArrayBuffer`s or typed arrays its `{"$buffer": n}` fields name.
    /// Returns `[replyJson, ...buffers]`, each buffer a `Uint8Array` of
    /// its own `ArrayBuffer`, ready to transfer.
    pub fn handle(&mut self, request: &str, buffers: js_sys::Array) -> js_sys::Array {
        let inputs = buffers
            .iter()
            .map(|b| js_sys::Uint8Array::new(&b).to_vec())
            .collect();
        // The memory probe reads this request's peak (`heap.rs`).
        crate::heap::reset_peak();
        let reply = self.worker.handle(request, inputs);
        let out = js_sys::Array::new();
        out.push(&JsValue::from_str(&reply.json));
        for b in reply.buffers {
            out.push(&js_sys::Uint8Array::from(&b[..]));
        }
        out
    }
}
