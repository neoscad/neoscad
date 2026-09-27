// Language features: @codemirror/lsp-client talking to the core's language
// server (crates/lsp) through the app. The client owns the protocol:
// it syncs the document (didOpen, then didChange half a second after
// typing, or before any request), and asks for completion, hover,
// signature help, formatting (Shift-Alt-F), rename (F2) and references
// (Shift-F12). This file adds what the client leaves to its host:
//
// - Diagnostics with fixes. The server's diagnostics (those of the app's
//   runs of the document, which it publishes for the version with the
//   run's text) are the editor's only lint markers, and each fix it sends
//   in a diagnostic's `data` becomes an action on the marker (the client's
//   own handler drops both codes and fixes).
// - Go to definition (F12, Command-click) that can leave the document: a
//   location in another file (an include, a library) goes to the app,
//   which opens it (`{type: "open", uri, line, character}`).
// - HTML from Markdown is cleaned before it is shown: hover text includes
//   comments from library files, which may hold anything.

import {
  LSPClient,
  LSPPlugin,
  findReferencesKeymap,
  formatDocument,
  hoverTooltips,
  renameKeymap,
  serverCompletion,
  serverDiagnostics,
  signatureHelp,
} from "@codemirror/lsp-client";
import { setDiagnostics } from "@codemirror/lint";
import { EditorView, keymap } from "@codemirror/view";
import { lintDiagnostics } from "./bridge.js";

/// Elements Markdown may produce in hover and completion documentation;
/// anything else is replaced by its text, and no attribute survives but a
/// code block's class (the highlighter's).
const ALLOWED = new Set([
  "P", "PRE", "CODE", "EM", "STRONG", "UL", "OL", "LI", "BR", "HR", "SPAN", "DIV",
  "H1", "H2", "H3", "H4", "BLOCKQUOTE", "TABLE", "THEAD", "TBODY", "TR", "TH", "TD",
]);

export function sanitizeHTML(html) {
  const t = document.createElement("template");
  t.innerHTML = html;
  const walk = (node) => {
    for (const child of [...node.childNodes]) {
      if (child.nodeType === Node.ELEMENT_NODE) {
        if (!ALLOWED.has(child.tagName)) {
          child.replaceWith(document.createTextNode(child.textContent));
          continue;
        }
        for (const a of [...child.attributes]) {
          if (a.name !== "class") child.removeAttribute(a.name);
        }
        walk(child);
      } else if (child.nodeType !== Node.TEXT_NODE) {
        child.remove();
      }
    }
  };
  walk(t.content);
  return t.innerHTML;
}

function applyChanges(view, changes) {
  view.dispatch({ changes, userEvent: "input.fix" });
}

/// `textDocument/publishDiagnostics`: the markers of the file it names,
/// if that is the file open here and the version it was computed on is
/// the one last sent (an older one is superseded by the next publish).
function publishDiagnostics(client, params) {
  const file = client.workspace.getFile(params.uri);
  if (!file) return true;
  if (params.version != null && params.version !== file.version) return true;
  const view = file.getView();
  const plugin = view && LSPPlugin.get(view);
  if (!plugin) return true;
  const toOffset = (pos) => {
    let at;
    try {
      at = plugin.fromPosition(pos, plugin.syncedDoc);
    } catch {
      return null;
    }
    return plugin.unsyncedChanges.mapPos(at);
  };
  const list = lintDiagnostics(params.diagnostics, toOffset, applyChanges);
  view.dispatch(setDiagnostics(view.state, list));
  return true;
}

/// Jump to what the name at the cursor names. In this document the
/// cursor moves there; elsewhere `open` gets the location.
export function goToDefinition(view, open) {
  const plugin = LSPPlugin.get(view);
  if (!plugin) return false;
  plugin.client.sync();
  const pos = view.state.selection.main.head;
  plugin.client
    .request("textDocument/definition", {
      textDocument: { uri: plugin.uri },
      position: plugin.toPosition(pos),
    })
    .then((result) => {
      const loc = Array.isArray(result) ? result[0] : result;
      if (!loc) return;
      if (loc.uri === plugin.uri) {
        const at = plugin.unsyncedChanges.mapPos(plugin.fromPosition(loc.range.start, plugin.syncedDoc));
        view.dispatch({ selection: { anchor: at }, scrollIntoView: true, userEvent: "select.definition" });
      } else {
        open(loc.uri, loc.range.start.line, loc.range.start.character);
      }
    })
    .catch((e) => plugin.reportError("Go to definition failed", e));
  return true;
}

/// ⌥⇧F formats the document. The client's own `formatKeymap` binds
/// "Shift-Alt-f", which never fires on a Mac: there ⌥⇧F types "Ï", and
/// CodeMirror deliberately does not fall back to the key's base name for
/// Option combinations ("Alt-combinations on macOS tend to be typed
/// characters", @codemirror/view's `runHandlers`), so the key would type
/// "Ï" into the model instead. The key is matched by its key code, as
/// CodeMirror matches base names; the event is taken, so nothing is typed.
const formatKey = EditorView.domEventHandlers({
  keydown(e, view) {
    if (!e.altKey || !e.shiftKey || e.metaKey || e.ctrlKey || e.keyCode !== 70) return false;
    formatDocument(view);
    e.preventDefault();
    return true;
  },
});

/// The client, connected over `transport`. `open(uri, line, character)`
/// shows a location in another file.
export function languageClient(transport, open) {
  const client = new LSPClient({
    // Every request answers in milliseconds (the app's runs produce the
    // diagnostics); the first request on a BOSL2 model indexes the
    // library, which takes longer.
    timeout: 10000,
    sanitizeHTML,
    notificationHandlers: {
      "textDocument/publishDiagnostics": publishDiagnostics,
    },
    extensions: [
      // For its capability (versioned diagnostics) and its sync after a
      // pause in typing; its handler is replaced by the one above.
      serverDiagnostics(),
      serverCompletion(),
      hoverTooltips(),
      signatureHelp(),
      formatKey,
      keymap.of([
        ...renameKeymap,
        ...findReferencesKeymap,
        { key: "F12", run: (view) => goToDefinition(view, open) },
      ]),
      // Command-click goes to the definition, as in Xcode and VS Code;
      // Option-click adds a cursor instead.
      EditorView.clickAddsSelectionRange.of((e) => e.altKey),
      EditorView.domEventHandlers({
        mousedown(e, view) {
          if (!e.metaKey || e.button !== 0) return false;
          const pos = view.posAtCoords({ x: e.clientX, y: e.clientY });
          if (pos === null) return false;
          view.dispatch({ selection: { anchor: pos } });
          goToDefinition(view, open);
          e.preventDefault();
          return true;
        },
      }),
    ],
  });
  client.connect(transport);
  return client;
}
