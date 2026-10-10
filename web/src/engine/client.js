// The engine client: the main thread's end of the worker
// (docs/web-protocol.md).
//
// It owns the worker's life. The worker is single-threaded and a run in
// it cannot be interrupted, so cancelling is terminating it; a panic or
// OOM traps the wasm instance (the worker replies `crashed` and posts a
// `crashed` message), which is also only fixed by a new worker. Either way
// the client respawns it and replays what the old one knew, in the
// protocol's order: `init` with the same seed, every `addFiles`, an `open`
// of each document with its current text, and the language client's
// `initialize`, `initialized` and `didOpen`s. The page carries on with
// "engine restarted" rather than a dead engine. Measurement handles do
// not survive; the page measures again.
//
// Runs are coalesced: at most one is in flight, and a newer request
// replaces any that is waiting (its promise resolves `{superseded: true}`).
// A run that has been going for longer than `staleMs` while a newer one
// waits is abandoned by respawning: typing into a model that takes
// seconds to evaluate would otherwise queue behind every stale run.
//
// The client knows the wire only through protocol.js.

import { ErrorKind, Requests, editorEdits, offsetAt } from "./protocol.js";

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
    this.kind = error?.kind ?? ErrorKind.failed;
  }
}

/// The id the client gives the language server's `initialize` when it
/// replays it to a new worker: its answer is the client's, not the
/// editor's, whose own request was answered by the old worker.
const REPLAY_ID = "neoscad-replay-initialize";

