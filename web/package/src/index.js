// @neoscad/viewer: NeoSCAD's wasm engine and viewer for any web page
// (docs/js-package.md). ESM, browser only.
//
//   import { render } from "@neoscad/viewer";
//   const view = await render(document.querySelector("#model"), "cube(10);");
//
// The custom element is a separate side-effect import: "@neoscad/viewer/element".

export { createEngine, sharedEngine, Engine, Model, EngineStartError, VERSION, checkRelativePath } from "./engine.js";
export { createViewer, Viewer, SceneHandle } from "./viewer.js";
export { render, View } from "./render.js";
export { ExportError } from "./result.js";
export { AbortError } from "./scheduler.js";
export { DEFAULT_VIEW } from "../../src/view/index.js";
export { fileViewChanged } from "../../src/engine/protocol.js";
