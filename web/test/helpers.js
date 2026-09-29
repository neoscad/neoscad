// Stand-ins for the tests: a worker that runs MockCore in-process, and
// timers and a clock the tests move by hand.

import { MockCore, MockCrash } from "../src/engine/mock-core.js";

/// A Worker over MockCore. Replies arrive asynchronously, as a worker's
/// do. With `hold` set, requests wait until `release()`: a long run.
export class FakeWorker {
  constructor({ hold = false } = {}) {
    this.core = new MockCore();
    this.listeners = { message: [], error: [], messageerror: [] };
    this.terminated = false;
    this.hold = hold;
    this.held = [];
    this.received = [];
  }

  addEventListener(type, fn) {
    this.listeners[type].push(fn);
  }

  emit(type, data) {
    if (this.terminated) return;
    for (const fn of this.listeners[type]) fn(type === "message" ? { data } : data);
  }

  postMessage(msg) {
    if (this.terminated) return;
    this.received.push(msg);
    if (this.hold && msg.type === "run") this.held.push(msg);
    else setImmediate(() => this.answer(msg));
  }

  release() {
    for (const m of this.held.splice(0)) setImmediate(() => this.answer(m));
  }

  answer(msg) {
    try {
      const out = this.core.handle(msg, (n) => this.emit("message", n));
      if (msg.id == null || out === null) return;
      this.emit("message", { id: msg.id, ok: true, result: out.result });
    } catch (e) {
      if (e instanceof MockCrash) this.emit("message", { type: "fatal", message: e.message });
      else this.emit("message", { id: msg.id, ok: false, error: { message: e.message } });
    }
  }

  terminate() {
    this.terminated = true;
  }
}

/// setTimeout / clearTimeout and a clock under the test's control.
export class FakeTime {
  constructor() {
    this.t = 0;
    this.timers = new Map();
    this.next = 1;
  }
  now = () => this.t;
  setTimeout = (fn, ms) => {
    const id = this.next++;
    this.timers.set(id, { at: this.t + ms, fn });
    return id;
  };
  clearTimeout = (id) => {
    this.timers.delete(id);
  };
  advance(ms) {
    this.t += ms;
    for (const [id, { at, fn }] of [...this.timers]) {
      if (at <= this.t) {
        this.timers.delete(id);
        fn();
      }
    }
  }
}

export const tick = () => new Promise((r) => setImmediate(r));

export async function settle(n = 5) {
  for (let i = 0; i < n; i++) await tick();
}
