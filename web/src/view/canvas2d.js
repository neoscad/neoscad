// A software 3D view on a 2D canvas: the fallback when neither the WebGPU
// nor the WebGL2 viewer starts (or the build has none), and the mock
// build's view. It draws the worker's packed scenes (`unpack`). Flat-shaded triangles in depth order, with
// the View menu's edges, axes, orthographic toggle and presets, orbit /
// pan / zoom, and the check and measure panels' annotations.
//
// It is meant for small models. Past `MAX_TRIANGLES` it draws the model's
// box instead: sorting and filling a million triangles per frame on the
// main thread would freeze the page, which is worse than a coarse view.

export const MAX_TRIANGLES = 150000;

const PRESETS = {
  top: [0, 0, 0],
  bottom: [180, 0, 0],
  left: [90, 0, 270],
  right: [90, 0, 90],
  front: [90, 0, 0],
  back: [90, 0, 180],
  diagonal: [55, 0, 25],
};

/// Background, face and edge colours of a few of OpenSCAD's schemes
/// (assets/color-schemes/render; Cornfield is built in).
const SCHEMES = {
  Cornfield: { bg: "#ffffe5", face: [0.98, 0.84, 0.17], edge: "#ff0000" },
  Metallic: { bg: "#aaaaff", face: [0.87, 0.87, 1.0], edge: "#ff0000" },
  Sunset: { bg: "#aa4444", face: [1.0, 0.67, 0.67], edge: "#ff0000" },
  Starnight: { bg: "#000000", face: [1.0, 1.0, 0.88], edge: "#0000ff" },
  "Tomorrow Night": { bg: "#1d1f21", face: [0.51, 0.64, 0.75], edge: "#c5c8c6" },
};

const rad = (d) => (d * Math.PI) / 180;

function rotation(vpr) {
  // OpenSCAD's camera: rotate about X by vpr[0], then about Z by vpr[2];
  // the matrix below maps model coordinates to eye coordinates.
  const [ax, , az] = vpr.map(rad);
  const cx = Math.cos(ax);
  const sx = Math.sin(ax);
  const cz = Math.cos(az);
  const sz = Math.sin(az);
  // Rz(-az) then Rx(-ax).
  return [
    [cz, sz, 0],
    [-cx * sz, cx * cz, sx],
    [sx * sz, -sx * cz, cx],
  ];
}

const mul = (m, p) => [
  m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2],
  m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2],
  m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2],
];

/// The inverse (transpose) applied: eye to model.
const mulT = (m, p) => [
  m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2],
  m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2],
  m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2],
];

/// Bytes of a face vertex and an edge segment (render::packed).
const FACE_VERTEX_SIZE = 44;
const EDGE_SEGMENT_SIZE = 24;

/// A packed scene (`{faces, edges, meta}`, docs/web-protocol.md) as
/// triangles `{v: [p, p, p], n: normal | null, color}`, 2D edge segments,
/// the edge colour, the box and the triangle count. Triangles past
/// `MAX_TRIANGLES` are counted but not unpacked.
export function unpack(scene) {
  const out = { tris: [], edges: [], edgeColor: "#ff0000", bbox: null, count: 0 };
  if (!scene) return out;
  const meta = typeof scene.meta === "string" ? JSON.parse(scene.meta) : scene.meta;
  const faces = new DataView(scene.faces);
  const f32 = (at) => faces.getFloat32(at, true);
  const draws = (meta.draws ?? []).filter((d) => d.state?.color_write !== false);
  out.count = draws.reduce((n, d) => n + Math.floor(d.count / 3), 0);
  if (out.count <= MAX_TRIANGLES) {
    for (const d of draws) {
      for (let t = 0; t + 2 < d.count; t += 3) {
        const at = (d.first + t) * FACE_VERTEX_SIZE;
        if (at + 3 * FACE_VERTEX_SIZE > scene.faces.byteLength) break;
        const v = [0, 1, 2].map((k) => [f32(at + k * FACE_VERTEX_SIZE), f32(at + k * FACE_VERTEX_SIZE + 4), f32(at + k * FACE_VERTEX_SIZE + 8)]);
        const n = [f32(at + 12), f32(at + 16), f32(at + 20)];
        const color = [f32(at + 24), f32(at + 28), f32(at + 32), f32(at + 36)];
        out.tris.push({ v, n: n[0] || n[1] || n[2] ? n : null, color });
      }
    }
  }
  const edges = new DataView(scene.edges ?? new ArrayBuffer(0));
  for (let at = 0; at + EDGE_SEGMENT_SIZE <= edges.byteLength; at += EDGE_SEGMENT_SIZE) {
    const e = (i) => edges.getFloat32(at + i * 4, true);
    out.edges.push([[e(0), e(1), e(2)], [e(3), e(4), e(5)]]);
  }
  if (meta.edge_color) {
    const [r, g, b] = meta.edge_color.map((x) => Math.round(255 * x));
    out.edgeColor = `rgb(${r},${g},${b})`;
  }
  if (meta.bbox) out.bbox = { min: meta.bbox[0], max: meta.bbox[1] };
  return out;
}

