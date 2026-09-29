// The engine client: the main thread's end of the worker.
//
// It owns the worker's life. The worker is single-threaded and a run in
// it cannot be interrupted, so cancelling is terminating it; a panic or
// OOM traps the wasm instance, which is also only fixed by a new worker.
// Either way the client respawns it and replays what the old one knew
// (init, each open document's current text, the library files added), so
// the page carries on with "engine restarted" rather than a dead engine.
//
// Runs are coalesced: at most one is in flight, and a newer request
// replaces any that is waiting (its promise resolves `{superseded: true}`).
// A run that has been going for longer than `staleMs` while a newer one
// waits is abandoned by respawning: typing into a model that takes
// seconds to evaluate would otherwise queue behind every stale run.
//
// The client knows the envelope only through protocol.js.

import { Requests } from "./protocol.js";

export class EngineRestarted extends Error {
  constructor(reason) {
    super(`engine restarted: ${reason}`);
    this.name = "EngineRestarted";
    this.reason = reason;
  }
}

export class EngineError extends Error {
  constructor(error) {
    super(error?.message ?? String(error));
    this.name = "EngineError";
    this.code = error?.code ?? null;
  }
}

export class EngineClient {
  /// `spawn()` makes a worker (anything with postMessage, terminate and
  /// addEventListener for "message" and "error"). `timers` defaults to the
  /// global ones; the tests pass their own.
  constructor({ spawn, staleMs = 3000, initOptions = {}, now = () => performance.now(), timers = globalThis } = {}) {
    this.spawn = spawn;
    this.staleMs = staleMs;
    this.initOptions = initOptions;
    this.now = now;
    this.timers = timers;
    this.listeners = new Map();
    this.worker = null;
    this.nextId = 1;
    this.pending = new Map(); // id -> {resolve, reject}
    this.documents = new Map(); // path -> text, replayed on respawn
    this.files = new Map(); // path -> {path, bytes|text}, replayed on respawn
    this.inFlight = null; // {id, started, request}
    this.waiting = null; // {request, resolve, reject}
    this.staleTimer = null;
    this.busy = 0;
    this.info = null;
    this.restarts = 0;
    this.ready = null;
  }

  // --- Events: "status" {state, message}, "lsp" message, "info" info ---

  on(event, fn) {
    if (!this.listeners.has(event)) this.listeners.set(event, new Set());
    this.listeners.get(event).add(fn);
    return () => this.listeners.get(event).delete(fn);
  }

  emit(event, value) {
    for (const fn of this.listeners.get(event) ?? []) fn(value);
  }

  status(state, message = "") {
    this.emit("status", { state, message });
  }

  // --- Life -------------------------------------------------------------

  /// Spawn the worker and initialise it; resolves to the worker's `init`
  /// answer (its version and what it supports).
  start() {
    this.worker = this.spawn();
    const worker = this.worker;
    worker.addEventListener("message", (e) => {
      if (worker === this.worker) this.receive(e.data);
    });
    worker.addEventListener("error", (e) => {
      if (worker !== this.worker) return;
      e.preventDefault?.();
      this.restart(`worker error: ${e.message ?? "unknown"}`);
    });
    worker.addEventListener("messageerror", () => {
      if (worker === this.worker) this.restart("a message could not be read");
    });
    this.ready = this.send(Requests.init(this.initOptions)).then(async (info) => {
      this.info = info;
      this.emit("info", info);
      // Replay what the previous worker had; a first start has nothing.
      if (this.files.size) await this.send(Requests.addFiles([...this.files.values()]));
      for (const [path, text] of this.documents) await this.send(Requests.open(path, text));
      return info;
    });
    return this.ready;
  }

  /// Terminate the worker and start another, failing everything the old
  /// one owed with `EngineRestarted`.
  restart(reason) {
    this.worker?.terminate();
    this.worker = null;
    this.restarts += 1;
    // A worker that dies as it starts (its script or wasm fails to load)
    // would otherwise be respawned forever, so a burst of restarts stops.
    const t = this.now();
    this.recent = (this.recent ?? []).filter((x) => t - x < 10000).concat(t);
    const owed = [...this.pending.values()];
    this.pending.clear();
    this.inFlight = null;
    this.busy = 0;
    this.clearStale();
    for (const p of owed) p.reject(new EngineRestarted(reason));
    if (this.recent.length > 4) {
      this.status("failed", `the engine keeps stopping (${reason})`);
      this.ready = Promise.reject(new EngineRestarted(reason));
      this.ready.catch(() => {});
      return this.ready;
    }
    this.status("restarted", reason);
    const started = this.start();
    // A run that was waiting goes to the new worker once it is ready.
    started.then(() => this.pump()).catch(() => {});
    return started;
  }

