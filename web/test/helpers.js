// Stand-ins for the tests: a worker that runs MockCore in-process, and
// timers and a clock the tests move by hand.

import { MockCore, MockCrash } from "../src/engine/mock-core.js";

/// A Worker over MockCore, with mock-worker.js's envelope (the real glue's:
/// a `crashed` reply and message when an instance traps, and `crashed`
/// replies after). Replies arrive asynchronously, as a worker's do. With
/// `hold` set, runs wait until `release()`: a long run.
export class FakeWorker {
  constructor({ hold = false } = {}) {
    this.core = new MockCore();
    this.listeners = { message: [], error: [], messageerror: [] };
    this.terminated = false;
    this.hold = hold;
    this.held = [];
    this.received = [];
    this.crashed = false;
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
    if (this.crashed) {
      this.emit("message", { id: msg.id, ok: false, error: { kind: "crashed", message: "respawn" } });
      return;
    }
    try {
      const out = this.core.handle(msg);
      this.emit("message", { id: msg.id, ok: true, result: out.result });
    } catch (e) {
      if (e instanceof MockCrash) {
        this.crashed = true;
        this.emit("message", { id: msg.id, ok: false, error: { kind: "crashed", message: e.message } });
        this.emit("message", { type: "crashed", message: e.message });
      } else {
        this.emit("message", { id: msg.id, ok: false, error: { kind: e.kind ?? "failed", message: e.message } });
      }
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
