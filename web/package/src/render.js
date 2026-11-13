// `render(target, source, options)`: an engine model and a viewer in one
// handle, the few lines most pages need.

import { fileViewChanged } from "../../src/engine/protocol.js";
import { sharedEngine } from "./engine.js";
import { createViewer } from "./viewer.js";

const RUN_KEYS = ["mode", "enable", "overrides", "files", "colorScheme"];

/// Show `source` in `target` (a canvas, or an element to draw in).
/// Options: everything createEngine, createViewer and engine.run take,
/// plus `engine` (an engine to use instead of the page's shared one),
/// `name` (the model's file name) and `onResult(result)`. Resolves once
/// the first run has been shown, to `{result, update, viewer, engine,
/// dispose}`; a failing model resolves too, with `result.ok` false.
export async function render(target, source, options = {}) {
  const engine = options.engine ?? sharedEngine(options);
  const viewer = await createViewer(target, options);
  const view = new View(engine, viewer, options);
  await view.update(source);
  return view;
}

export class View {
  constructor(engine, viewer, options) {
    this.engine = engine;
    this.viewer = viewer;
    this.options = {};
    for (const k of RUN_KEYS) if (options[k] !== undefined) this.options[k] = options[k];
    this.options.colorScheme ??= viewer.settings.scheme;
    this.onResult = options.onResult ?? null;
    this.model = engine.model({ name: options.name ?? "main.scad", camera: () => this.viewer?.camera() ?? null });
    this.result = null;
    this.source = null;
    this.fileView = null;
  }

  /// Run `source` (the last one when omitted) with `options` merged over
  /// the handle's. Resolves to the result, or `{superseded: true}` when a
  /// newer update replaced it before it ran.
  async update(source = this.source, options = {}) {
    this.source = source;
    for (const k of RUN_KEYS) if (options[k] !== undefined) this.options[k] = options[k];
    const r = await this.model.run(source, { ...this.options, signal: options.signal });
    if (r.superseded || !this.viewer) return r;
    this.show(r);
    return r;
  }

  show(r) {
    this.result = r;
    // A failed run keeps the last picture unless it has one of its own.
    if (r.scene || r.ok) this.viewer.setScene(r.scene);
    if (r.fileView && fileViewChanged(r.fileView, this.fileView)) this.viewer.setFileView(r.fileView);
    this.fileView = r.fileView;
    this.onResult?.(r);
  }

  export(format, options = {}) {
    return this.model.export(this.source, format, { ...this.options, ...options });
  }

  dispose() {
    this.model.dispose();
    this.viewer?.dispose();
    this.viewer = null;
  }
}