export class Canvas2DViewer {
  constructor(canvas) {
    this.kind = "canvas2d";
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.schemes = Object.keys(SCHEMES);
    this.settings = { axes: true, scales: false, grid: false, edges: false, crosshairs: false, orthographic: false, scheme: "Cornfield" };
    this.vpr = [...PRESETS.diagonal];
    this.vpt = [0, 0, 0];
    this.vpd = 140;
    this.tris = null;
    this.edges2d = [];
    this.edgeColor = "#ff0000";
    this.bbox = null;
    this.tooLarge = false;
    this.annotations = { markers: [], lines: [] };
    this.onPick = null;
    this.picking = false;
    this.frame = 0;
    this.listen();
    this.resize();
  }

  // --- The viewer API (web/src/view/index.js) ---

  /// A packed scene (render::packed, as the worker sends it): the face
  /// vertices of each draw that writes colour, in their baked colours.
  /// Depth-only draws and image-space CSG primitives (a preview's
  /// subtracted and intersected shapes) are left out: this view has no
  /// depth buffer to compose them in.
  setScene(scene) {
    const { tris, bbox, edges, edgeColor, count } = unpack(scene);
    this.tooLarge = count > MAX_TRIANGLES;
    const first = this.bbox === null;
    this.tris = this.tooLarge ? [] : tris;
    this.edges2d = edges;
    this.edgeColor = edgeColor;
    this.bbox = bbox;
    if (first && this.bbox) this.viewAll();
    this.redraw();
  }

  setSettings(s) {
    this.settings = { ...this.settings, ...s };
    this.redraw();
  }

  preset(name) {
    if (PRESETS[name]) this.vpr = [...PRESETS[name]];
    this.redraw();
  }

  viewAll() {
    if (!this.bbox) return;
    const { min, max } = this.bbox;
    this.vpt = min.map((x, k) => (x + max[k]) / 2);
    // Far enough that the whole box fits with a margin and the
    // perspective stays mild.
    this.vpd = Math.max(10, 3.2 * Math.hypot(...max.map((x, k) => x - min[k])));
    this.redraw();
  }

  resetView() {
    this.vpr = [...PRESETS.diagonal];
    this.viewAll();
  }

  setAnnotations(a) {
    this.annotations = { markers: a?.markers ?? [], lines: a?.lines ?? [] };
    this.redraw();
  }

  focus(point) {
    if (point) this.vpt = [...point];
    this.redraw();
  }

  camera() {
    return { vpt: [...this.vpt], vpr: [...this.vpr], vpd: this.vpd, vpf: 22.5 };
  }

  /// The `$vp*` the program assigned (only those it did).
  setFileView(v) {
    this.setCamera(v);
  }

  setCamera(c) {
    if (c?.vpt) this.vpt = [...c.vpt];
    if (c?.vpr) this.vpr = [...c.vpr];
    if (c?.vpd) this.vpd = c.vpd;
    this.redraw();
  }

  /// A ray through a point of the canvas (CSS pixels), in model
  /// coordinates: {origin, direction}.
  rayAt(x, y) {
    const { w, h, scale } = this.metrics();
    const m = rotation(this.vpr);
    const sx = (x - w / 2) / scale;
    const sy = -(y - h / 2) / scale;
    if (this.settings.orthographic) {
      const origin = mulT(m, [sx, sy, this.vpd]).map((v, k) => v + this.vpt[k]);
      return { origin, direction: mulT(m, [0, 0, -1]) };
    }
    // A point on the target's plane is seen at its own size (see project).
    const eye = mulT(m, [0, 0, this.vpd]).map((v, k) => v + this.vpt[k]);
    const d = mulT(m, [sx, sy, -this.vpd]);
    const len = Math.hypot(...d);
    return { origin: eye, direction: d.map((v) => v / len) };
  }

