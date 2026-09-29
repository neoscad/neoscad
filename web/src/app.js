// The NeoSCAD web demo: the macOS app's document window in a page
// (apple/App/Document/DocumentView.swift): the editor above the console
// on the left, the 3D view in the middle, the inspector (Customizer,
// Check, Measure) on the right, and a top bar with the example picker,
// Preview / Render, Export and View. On a phone the view stays on top and
// the editor, console and inspector panels become tabs below it.
//
// The document loop is the app's too (DocumentLoop.swift): each pause in
// typing and each customizer edit runs a preview; Render (F6) renders.
// Runs go through the engine client, which coalesces them and respawns
// the worker when one is cancelled or crashes.

import { parseConnect, stripConnect } from "./agent/link.js";
import { setEditorHandler, loadEditor } from "./editor-host.js";
import { createEngine, ensureLibraries } from "./engine/index.js";
import { EngineError, EngineRestarted } from "./engine/client.js";
import {
  EXPORT_FORMATS,
  ErrorKind,
  Requests,
  applyEdits,
  betweenResult,
  checkReport,
  docPath,
  fileURI,
  fileViewChanged,
  measureResult,
  parameterGroups,
  runOptions,
  runResult,
  sectionResult,
  uriPath,
} from "./engine/protocol.js";
import { loadExampleText, loadManifest } from "./examples.js";
import { CustomizerModel } from "./model/customizer.js";
import { applyTheme, loadSite } from "./site.js";
import { Store } from "./store.js";
import { AgentPanel } from "./ui/agent.js";
import { CheckPanel, checkOptions, findingMarker } from "./ui/check.js";
import { ConsolePanel, describe } from "./ui/console.js";
import { CustomizerPanel } from "./ui/customizer.js";
import { clear, download, h } from "./ui/dom.js";
import { MeasurePanel } from "./ui/measure.js";
import { Menu } from "./ui/menu.js";
import { createViewer } from "./view/index.js";

const PREVIEW_DELAY_MS = 300;
const INSPECTOR_TABS = [
  ["customizer", "Customizer"],
  ["check", "Check"],
  ["measure", "Measure"],
];
const SETTINGS = {
  example: null,
  inspector: "customizer",
  inspectorShown: true,
  consoleCollapsed: false,
  view: { axes: true, scales: false, grid: false, edges: false, crosshairs: false, orthographic: false, lighting: "openscad", scheme: "Cornfield" },
  check: {},
  layout: { left: 34, right: 22, console: 30 },
};

const $ = (sel) => document.querySelector(sel);

