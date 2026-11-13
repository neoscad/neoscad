// The package's queue in front of the engine client (web/src/engine/
// client.js), so several models (elements, `render()` handles) can share
// one worker without spoiling each other's work.
//
// The client was made for one page with one document: a single
// latest-wins slot, a stale-run kill that fires for any waiting run, and a
// cancel that restarts the worker and fails everything owed. Shared as
// is, one element's edit would replace another's waiting run, and one
// element's cancel would kill the run of whichever element was busy.
// Here every job carries a key (a model) and:
//
//   - jobs start one at a time, first in, first out across keys;
//   - a newer job of a key replaces its waiting one (latest wins per
//     key; the replaced one resolves `{superseded: true}`), keeping its
//     place in the queue so a busy model does not starve the others;
//   - a key's job that has run longer than `staleMs` while a newer one of
//     the same key waits is abandoned by restarting the worker (off by
//     default: Infinity);
//   - aborting a waiting job drops it; aborting the job in flight restarts
//     the worker (a run cannot be interrupted otherwise), and the other
//     keys' jobs, never sent, simply go next;
//   - a job that fails because the worker restarted is retried once,
//     unless the restart was its own doing (a crash in its run): a model
//     that crashes the engine would crash it again.
//
// A job is `async (client) => value`, so a model's whole turn (update the
// document, add changed files, fetch a library, run, run again with
// fonts) happens without another key's requests in between.

import { EngineRestarted } from "../../src/engine/client.js";

export class AbortError extends Error {
  constructor(message = "The run was aborted.") {
    super(message);
    this.name = "AbortError";
  }
}

export class Scheduler {
  /// `client`: an EngineClient (started by the caller). `staleMs`: see above.
  constructor(client, { staleMs = Infinity, timers = globalThis, now = () => performance.now() } = {}) {
    this.client = client;
    this.staleMs = staleMs;
    this.timers = timers;
    this.now = now;
    this.queue = []; // [{key, job, resolve, reject, signal, onAbort}]
    this.current = null; // {key, started, killed: null | "abort" | "stale", ...}
    this.staleTimer = null;
    this.closed = false;
  }

  /// Queue `job` under `key`. With `coalesce` (the default) it replaces a
  /// waiting job of the same key. Resolves to the job's value, or
  /// `{superseded: true}`; rejects with AbortError when `signal` aborts.
  submit(key, job, { signal = null, coalesce = true } = {}) {
    return new Promise((resolve, reject) => {
      if (this.closed) return reject(new Error("the engine was disposed"));
      if (signal?.aborted) return reject(new AbortError());
      const entry = { key, job, resolve, reject, signal, onAbort: null };
      const i = coalesce ? this.queue.findIndex((e) => e.key === key) : -1;
      if (i >= 0) {
        this.detach(this.queue[i]);
        this.queue[i].resolve({ superseded: true });
        this.queue[i] = entry;
      } else {
        this.queue.push(entry);
      }
      if (signal) {
        entry.onAbort = () => this.abort(entry);
        signal.addEventListener("abort", entry.onAbort, { once: true });
      }
      this.armStale();
      this.pump();
    });
  }

  detach(entry) {
    if (entry.signal && entry.onAbort) entry.signal.removeEventListener("abort", entry.onAbort);
    entry.onAbort = null;
  }

  abort(entry) {
    const i = this.queue.indexOf(entry);
    if (i >= 0) {
      this.queue.splice(i, 1);
      this.detach(entry);
      entry.reject(new AbortError());
      this.armStale();
      return;
    }
    if (this.current?.entry === entry && !this.current.killed) {
      this.current.killed = "abort";
      this.client.restart("aborted").catch(() => {});
    }
  }

  async pump() {
    if (this.current || !this.queue.length || this.closed) return;
    const entry = this.queue.shift();
    const flight = { entry, key: entry.key, started: this.now(), killed: null };
    this.current = flight;
    this.armStale();
    let retried = false;
    try {
      for (;;) {
        try {
          const value = await entry.job(this.client);
          // Killed while it was between requests (fetching a library):
          // what it got is no longer wanted.
          if (flight.killed === "abort") throw new AbortError();
          this.detach(entry);
          entry.resolve(flight.killed === "stale" ? { superseded: true } : value);
          break;
        } catch (e) {
          if (flight.killed === "abort") throw new AbortError();
          if (flight.killed === "stale") {
            entry.resolve({ superseded: true });
            break;
          }
          // A restart this job did not cause (a worker error while a
          // respawn started, another host of the client restarting it):
          // try once more. A crash in its own run is its own.
          const own = e instanceof EngineRestarted && /^crashed/.test(e.reason ?? "");
          if (e instanceof EngineRestarted && !own && !retried && !this.closed) {
            retried = true;
            continue;
          }
          throw e;
        }
      }
    } catch (e) {
      this.detach(entry);
      entry.reject(e);
    } finally {
      if (this.current === flight) this.current = null;
      this.armStale();
      this.pump();
    }
  }

  armStale() {
    if (this.staleTimer != null) this.timers.clearTimeout(this.staleTimer);
    this.staleTimer = null;
    const c = this.current;
    if (!c || c.killed || !Number.isFinite(this.staleMs)) return;
    if (!this.queue.some((e) => e.key === c.key)) return;
    const left = Math.max(0, this.staleMs - (this.now() - c.started));
    this.staleTimer = this.timers.setTimeout(() => {
      this.staleTimer = null;
      if (this.current === c && !c.killed && this.queue.some((e) => e.key === c.key)) {
        c.killed = "stale";
        this.client.restart("a newer run replaced a long one").catch(() => {});
      }
    }, left);
  }

  /// Drop every waiting job of `key` (a model being disposed).
  forget(key) {
    for (const e of this.queue.filter((x) => x.key === key)) {
      this.detach(e);
      e.resolve({ superseded: true });
    }
    this.queue = this.queue.filter((x) => x.key !== key);
  }

  dispose() {
    this.closed = true;
    if (this.staleTimer != null) this.timers.clearTimeout(this.staleTimer);
    for (const e of this.queue.splice(0)) {
      this.detach(e);
      e.reject(new Error("the engine was disposed"));
    }
  }
}
