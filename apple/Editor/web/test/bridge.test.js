// The bridge's logic: change sets become sequential edits the app can apply
// in order, the language server's diagnostics become lint markers with
// their fixes, and the LSP transport routes both ways.

import assert from "node:assert/strict";
import { test } from "node:test";
import { ChangeSet, EditorState, Text } from "@codemirror/state";
import { history, undo } from "@codemirror/commands";
import {
  Versions,
  changesToEdits,
  editKind,
  lintDiagnostics,
  lspTransport,
} from "../src/bridge.js";

/// What the app does with the edits: each applied to the previous result.
function applyEdits(text, edits) {
  for (const [from, to, insert] of edits) {
    text = text.slice(0, from) + insert + text.slice(to);
  }
  return text;
}

test("a change set becomes edits that apply in order", () => {
  const doc = "cube(1);\nsphere(2);\n";
  // Three changes in one transaction (multiple cursors), with an emoji and
  // CJK text whose UTF-16 lengths differ from their character counts.
  const changes = ChangeSet.of(
    [
      { from: 0, to: 4, insert: "cylinder" },
      { from: 9, insert: "😀 " },
      { from: 17, to: 18, insert: "漢字" },
    ],
    doc.length,
  );
  const expected = changes.apply(Text.of(doc.split("\n"))).toString();
  const edits = changesToEdits(changes);
  assert.deepEqual(
    edits.map((e) => e[0]),
    [17, 9, 0],
    "last change first",
  );
  assert.equal(applyEdits(doc, edits), expected);
});

test("undo and redo are reported as such", () => {
  let s = EditorState.create({ doc: "a", extensions: [history()] });
  const typed = s.update({ changes: { from: 1, insert: "b" }, userEvent: "input.type" });
  assert.equal(editKind(typed), "edit");
  s = typed.state;
  let undone;
  undo({ state: s, dispatch: (tr) => (undone = tr) });
  assert.equal(editKind(undone), "undo");
});

test("versions count changes and restart at a load", () => {
  const v = new Versions();
  const a = v.reset();
  assert.equal(v.push(), a + 1);
  assert.equal(v.push(), a + 2);
  assert.equal(v.reset(), a + 3);
});

test("server diagnostics become lint markers with their fixes", () => {
  // Positions are {line, character}; this stand-in maps them into a text
  // where two characters were typed at the start since the server saw it.
  const lines = ["cub(1);", "x = zz;"];
  const toOffset = (p) => {
    if (p.line >= lines.length) return null;
    let at = 0;
    for (let i = 0; i < p.line; i++) at += lines[i].length + 1;
    return at + p.character + 2;
  };
  const applied = [];
  const out = lintDiagnostics(
    [
      {
        range: { start: { line: 0, character: 0 }, end: { line: 0, character: 7 } },
        severity: 2,
        code: "unknown-module",
        message: "Ignoring unknown module 'cub'\ndid you mean 'cube'?",
        data: {
          fixes: [
            {
              title: "Change 'cub' to 'cube'",
              edits: [{ range: { start: { line: 0, character: 0 }, end: { line: 0, character: 3 } }, newText: "cube" }],
            },
          ],
        },
      },
      { range: { start: { line: 1, character: 4 }, end: { line: 1, character: 6 } }, severity: 1, message: "e" },
      // A position that no longer exists is dropped.
      { range: { start: { line: 9, character: 0 }, end: { line: 9, character: 1 } }, message: "gone" },
    ],
    toOffset,
    (_view, changes) => applied.push(changes),
  );
  assert.equal(out.length, 2);
  assert.deepEqual([out[0].from, out[0].to, out[0].severity, out[0].source], [2, 9, "warning", "unknown-module"]);
  assert.deepEqual([out[1].from, out[1].to, out[1].severity], [14, 16, "error"]);
  assert.equal(out[0].actions[0].name, "Change 'cub' to 'cube'");
  // The action gets the marker's current start (it moved by 3 more).
  out[0].actions[0].apply(null, 5, 12);
  assert.deepEqual(applied, [[{ from: 5, to: 8, insert: "cube" }]]);
  assert.equal(out[1].actions, undefined);
});

test("the LSP transport routes messages both ways", () => {
  const sent = [];
  const t = lspTransport((m) => sent.push(m));
  const got = [];
  const handler = (m) => got.push(m);
  t.subscribe(handler);
  t.send('{"jsonrpc":"2.0","id":1,"method":"initialize"}');
  t.receive('{"jsonrpc":"2.0","id":1,"result":{}}');
  t.unsubscribe(handler);
  t.receive("ignored");
  assert.deepEqual(sent, [{ type: "lsp", message: '{"jsonrpc":"2.0","id":1,"method":"initialize"}' }]);
  assert.deepEqual(got, ['{"jsonrpc":"2.0","id":1,"result":{}}']);
});