class App {
  async start() {
    this.store = Store.fromWindow();
    this.settings = this.store.settings(SETTINGS);
    this.settings.view = { ...SETTINGS.view, ...this.settings.view };
    this.settings.layout = { ...SETTINGS.layout, ...this.settings.layout };
    this.build = await fetch(new URL("./build.json", document.baseURI))
      .then((r) => (r.ok ? r.json() : {}))
      .catch(() => ({}));
    this.build = { engine: "mock", view: "none", version: "dev", ...this.build };

    this.doc = null; // {example, path, text, version, parts, customizer}
    // The document's revision for a connected agent (ui/agent.js): every
    // change of the text and every switch of example counts, so an edit
    // made on text the agent read earlier is refused.
    this.revision = 1;
    this.running = null; // the run in progress, for whenIdle()
    // An agent's connect link (`#connect=PORT.KEY`, from `neoscad mcp
    // --browser`) is read, then taken out of the address bar at once: it
    // is a key, and should not stay in the history or a shared URL.
    const hash = location.hash;
    const connectLink = parseConnect(hash);
    if (connectLink) history.replaceState(null, "", location.pathname + location.search + stripConnect(hash));
    this.tabs = []; // library files open read-only: {path, text}
    this.activeTab = null; // null: the document
    this.previewTimer = null;
    this.lastMode = null;

    this.layout();
    loadSite().then((site) => this.applySite(site));

    this.engine = createEngine(this.build);
    this.engine.on("status", (s) => this.engineStatus(s));
    this.engine.on("lsp", (m) => this.editor?.lspReceive(m));
    setEditorHandler((m) => this.editorMessage(m));
    this.editor = await loadEditor();

    const { viewer, canvas, notice } = await createViewer($("#viewport"), {
      view: this.build.view,
      scheme: this.settings.view.scheme,
    });
    this.viewer = viewer;
    // A scheme saved by a build whose viewer knew more schemes than this
    // one (the canvas fallback knows a few) falls back to Cornfield.
    if (!viewer.schemes.includes(this.settings.view.scheme)) this.settings.view.scheme = "Cornfield";
    this.viewer.setSettings(this.settings.view);
    this.viewer.onPick = (ray) => this.pick(ray);
    document.documentElement.dataset.view = viewer.kind;
    if (notice) this.viewNotice(notice);
    new ResizeObserver(() => this.viewer.resize()).observe(canvas);

    if (this.build.engine === "mock") {
      this.banner(
        "This build has no wasm engine, so every result is a canned stand-in (the mock worker). " +
          "Build the core with scripts/web/build-core.sh for real ones.",
      );
    }

    this.engine.start().catch((e) => this.console.setSummary(`The engine did not start: ${e.message}`, "failed"));

    try {
      this.manifest = await loadManifest();
    } catch (e) {
      this.console.setSummary(`No examples: ${e.message}`, "failed");
      return;
    }
    this.fillExamples();
    const wanted = new URLSearchParams(hash.slice(1)).get("example");
    const id = [wanted, this.settings.example, this.manifest.default].find((x) => this.manifest.examples.some((e) => e.id === x));
    await this.openExample(id);
    document.documentElement.dataset.ready = "true";
    // Connect an agent: the link this page was opened with, or this tab's
    // link from before a reload (quietly: its agent may be gone).
    const link = connectLink ?? AgentPanel.saved();
    if (connectLink && new URLSearchParams(hash.slice(1)).get("via") === "relay") this.agent.offerRelay(connectLink);
    else if (link) this.agent.connect(link, { quiet: !connectLink });
    // `#agent` (the home page's "Connect your AI agent" link) opens the
    // setup dialog, and is taken out of the address bar like a link.
    else if (hash === "#agent") {
      history.replaceState(null, "", location.pathname + location.search);
      this.agent.open();
    }
  }

  // --- Layout -----------------------------------------------------------

  layout() {
    const L = this.settings.layout;
    const root = document.documentElement;
    root.style.setProperty("--left-width", `${L.left}%`);
    root.style.setProperty("--right-width", `${L.right}%`);
    root.style.setProperty("--console-height", `${L.console}%`);

    this.exampleSelect = h("select", {
      id: "example",
      "aria-label": "Example",
      "data-testid": "example-picker",
      onchange: (e) => this.openExample(e.target.value),
    });
    this.status = h("span", { class: "engine-status", role: "status", "data-testid": "engine-status" });
    this.cancelButton = h("button", { class: "cancel", hidden: true, onclick: () => this.engine.cancel(), title: "Stop the engine (it restarts)" }, "Cancel");

    const exportMenu = new Menu(
      "Export",
      () =>
        Object.entries(EXPORT_FORMATS).map(([id, f]) => ({
          label: `${f.label}…`,
          run: () => this.export(id),
        })),
      { testid: "export-menu" },
    );
    const viewMenu = new Menu("View", () => this.viewItems(), { testid: "view-menu" });
    this.agent = new AgentPanel(this);

    clear(
      $("#topbar"),
      h("a", { class: "home", id: "home-link", href: "https://neoscad.org/" }, "← neoscad.org"),
      h("nav", { class: "site-nav", id: "site-nav", "aria-label": "Site" }),
      this.agent.button,
      h(
        "div",
        { class: "tools" },
        this.exampleSelect,
        h("button", { class: "quiet", title: "Forget your edits to this example", "data-testid": "reset-example", onclick: () => this.resetExample() }, "Reset example"),
        h("span", { class: "divider" }),
        h("button", { class: "primary", title: "Preview (F5)", "data-testid": "preview", onclick: () => this.run("preview") }, "Preview"),
        h("button", { title: "Render (F6, ⌘/Ctrl-Enter)", "data-testid": "render", onclick: () => this.run("render") }, "Render"),
        this.cancelButton,
        h("span", { class: "divider" }),
        exportMenu.el,
        viewMenu.el,
      ),
      this.status,
    );

    this.console = new ConsolePanel($("#console"), { onJump: (at) => this.jump(at) });
    if (this.settings.consoleCollapsed) {
      this.console.collapsed = true;
      this.console.render();
    }
    this.console.collapse.addEventListener("click", () => this.saveSettings({ consoleCollapsed: this.console.collapsed }));

    this.customizer = new CustomizerPanel($("#panel-customizer"), new CustomizerModel(), {
      onChange: () => this.customizerChanged(),
    });
    this.check = new CheckPanel($("#panel-check"), {
      settings: this.settings.check,
      run: () => this.runCheck(),
      onSelect: (f) => {
        this.viewer?.setAnnotations(f ? { markers: [findingMarker(f)] } : null);
        if (f?.point?.length === 3) this.viewer.focus(f.point);
      },
      onSettings: (s) => this.saveSettings({ check: s }),
      onParts: (on) => this.setParts(on),
    });
    this.measure = new MeasurePanel($("#panel-measure"), {
      run: () => this.runMeasure(),
      section: (axis, offset, target) => this.section(axis, offset, target),
      between: (a, b) => this.between(a, b),
      onParts: (on) => this.setParts(on),
      onPicking: () => this.viewer.setAnnotations(null),
    });

    const tabs = $("#inspector-tabs");
    clear(
      tabs,
      INSPECTOR_TABS.map(([id, label]) =>
        h("button", { role: "tab", id: `tab-${id}`, "aria-controls": `panel-${id}`, onclick: () => this.showInspector(id) }, label),
      ),
    );
    this.showInspector(this.settings.inspector, this.settings.inspectorShown);

    // Phone layout: the view on top, and these tabs choose what is below it.
    clear(
      $("#mobile-tabs"),
      [["editor", "Editor"], ["console", "Console"], ...INSPECTOR_TABS].map(([id, label]) =>
        h("button", { role: "tab", "data-pane": id, onclick: () => this.showMobile(id) }, label),
      ),
    );
    this.showMobile("editor");

    this.splitters();
    this.keys();
  }

