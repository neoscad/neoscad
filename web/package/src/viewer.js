// The package's viewer: the demo's backend choice (web/src/view/index.js:
// WebGPU, then the WebGL2 build, then a canvas2d fallback) on a canvas of
// the page's, or one made inside a container, with its own resize
// observer and `image()` as a PNG Blob. `addScene` shows several scenes
// at once, each moved by a rigid matrix (the parts of a mechanism, rendered
// once and moved per frame), and `onFrame` drives such an animation.

import { DEFAULT_VIEW, createViewer as createBackend } from "../../src/view/index.js";
import { defaultAssets, resolveAssets } from "./assets.js";
import { FrameLoop } from "./frames.js";

const BACKENDS = new Set(["auto", "webgpu", "webgl", "canvas2d"]);

/// `target`: a canvas, or an element to make one in (filling it).
function canvasFor(target) {
  if (target?.tagName === "CANVAS") return { canvas: target, made: false };
  if (!target?.appendChild) throw new TypeError("NeoSCAD: createViewer needs a canvas or a container element");
  const canvas = target.ownerDocument.createElement("canvas");
  canvas.style.cssText = "display:block;width:100%;height:100%;touch-action:none;outline:none";
  canvas.tabIndex = 0;
  target.appendChild(canvas);
  return { canvas, made: true };
}

/// RGBA pixels (`{width, height, rgba}`, top row first) as a Blob of
/// `type` (PNG by default).
export async function encodeImage({ width, height, rgba }, type = "image/png") {
  const data = new ImageData(new Uint8ClampedArray(rgba.buffer, rgba.byteOffset, rgba.byteLength), width, height);
  if (typeof OffscreenCanvas !== "undefined") {
    const c = new OffscreenCanvas(width, height);
    c.getContext("2d").putImageData(data, 0, 0);
    return c.convertToBlob({ type });
  }
  const c = document.createElement("canvas");
  c.width = width;
  c.height = height;
  c.getContext("2d").putImageData(data, 0, 0);
  return new Promise((resolve, reject) => c.toBlob((b) => (b ? resolve(b) : reject(new Error("toBlob failed"))), type));
}

/// A 2D canvas copied at `width` by `height`, as a Blob.
function copyCanvas(source, width, height, type) {
  const c = document.createElement("canvas");
  c.width = width;
  c.height = height;
  c.getContext("2d").drawImage(source, 0, 0, width, height);
  return new Promise((resolve, reject) => c.toBlob((b) => (b ? resolve(b) : reject(new Error("toBlob failed"))), type));
}

/// Make a viewer. Options: assetsUrl, backend ("auto" | "webgpu" | "webgl"
/// | "canvas2d"), settings (axes, scales, grid, edges, crosshairs,
/// orthographic, lighting, scheme).
export async function createViewer(target, options = {}) {
  const backend = options.backend ?? "auto";
  if (!BACKENDS.has(backend)) throw new TypeError(`NeoSCAD: unknown backend "${backend}"`);
  const assets = resolveAssets(options.assetsUrl, options.base ?? defaultAssets());
  const settings = { ...DEFAULT_VIEW, ...(options.settings ?? {}) };
  const { canvas: first, made } = canvasFor(target);
  const { viewer, canvas, notice, errors } = await createBackend(first, {
    view: "wasm",
    base: assets.href,
    scheme: settings.scheme,
    backend,
  });
  viewer.setSettings(settings);
  return new Viewer(viewer, canvas, { notice, errors: errors ?? [], made, settings });
}

export class Viewer {
  constructor(inner, canvas, { notice, errors, made, settings }) {
    this.inner = inner;
    this.canvas = canvas;
    this.kind = inner.kind;
    /// The message to show over the view when it fell back to canvas2d.
    this.notice = notice;
    this.errors = errors;
    this.settings = settings;
    this.made = made;
    this.loops = new Set();
    this.observer = typeof ResizeObserver !== "undefined" ? new ResizeObserver(() => this.inner?.resize()) : null;
    this.observer?.observe(canvas);
  }

  get schemes() {
    return this.inner.schemes;
  }

  /// Show one scene (null: none). Scenes added with `addScene` go too.
  setScene(scene) {
    this.inner.setScene(scene ?? null);
  }

