// The package's viewer: the demo's backend choice (web/src/view/index.js:
// WebGPU, then the WebGL2 build, then a canvas2d fallback) on a canvas of
// the page's, or one made inside a container, with its own resize
// observer and `image()` as a PNG Blob.

import { DEFAULT_VIEW, createViewer as createBackend } from "../../src/view/index.js";
import { defaultAssets, resolveAssets } from "./assets.js";

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
    this.observer = typeof ResizeObserver !== "undefined" ? new ResizeObserver(() => this.inner?.resize()) : null;
    this.observer?.observe(canvas);
  }

  get schemes() {
    return this.inner.schemes;
  }

  setScene(scene) {
    this.inner.setScene(scene ?? null);
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
    this.observer?.disconnect();
    this.observer = null;
    this.inner?.dispose();
    this.inner = null;
    if (this.made) this.canvas.remove();
  }
}