  dispose() {
    this.abort.abort();
    cancelAnimationFrame(this.frame);
  }

  // --- Drawing ---

  resize() {
    const r = this.canvas.getBoundingClientRect();
    const dpr = globalThis.devicePixelRatio || 1;
    this.canvas.width = Math.max(1, Math.round(r.width * dpr));
    this.canvas.height = Math.max(1, Math.round(r.height * dpr));
    this.redraw();
  }

  metrics() {
    const dpr = globalThis.devicePixelRatio || 1;
    const w = this.canvas.width / dpr;
    const h = this.canvas.height / dpr;
    // Scale so that the distance's view spans the canvas's smaller side.
    const scale = Math.min(w, h) / (this.vpd * 0.45);
    return { w, h, dpr, scale };
  }

  project(p, m, metrics) {
    const q = mul(m, [p[0] - this.vpt[0], p[1] - this.vpt[1], p[2] - this.vpt[2]]);
    let k = 1;
    if (!this.settings.orthographic) {
      // The eye is `vpd` from the target: the target's plane keeps its
      // size, nearer points grow and farther ones shrink.
      const depth = this.vpd - q[2];
      k = this.vpd / Math.max(depth, 1e-3);
    }
    return [metrics.w / 2 + q[0] * k * metrics.scale, metrics.h / 2 - q[1] * k * metrics.scale, q[2]];
  }

