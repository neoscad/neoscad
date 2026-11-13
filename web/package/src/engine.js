// The package's engine: one worker (the wasm core) behind the demo's
// engine client, the per-key queue (scheduler.js) and models.
//
// A model is one document the worker keeps: its own directory
// `/doc/m<n>/`, the document in it named after the source (default
// main.scad), and the `files` its includes read, relative to that
// directory. The first run opens the document and later ones update it,
// so a respawned worker gets it back (the client replays documents and
// files); `dispose` closes it. Elements and `render()` handles each own a
// model; `engine.run` and friends use one scratch model and are never
// coalesced.

import { EngineRestarted } from "../../src/engine/client.js";
import { createEngine as createClient, ensureFonts, ensureLibraries } from "../../src/engine/index.js";
import { Requests, parameterGroups, runOptions } from "../../src/engine/protocol.js";
import { defaultAssets, resolveAssets, startFailure, workerSpawner } from "./assets.js";
import { ExportError, shapeExport, shapeResult } from "./result.js";
import { AbortError, Scheduler } from "./scheduler.js";

/// The package's version, set by build.mjs; the engine inside it reports
/// its own in `init` (the workspace version both come from).
export const VERSION = typeof __NEOSCAD_VERSION__ !== "undefined" ? __NEOSCAD_VERSION__ : "dev";

/// 3MF and PDF metadata date: the protocol's default, so an export's bytes
/// are the same on every visit (the demo passes the current time).
export const EPOCH = "1970-01-01T00:00:00Z";

/// The engine could not start, or stopped for good.
export class EngineStartError extends Error {
  constructor(message) {
    super(message);
    this.name = "EngineStartError";
  }
}

/// A file name or a `files` key: relative, `/`-separated, no `..`, no
/// empty segments. Throws a TypeError otherwise, since a path that leaves
/// the model's directory would write over another model's files.
export function checkRelativePath(p) {
  const s = String(p);
  if (!s || s.startsWith("/") || s.includes("\\") || s.split("/").some((seg) => seg === "" || seg === "." || seg === "..")) {
    throw new TypeError(`NeoSCAD: "${s}" is not a relative path inside the model's directory`);
  }
  return s;
}

/// The `enable` option as the run's `{parts, enable}`: `part` is the
/// wire's `parts` flag, the rest go as `--enable` names.
export function splitEnable(enable) {
  const names = typeof enable === "string" ? enable.split(/[\s,]+/) : [...(enable ?? [])];
  const clean = names.map((n) => String(n).trim()).filter(Boolean);
  return { parts: clean.includes("part"), enable: clean.filter((n) => n !== "part") };
}

const sameData = (a, b) => {
  if (typeof a === "string" || typeof b === "string") return a === b;
  const x = a instanceof Uint8Array ? a : new Uint8Array(a);
  const y = b instanceof Uint8Array ? b : new Uint8Array(b);
  if (x.length !== y.length) return false;
  for (let i = 0; i < x.length; i++) if (x[i] !== y[i]) return false;
  return true;
};

/// Start an engine. Options (all optional):
///   assetsUrl  the assets directory (default: ./assets/ beside the bundle)
///   seed       unseeded rands()'s seed (default 0: the same model every load)
///   limits     ResourceLimits for the worker (default: the worker's)
///   worker     () => Worker, for a page that starts the worker itself
///   staleMs    abandon a model's run this old when a newer one waits
///   lazyFonts  fetch fonts.tar.gz when a model draws text (default true)
export function createEngine(options = {}) {
  const base = options.base ?? defaultAssets();
  const assets = resolveAssets(options.assetsUrl, base);
  const spawner = options.worker
    ? { url: null, viaBlob: false, spawn: options.worker, dispose() {} }
    : workerSpawner(new URL("core/worker.js", assets), { pageOrigin: options.pageOrigin });
  const client = createClient({ engine: "wasm" }, assets.href, {
    seed: options.seed ?? 0,
    limits: options.limits ?? null,
    spawn: spawner.spawn,
    lazyFonts: options.lazyFonts ?? true,
    // The scheduler decides what is stale, per model; the client's own
    // kill would fire for any model's waiting run.
    staleMs: Infinity,
  });
  return new Engine(client, { assets, spawner, staleMs: options.staleMs ?? Infinity, timers: options.timers });
}

