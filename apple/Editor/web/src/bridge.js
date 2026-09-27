// The editor's side of the Swift bridge, apart from the DOM so that node
// can test it (test/bridge.test.js). The protocol itself is described in
// apple/App/Editor/EditorController.swift, the other end.

import { MapMode } from "@codemirror/state";

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

/// The document's versions and the changes between them, so positions the
/// core computed for an older version (diagnostics of a render that
/// finished after more typing) map to the current text. A load starts a
/// new line of versions: nothing maps across it.
export class VersionHistory {
  constructor(limit = 1000) {
    this.limit = limit;
    this.version = 0;
    this.base = 0;
    // entries[i] takes version base + i to base + i + 1.
    this.entries = [];
  }

  /// A new document: no version before this one maps to it.
  reset() {
    this.version += 1;
    this.base = this.version;
    this.entries = [];
    return this.version;
  }

  /// A change: the version it makes.
  push(changes) {
    this.entries.push(changes);
    this.version += 1;
    if (this.entries.length > this.limit) {
      this.entries.shift();
      this.base += 1;
    }
    return this.version;
  }

  /// `pos` at `version` in the current document, or null if that version
  /// is unknown (too old, or before a load) or the text at `pos` was
  /// deleted since. `assoc` is the side the position sticks to.
  map(pos, version, assoc = -1) {
    if (version < this.base || version > this.version) return null;
    for (let i = version - this.base; i < this.entries.length; i++) {
      pos = this.entries[i].mapPos(pos, assoc, MapMode.TrackDel);
      if (pos === null) return null;
    }
    return pos;
  }

  /// A range at `version` in the current document, or null. A range that
  /// shrank to nothing keeps its start, so a marker stays visible.
  mapRange(from, to, version) {
    const a = this.map(from, version, 1);
    const b = this.map(to, version, -1);
    if (a !== null && b !== null) return { from: a, to: Math.max(a, b) };
    // TrackDel dropped an end whose neighbourhood changed; fall back to
    // the positions without it, which never fail.
    if (version < this.base || version > this.version) return null;
    let x = from;
    let y = to;
    for (let i = version - this.base; i < this.entries.length; i++) {
      x = this.entries[i].mapPos(x, 1);
      y = this.entries[i].mapPos(y, -1);
    }
    return { from: x, to: Math.max(x, y) };
  }
}

/// Diagnostics from Swift (UTF-16 ranges at `version`) as CodeMirror lint
/// diagnostics in the current document. `apply(view, from, to, insert)`
/// makes a fix's edit.
export function mapDiagnostics(history, version, list, apply) {
  const out = [];
  for (const d of list) {
    const range = history.mapRange(d.from, d.to, version);
    if (!range) continue;
    const diagnostic = {
      from: range.from,
      to: range.to,
      severity: d.severity,
      message: d.message,
    };
    if (d.source) diagnostic.source = d.source;
    const actions = [];
    for (const a of d.actions ?? []) {
      const r = history.mapRange(a.from, a.to, version);
      if (!r) continue;
      actions.push({
        name: a.name,
        apply: (view) => apply(view, r.from, r.to, a.insert),
      });
    }
    if (actions.length) diagnostic.actions = actions;
    out.push(diagnostic);
  }
  return out;
}

/// A `Transport` for @codemirror/lsp-client (its `dist/index.d.ts:170`:
/// `send`, `subscribe`, `unsubscribe`) over the Swift bridge: `post` sends
/// a JSON-RPC message to the app, and the app's messages come back through
/// `receive`. The language server itself is 8e's; this is the channel.
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