  redraw() {
    if (this.frame) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = 0;
      this.draw();
    });
  }

  draw() {
    const ctx = this.ctx;
    const metrics = this.metrics();
    const { w, h, dpr } = metrics;
    const scheme = SCHEMES[this.settings.scheme] ?? SCHEMES.Cornfield;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = scheme.bg;
    ctx.fillRect(0, 0, w, h);
    const m = rotation(this.vpr);
    const eyeDir = mulT(m, [0, 0, 1]);

    if (this.tris && !this.tooLarge) {
      const drawn = this.tris.map((t) => {
        const p = t.v.map((v) => this.project(v, m, metrics));
        return { p, z: (p[0][2] + p[1][2] + p[2][2]) / 3, t };
      });
      drawn.sort((a, b) => a.z - b.z);
      ctx.lineJoin = "round";
      for (const { p, t } of drawn) {
        const c = t.color ?? scheme.face;
        // A zero normal marks an unlit vertex (render::packed).
        const lit = t.n ? 0.35 + 0.65 * Math.abs(t.n[0] * eyeDir[0] + t.n[1] * eyeDir[1] + t.n[2] * eyeDir[2]) : 1;
        const rgb = `rgb(${[0, 1, 2].map((k) => Math.round(255 * Math.min(1, c[k] * lit))).join(",")})`;
        ctx.beginPath();
        ctx.moveTo(p[0][0], p[0][1]);
        ctx.lineTo(p[1][0], p[1][1]);
        ctx.lineTo(p[2][0], p[2][1]);
        ctx.closePath();
        ctx.fillStyle = rgb;
        ctx.fill();
        // A hairline in the face's own colour hides the seams between
        // triangles that antialiasing leaves.
        ctx.strokeStyle = this.settings.edges ? scheme.edge : rgb;
        ctx.lineWidth = this.settings.edges ? 0.7 : 0.5;
        ctx.stroke();
      }
    } else if (this.tooLarge && this.bbox) {
      this.drawBox(this.bbox.min, this.bbox.max, m, metrics, scheme.edge);
    }
    if (this.edges2d.length) {
      ctx.strokeStyle = this.edgeColor;
      ctx.lineWidth = 2;
      ctx.beginPath();
      for (const [a, b] of this.edges2d) {
        const pa = this.project(a, m, metrics);
        const pb = this.project(b, m, metrics);
        ctx.moveTo(pa[0], pa[1]);
        ctx.lineTo(pb[0], pb[1]);
      }
      ctx.stroke();
    }

    if (this.settings.axes) {
      const len = this.vpd * 0.3;
      const o = this.project([0, 0, 0], m, metrics);
      [
        [[len, 0, 0], "#d33"],
        [[0, len, 0], "#3a3"],
        [[0, 0, len], "#33d"],
      ].forEach(([e, col]) => {
        const q = this.project(e, m, metrics);
        ctx.strokeStyle = col;
        ctx.lineWidth = 1;
        ctx.beginPath();
        ctx.moveTo(o[0], o[1]);
        ctx.lineTo(q[0], q[1]);
        ctx.stroke();
      });
    }

    if (this.settings.crosshairs) {
      ctx.strokeStyle = "rgba(128,128,128,0.8)";
      ctx.beginPath();
      ctx.moveTo(w / 2 - 10, h / 2);
      ctx.lineTo(w / 2 + 10, h / 2);
      ctx.moveTo(w / 2, h / 2 - 10);
      ctx.lineTo(w / 2, h / 2 + 10);
      ctx.stroke();
    }

    for (const line of this.annotations.lines) {
      const pts = (line.points ?? []).map((p) => this.project(p, m, metrics));
      if (pts.length < 2) continue;
      ctx.strokeStyle = line.color ?? "#e0f";
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(pts[0][0], pts[0][1]);
      for (const p of pts.slice(1)) ctx.lineTo(p[0], p[1]);
      if (line.closed) ctx.closePath();
      ctx.stroke();
    }
    for (const mk of this.annotations.markers) {
      if (mk.bboxMin && mk.bboxMax) this.drawBox(mk.bboxMin, mk.bboxMax, m, metrics, mk.color ?? "#e0f");
      if (mk.point?.length !== 3) continue;
      const p = this.project(mk.point, m, metrics);
      ctx.fillStyle = mk.color ?? "#e0f";
      ctx.beginPath();
      ctx.arc(p[0], p[1], 9, 0, 2 * Math.PI);
      ctx.fill();
      if (mk.label != null) {
        ctx.fillStyle = "#fff";
        ctx.font = "bold 11px system-ui, sans-serif";
        ctx.textAlign = "center";
        ctx.textBaseline = "middle";
        ctx.fillText(String(mk.label), p[0], p[1] + 0.5);
      }
    }
  }

  drawBox(min, max, m, metrics, color) {
    const c = [];
    for (const x of [min[0], max[0]]) for (const y of [min[1], max[1]]) for (const z of [min[2], max[2]]) c.push(this.project([x, y, z], m, metrics));
    const edges = [[0, 1], [2, 3], [4, 5], [6, 7], [0, 2], [1, 3], [4, 6], [5, 7], [0, 4], [1, 5], [2, 6], [3, 7]];
    const ctx = this.ctx;
    ctx.strokeStyle = color;
    ctx.lineWidth = 1;
    ctx.setLineDash([4, 3]);
    ctx.beginPath();
    for (const [a, b] of edges) {
      ctx.moveTo(c[a][0], c[a][1]);
      ctx.lineTo(c[b][0], c[b][1]);
    }
    ctx.stroke();
    ctx.setLineDash([]);
  }

  // --- Input: drag to orbit, right or shift drag to pan, wheel to zoom ---

  listen() {
    this.abort = new AbortController();
    const opts = { signal: this.abort.signal };
    const c = this.canvas;
    let drag = null;
    c.addEventListener(
      "pointerdown",
      (e) => {
        c.setPointerCapture(e.pointerId);
        drag = { x: e.clientX, y: e.clientY, pan: e.button === 2 || e.shiftKey, moved: false };
      },
      opts,
    );
    c.addEventListener(
      "pointermove",
      (e) => {
        if (!drag) return;
        const dx = e.clientX - drag.x;
        const dy = e.clientY - drag.y;
        if (Math.abs(dx) + Math.abs(dy) > 2) drag.moved = true;
        drag.x = e.clientX;
        drag.y = e.clientY;
        if (drag.pan) {
          const { scale } = this.metrics();
          const m = rotation(this.vpr);
          const d = mulT(m, [-dx / scale, dy / scale, 0]);
          this.vpt = this.vpt.map((v, k) => v + d[k]);
        } else {
          this.vpr = [Math.max(0, Math.min(180, this.vpr[0] - dy * 0.5)), 0, (this.vpr[2] - dx * 0.5 + 360) % 360];
        }
        this.redraw();
      },
      opts,
    );
    c.addEventListener(
      "pointerup",
      (e) => {
        if (drag && !drag.moved && this.onPick) {
          const r = c.getBoundingClientRect();
          this.onPick(this.rayAt(e.clientX - r.left, e.clientY - r.top));
        }
        drag = null;
      },
      opts,
    );
    c.addEventListener("contextmenu", (e) => e.preventDefault(), opts);
    c.addEventListener(
      "wheel",
      (e) => {
        e.preventDefault();
        this.vpd = Math.max(1, this.vpd * Math.exp(e.deltaY * 0.002));
        this.redraw();
      },
      { ...opts, passive: false },
    );
  }
}
