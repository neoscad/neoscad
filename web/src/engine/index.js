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
import { FONT_ROOT, LIB_ROOT, usesLibrary, usesText } from "./protocol.js";

export function workerURL(engine, base = import.meta.url) {
  return new URL(engine === "wasm" ? "./core/worker.js" : "./mock-worker.js", base);
}

export function createEngine(build, base = import.meta.url) {
  const url = workerURL(build.engine, base);
  const engine = new EngineClient({
    spawn: () => new Worker(url, { type: "module", name: "neoscad-engine" }),
    // One seed per page load, kept across respawns: unseeded rands()
    // differ between visits, as between app launches, but a respawn does
    // not change the model on screen.
    seed: Date.now() >>> 0,
  });
  // The wasm core's fonts are fetched on first use (FONTS); the mock draws
  // no text, and the mock build has no fonts.tar.gz.
  engine.lazyFonts = build.engine === "wasm";
  return engine;
}

/// Libraries fetched on first use rather than shipped in the core: each
/// is a gzipped tar beside the bundle whose paths start with its name,
/// unpacked by the worker under the library directory.
export const LAZY_LIBRARIES = [{ name: "BOSL2", archive: "bosl2.tar.gz", root: LIB_ROOT }];

/// The fonts OpenSCAD bundles (Liberation Sans, Serif and Mono), fetched
/// the same way the first time a model draws text: they were 2.3 MB of
/// the core's 4.6 MB gzipped, and most models have no text.
export const FONTS = { name: "fonts", archive: "fonts.tar.gz", root: FONT_ROOT };

/// Fetch and add the lazy libraries `text` includes that the engine does
/// not have yet, and the fonts when it seems to draw text.
/// `onProgress(name)` is told before each fetch. Resolves to the names
/// added.
export async function ensureLibraries(engine, text, { base = import.meta.url, onProgress = () => {} } = {}) {
  const added = [];
  for (const lib of LAZY_LIBRARIES) {
    if (!usesLibrary(text, lib.name) || engine.hasLibrary(lib.name)) continue;
    await addLazy(engine, lib, { base, onProgress });
    added.push(lib.name);
  }
  if (usesText(text) && (await ensureFonts(engine, { base, onProgress }))) added.push(FONTS.name);
  return added;
}

/// Fetch and add the fonts, unless the engine has them or has its own.
/// Resolves to whether they were added now.
export async function ensureFonts(engine, { base = import.meta.url, onProgress = () => {} } = {}) {
  if (!engine.lazyFonts || engine.hasLibrary(FONTS.name)) return false;
  await addLazy(engine, FONTS, { base, onProgress });
  return true;
}

async function addLazy(engine, lib, { base, onProgress }) {
  onProgress(lib.name);
  const res = await fetch(new URL(`./${lib.archive}`, base));
  if (!res.ok) throw new Error(`${lib.archive}: HTTP ${res.status}`);
  // Gunzipped here (the worker takes a plain ustar archive); a server
  // that already decoded it (Content-Encoding) hands over the tar.
  const bytes = new Uint8Array(await res.arrayBuffer());
  const tar = bytes[0] === 0x1f && bytes[1] === 0x8b ? await gunzip(bytes) : bytes.buffer;
  await engine.addFiles({ tar, root: lib.root }, lib.name);
}

async function gunzip(bytes) {
  const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip"));
  return new Response(stream).arrayBuffer();
}