export class Engine {
  constructor(client, { assets, spawner, staleMs = Infinity, timers = globalThis }) {
    this.client = client;
    this.assets = assets;
    this.spawner = spawner;
    this.scheduler = new Scheduler(client, { staleMs, timers });
    this.nextModel = 1;
    this.failure = null;
    this.started = false;
    this.version = null;
    this.info = null;
    this.scratch = null;
    // Ready at the first worker that answers `init` (the client's "info"
    // event); a worker that fails as it loads is respawned by the client a
    // few times before it gives up ("failed"), so the first error alone
    // does not decide.
    this.ready = new Promise((resolve, reject) => {
      client.on("info", (info) => {
        if (this.started) return;
        this.started = true;
        this.info = info;
        this.version = info?.version ?? null;
        if (VERSION !== "dev" && this.version && this.version !== VERSION && !/mock/.test(this.version)) {
          console.warn(`NeoSCAD: the package is ${VERSION} but its engine is ${this.version}; are the assets from another version?`);
        }
        resolve(info);
      });
      client.on("status", ({ state, message }) => {
        if (state !== "failed") return;
        this.failure = this.started
          ? `NeoSCAD's engine keeps stopping (${message}).`
          : startFailure(assets, spawner.viaBlob, message);
        reject(new EngineStartError(this.failure));
      });
      // The client's pre-start promise follows the first start's; a first
      // start that fails is reported through "failed", not as an
      // unhandled rejection.
      client.ready.catch(() => {});
      try {
        client.start().catch(() => {});
      } catch (e) {
        // `new Worker` itself refused: a CSP without `worker-src blob:`,
        // or a cross-origin URL given to a spawner of the page's own.
        this.failure = startFailure(assets, spawner.viaBlob, e?.message ?? String(e));
        reject(new EngineStartError(this.failure));
      }
    });
    this.ready.catch(() => {});
  }

  /// A model: one document the worker keeps for repeated runs.
  model(options = {}) {
    return new Model(this, this.nextModel++, options);
  }

  /// Submit a job, turning a dead engine into an EngineStartError.
  async submit(key, job, options) {
    try {
      await this.ready;
      return await this.scheduler.submit(key, job, options);
    } catch (e) {
      if (e instanceof AbortError) throw e;
      if (this.failure && (e instanceof EngineRestarted || e instanceof EngineStartError)) throw new EngineStartError(this.failure);
      throw e;
    }
  }

  scratchModel(name) {
    checkRelativePath(name ?? "main.scad");
    this.scratch ??= this.model({ name: "main.scad", scratch: true });
    return this.scratch;
  }

  /// One run of `source` (never coalesced with another call).
  async run(source, options = {}) {
    return this.scratchModel(options.name).run(source, { ...options, coalesce: false });
  }

  async export(source, format, options = {}) {
    return this.scratchModel(options.name).export(source, format, options);
  }

  async parameters(source, options = {}) {
    return this.scratchModel(options.name).parameters(source, options);
  }

  dispose() {
    this.scheduler.dispose();
    this.client.dispose();
    this.spawner.dispose();
    this.failure ??= "the engine was disposed";
  }
}

export class Model {
  constructor(engine, n, { name = "main.scad", camera = null, scratch = false } = {}) {
    this.engine = engine;
    this.key = scratch ? "scratch" : `m${n}`;
    this.dir = scratch ? "/doc/run/" : `/doc/m${n}/`;
    this.camera = camera;
    this.opened = new Map(); // path -> text, documents open in the worker
    this.files = new Map(); // path -> data, as last sent
    this.disposed = false;
    this.scratch = scratch;
    this.setName(name);
  }

  setName(name) {
    this.name = checkRelativePath(name);
    this.path = this.dir + this.name;
  }

  /// At the start of a job: the scratch model (engine.run and friends)
  /// takes the call's file name then, not when the call was queued, since
  /// a queued call must not rename the document of the one before it.
  claim(name) {
    if (this.scratch) this.setName(name ?? "main.scad");
  }

  alive() {
    if (this.disposed) throw new Error("NeoSCAD: the model was disposed");
  }