  dispose() {
    this.worker?.terminate();
    this.worker = null;
    this.clearStale();
  }

  // --- Requests -----------------------------------------------------------

  send(request, transfer = []) {
    if (!this.worker) return Promise.reject(new EngineRestarted("no worker"));
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.busy += 1;
      if (this.busy === 1) this.status("working");
      this.worker.postMessage({ id, ...request }, transfer);
    });
  }

  /// A request once the worker is initialised.
  async request(request, transfer = []) {
    await this.ready;
    return this.send(request, transfer);
  }

  receive(msg) {
    if (msg == null || typeof msg !== "object") return;
    if (msg.id != null && this.pending.has(msg.id)) {
      const p = this.pending.get(msg.id);
      this.pending.delete(msg.id);
      this.busy = Math.max(0, this.busy - 1);
      if (this.busy === 0) this.status("idle");
      if (msg.ok) p.resolve(msg.result);
      else p.reject(new EngineError(msg.error));
      return;
    }
    switch (msg.type) {
      case "lsp":
        this.emit("lsp", msg.message);
        break;
      case "fatal":
        this.restart(msg.message ?? "the engine stopped");
        break;
      case "log":
        this.emit("log", msg);
        break;
    }
  }

  // --- Documents and files, remembered for a respawn ----------------------

  open(path, text) {
    this.documents.set(path, text);
    return this.request(Requests.open(path, text));
  }

  edit(path, version, edits, text) {
    this.documents.set(path, text);
    return this.request(Requests.edit(path, version, edits, text));
  }

  close(path) {
    this.documents.delete(path);
  }

  /// Files copied into the worker's file system (not transferred: the
  /// client keeps its copy to give a respawned worker).
  addFiles(files) {
    for (const f of files) this.files.set(f.path, f);
    return this.request(Requests.addFiles(files));
  }

  hasFile(path) {
    return this.files.has(path);
  }

  lsp(message) {
    // Notifications carry no id and get no answer. Before the worker
    // exists (a respawn in progress) the message is dropped; the language
    // client re-sends the document on its next change.
    this.worker?.postMessage({ type: "lsp", message });
  }

  // --- Runs, coalesced ------------------------------------------------------

  /// A run (Requests.run's fields). Resolves to the raw result, or to
  /// `{superseded: true}` when a newer run replaced this one first.
  run(request) {
    return new Promise((resolve, reject) => {
      if (this.waiting) this.waiting.resolve({ superseded: true });
      this.waiting = { request, resolve, reject };
      this.pump();
      this.armStale();
    });
  }

  async pump() {
    if (this.inFlight || !this.waiting || !this.worker) return;
    const { request, resolve, reject } = this.waiting;
    this.waiting = null;
    const flight = { started: this.now(), request };
    this.inFlight = flight;
    try {
      await this.ready;
      resolve(await this.send(request));
    } catch (e) {
      if (e instanceof EngineRestarted) resolve({ superseded: true, restarted: true });
      else reject(e);
    } finally {
      if (this.inFlight === flight) this.inFlight = null;
      this.clearStale();
      this.pump();
    }
  }

  armStale() {
    this.clearStale();
    if (!this.inFlight || !this.waiting) return;
    const left = Math.max(0, this.staleMs - (this.now() - this.inFlight.started));
    this.staleTimer = this.timers.setTimeout(() => {
      this.staleTimer = null;
      if (this.inFlight && this.waiting) this.restart("a newer run replaced a long one");
    }, left);
  }

  clearStale() {
    if (this.staleTimer != null) this.timers.clearTimeout(this.staleTimer);
    this.staleTimer = null;
  }

  /// Stop whatever the engine is doing (the Cancel button).
  cancel() {
    if (this.waiting) {
      this.waiting.resolve({ superseded: true });
      this.waiting = null;
    }
    if (this.busy > 0) return this.restart("cancelled");
    return Promise.resolve(this.info);
  }
}