  applySite(site) {
    applyTheme(site);
    this.agent.setSite(site);
    const home = $("#home-link");
    home.href = site.home;
    home.textContent = `← ${new URL(site.home).host || site.name}`;
    clear(
      $("#site-nav"),
      site.nav.map((n) =>
        h("a", { href: n.href, "aria-current": new URL(n.href).pathname === location.pathname ? "page" : null }, n.label),
      ),
    );
  }

  showInspector(id, shown = true) {
    this.settings.inspector = id;
    document.body.dataset.inspector = id;
    document.body.classList.toggle("inspector-hidden", !shown);
    for (const [tab] of INSPECTOR_TABS) {
      $(`#tab-${tab}`).setAttribute("aria-selected", String(tab === id));
      $(`#panel-${tab}`).hidden = tab !== id;
    }
    this.saveSettings({ inspector: id, inspectorShown: shown });
    requestAnimationFrame(() => this.viewer?.resize());
  }

  /// The View menu's inspector items: choosing the one shown hides it,
  /// as in the macOS app.
  toggleInspector(id) {
    const hidden = document.body.classList.contains("inspector-hidden");
    this.showInspector(id, hidden || this.settings.inspector !== id);
  }

  showMobile(id) {
    document.body.dataset.mobile = id;
    for (const b of document.querySelectorAll("#mobile-tabs button")) b.setAttribute("aria-selected", String(b.dataset.pane === id));
    if (INSPECTOR_TABS.some(([t]) => t === id)) this.showInspector(id, true);
  }

