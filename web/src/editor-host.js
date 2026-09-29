// The editor's host on the web: the part the macOS app plays for the
// CodeMirror bundle (apple/Editor/web/src/editor.js). The bundle posts
// every message to `window.NeoSCADHost.postMessage` when it is set
// (bridge.js's `editorHost`), so this module sets it before the editor
// module is evaluated, which is why the editor is imported dynamically:
// a static import would run first.
//
// Messages that arrive before the page installs its handler (the
// language client's `initialize`, the editor's "ready") are queued, not
// dropped: losing `initialize` would leave the language client waiting
// for ever.

let handler = null;
const queue = [];

window.NeoSCADHost = {
  postMessage(message) {
    if (!handler) {
      queue.push(message);
      return Promise.resolve(null);
    }
    try {
      return Promise.resolve(handler(message));
    } catch (e) {
      return Promise.reject(e);
    }
  },
};

/// Install the handler; queued messages are delivered to it first.
export function setEditorHandler(fn) {
  handler = fn;
  for (const m of queue.splice(0)) fn(m);
}

/// Evaluate the editor bundle into `#editor` and return its API
/// (`window.NeoSCADEditor`: load, revealRange, lspReceive, ...).
export async function loadEditor() {
  await import("../../apple/Editor/web/src/editor.js");
  return window.NeoSCADEditor;
}
