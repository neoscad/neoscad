// `<neoscad-viewer>`: a model in a page with no script of the page's own.
//
//   <neoscad-viewer src="bracket.scad" mode="render" edges></neoscad-viewer>
//   <neoscad-viewer><script type="text/x-openscad">cube(10);</script></neoscad-viewer>
//
// Every element on a page shares one engine (one worker: each is a whole
// wasm instance), and the queue in front of it (scheduler.js) keeps their
// runs from replacing or cancelling each other.
//
// GPU contexts are the scarce thing with many elements: browsers keep a
// handful of WebGL contexts per page (Chromium 16) and drop the oldest
// beyond that. So an element makes its viewer only once it is on screen
// with a size, and gives it back after it has been off screen for a while
// (OFFSCREEN_MS), keeping its scene and camera to show again on return.
// Its model runs the first time it comes into view, not on load.

import { OBSERVED, VIEW_ONLY, inlineSource, isSourceScript, nameFromSrc, parseAttributes } from "./attributes.js";
// From the package entry, not its sources: dist/element.js imports
// dist/index.js, so the page has one shared engine and one copy of it.
import { DEFAULT_VIEW, createViewer, fileViewChanged, sharedEngine } from "./index.js";

/// How long an element stays off screen before its viewer is disposed.
export const OFFSCREEN_MS = 4000;

const STYLE = `
:host { display: block; position: relative; width: 400px; height: 300px; contain: content; }
:host([hidden]) { display: none; }
.view { position: absolute; inset: 0; }
.status, .notice { position: absolute; left: 8px; right: 8px; font: 12px/1.4 system-ui, sans-serif;
  padding: 4px 8px; border-radius: 4px; pointer-events: none; white-space: pre-wrap; overflow-wrap: anywhere; }
.status { bottom: 8px; background: rgba(0, 0, 0, 0.6); color: #fff; }
.status[data-kind="failed"] { background: rgba(160, 20, 20, 0.85); }
.notice { top: 8px; background: rgba(255, 255, 255, 0.85); color: #333; }
[hidden] { display: none; }
`;

const Base = globalThis.HTMLElement ?? class {};

export class NeoSCADViewer extends Base {
  static observedAttributes = OBSERVED;

  constructor() {
    super();
    this.state = { connected: false, visible: false, sized: false, started: false };
    this._source = null; // set through the property: wins over src and inline
    this._loaded = null; // the source last loaded from src or inline
    this.viewer = null;
    this.model = null;
    this.engine = null;
    this.result = null;
    this.scene = null;
    this.savedCamera = null;
    this.fileView = null;
    this.offTimer = null;
    this.viewerToken = 0;
    this.runToken = 0;
  }

  // --- Lifecycle -------------------------------------------------------------

  connectedCallback() {
    if (!this.shadowRoot) {
      const root = this.attachShadow({ mode: "open" });
      const style = document.createElement("style");
      style.textContent = STYLE;
      this.host = document.createElement("div");
      this.host.className = "view";
      this.host.setAttribute("part", "view");
      this.statusEl = document.createElement("div");
      this.statusEl.className = "status";
      this.statusEl.setAttribute("part", "status");
      this.statusEl.setAttribute("role", "status");
      this.statusEl.hidden = true;
      this.noticeEl = document.createElement("div");
      this.noticeEl.className = "notice";
      this.noticeEl.setAttribute("part", "notice");
      this.noticeEl.hidden = true;
      root.append(style, this.host, this.noticeEl, this.statusEl);
    }
    this.state.connected = true;
    this.opts = parseAttributes((n) => this.getAttribute(n), DEFAULT_VIEW);
    this.intersection = new IntersectionObserver((entries) => {
      const e = entries[entries.length - 1];
      this.state.visible = e.isIntersecting;
      this.state.sized = e.boundingClientRect.width > 0 && e.boundingClientRect.height > 0;
      this.visibilityChanged();
    });
    this.intersection.observe(this);
    this.sizes = new ResizeObserver(([e]) => {
      this.state.sized = e.contentRect.width > 0 && e.contentRect.height > 0;
      this.visibilityChanged();
    });
    this.sizes.observe(this);
    // Inline source edited in place (a page's own editor writing into the
    // script element): run it again.
    this.mutations = new MutationObserver(() => {
      if (!this.state.started || this._source != null || this.opts.src) return;
      this.loadInline();
      this.run();
    });
    this.mutations.observe(this, { childList: true, subtree: true, characterData: true });
  }