  /// Whether `addScene` works: the WebGPU and WebGL viewers place scenes;
  /// the canvas2d fallback does not, and a page should then show something
  /// else (a still, or one `setScene`).
  get supportsTransforms() {
    return !!this.inner?.supportsTransforms;
  }

  /// Show `scene` as well as the scenes already shown, moved by `matrix`
  /// (16 numbers, column-major, a rotation and a translation only;
  /// identity by default). Returns a handle: `setMatrix(m)` moves it,
  /// which only redraws (call it every frame if you like), and `remove()`
  /// takes it away. Throws on the canvas2d fallback (`supportsTransforms`
  /// is false), and on a preview of a difference or an intersection,
  /// which only `setScene` can show: run such models in `render` mode.
  addScene(scene, { matrix } = {}) {
    if (!scene?.faces) throw new TypeError("NeoSCAD: addScene needs a scene (a result's `scene`)");
    if (!this.supportsTransforms) {
      throw new Error(`NeoSCAD: the ${this.kind} viewer cannot show several scenes (check viewer.supportsTransforms)`);
    }
    const handle = new SceneHandle(this, this.inner.addScene(scene));
    if (matrix) handle.setMatrix(matrix);
    return handle;
  }

  /// Call `callback(timeMs, stepMs)` on every animation frame while the
  /// canvas is on screen and the tab is visible; `timeMs` counts running
  /// time only, so an animation resumes where it paused. Returns
  /// `{pause(), play(), stop(), paused, running, time}`; `dispose()`
  /// stops it too.
  onFrame(callback) {
    const loop = new FrameLoop(this.canvas, callback);
    this.loops.add(loop);
    const stop = loop.stop.bind(loop);
    loop.stop = () => {
      stop();
      this.loops.delete(loop);
    };
    return loop;
  }
  setSettings(s) {
    this.settings = { ...this.settings, ...s };
    this.inner.setSettings(s);
  }
  preset(name) {
    this.inner.preset(name);
  }
  viewAll() {
    this.inner.viewAll();
  }
  resetView() {
    this.inner.resetView();
  }
  camera() {
    return this.inner.camera();
  }
  setFileView(view) {
    if (view) this.inner.setFileView(view);
  }
  setAnnotations(a) {
    this.inner.setAnnotations(a ?? null);
  }
  resize() {
    this.inner.resize();
  }

  /// The view drawn at `width` by `height` pixels, as a Blob (PNG unless
  /// `type` says otherwise).
  async image(width, height, { type = "image/png" } = {}) {
    if (this.kind === "canvas2d") {
      // It draws on the next animation frame after a change (a scene, a
      // resize, which also clears the canvas): copied as it stands, a
      // scene set just now would not be in the picture. Draw it now.
      this.inner.draw();
      return copyCanvas(this.canvas, width, height, type);
    }
    return encodeImage(await this.inner.image(width, height), type);
  }

  dispose() {
    for (const loop of [...this.loops]) loop.stop();
    this.observer?.disconnect();
    this.observer = null;
    this.inner?.dispose();
    this.inner = null;
    if (this.made) this.canvas.remove();
  }
}

/// A scene shown with `viewer.addScene`.
export class SceneHandle {
  constructor(viewer, id) {
    this.viewer = viewer;
    this.id = id;
    this.removed = false;
    // One array reused for every matrix, so moving a scene each frame
    // allocates nothing.
    this.scratch = new Float32Array(16);
  }

  /// Move the scene: 16 numbers (an Array, a Float32Array or a
  /// DOMMatrix), column-major. False once the scene is gone (removed,
  /// replaced by `setScene`, or the viewer disposed). Throws on a matrix
  /// that scales, shears or mirrors.
  setMatrix(matrix) {
    const inner = this.viewer.inner;
    if (this.removed || !inner) return false;
    const m = typeof matrix?.toFloat32Array === "function" ? matrix.toFloat32Array() : matrix;
    if (m?.length !== 16) throw new TypeError("NeoSCAD: a scene matrix has 16 numbers (column-major)");
    for (let i = 0; i < 16; i++) this.scratch[i] = m[i];
    const shown = inner.setSceneMatrix(this.id, this.scratch);
    if (!shown) this.removed = true;
    return shown;
  }

  /// Take the scene away. Whether it was still shown.
  remove() {
    const inner = this.viewer.inner;
    if (this.removed || !inner) return false;
    this.removed = true;
    return inner.removeScene(this.id);
  }
}