  splitters() {
    const drag = (el, apply) => {
      el.addEventListener("pointerdown", (e) => {
        el.setPointerCapture(e.pointerId);
        const move = (ev) => apply(ev);
        const up = () => {
          el.removeEventListener("pointermove", move);
          el.removeEventListener("pointerup", up);
          this.saveSettings({ layout: this.settings.layout });
          this.viewer?.resize();
        };
        el.addEventListener("pointermove", move);
        el.addEventListener("pointerup", up);
      });
    };
    const root = document.documentElement;
    const clampPct = (x, lo, hi) => Math.min(hi, Math.max(lo, x));
    drag($("#split-left"), (e) => {
      const w = $("#workspace").getBoundingClientRect();
      this.settings.layout.left = clampPct(((e.clientX - w.left) / w.width) * 100, 18, 60);
      root.style.setProperty("--left-width", `${this.settings.layout.left}%`);
    });
    drag($("#split-right"), (e) => {
      const w = $("#workspace").getBoundingClientRect();
      this.settings.layout.right = clampPct(((w.right - e.clientX) / w.width) * 100, 14, 45);
      root.style.setProperty("--right-width", `${this.settings.layout.right}%`);
    });
    drag($("#split-console"), (e) => {
      const p = $("#pane-left").getBoundingClientRect();
      this.settings.layout.console = clampPct(((p.bottom - e.clientY) / p.height) * 100, 8, 80);
      root.style.setProperty("--console-height", `${this.settings.layout.console}%`);
    });
  }

  /// F5 and F6 are the app's Preview and Render; F5 would reload the page,
  /// so it is taken in the capture phase, before CodeMirror or the
  /// browser. ⌘/Ctrl-Enter renders too (CodeMirror would insert a line).
  keys() {
    window.addEventListener(
      "keydown",
      (e) => {
        const mod = e.metaKey || e.ctrlKey;
        let mode = null;
        if (e.key === "F5" && !mod) mode = "preview";
        else if (e.key === "F6" || (e.key === "Enter" && mod)) mode = "render";
        if (!mode) return;
        e.preventDefault();
        e.stopPropagation();
        if (!e.repeat) this.run(mode);
      },
      { capture: true },
    );
  }

  banner(text) {
    const b = $("#banner");
    b.textContent = text;
    b.hidden = false;
  }

  viewNotice(text) {
    const n = $("#view-notice");
    n.textContent = text;
    n.hidden = false;
  }

  saveSettings(change) {
    Object.assign(this.settings, change);
    this.store.setSettings(this.settings);
  }

  engineStatus({ state, message }) {
    const s = this.status;
    this.cancelButton.hidden = state !== "working";
    clearTimeout(this.statusTimer);
    if (state === "working") {
      // A short request should not flash the indicator.
      this.statusTimer = setTimeout(() => {
        s.dataset.state = "working";
        clear(s, h("span", { class: "spinner", "aria-hidden": "true" }), "working…");
      }, 150);
    } else if (state === "restarted") {
      s.dataset.state = "restarted";
      s.textContent = "engine restarted";
      s.title = message;
      this.restartedAt = performance.now();
      if (this.lastMode && message !== "cancelled") this.console.setSummary(`The engine restarted: ${message}.`, "failed");
      else if (message === "cancelled") this.console.setSummary("Cancelled; the engine restarted.", "failed");
    } else if (state === "failed") {
      s.dataset.state = "failed";
      s.textContent = "engine stopped";
      s.title = message;
      this.console.setSummary(`The engine stopped: ${message}. Reload the page to try again.`, "failed");
    } else {
      // Keep "engine restarted" readable for a few seconds.
      const wait = this.restartedAt ? Math.max(0, 4000 - (performance.now() - this.restartedAt)) : 0;
      this.statusTimer = setTimeout(() => {
        s.dataset.state = "idle";
        s.textContent = this.build.engine === "mock" ? "mock engine" : "ready";
        s.title = this.engine.info?.version ? `Engine ${this.engine.info.version}` : "";
      }, wait);
    }
  }

  // --- Examples and the document -----------------------------------------

  fillExamples() {
    clear(
      this.exampleSelect,
      this.manifest.examples.map((e) => h("option", { value: e.id }, e.heavy ? `${e.title} (heavy)` : e.title)),
    );
  }

