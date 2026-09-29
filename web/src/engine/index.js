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
import { untar } from "./tar.js";

export function workerURL(engine, base = import.meta.url) {
  return new URL(engine === "wasm" ? "./core/worker.js" : "./mock-worker.js", base);
}

export function createEngine(build, base = import.meta.url) {
  const url = workerURL(build.engine, base);
  return new EngineClient({
    spawn: () => new Worker(url, { type: "module", name: "neoscad-engine" }),
    // The plan's worker limits: Limits::AGENT with a 1 GiB memory cap.
    initOptions: { limits: "agent", memoryBytes: 1 << 30 },
  });
}

/// Libraries fetched on first use rather than shipped in the core: each
/// is a gzipped tar beside the bundle whose paths start with its name.
export const LAZY_LIBRARIES = [{ name: "BOSL2", archive: "bosl2.tar.gz" }];

const TEXT = /\.(scad|txt|md|json)$|(^|\/)LICENSE$/;

/// Fetch and add the lazy libraries `text` includes that the engine does
/// not have yet. `onProgress(name)` is told before each fetch. Resolves to
/// the names added.
export async function ensureLibraries(engine, text, { base = import.meta.url, onProgress = () => {} } = {}) {
  const added = [];
  for (const lib of LAZY_LIBRARIES) {
    if (!usesLibrary(text, lib.name)) continue;
    if (engine.hasFile(`${LIB_ROOT}/${lib.name}/.loaded`)) continue;
    onProgress(lib.name);
    const res = await fetch(new URL(`./${lib.archive}`, base));
    if (!res.ok) throw new Error(`${lib.archive}: HTTP ${res.status}`);
    const gz = res.body.pipeThrough(new DecompressionStream("gzip"));
    const data = new Uint8Array(await new Response(gz).arrayBuffer());
    const dec = new TextDecoder();
    const files = untar(data)
      .filter((f) => f.path.startsWith(`${lib.name}/`))
      .map((f) =>
        TEXT.test(f.path)
          ? { path: `${LIB_ROOT}/${f.path}`, text: dec.decode(f.bytes) }
          : // Copied out of the archive's buffer: posting a view would
            // clone the whole archive once per file.
            { path: `${LIB_ROOT}/${f.path}`, bytes: f.bytes.slice().buffer },
      );
    files.push({ path: `${LIB_ROOT}/${lib.name}/.loaded`, text: "" });
    await engine.addFiles(files);
    added.push(lib.name);
  }
  return added;
}
