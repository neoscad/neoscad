// The bridge's logic: change sets become sequential edits the app can apply
// in order, versions map late diagnostics to the current text, and the LSP
// transport routes both ways.

import assert from "node:assert/strict";
import { test } from "node:test";
import { ChangeSet, EditorState, Text } from "@codemirror/state";
import { history, undo } from "@codemirror/commands";
import {
  VersionHistory,
  changesToEdits,
  editKind,
  lspTransport,
  mapDiagnostics,
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

test("positions of an older version map to the current text", () => {
  const h = new VersionHistory();
  const v0 = h.reset();
  // "abc def" -> insert "XX" at 0 -> delete "def"
  const c1 = ChangeSet.of([{ from: 0, insert: "XX" }], 7);
  const v1 = h.push(c1);
  const c2 = ChangeSet.of([{ from: 6, to: 9 }], 9);
  h.push(c2);
  // "abc" at v0 is 0..3; now 2..5.
  assert.deepEqual(h.mapRange(0, 3, v0), { from: 2, to: 5 });
  // "def" at v1 (6..9) was deleted: the range collapses to where it was.
  assert.deepEqual(h.mapRange(6, 9, v1), { from: 6, to: 6 });
  // Nothing maps across a load, or from the future.
  const v3 = h.reset();
  assert.equal(h.mapRange(0, 1, v0), null);
  assert.equal(h.map(0, v3 + 1), null);
});

test("the history forgets its oldest versions past its limit", () => {
  const h = new VersionHistory(2);
  const v0 = h.reset();
  for (let i = 0; i < 3; i++) h.push(ChangeSet.of([{ from: 0, insert: "x" }], i));
  assert.equal(h.map(0, v0), null);
  // Each push inserted before position 0; a position that sticks right
  // moves past both insertions still known.
  assert.equal(h.map(0, v0 + 1, 1), 2);
});

test("diagnostics map with their fixes", () => {
  const h = new VersionHistory();
  const v = h.reset();
  h.push(ChangeSet.of([{ from: 0, insert: "// x\n" }], 10));
  const applied = [];
  const out = mapDiagnostics(
    h,
    v,
    [
      {
        from: 0,
        to: 4,
        severity: "warning",
        message: "unknown module",
        source: "unknown-module",
        actions: [{ name: "cube", from: 0, to: 4, insert: "cube" }],
      },
    ],
    (_view, from, to, insert) => applied.push([from, to, insert]),
  );
  assert.equal(out.length, 1);
  assert.deepEqual([out[0].from, out[0].to], [5, 9]);
  assert.equal(out[0].source, "unknown-module");
  out[0].actions[0].apply(null);
  assert.deepEqual(applied, [[5, 9, "cube"]]);
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