  async openExample(id) {
    const example = this.manifest.examples.find((e) => e.id === id);
    if (!example) return;
    const previous = this.doc;
    this.exampleSelect.value = id;
    this.saveSettings({ example: id });
    history.replaceState(null, "", `#example=${encodeURIComponent(id)}`);
    clearTimeout(this.previewTimer);
    this.previewTimer = null;
    this.revision += 1;

    const original = await loadExampleText(example);
    const text = this.store.exampleText(id) ?? original;
    // A heavy example leaves the worker's wasm memory at its high-water
    // mark (it never shrinks), so the next one starts in a fresh worker.
    if (previous?.example.heavy) await this.engine.restart("freeing the heavy example's memory").catch(() => {});
    if (previous) this.engine.close(previous.path);
    this.measurement = null;
    this.fileView = null;

    const path = docPath(example.file);
    this.doc = {
      example,
      original,
      path,
      text,
      version: 0,
      parts: this.store.get(`example.${id}.parts`, example.parts),
      customizer: new CustomizerModel([], this.store.parameterValues(id)),
    };
    this.tabs = [];
    this.activeTab = null;
    this.renderTabs();
    this.editor.load(text, fileURI(path), false);
    this.customizer.setModel(this.doc.customizer);
    this.check.setParts(this.doc.parts);
    this.measure.setParts(this.doc.parts);
    this.check.report = null;
    this.check.render();
    this.measure.result = null;
    this.measure.render();
    this.viewer.setAnnotations(null);
    this.console.setLines([]);
    await this.engine.open(path, text).catch((e) => this.fail(e));
    if (example.autorun) this.run("preview");
    else {
      this.console.setSummary(
        example.note || "This example is heavy: press Preview (F5) or Render (F6) to run it.",
        "idle",
      );
      this.refreshParameters();
    }
  }

  resetExample() {
    if (!this.doc) return;
    const id = this.doc.example.id;
    this.store.resetExample(id);
    this.store.set(`example.${id}.parts`, null);
    // Reopen from the shipped text.
    this.openExample(id);
  }

  /// A message from the editor (the macOS protocol, EditorController.swift).
  editorMessage(m) {
    switch (m.type) {
      case "changes": {
        if (this.activeTab !== null || !this.doc) return null;
        const d = this.doc;
        d.text = applyEdits(d.text, m.edits);
        d.version = m.version;
        this.revision += 1;
        this.engine.edit(d.path, m.edits).catch(() => {});
        this.store.setExampleText(d.example.id, d.text, d.original);
        this.schedulePreview();
        return null;
      }
      case "lsp":
        this.engine.lsp(m.message);
        return null;
      case "open":
        this.openLibrary(uriPath(m.uri), { line: m.line, character: m.character });
        return null;
      case "command":
        if (m.name === "preview" || m.name === "render") this.run(m.name);
        return null;
      case "log":
        console.warn("editor:", m.message);
        return null;
      default:
        return null;
    }
  }

  schedulePreview() {
    clearTimeout(this.previewTimer);
    this.previewTimer = setTimeout(() => {
      this.previewTimer = null;
      this.run("preview");
    }, PREVIEW_DELAY_MS);
  }

  /// Resolves when no preview is waiting to start and no run is going:
  /// an agent's capture after its edit should show the edited model. Gives
  /// up after a minute (a run that long has its own cancel).
  async whenIdle() {
    const until = performance.now() + 60000;
    while ((this.previewTimer || this.running) && performance.now() < until) {
      // Always through a timer: a preview that is only scheduled has no
      // promise yet, and racing an already-settled one would spin here
      // without ever letting its timer fire.
      const tick = new Promise((r) => setTimeout(r, 50));
      await (this.running ? Promise.race([this.running.catch(() => {}), tick]) : tick);
    }
  }

  customizerChanged() {
    if (!this.doc) return;
    this.store.setParameterValues(this.doc.example.id, this.doc.customizer.values);
    this.schedulePreview();
  }

  setParts(on) {
    if (!this.doc) return;
    this.doc.parts = on;
    this.store.set(`example.${this.doc.example.id}.parts`, on === this.doc.example.parts ? null : on);
    this.check.setParts(on);
    this.measure.setParts(on);
    this.schedulePreview();
  }

  // --- Library tabs: go-to-definition into another file ----------------------

  renderTabs() {
    // The document's tab is always there (it names the file, as the
    // macOS window's title does); library files add read-only tabs.
    const bar = $("#editor-tabs");
    clear(
      bar,
      h(
        "button",
        { role: "tab", "aria-selected": String(this.activeTab === null), onclick: () => this.showTab(null) },
        this.doc?.example.file ?? "",
      ),
      this.tabs.map((t, i) =>
        h(
          "span",
          { class: "tab-wrap" },
          h(
            "button",
            { role: "tab", "aria-selected": String(this.activeTab === i), title: `${t.path} (read-only)`, onclick: () => this.showTab(i) },
            t.path.split("/").pop(),
            h("span", { class: "muted" }, " (read-only)"),
          ),
          h("button", { class: "icon-button", "aria-label": `Close ${t.path}`, onclick: () => this.closeTab(i) }, "×"),
        ),
      ),
    );
  }