export class EngineClient {
  /// `spawn()` makes a worker (anything with postMessage, terminate and
  /// addEventListener for "message" and "error"). `seed` is the seed of
  /// unseeded `rands()`, the same for every worker this client starts.
  /// `timers` defaults to the global ones; the tests pass their own.
  constructor({ spawn, staleMs = 3000, seed = 0, now = () => performance.now(), timers = globalThis } = {}) {
    this.spawn = spawn;
    this.staleMs = staleMs;
    this.seed = seed >>> 0;
    this.now = now;
    this.timers = timers;
    this.listeners = new Map();
    this.worker = null;
    this.nextId = 1;
    this.pending = new Map(); // id -> {resolve, reject, quiet}
    this.documents = new Map(); // path -> text, replayed on respawn
    this.fileRequests = []; // addFiles requests, replayed on respawn
    this.libraries = new Set(); // names of the lazy libraries added
    this.lspInit = []; // the language client's initialize and initialized
    this.lspDocs = new Map(); // uri -> {languageId, version, text}
    this.extensions = []; // the language server's `enable` (setExtensions)
    this.inFlight = null; // {started, request}
    this.waiting = null; // {request, resolve, reject}
    this.staleTimer = null;
    this.busy = 0;
    this.info = null;
    this.defaults = null;
    this.restarts = 0;
    // Requests made before `start` (the editor's language client starts
    // first) wait for the first worker.
    this.ready = new Promise((resolve) => (this.firstStart = resolve));
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
  /// answer ({version, limits, libraryDirs}). A respawn (`replay`) also
  /// gives the new worker what the old one had been told.
  start(replay = false) {
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
    // The worker queues requests that arrive while its module loads, so
    // `init` goes now rather than after `ready`.
    const ready = this.send(Requests.init(this.seed)).then(async (info) => {
      this.info = info;
      this.defaults ??= await this.send(Requests.defaults());
      for (const r of this.fileRequests) await this.send(r);
      for (const [path, text] of this.documents) await this.send(Requests.open(path, text));
      // Only a new worker's server needs the language client's setup
      // replayed: on the first start the client's own messages are still
      // queued behind `ready`, and a second `initialize` would be refused.
      if (replay) await this.replayLanguage();
      this.emit("info", info);
      return info;
    });
    this.ready = ready;
    this.firstStart?.(ready);
    this.firstStart = null;
    return ready;
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
    const started = this.start(true);
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

  /// Post a request now. `quiet` requests (the language server's) do not
  /// count as the engine working: the status and the Cancel button follow
  /// runs and panels, not hovers.
  send(request, { transfer = [], quiet = false } = {}) {
    if (!this.worker) return Promise.reject(new EngineRestarted("no worker"));
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject, quiet });
      if (!quiet) {
        this.busy += 1;
        if (this.busy === 1) this.status("working");
      }
      this.worker.postMessage({ id, ...request }, transfer);
    });
  }

  /// A request once the worker is initialised.
  async request(request, options) {
    await this.ready;
    return this.send(request, options);
  }

  receive(msg) {
    if (msg == null || typeof msg !== "object") return;
    if (msg.id != null && this.pending.has(msg.id)) {
      const p = this.pending.get(msg.id);
      this.pending.delete(msg.id);
      if (!p.quiet) {
        this.busy = Math.max(0, this.busy - 1);
        if (this.busy === 0) this.status("idle");
      }
      if (msg.ok) {
        p.resolve(msg.result);
      } else if (msg.error?.kind === ErrorKind.crashed || msg.error?.kind === ErrorKind.panicked) {
        // The instance is unusable (a `crashed` message follows, from a
        // worker this client will have dropped by then).
        const reason = `crashed: ${msg.error.message}`;
        p.reject(new EngineRestarted(reason));
        this.restart(reason);
      } else {
        p.reject(new EngineError(msg.error));
      }
      return;
    }
    switch (msg.type) {
      case "ready":
        this.emit("ready", msg);
        break;
      case "crashed":
        this.restart(`crashed: ${msg.message ?? "the engine stopped"}`);
        break;
    }
  }

  // --- Documents and files, remembered for a respawn ----------------------

  open(path, text) {
    this.documents.set(path, text);
    return this.request(Requests.open(path, text));
  }

  /// The editor's edits (`[from, to, insert]`, UTF-16 offsets) to the
  /// document at `path`, sent as the wire's positions against the
  /// client's copy of its text.
  edit(path, edits) {
    const { edits: wire, text } = editorEdits(this.documents.get(path) ?? "", edits);
    this.documents.set(path, text);
    return this.request(Requests.edit(path, wire));
  }

  close(path) {
    if (!this.documents.delete(path)) return Promise.resolve({ closed: false });
    return this.request(Requests.close(path)).catch(() => ({ closed: false }));
  }

  /// Files or an archive copied into the worker's memory (not
  /// transferred: the client keeps its copy to give a respawned worker).
  /// `library` names the lazy library the request adds, if it is one.
  addFiles(request, library = null) {
    const r = Requests.addFiles(request);
    this.fileRequests.push(r);
    if (library) this.libraries.add(library);
    return this.request(r);
  }

  hasLibrary(name) {
    return this.libraries.has(name);
  }

  // --- The language server --------------------------------------------------

  /// One JSON-RPC message from the editor's language client. The server's
  /// replies come back as the request's result and go out as "lsp"
  /// events; the messages that set a server up are remembered for the
  /// next worker.
  lsp(message) {
    this.ready
      .then(() => {
        // Remembered as it is sent, not before: a message still waiting
        // for a respawned worker must not be in the state replayed to it,
        // or the server would see its change twice.
        this.remember(message);
        return this.send(Requests.lsp(message, this.extensions), { quiet: true });
      })
      .then((r) => {
        for (const m of r?.messages ?? []) this.emit("lsp", m);
      })
      .catch(() => {
        // A respawn dropped it; the replay gives the new server the
        // client's state, and a request the client made is retried by
        // the user's next action.
      });
  }

  remember(message) {
    let m;
    try {
      m = JSON.parse(message);
    } catch {
      return;
    }
    const doc = m.params?.textDocument;
    switch (m.method) {
      case "initialize":
        this.lspInit = [{ ...m, id: REPLAY_ID }];
        this.lspDocs.clear();
        break;
      case "initialized":
        this.lspInit = [this.lspInit[0], m].filter(Boolean);
        break;
      case "textDocument/didOpen":
        this.lspDocs.set(doc.uri, { languageId: doc.languageId, version: doc.version, text: doc.text });
        break;
      case "textDocument/didChange": {
        const d = this.lspDocs.get(doc.uri);
        if (!d) break;
        for (const c of m.params.contentChanges ?? []) {
          if (!c.range) d.text = c.text;
          else d.text = d.text.slice(0, offsetAt(d.text, c.range.start)) + c.text + d.text.slice(offsetAt(d.text, c.range.end));
        }
        d.version = doc.version;
        break;
      }
      case "textDocument/didClose":
        this.lspDocs.delete(doc.uri);
        break;
    }
  }

  /// Give a new worker's language server what the old one had been told.
  async replayLanguage() {
    if (!this.lspInit.length) return;
    for (const m of this.lspInit) await this.send(Requests.lsp(JSON.stringify(m), this.extensions), { quiet: true });
    for (const [uri, d] of this.lspDocs) {
      const open = {
        jsonrpc: "2.0",
        method: "textDocument/didOpen",
        params: { textDocument: { uri, languageId: d.languageId, version: d.version, text: d.text } },
      };
      await this.send(Requests.lsp(JSON.stringify(open), this.extensions), { quiet: true });
    }
  }

  /// The language extensions the page's documents run with (`--enable`
  /// names). Every language server message carries them from now on, and
  /// a respawned worker's replay too: runs also tell the server theirs,
  /// but a toggle changed before the next run, or a heavy example not run
  /// yet, would otherwise complete with the last run's set (or none).
  setExtensions(names) {
    this.extensions = [...names];
  }

  // --- Runs, coalesced ------------------------------------------------------

  /// A run (Requests.run's result). Resolves to the raw result, or to
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
      flight.started = this.now();
      resolve(await this.send(request));
    } catch (e) {
      if (e instanceof EngineRestarted) resolve({ superseded: true, restarted: true });
      else if (e instanceof EngineError && e.kind === ErrorKind.cancelled) resolve({ superseded: true });
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
