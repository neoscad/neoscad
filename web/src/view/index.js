// The 3D view behind one interface, whichever renders it:
//
//   "webgpu"    crates/web-view built WebGPU-only (./view/web_view.js),
//               when the browser has `navigator.gpu` and it gives a device
//   "webgl"     the same viewer built with wgpu's WebGL2 backend
//               (./view-webgl/), fetched only when WebGPU is missing or
//               fails: it is about six times the size, so browsers with
//               WebGPU never download it
//   "canvas2d"  canvas2d.js, a software fallback for small models, when
//               neither starts (or the build has no viewer)
//
// The interface the page uses (Canvas2DViewer implements it; the wasm
// viewer is wrapped to it in `wrapWasm`):
//
//   setScene(packedScene | null)  setSettings({axes, scales, grid, edges,
//   crosshairs, orthographic, lighting, scheme})  preset(name)  viewAll()
//   resetView()  setAnnotations({markers, lines} | null)  focus(point)
//   camera() -> {vpt, vpr, vpd, vpf}  setFileView({vpt?, vpr?, vpd?, vpf?})
//   rayAt(x, y) -> {origin, direction}  resize()  dispose()  schemes  kind
//   onPick = (ray) => ...
//   image(width, height) -> Promise<{width, height, rgba}>   (wasm only; the
//   canvas fallback's 2D canvas is copied instead)
//
// Annotations are the viewer's shape: lines `{points: [[x,y,z], ...],
// closed, color}` and markers `{point, label, color, bboxMin?, bboxMax?}`,
// colours as CSS hex strings (converted to the wasm viewer's 0..1 arrays
// here; a marker's box becomes twelve lines).
//
// `createViewer` resolves to `{viewer, canvas, notice}`: `canvas` is the
// element drawn into (a failed WebGPU attempt leaves a canvas that can
// have no other context, so a fallback gets a fresh copy), and `notice`
// the message the page shows over the view when it fell back to the
// canvas, or null.

import { Canvas2DViewer } from "./canvas2d.js";

export const NO_3D =
  "This browser has neither WebGPU nor WebGL 2, so the 3D view is a simplified one (large models show only their box; " +
  "previews of differences show only what is kept). Everything else works as usual.";

/// The viewer builds the bundle carries (build.json's `view`): "wasm" has
/// ./view/ and ./view-webgl/; "none" has neither.
export async function createViewer(
  canvas,
  { view = "none", base = import.meta.url, gpu = globalThis.navigator?.gpu, scheme = "Cornfield" } = {},
) {
  if (view !== "wasm") return { viewer: new Canvas2DViewer(canvas), canvas, notice: null };
  const errors = [];
  const attempts = gpu ? [["view", "webgpu"], ["view-webgl", "webgl"]] : [["view-webgl", "webgl"]];
  for (const [dir, backend] of attempts) {
    try {
      const mod = await import(new URL(`./${dir}/web_view.js`, base).href);
      await mod.default();
      const v = await mod.Viewer.create(canvas, { backend, colorScheme: scheme });
      return { viewer: wrapWasm(mod, v, canvas), canvas, notice: null };
    } catch (e) {
      errors.push(`${backend}: ${e?.message ?? e}`);
      console.warn(`3D view (${backend}):`, e);
      canvas = freshCanvas(canvas);
    }
  }
  // Why each backend failed is for the console (logged above), not the
  // notice: a reader can do nothing with wgpu's error text.
  return { viewer: new Canvas2DViewer(canvas), canvas, notice: NO_3D, errors };
}

/// A copy of `canvas` in its place: a canvas keeps the first context type
/// it was asked for, so one a failed viewer touched cannot be reused.
function freshCanvas(canvas) {
  const copy = canvas.cloneNode(false);
  canvas.replaceWith(copy);
  return copy;
}

/// "#rrggbb" (or "#rgb", or an array already) as `[r, g, b, a]` from 0 to 1.
export function rgba(color) {
  if (Array.isArray(color)) return color;
  let hex = String(color ?? "#e0f").replace(/^#/, "");
  if (hex.length === 3) hex = [...hex].map((c) => c + c).join("");
  const n = parseInt(hex.slice(0, 6), 16);
  if (!Number.isFinite(n)) return [1, 0, 1, 1];
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255].map((x) => x / 255).concat(1);
}

/// The twelve edges of a box as annotation lines.
export function boxLines(min, max, color) {
  const c = [];
  for (const x of [min[0], max[0]]) for (const y of [min[1], max[1]]) for (const z of [min[2], max[2]]) c.push([x, y, z]);
  const edges = [[0, 1], [2, 3], [4, 5], [6, 7], [0, 2], [1, 3], [4, 6], [5, 7], [0, 4], [1, 5], [2, 6], [3, 7]];
  return edges.map(([a, b]) => ({ points: [c[a], c[b]], closed: false, color }));
}

/// The page's annotations as the wasm viewer's `setAnnotations` takes them.
export function wasmAnnotations(a) {
  const lines = [];
  const markers = [];
  for (const l of a?.lines ?? []) {
    if ((l.points?.length ?? 0) >= 2) lines.push({ points: l.points, closed: !!l.closed, color: rgba(l.color) });
  }
  for (const m of a?.markers ?? []) {
    if (m.bboxMin?.length === 3 && m.bboxMax?.length === 3) lines.push(...boxLines(m.bboxMin, m.bboxMax, rgba(m.color)));
    if (m.point?.length === 3) markers.push({ point: m.point, label: String(m.label ?? ""), color: rgba(m.color) });
  }
  return { lines, markers };
}

const SETTING_KEYS = ["axes", "scales", "grid", "edges", "crosshairs", "orthographic", "lighting"];

/// crates/web-view's `Viewer` to the interface above.
function wrapWasm(mod, v, canvas) {
  let generation = 0;
  let scheme = v.colorScheme();
  const w = {
    kind: v.backend,
    adapter: v.adapter,
    schemes: mod.Viewer.colorSchemes(),
    onPick: null,
    raw: v,
    setScene(scene) {
      if (!scene) {
        v.clearModel();
        return;
      }
      v.setModel(new Uint8Array(scene.faces), new Uint8Array(scene.edges), scene.meta, ++generation);
    },
    setSettings(s) {
      const picked = {};
      for (const k of SETTING_KEYS) if (s[k] !== undefined) picked[k] = s[k];
      v.setSettings(picked);
      if (s.scheme && s.scheme !== scheme) {
        v.setColorScheme(s.scheme);
        scheme = s.scheme;
      }
    },
    preset: (name) => v.setView(name),
    viewAll: () => v.viewAll(),
    resetView: () => v.resetView(),
    setAnnotations: (a) => v.setAnnotations(wasmAnnotations(a)),
    focus: (p) => p?.length === 3 && v.lookAt(p[0], p[1], p[2]),
    camera: () => v.camera(),
    setFileView: (view) => v.setFileView(view),
    rayAt: (x, y) => v.rayAt(x, y),
    // The view as shown (grid and annotations too), drawn offscreen: an
    // agent's capture (agent/page.js). A WebGPU or WebGL canvas cannot be
    // read back once presented, so the canvas itself is never copied.
    image: (width, height) => v.image(width, height, true),
    resize: () => v.resize(),
    dispose: () => v.free(),
    canvas,
  };
  v.onClick((x, y) => w.onPick?.(v.rayAt(x, y)));
  return w;
}