  async openLibrary(path, at) {
    if (this.doc && path === this.doc.path) {
      this.showTab(null);
      this.editor.reveal(at.line, at.character);
      return;
    }
    let i = this.tabs.findIndex((t) => t.path === path);
    if (i < 0) {
      try {
        const r = await this.engine.request(Requests.readFile(path));
        this.tabs.push({ path, text: r.text });
        i = this.tabs.length - 1;
      } catch (e) {
        this.console.setSummary(`Could not open ${path}: ${e.message}`, "failed");
        return;
      }
    }
    this.showTab(i);
    if (at) this.editor.reveal(at.line, at.character);
  }

  /// Switch the one editor between the document and a library file. The
  /// document is reloaded from the page's copy when it comes back, which
  /// starts its undo history over (the editor has one state at a time).
  showTab(i) {
    if (i === this.activeTab) return;
    this.activeTab = i;
    if (i === null) this.editor.load(this.doc.text, fileURI(this.doc.path), false);
    else this.editor.load(this.tabs[i].text, fileURI(this.tabs[i].path), true);
    this.renderTabs();
  }

  closeTab(i) {
    this.tabs.splice(i, 1);
    if (this.activeTab === i) {
      this.activeTab = -1;
      this.showTab(null);
    } else {
      if (this.activeTab !== null && this.activeTab > i) this.activeTab -= 1;
      this.renderTabs();
    }
  }

  jump(at) {
    if (this.doc && at.path === this.doc.path) {
      this.showTab(null);
      this.showMobile("editor");
      this.editor.revealRange(at.startLine, at.startCharacter, at.endLine, at.endCharacter);
    } else {
      this.openLibrary(at.path, { line: at.startLine, character: at.startCharacter });
    }
  }

  // --- Runs -----------------------------------------------------------------

  /// Load the lazy libraries the text uses; true when the run can go on.
  async libraries() {
    try {
      await ensureLibraries(this.engine, this.doc.text, {
        onProgress: (name) => this.console.setSummary(`Loading ${name}…`, "running"),
      });
      return true;
    } catch (e) {
      this.console.setSummary(`Could not load a library: ${e.message}`, "failed");
      return false;
    }
  }

  runOptions() {
    return runOptions(this.doc.customizer.values, this.doc.parts);
  }

  async run(mode) {
    const run = this.runOnce(mode);
    this.running = run;
    try {
      await run;
    } finally {
      if (this.running === run) this.running = null;
    }
  }

  async runOnce(mode) {
    if (!this.doc) return;
    clearTimeout(this.previewTimer);
    this.previewTimer = null;
    const d = this.doc;
    this.lastMode = mode;
    this.editor.lspSync();
    this.console.setSummary(mode === "preview" ? "Previewing…" : "Rendering…", "running");
    if (!(await this.libraries())) return;
    let raw;
    try {
      raw = await this.engine.run(
        Requests.run({
          path: d.path,
          mode,
          values: d.customizer.values,
          parts: d.parts,
          // The view the model is shown in, for `$vpt` and friends, and the
          // scheme its face colours are baked in (the viewer cannot
          // recolour a packed scene).
          camera: this.viewer.camera(),
          colorScheme: this.settings.view.scheme,
        }),
      );
    } catch (e) {
      this.fail(e);
      return;
    }
    if (raw?.superseded || this.doc !== d) return;
    const r = runResult(raw);
    this.lastRun = { mode, timings: r.timings, exitCode: r.exitCode };
    this.console.summaryTitle = r.timings
      ? `Parse ${r.timings.parseMs?.toFixed(1)} ms, evaluate ${r.timings.evaluateMs?.toFixed(1)} ms, geometry ${r.timings.geometryMs?.toFixed(1)} ms`
      : "";
    this.console.setLines(r.console);
    this.console.setSummary(describe(r, mode), r.exitCode === 0 ? "done" : "failed");
    for (const m of r.language) this.editor.lspReceive(m);
    // A failed run with nothing to draw keeps the last model on screen, as
    // the app does while the text is mid-edit; a successful empty one
    // clears it.
    if (r.scene || r.exitCode === 0) {
      try {
        this.viewer.setScene(r.scene);
      } catch (e) {
        this.console.setSummary(`The view could not show the model: ${e.message ?? e}`, "failed");
      }
    }
    // The view follows the file's `$vp*` only when they change, so a live
    // preview does not undo the user's orbit.
    if (r.fileView && fileViewChanged(r.fileView, this.fileView)) this.viewer.setFileView(r.fileView);
    this.fileView = r.fileView;
    this.refreshParameters();
  }

