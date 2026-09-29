// The editor's side of the Swift bridge, apart from the DOM so that node
// can test it (test/bridge.test.js). The protocol itself is described in
// apple/App/Editor/EditorController.swift, the other end.

/// The other end of `post()`: the macOS app's WebKit message handler,
/// unless the page that embeds the editor set `window.NeoSCADHost` before
/// the bundle ran (the web demo, web/src/editor-host.js). Both have the
/// same shape, `postMessage(message)` returning a promise of the reply.
/// The injected host wins so that the demo, opened in some other WebKit
/// shell that happens to register an `editor` handler, never talks to it.
/// Null when there is neither (a page opened on its own): posts are
/// dropped.
export function editorHost(win) {
  return win.NeoSCADHost ?? win.webkit?.messageHandlers?.editor ?? null;
}

/// A transaction's changes as the edits Swift applies: `[from, to, insert]`
/// in UTF-16 offsets, each to the text the previous one left. They come
/// from the change set's original coordinates, last change first, so an
/// earlier edit never moves a later one's offsets.
export function changesToEdits(changes) {
  const edits = [];
  changes.iterChanges((fromA, toA, _fromB, _toB, inserted) => {
    edits.push([fromA, toA, inserted.toString()]);
  });
  return edits.reverse();
}

/// How a transaction changed the document, for NSDocument's change count:
/// an undo takes a change back and a redo repeats one.
export function editKind(tr) {
  if (tr.isUserEvent("undo")) return "undo";
  if (tr.isUserEvent("redo")) return "redo";
  return "edit";
}

/// The document's versions, so the app applies each change to the text it
/// was made on: a change carries the version it applies to (`base`) and
/// the one it makes. A load starts a new line of versions.
export class Versions {
  constructor() {
    this.version = 0;
  }

  /// A new document: the version it starts at.
  reset() {
    this.version += 1;
    return this.version;
  }

  /// A change: the version it makes.
  push() {
    this.version += 1;
    return this.version;
  }
}

/// CodeMirror's severity of an LSP diagnostic's (1 error ... 4 hint).
function severityName(n) {
  return n === 1 ? "error" : n === 2 ? "warning" : n === 3 ? "info" : "hint";
}

/// The language server's diagnostics (a `publishDiagnostics` list) as
/// CodeMirror lint diagnostics. `toOffset(position)` turns a position in
/// the text they were computed on into an offset in the current text, or
/// null. Each fix the server sent in `data.fixes` becomes an action;
/// `apply(view, changes)` makes its edits. The edits are kept relative to
/// the diagnostic's start, because CodeMirror hands an action the
/// diagnostic's current range, which moves with typing done since.
export function lintDiagnostics(items, toOffset, apply) {
  const out = [];
  for (const d of items) {
    const from = toOffset(d.range.start);
    const to = toOffset(d.range.end);
    if (from === null || to === null) continue;
    const diagnostic = {
      from,
      to: Math.max(from, to),
      severity: severityName(d.severity ?? 1),
      message: d.message,
    };
    if (d.code !== undefined) diagnostic.source = String(d.code);
    const actions = [];
    for (const fix of d.data?.fixes ?? []) {
      const edits = [];
      for (const e of fix.edits ?? []) {
        const a = toOffset(e.range.start);
        const b = toOffset(e.range.end);
        if (a === null || b === null) break;
        edits.push({ from: a - from, to: b - from, insert: e.newText });
      }
      if (edits.length !== (fix.edits ?? []).length || !edits.length) continue;
      actions.push({
        name: fix.title,
        apply: (view, at) =>
          apply(
            view,
            edits.map((e) => ({ from: at + e.from, to: at + e.to, insert: e.insert })),
          ),
      });
    }
    if (actions.length) diagnostic.actions = actions;
    out.push(diagnostic);
  }
  return out;
}

/// A `Transport` for @codemirror/lsp-client (its `dist/index.d.ts:170`:
/// `send`, `subscribe`, `unsubscribe`) over the Swift bridge: `post` sends
/// a JSON-RPC message to the app, which hands it to the core's language
/// server (`crates/lsp`), and the server's messages come back through
/// `receive`.
export function lspTransport(post) {
  const handlers = new Set();
  return {
    send(message) {
      post({ type: "lsp", message });
    },
    subscribe(handler) {
      handlers.add(handler);
    },
    unsubscribe(handler) {
      handlers.delete(handler);
    },
    receive(message) {
      for (const h of handlers) h(message);
    },
  };
}
