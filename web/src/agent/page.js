// What the page does for the agent's tools (crates/cli/src/mcp/tools/
// browser.rs): each request of the bridge, answered from the app's
// document, editor, 3D view and console.
//
// Positions are the editor's, `[line, character]` with 0-based lines and
// UTF-16 columns; the command line converts them to and from the agent's
// byte columns through lang::source, so nothing here counts bytes.
//
// `version` is the page's revision of the document: it counts every change
// of the text (the user's and the agent's) and every switch of example, so
// an agent that read version 12 can only edit version 12. The editor's own
// counter restarts its line with each load, and a library tab loads, which
// would make a stale edit look current.

import { agentLine, browserName, captureSize, diagnostics, toBase64 } from "./link.js";

export class PageAgent {
  /// `app` is the page (app.js); `ui` the panel, which confirms edits
  /// when the user asked to be asked, and shows what the agent is doing.
  constructor(app, ui) {
    this.app = app;
    this.ui = ui;
  }

  hello() {
    const d = this.app.doc;
    return {
      file: d?.example.file ?? null,
      browser: browserName(navigator.userAgent),
      page: location.origin + location.pathname,
    };
  }

  async handle(method, params) {
    const fn = {
      read: () => this.read(),
      edit: () => this.edit(params),
      reveal: () => this.reveal(params),
      camera: () => this.camera(params),
      capture: () => this.capture(params),
      annotate: () => this.annotate(params),
      console: () => this.console(),
    }[method];
    if (!fn) throw new Error(`the page does not know "${method}" (reload it: it may be older than your neoscad)`);
    this.ui.activity(method);
    try {
      return await fn();
    } finally {
      this.ui.activity(null);
    }
  }

  doc() {
    const d = this.app.doc;
    if (!d) throw new Error("the page has no document open yet");
    return d;
  }

  run() {
    const c = this.app.console;
    return { mode: this.app.lastMode, summary: c.summaryText ?? "", state: c.summaryKind ?? "idle" };
  }

  read() {
    const d = this.doc();
    const editor = this.app.editor;
    return {
      file: d.example.file,
      version: this.app.revision,
      text: d.text,
      // The selection only when the editor shows the document (not a
      // library file opened from it).
      selection: this.app.activeTab === null ? editor.selectionPositions() : null,
      values: d.customizer.values,
      parts: !!d.parts,
      // The NeoSCAD extensions the page runs with (View menu, or a link's),
      // which the agent's model tools add to its server's --enable: a
      // `sketch` model that previews here must not be unknown modules to
      // the agent.
      enable: this.app.extensions(),
      run: this.run(),
      diagnostics: diagnostics(this.app.console.lines, d.path),
    };
  }

  async edit({ version, edits, summary }) {
    this.doc();
    const stale = () =>
      new Error(`the page's text changed (it is at version ${this.app.revision}, not ${version}): editor_read again`);
    if (version !== this.app.revision) throw stale();
    if (!Array.isArray(edits) || !edits.length) throw new Error("no edits");
    if (this.ui.askFirst) {
      const ok = await this.ui.confirm(`${this.ui.clientName()} wants to change ${summary || "the text"}.`);
      if (!ok) throw new Error("the user declined this edit (the text is unchanged); ask them what they want instead");
      // The user could have typed while deciding.
      if (version !== this.app.revision) throw stale();
    }
    // Back to the document if a library file is showing.
    if (this.app.activeTab !== null) this.app.showTab(null);
    this.app.editor.agentEdit(edits);
    // The editor reported the change synchronously (app.editorMessage),
    // which counted it and scheduled a preview.
    return { version: this.app.revision };
  }

  reveal({ from, to }) {
    this.doc();
    if (this.app.activeTab !== null) this.app.showTab(null);
    this.app.showMobile("editor");
    this.app.editor.revealRange(from[0], from[1], to[0], to[1]);
    return {};
  }

  camera({ view, fit, vpt, vpr, vpd }) {
    const v = this.app.viewer;
    if (view) v.preset(view);
    if (fit) v.viewAll();
    const set = {};
    if (vpt) set.vpt = vpt;
    if (vpr) set.vpr = vpr;
    if (vpd) set.vpd = vpd;
    if (Object.keys(set).length) v.setFileView(set);
    return v.camera();
  }

  async capture({ size = 768 }) {
    // The agent's edit schedules a preview; it wants to see its result.
    await this.app.whenIdle();
    const v = this.app.viewer;
    const canvas = v.canvas ?? document.querySelector("#viewport");
    const dpr = globalThis.devicePixelRatio || 1;
    const [w, h] = captureSize(canvas.clientWidth * dpr, canvas.clientHeight * dpr, size);
    let png;
    if (v.image) {
      const img = await v.image(w, h);
      png = await encodePNG(img.width, img.height, img.rgba);
    } else {
      // The canvas fallback draws with a 2D context, which keeps its
      // pixels: scale a copy.
      png = await copyCanvas(canvas, w, h);
    }
    return { png: toBase64(png), width: w, height: h, backend: v.kind, camera: v.camera(), run: this.run() };
  }

  annotate({ markers = [], lines = [] }) {
    const empty = !markers.length && !lines.length;
    this.app.viewer.setAnnotations(empty ? null : { markers, lines });
    return {};
  }

  console() {
    const d = this.doc();
    return { ...this.run(), lines: this.app.console.lines.map((l) => agentLine(l, d.path)) };
  }
}

/// RGBA rows (top first) as PNG bytes.
async function encodePNG(width, height, rgba) {
  const data = new ImageData(new Uint8ClampedArray(rgba.buffer, rgba.byteOffset, rgba.byteLength), width, height);
  const c = new OffscreenCanvas(width, height);
  c.getContext("2d").putImageData(data, 0, 0);
  return new Uint8Array(await (await c.convertToBlob({ type: "image/png" })).arrayBuffer());
}

async function copyCanvas(canvas, width, height) {
  const c = new OffscreenCanvas(width, height);
  c.getContext("2d").drawImage(canvas, 0, 0, width, height);
  return new Uint8Array(await (await c.convertToBlob({ type: "image/png" })).arrayBuffer());
}
