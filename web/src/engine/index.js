// The engine as the page sees it: which worker to start, and the lazy
// libraries. The build decides the worker (build.json's `engine`):
//
//   "wasm"  ./core/worker.js, the core's own module worker (crates/web and
//           scripts/web/build-core.sh; it loads its wasm relative to itself)
//   "mock"  ./mock-worker.js, canned results (mock-core.js)
//
// Both are addressed relative to this bundle (import.meta.url), so the
// page works under any sub-path, /try/ on the website included.

import { EngineClient } from "./client.js";
import { LIB_ROOT, usesLibrary } from "./protocol.js";

export function workerURL(engine, base = import.meta.url) {
  return new URL(engine === "wasm" ? "./core/worker.js" : "./mock-worker.js", base);
}

export function createEngine(build, base = import.meta.url) {
  const url = workerURL(build.engine, base);
  return new EngineClient({
    spawn: () => new Worker(url, { type: "module", name: "neoscad-engine" }),
    // One seed per page load, kept across respawns: unseeded rands()
    // differ between visits, as between app launches, but a respawn does
    // not change the model on screen.
    seed: Date.now() >>> 0,
  });
}

/// Libraries fetched on first use rather than shipped in the core: each
/// is a gzipped tar beside the bundle whose paths start with its name,
/// unpacked by the worker under the library directory.
export const LAZY_LIBRARIES = [{ name: "BOSL2", archive: "bosl2.tar.gz" }];

/// Fetch and add the lazy libraries `text` includes that the engine does
/// not have yet. `onProgress(name)` is told before each fetch. Resolves to
/// the names added.
export async function ensureLibraries(engine, text, { base = import.meta.url, onProgress = () => {} } = {}) {
  const added = [];
  for (const lib of LAZY_LIBRARIES) {
    if (!usesLibrary(text, lib.name) || engine.hasLibrary(lib.name)) continue;
    onProgress(lib.name);
    const res = await fetch(new URL(`./${lib.archive}`, base));
    if (!res.ok) throw new Error(`${lib.archive}: HTTP ${res.status}`);
    // Gunzipped here (the worker takes a plain ustar archive); a server
    // that already decoded it (Content-Encoding) hands over the tar.
    const bytes = new Uint8Array(await res.arrayBuffer());
    const tar = bytes[0] === 0x1f && bytes[1] === 0x8b ? await gunzip(bytes) : bytes.buffer;
    await engine.addFiles({ tar, root: LIB_ROOT }, lib.name);
    added.push(lib.name);
  }
  return added;
}

async function gunzip(bytes) {
  const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip"));
  return new Response(stream).arrayBuffer();
}
