// The language support in CodeMirror terms: highlighting tags, builtin
// names, indentation, folding, bracket matching and comment toggling, on
// editor states (no DOM needed).

import assert from "node:assert/strict";
import { test } from "node:test";
import { toggleComment } from "@codemirror/commands";
import {
  ensureSyntaxTree,
  foldable,
  getIndentation,
  matchBrackets,
} from "@codemirror/language";
import { EditorState } from "@codemirror/state";
import { highlightTree, tagHighlighter, tags as t } from "@lezer/highlight";
import { builtinNames } from "../src/lang/builtins.js";
import { openscad, openscadLanguage } from "../src/lang/openscad.js";

function state(doc) {
  const s = EditorState.create({
    doc,
    extensions: [openscad(), EditorState.lineSeparator.of("\n")],
  });
  ensureSyntaxTree(s, s.doc.length, 5000);
  return s;
}

// The tags the themes colour (src/theme.js), by name. The most specific
// matching entry wins, as in a HighlightStyle.
const byTag = tagHighlighter([
  { tag: t.keyword, class: "keyword" },
  { tag: t.definitionKeyword, class: "definitionKeyword" },
  { tag: t.controlKeyword, class: "controlKeyword" },
  { tag: t.moduleKeyword, class: "moduleKeyword" },
  { tag: t.bool, class: "bool" },
  { tag: t.null, class: "null" },
  { tag: t.number, class: "number" },
  { tag: t.string, class: "string" },
  { tag: t.special(t.string), class: "path" },
  { tag: t.escape, class: "escape" },
  { tag: t.comment, class: "comment" },
  { tag: t.special(t.variableName), class: "special" },
  { tag: t.definition(t.variableName), class: "definition" },
  { tag: t.modifier, class: "modifier" },
  { tag: t.operator, class: "operator" },
]);

/// Each highlighted token as "text:class".
function highlights(src) {
  const out = [];
  highlightTree(openscadLanguage.parser.parse(src), byTag, (from, to, cls) => {
    out.push(`${src.slice(from, to)}:${cls}`);
  });
  return out;
}

test("tokens get OpenSCAD's highlighting categories", () => {
  const h = highlights(
    'module m($fn = 8) { x = "a\\n"; // c\n if (true) #cube(1.5); }\ninclude <a.scad>\ny = undef + 1;',
  );
  for (const expected of [
    "module:definitionKeyword",
    "m:definition",
    "$fn:special",
    "x:definition",
    '"a:string',
    "\\n:string escape",
    '":string',
    "// c:comment",
    "if:controlKeyword",
    "true:bool",
    "#:modifier",
    "1.5:number",
    "include:moduleKeyword",
    "<a.scad>:path",
    "undef:null",
    "+:operator",
  ]) {
    assert.ok(h.includes(expected), `${expected} in ${JSON.stringify(h)}`);
  }
});

test("builtin names are classified as OpenSCAD's editor classifies them", () => {
  const s = state(
    "translate([1,0,0]) cube(max(1, PI)); difference() sphere(); my(); $fn = 1; render() x();",
  );
  const found = builtinNames(s).map((n) => `${s.sliceDoc(n.from, n.to)}:${n.kind}`);
  assert.deepEqual(found, [
    "translate:transformation",
    "cube:model",
    "max:function",
    "PI:value",
    "difference:boolean",
    "sphere:model",
    "render:keyword",
  ]);
});

test("the sketch vocabulary is coloured only inside sketch bodies", () => {
  const s = state(
    "circle(1); arc(2); sketch(name = \"s\") { c = circle(point([0, 0]), r = 1); fix(c.center); if (true) horizontal(l); } x = line;",
  );
  const found = builtinNames(s).map((n) => `${s.sliceDoc(n.from, n.to)}:${n.kind}`);
  assert.deepEqual(found, [
    "circle:model",
    "circle:sketch",
    "point:sketch",
    "fix:sketch",
    "horizontal:sketch",
  ]);
});

test("the geometry queries are coloured as functions and modules", () => {
  const s = state(
    "module m() { b = child_bounds(0); d = child_distance(0, 1); anchor(\"tip\", [0, 0, 1]); children(); }",
  );
  const found = builtinNames(s).map((n) => `${s.sliceDoc(n.from, n.to)}:${n.kind}`);
  assert.deepEqual(found, [
    "child_bounds:function",
    "child_distance:function",
    "anchor:transformation",
    "children:transformation",
  ]);
});

test("new lines indent by the structure", () => {
  // Each line is asked for its indentation given the lines above it as
  // they stand; the expected values are the indentation shown.
  const doc = [
    "module m() {",
    "    cube();", // inside a block: one unit
    "    translate([1,",
    "               2])", // inside an argument list: aligned after "("
    "        sphere();", // the child of a call, on its own line
    "    translate([1, 0, 0])",
    "    {", // a block opening a child: back to the call's column
    "        sphere();",
    "    }", // a closing brace: back to its block's line
    "}",
  ].join("\n");
  const s = state(doc);
  for (let n = 2; n <= s.doc.lines; n++) {
    const line = s.doc.line(n);
    const shown = /^ */.exec(line.text)[0].length;
    assert.equal(getIndentation(s, line.from), shown, `line ${n}: ${line.text}`);
  }
});

test("blocks, lists and comments fold", () => {
  const s = state("module m() {\n  cube();\n}\n/* a\n b */\nv = [\n 1,\n 2];");
  const at = (n) => {
    const line = s.doc.line(n);
    return foldable(s, line.from, line.to);
  };
  assert.deepEqual(at(1), { from: 12, to: 23 });
  assert.deepEqual(at(4), { from: 27, to: 33 });
  assert.ok(at(6), "a vector over several lines folds");
});

test("brackets match", () => {
  const src = "cube([1, (2)]);";
  const s = state(src);
  const m = matchBrackets(s, src.indexOf("["), 1);
  assert.ok(m?.matched);
  assert.equal(m.end.from, src.indexOf("]"));
  // The "[" is never closed.
  const bad = state("cube([1, 2);");
  assert.equal(matchBrackets(bad, 5, 1)?.matched, false);
});

test("comment toggling uses // lines", () => {
  let s = state("cube();\nsphere();");
  s = s.update({ selection: { anchor: 0, head: s.doc.length } }).state;
  const run = (st) => {
    let out = st;
    toggleComment({ state: st, dispatch: (tr) => (out = tr.state) });
    return out;
  };
  const on = run(s);
  assert.equal(on.doc.toString(), "// cube();\n// sphere();");
  const off = run(on.update({ selection: { anchor: 0, head: on.doc.length } }).state);
  assert.equal(off.doc.toString(), "cube();\nsphere();");
});
