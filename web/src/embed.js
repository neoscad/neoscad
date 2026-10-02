// The embed view (`#embed=1&code=...` or `#embed=1&example=...`,
// share.js): the 3D view alone, for an iframe in a neoscad.org page such
// as a blog post. It previews the model once it loads and can be orbited,
// and a small link opens the same model in the whole page, in a new tab.
//
// It is the page's bundle with most of the page left out, so a post with
// several embeds stays light: no editor (and so no language server, which
// only the editor's language client starts), no agent bridge, no
// inspector, and nothing read from or written to the visitor's storage
// (store.js): their saved examples, edits and view settings are the whole
// page's, and an embed always starts from the shipped defaults.

import { ensureLibraries, createEngine } from "./engine/index.js";
import { EngineRestarted } from "./engine/client.js";
import { Requests, docPath, fileViewChanged, runResult } from "./engine/protocol.js";
import { loadExampleText, loadManifest } from "./examples.js";
import { DEFAULT_NAME, ShareError, decodeSource, exampleHash, fileName } from "./share.js";
import { applyTheme, loadSite } from "./site.js";
import { clear, h } from "./ui/dom.js";
import { DEFAULT_VIEW, createViewer } from "./view/index.js";

const $ = (sel) => document.querySelector(sel);

export class EmbedApp {
  async start(share) {
    const root = document.documentElement;
    root.classList.add("embed");
    // The site's tokens, for the overlay's colours; not its nav.
    loadSite().then(applyTheme);

    this.status = h("div", { class: "embed-status", role: "status", "data-testid": "embed-status" }, "Loading…");
    this.open = h(
      "a",
      { class: "embed-open", target: "_blank", rel: "noopener", "data-testid": "embed-open", hidden: true },
      "Open in NeoSCAD",
      h("span", { "aria-hidden": "true" }, " ↗"),
    );
    this.previewButton = h("button", { class: "primary", hidden: true, "data-testid": "embed-preview", onclick: () => this.run() }, "Preview");
    $("#pane-view").append(h("div", { class: "embed-bar" }, this.status, this.previewButton, this.open));

    this.build = await fetch(new URL("./build.json", document.baseURI))
      .then((r) => (r.ok ? r.json() : {}))
      .catch(() => ({}));
    this.build = { engine: "mock", view: "none", ...this.build };

    // The model first: a bad link fails here without starting a worker.
    const doc = await this.source(share);
    // The whole page's link to the same model: the payload as it came
    // (no need to encode it again), or the example.
    const page = new URL("./", location.href).href;
    this.open.href = share.code !== null ? `${page}#code=${share.code}${doc.file === DEFAULT_NAME ? "" : `&name=${encodeURIComponent(doc.file)}`}` : `${page}${exampleHash(doc.id)}`;
    this.open.hidden = false;
    this.open.title = `Open ${doc.file} in NeoSCAD, with the editor (a new tab)`;
    document.title = `${doc.file} – NeoSCAD`;

    this.engine = createEngine(this.build);
    this.engine.on("status", ({ state, message }) => {
      if (state === "failed") this.say(`The engine stopped: ${message}.`, "failed");
    });
    const started = this.engine.start();
    const { viewer, canvas, notice } = await createViewer($("#viewport"), { view: this.build.view, scheme: DEFAULT_VIEW.scheme });
    this.viewer = viewer;
    viewer.setSettings({ ...DEFAULT_VIEW });
    root.dataset.view = viewer.kind;
    if (notice) {
      const n = $("#view-notice");
      n.textContent = notice;
      n.hidden = false;
    }
    new ResizeObserver(() => this.viewer.resize()).observe(canvas);
    await started;

    this.doc = { ...doc, path: docPath(doc.file) };
    await this.engine.open(this.doc.path, doc.text);
    root.dataset.ready = "true";
    if (doc.heavy) {
      this.say(doc.note || "This model is heavy: press Preview to run it.", "idle");
      this.previewButton.hidden = false;
    } else {
      await this.run();
    }
  }

  /// `{id, file, text, heavy, note}` from the link: its code, or a bundled
  /// example (always as shipped: the visitor's edits are the whole page's).
  async source(share) {
    if (share.code !== null) {
      try {
        return { id: null, file: share.name, text: await decodeSource(share.code), heavy: false, note: "" };
      } catch (e) {
        throw new ShareError(`The link's model could not be opened: ${e.message}.`);
      }
    }
    const manifest = await loadManifest();
    const id = share.example ?? manifest.default;
    const example = manifest.examples.find((e) => e.id === id);
    if (!example) throw new ShareError(`There is no example "${id}".`);
    return { id, file: fileName(example.file), text: await loadExampleText(example), heavy: example.heavy, note: example.note };
  }

  async run() {
    const d = this.doc;
    this.previewButton.hidden = true;
    this.say("Previewing…", "running");
    try {
      await ensureLibraries(this.engine, d.text, { onProgress: (name) => this.say(`Loading ${name}…`, "running") });
      const raw = await this.engine.run(
        Requests.run({ path: d.path, mode: "preview", camera: this.viewer.camera(), colorScheme: DEFAULT_VIEW.scheme }),
      );
      if (raw?.superseded) return;
      const r = runResult(raw);
      if (r.scene || r.exitCode === 0) this.viewer.setScene(r.scene);
      if (r.fileView && fileViewChanged(r.fileView, null)) this.viewer.setFileView(r.fileView);
      if (r.exitCode === 0) this.say(null);
      else {
        // The summary and the first error: there is no console to read.
        const first = r.console.find((l) => l.kind === "error");
        this.say(first ? `${r.summary} ${first.text}` : r.summary, "failed");
      }
      document.documentElement.dataset.ran = r.exitCode === 0 ? "ok" : "failed";
    } catch (e) {
      if (e instanceof EngineRestarted) return;
      this.say(`Failed: ${e.message}`, "failed");
      document.documentElement.dataset.ran = "failed";
    }
  }

  /// The overlay's line: null hides it.
  say(text, kind = "idle") {
    this.status.hidden = text == null;
    this.status.dataset.kind = kind;
    if (text != null) clear(this.status, kind === "running" ? h("span", { class: "spinner", "aria-hidden": "true" }) : null, text);
  }

  failed(e) {
    if (!(e instanceof ShareError)) console.error(e);
    this.say(e.message, "failed");
    document.documentElement.dataset.ready = "true";
    document.documentElement.dataset.ran = "failed";
  }
}