  async refreshParameters() {
    const d = this.doc;
    try {
      const res = await this.engine.request(Requests.parameters(d.path));
      if (this.doc === d) this.setParameters(parameterGroups(res.groups ?? res));
    } catch {
      // The console shows the run's errors; an empty customizer is enough here.
    }
  }

  setParameters(groups) {
    this.doc.customizer.setGroups(groups);
    this.store.setParameterValues(this.doc.example.id, this.doc.customizer.values);
    this.customizer.render();
  }

  fail(e) {
    if (e instanceof EngineRestarted) return;
    this.console.setSummary(`Failed: ${e.message}`, "failed");
  }

  async export(format) {
    if (!this.doc) return;
    const d = this.doc;
    const f = EXPORT_FORMATS[format];
    this.console.setSummary(`Exporting ${f.label}…`, "running");
    if (!(await this.libraries())) return;
    try {
      const r = await this.engine.request(Requests.export(d.path, format, this.runOptions()));
      if (r.exitCode !== 0 || !r.data) {
        const why = (r.console ?? "").trim().split("\n").pop() || `exit code ${r.exitCode}`;
        this.console.setSummary(`Export failed: ${why}`, "failed");
        return;
      }
      const name = `${d.example.file.replace(/\.scad$/, "")}.${f.ext}`;
      download(r.data, name, r.mime);
      this.console.setSummary(`Exported ${name} (${r.bytes} bytes).`, "done");
    } catch (e) {
      this.fail(e);
    }
  }

  async runCheck() {
    const d = this.doc;
    if (!d) return;
    this.check.setRunning();
    if (!(await this.libraries())) return this.check.setError("A library did not load.");
    try {
      await this.engine.ready;
      // The wire's CheckOptions has no per-field defaults: the panel's
      // settings go over the worker's own.
      const options = { ...this.engine.defaults?.checkOptions };
      for (const [k, v] of Object.entries(checkOptions(this.check.settings))) if (v !== undefined) options[k] = v;
      const r = await this.engine.request(Requests.check(d.path, this.runOptions(), options));
      if (this.doc === d) this.check.setReport(checkReport(r));
    } catch (e) {
      this.check.setError(e instanceof EngineRestarted ? "The engine restarted during the check." : e.message);
    }
  }

  async runMeasure() {
    const d = this.doc;
    if (!d) return;
    this.measure.setRunning();
    this.viewer.setAnnotations(null);
    if (!(await this.libraries())) return this.measure.setError("A library did not load.");
    try {
      const r = await this.engine.request(Requests.measure(d.path, this.runOptions()));
      if (this.doc !== d) return;
      // The handle section, between and pick name; the worker keeps only
      // the latest, and a respawn forgets it.
      this.measurement = r.measurement ?? null;
      this.measurementWorker = this.engine.restarts;
      this.measure.setResult(measureResult(r));
    } catch (e) {
      this.measure.setError(e instanceof EngineRestarted ? "The engine restarted during the measurement." : e.message);
    }
  }

  async section(axis, offset, target) {
    if (axis === null) {
      this.viewer.setAnnotations(null);
      return;
    }
    try {
      const r = sectionResult(await this.engine.request(Requests.section(this.measurementHandle(), axis, offset, target)));
      this.measure.setSection(r);
      this.viewer.setAnnotations({ lines: r.outline.map((points) => ({ points, closed: true, color: "#d04fc4" })) });
    } catch (e) {
      this.measureFailed(e);
    }
  }