  disconnectedCallback() {
    this.state.connected = false;
    this.state.started = false;
    this.intersection?.disconnect();
    this.sizes?.disconnect();
    this.mutations?.disconnect();
    clearTimeout(this.offTimer);
    this.disposeViewer(false);
    this.model?.dispose();
    this.model = null;
    this.scene = null;
  }

  attributeChangedCallback(name, before, after) {
    if (!this.state.connected || before === after) return;
    this.opts = parseAttributes((n) => this.getAttribute(n), DEFAULT_VIEW);
    if (VIEW_ONLY.has(name)) {
      this.applyView(name === "view");
      return;
    }
    if (name === "assets-url" || name === "backend") {
      // Another engine or viewer: start over.
      this.disconnectedCallback();
      this.connectedCallback();
      return;
    }
    if (!this.state.started) return;
    if (name === "src" || name === "name") {
      this._loaded = null;
      this.model?.dispose();
      this.model = null;
    }
    if (name === "color-scheme") this.applyView(false);
    this.run();
  }

  visibilityChanged() {
    const on = this.state.connected && this.state.visible && this.state.sized;
    if (on) {
      clearTimeout(this.offTimer);
      this.offTimer = null;
      if (!this.viewer) this.makeViewer();
      if (!this.state.started) {
        this.state.started = true;
        this.run();
      }
    } else if (this.viewer && this.offTimer == null) {
      this.offTimer = setTimeout(() => {
        this.offTimer = null;
        this.disposeViewer(true);
      }, OFFSCREEN_MS);
    }
  }

  // --- The viewer --------------------------------------------------------------

  async makeViewer() {
    const token = ++this.viewerToken;
    const viewer = await createViewer(this.host, {
      assetsUrl: this.opts.assetsUrl,
      backend: this.opts.backend,
      settings: this.opts.settings,
    });
    if (token !== this.viewerToken || !this.state.connected) {
      viewer.dispose();
      return;
    }
    this.viewer = viewer;
    this.dataset.backend = viewer.kind;
    this.noticeEl.textContent = viewer.notice ?? "";
    this.noticeEl.hidden = !viewer.notice;
    if (this.scene) viewer.setScene(this.scene);
    if (this.savedCamera) viewer.setFileView(this.savedCamera);
    else if (this.opts.view) viewer.preset(this.opts.view);
    this.dispatchEvent(new CustomEvent("neoscad-viewer", { detail: { kind: viewer.kind } }));
  }

  disposeViewer(keepCamera) {
    this.viewerToken += 1;
    if (!this.viewer) return;
    this.savedCamera = keepCamera ? this.viewer.camera() : null;
    this.viewer.dispose();
    this.viewer = null;
    delete this.dataset.backend;
  }

  applyView(preset) {
    if (!this.viewer) return;
    this.viewer.setSettings(this.opts.settings);
    if (preset && this.opts.view) this.viewer.preset(this.opts.view);
  }

  // --- The model -----------------------------------------------------------------

  /// The model's source: what was set through this property, else the
  /// fetched `src`, else the inline script's.
  get source() {
    return this._source ?? this._loaded;
  }
  set source(text) {
    this._source = text == null ? null : String(text);
    if (this.state.started) this.run();
  }

  loadInline() {
    const script = [...this.children].find(isSourceScript);
    this._loaded = script ? inlineSource(script.textContent) : null;
  }