  /// Make the worker's copy of the document and its files current, and
  /// fetch the lazy libraries they include. Inside a job, so no other
  /// model's requests come between this and the run.
  async prepare(client, source, files) {
    const text = String(source ?? "");
    const added = [];
    for (const [rel, data] of Object.entries(files ?? {})) {
      const path = this.dir + checkRelativePath(rel);
      if (path === this.path) throw new TypeError(`NeoSCAD: files["${rel}"] is the model itself`);
      const value = typeof data === "string" || data instanceof ArrayBuffer ? data : ArrayBuffer.isView(data) ? data.slice().buffer : String(data);
      if (this.files.has(path) && sameData(this.files.get(path), value)) continue;
      this.files.set(path, value);
      added.push({ path, data: value });
    }
    if (added.length) await client.addFiles({ files: added });
    if (!this.opened.has(this.path)) await client.open(this.path, text);
    else if (this.opened.get(this.path) !== text) await client.update(this.path, text);
    this.opened.set(this.path, text);
    const texts = [text, ...[...this.files.values()].filter((d) => typeof d === "string")].join("\n");
    await ensureLibraries(client, texts, { base: this.engine.assets.href });
  }

  texts() {
    const m = new Map([[this.path, this.opened.get(this.path)]]);
    for (const [p, d] of this.files) if (typeof d === "string") m.set(p, d);
    return m;
  }

  /// Run `source`. Options: mode ("preview" | "render"), enable, overrides,
  /// files, colorScheme, signal. Resolves to the result (result.js), or to
  /// `{superseded: true}` when a newer run of this model replaced it.
  async run(source, options = {}) {
    this.alive();
    const mode = options.mode ?? "preview";
    if (!["preview", "render", "force"].includes(mode)) throw new TypeError(`NeoSCAD: unknown mode "${mode}"`);
    const { parts, enable } = splitEnable(options.enable);
    return this.engine.submit(
      this.key,
      async (client) => {
        this.claim(options.name);
        await this.prepare(client, source, options.files);
        const request = () =>
          Requests.run({
            path: this.path,
            mode,
            values: options.overrides ?? {},
            parts,
            enable,
            camera: this.camera?.() ?? null,
            colorScheme: options.colorScheme ?? null,
          });
        let raw = await client.request(request());
        // Text the source did not show (drawn by a library): fetch the
        // fonts and run again, as the demo does.
        if (raw?.fontsWanted && (await ensureFonts(client, { base: this.engine.assets.href }))) raw = await client.request(request());
        return shapeResult(raw, { dir: this.dir, texts: this.texts() });
      },
      { signal: options.signal, coalesce: options.coalesce ?? true },
    );
  }

  /// Export `source` as `format` ("binstl", "stl", "3mf", "off", "svg",
  /// ...). Resolves to `{data, ok, exitCode, failure, mime, messages}`;
  /// rejects with ExportError when no file was written.
  async export(source, format, options = {}) {
    this.alive();
    const { parts, enable } = splitEnable(options.enable);
    const r = await this.engine.submit(
      `${this.key}:export`,
      async (client) => {
        this.claim(options.name);
        await this.prepare(client, source, options.files);
        const req = Requests.export(this.path, format, runOptions(options.overrides ?? {}, parts, enable), options.creationDate ?? EPOCH);
        if (options.options) req.options = options.options;
        return shapeExport(await client.request(req), { dir: this.dir, texts: this.texts() });
      },
      { signal: options.signal, coalesce: false },
    );
    if (r.data === null) throw new ExportError(r);
    return r;
  }

  /// The customizer's parameters: [{name, parameters: [{name, description,
  /// control, defaultValue}]}].
  async parameters(source, options = {}) {
    this.alive();
    return this.engine.submit(
      `${this.key}:parameters`,
      async (client) => {
        this.claim(options.name);
        await this.prepare(client, source, options.files);
        return parameterGroups((await client.request(Requests.parameters(this.path))).groups);
      },
      { signal: options.signal, coalesce: false },
    );
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    const s = this.engine.scheduler;
    s.forget(this.key);
    if (this.engine.failure) return;
    s.submit(`${this.key}:close`, (client) => Promise.all([...this.opened.keys()].map((p) => client.close(p))), { coalesce: false }).catch(
      () => {},
    );
  }
}

const shared = new Map();

/// The page's shared engine for an assets directory: every element (and
/// `render()` without an `engine`) uses one worker, since each is a whole
/// wasm instance. It lives as long as the page.
export function sharedEngine(options = {}) {
  const base = options.base ?? defaultAssets();
  const key = resolveAssets(options.assetsUrl, base).href;
  let e = shared.get(key);
  if (!e || (e.failure && e.started)) {
    e = createEngine(options);
    shared.set(key, e);
  }
  return e;
}