  async between(a, b) {
    try {
      const r = betweenResult(await this.engine.request(Requests.between(this.measurementHandle(), a, b)));
      this.measure.setBetween(r);
      if (r.pointA && r.pointB) {
        this.viewer.setAnnotations({
          lines: [{ points: [r.pointA, r.pointB], color: "#d04fc4" }],
          markers: [
            { point: r.pointA, label: "A", color: "#6a5cf2" },
            { point: r.pointB, label: "B", color: "#6a5cf2" },
          ],
        });
      }
    } catch (e) {
      this.measureFailed(e);
    }
  }

  /// The latest measurement's handle, or an error saying to measure
  /// (again: a respawned worker has none).
  measurementHandle() {
    if (this.measurement == null || this.measurementWorker !== this.engine.restarts) {
      throw new Error("Measure again: the engine restarted since the last measurement.");
    }
    return this.measurement;
  }

  measureFailed(e) {
    if (e instanceof EngineError && e.kind === ErrorKind.invalidArgument && /measurement/.test(e.message)) {
      this.measure.setError("The measurement is out of date: measure again.");
    } else {
      this.measure.setError(e instanceof EngineRestarted ? "The engine restarted: measure again." : e.message);
    }
  }

  async pick(ray) {
    if (!this.measure.picking || !ray) return;
    try {
      const r = await this.engine.request(Requests.pick(this.measurementHandle(), ray.origin, ray.direction));
      if (!r?.point) return;
      this.measure.addPick(r.point);
      const picks = this.measure.picks;
      this.viewer.setAnnotations({
        markers: picks.map((p, i) => ({ point: p, label: i ? "B" : "A", color: "#6a5cf2" })),
        lines: picks.length === 2 ? [{ points: [picks[0], picks[1]], color: "#6a5cf2" }] : [],
      });
    } catch (e) {
      this.measureFailed(e);
    }
  }

  // --- The View menu --------------------------------------------------------

  viewItems() {
    const v = this.settings.view;
    const toggle = (key, label) => ({
      label,
      checked: v[key],
      run: () => this.setView({ [key]: !v[key] }),
    });
    const hidden = document.body.classList.contains("inspector-hidden");
    return [
      toggle("axes", "Show Axes"),
      toggle("scales", "Show Scale Markers"),
      toggle("grid", "Show Grid"),
      toggle("edges", "Show Edges"),
      toggle("crosshairs", "Show Crosshairs"),
      "-",
      { label: "Perspective", checked: !v.orthographic, run: () => this.setView({ orthographic: false }) },
      { label: "Orthographic", checked: v.orthographic, run: () => this.setView({ orthographic: true }) },
      { label: "OpenSCAD Lighting", checked: v.lighting === "openscad", run: () => this.setView({ lighting: "openscad" }) },
      { label: "Headlight", checked: v.lighting === "headlight", run: () => this.setView({ lighting: "headlight" }) },
      "-",
      ...["Top", "Bottom", "Left", "Right", "Front", "Back", "Diagonal"].map((n) => ({
        label: n,
        run: () => this.viewer.preset(n.toLowerCase()),
      })),
      { label: "View All", run: () => this.viewer.viewAll() },
      { label: "Reset View", run: () => this.viewer.resetView() },
      "-",
      { heading: "Colour scheme" },
      ...this.viewer.schemes.map((s) => ({ label: s, checked: v.scheme === s, run: () => this.setView({ scheme: s }) })),
      "-",
      ...INSPECTOR_TABS.map(([id, label]) => ({
        label,
        checked: !hidden && this.settings.inspector === id,
        run: () => this.toggleInspector(id),
      })),
    ];
  }

  setView(change) {
    const scheme = this.settings.view.scheme;
    this.settings.view = { ...this.settings.view, ...change };
    this.viewer.setSettings(this.settings.view);
    this.saveSettings({ view: this.settings.view });
    // The worker bakes the scheme's face colours into the scene, so a new
    // scheme is a new run (in the mode last run; the viewer has already
    // changed the background and lines).
    if (this.settings.view.scheme !== scheme && this.doc && this.lastMode) this.run(this.lastMode);
  }
}

const app = new App();
window.NeoSCADWeb = app;
app.start().catch((e) => {
  console.error(e);
  const b = document.getElementById("banner");
  b.textContent = `The page failed to start: ${e.message}`;
  b.hidden = false;
  b.dataset.kind = "error";
});
