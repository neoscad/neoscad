// The 3D view behind one interface, whichever renders it:
//
//   "webgpu"    crates/web-view (builder B), wasm-bindgen output in ./view/,
//               when the build has it and the browser has WebGPU
//   "canvas2d"  canvas2d.js, a software fallback for small models
//
// The interface the page uses (Canvas2DViewer implements it; the wasm
// viewer is wrapped to it in `wrapWasm`, the one place to align with B):
//
//   setScene(packedScene)  setSettings({axes, scales, grid, edges,
//   crosshairs, orthographic, lighting, scheme})  preset(name)  viewAll()
//   resetView()  setAnnotations({markers, lines})  focus(point)
//   camera() / setCamera({vpt, vpr, vpd})  rayAt(x, y) -> {origin,
//   direction}  resize()  dispose()  schemes  kind  onPick = (ray) => ...
//
// `createViewer` resolves to `{viewer, notice}`: `notice` is the message
// the page shows over the view when it fell back (no WebGPU, or the
// wasm viewer failed to start), or null.

import { Canvas2DViewer } from "./canvas2d.js";

export const NO_WEBGPU =
  "This browser has no WebGPU, so the 3D view is a simplified one (large models show only their box). " +
  "Everything else works as usual.";

export async function createViewer(canvas, { view = "none", base = import.meta.url, gpu = globalThis.navigator?.gpu } = {}) {
  if (view === "wasm") {
    if (!gpu) return { viewer: new Canvas2DViewer(canvas), notice: NO_WEBGPU };
    try {
      const url = new URL("./view/neoscad_web_view.js", base).href;
      const mod = await import(url);
      await mod.default?.();
      return { viewer: await wrapWasm(mod, canvas), notice: null };
    } catch (e) {
      console.error("3D view:", e);
      return {
        viewer: new Canvas2DViewer(canvas),
        notice: `The 3D view could not start (${e.message ?? e}), so it is a simplified one.`,
      };
    }
  }
  return { viewer: new Canvas2DViewer(canvas), notice: null };
}

/// B's viewer to the interface above. Its exports are a guess until
/// crates/web-view lands: a `Viewer` class made by `Viewer.create(canvas)`
/// (async, it requests the GPU device) with methods of the same names in
/// snake_case.
async function wrapWasm(mod, canvas) {
  const v = await mod.Viewer.create(canvas);
  const call = (name, ...args) => (v[name] ?? v[name.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`)])?.apply(v, args);
  const w = {
    kind: "webgpu",
    schemes: call("schemes") ?? ["Cornfield"],
    onPick: null,
    setScene: (s) => call("uploadPacked", s),
    setSettings: (s) => call("setSettings", s),
    preset: (n) => call("preset", n),
    viewAll: () => call("viewAll"),
    resetView: () => call("resetView"),
    setAnnotations: (a) => call("setAnnotations", a),
    focus: (p) => call("focus", p),
    camera: () => call("camera"),
    setCamera: (c) => call("setCamera", c),
    rayAt: (x, y) => call("rayAt", x, y),
    resize: () => call("resize"),
    dispose: () => call("free"),
  };
  canvas.addEventListener("click", (e) => {
    const r = canvas.getBoundingClientRect();
    w.onPick?.(w.rayAt(e.clientX - r.left, e.clientY - r.top));
  });
  return w;
}