  async loadSource() {
    if (this._source != null) return this._source;
    if (this.opts.src) {
      const url = new URL(this.opts.src, document.baseURI);
      const res = await fetch(url);
      if (!res.ok) throw new Error(`${this.opts.src}: HTTP ${res.status}`);
      this._loaded = await res.text();
    } else if (this._loaded == null) {
      this.loadInline();
    }
    return this._loaded;
  }

  /// Run the model again (after the page changed something it reads).
  /// Resolves to the result, or null when a newer run replaced it.
  async update() {
    return this.run();
  }

  async run() {
    if (!this.state.connected) return null;
    const token = ++this.runToken;
    this.say("Loading…", "running");
    try {
      this.engine ??= sharedEngine({ assetsUrl: this.opts.assetsUrl });
      const source = this._source ?? this._loaded ?? (await this.loadSource());
      if (token !== this.runToken) return null;
      if (source == null) {
        this.say("No model: give the element a src, or a <script type=\"text/x-openscad\"> inside it.", "failed");
        return null;
      }
      this.model ??= this.engine.model({
        name: this.opts.name ?? nameFromSrc(this.opts.src),
        camera: () => this.viewer?.camera() ?? this.savedCamera,
      });
      this.say(this.opts.mode === "render" ? "Rendering…" : "Previewing…", "running");
      const r = await this.model.run(source, {
        mode: this.opts.mode,
        enable: this.opts.enable,
        colorScheme: this.opts.settings.scheme,
      });
      if (r.superseded || token !== this.runToken) return null;
      this.show(r);
      return r;
    } catch (e) {
      if (token !== this.runToken || e?.name === "AbortError") return null;
      this.say(e.message, "failed");
      this.dataset.state = "failed";
      this.dispatchEvent(new CustomEvent("neoscad-error", { detail: { message: e.message, error: e, result: null } }));
      return null;
    }
  }

  show(r) {
    this.result = r;
    if (r.scene || r.ok) {
      this.scene = r.scene;
      this.viewer?.setScene(r.scene);
    }
    if (r.fileView && fileViewChanged(r.fileView, this.fileView)) {
      this.savedCamera = null;
      this.viewer?.setFileView(r.fileView);
    }
    this.fileView = r.fileView;
    this.dataset.state = r.ok ? "ok" : "failed";
    if (r.ok) this.say(null);
    else {
      const first = r.errors[0];
      const where = first?.line != null ? ` (${first.path ?? "main.scad"}, line ${first.line + 1})` : "";
      this.say(first ? `${first.message}${where}` : r.summary || "The model failed.", "failed");
    }
    this.dispatchEvent(new CustomEvent("neoscad-result", { detail: r }));
    if (!r.ok) {
      const message = r.errors[0]?.text ?? r.summary;
      this.dispatchEvent(new CustomEvent("neoscad-error", { detail: { message, error: null, result: r } }));
    }
  }

  /// Export the model (see Model.export).
  async export(format, options = {}) {
    const source = this.source ?? (await this.loadSource());
    this.engine ??= sharedEngine({ assetsUrl: this.opts.assetsUrl });
    this.model ??= this.engine.model({ name: this.opts.name ?? nameFromSrc(this.opts.src) });
    return this.model.export(source, format, { enable: this.opts.enable, ...options });
  }

  /// The view as a PNG Blob (null while the element has no viewer).
  image(width, height, options) {
    return this.viewer ? this.viewer.image(width, height, options) : Promise.resolve(null);
  }

  say(text, kind = "idle") {
    if (!this.statusEl) return;
    this.statusEl.hidden = text == null;
    this.statusEl.dataset.kind = kind;
    this.statusEl.textContent = text ?? "";
  }
}

if (globalThis.customElements && !customElements.get("neoscad-viewer")) {
  customElements.define("neoscad-viewer", NeoSCADViewer);
}
