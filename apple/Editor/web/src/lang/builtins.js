// OpenSCAD's editor colours builtin names by kind: primitives ("models"),
// transformations, the boolean operations, builtin functions, and a few
// more keywords. The lists are its lexertl lexer's
// (.reference/openscad/src/gui/ScadLexer.cc:115-153), so a colour scheme
// ported from OpenSCAD colours the same words.
//
// The grammar cannot tell `cube` from any other module name (a name is a
// name), so these are mark decorations over the visible syntax tree rather
// than highlighting tags. The highlight style leaves plain names
// unstyled, so the decoration's colour is the one that shows.
//
// NeoSCAD's constrained sketches (`--enable sketch`, docs/sketch.md) bind
// their vocabulary only inside the body of a `sketch()` call, so those
// names are coloured as "sketch" there and nowhere else: a coloured `arc`
// is a sketch arc, never BOSL2's arc() (docs/language-extensions.md,
// section 7). The geometry queries (`--enable query`,
// docs/geometry-queries.md) are coloured everywhere, with the functions
// (`child_bounds` and the like) and the modules (`anchor`), since a
// program rarely defines those names; like the sketch vocabulary they are
// coloured whether the extension is on or not, as the decorations do not
// know the setting. The fillets (`--enable fillet`, docs/fillet-edges.md),
// `fillet_edges` and `chamfer_edges`, are operations on their children,
// coloured as transformations like `offset` and `hull`, on the same terms;
// their selector strings stay strings (the language server completes and
// explains them).

import { syntaxTree } from "@codemirror/language";
import { RangeSetBuilder } from "@codemirror/state";
import { Decoration, ViewPlugin } from "@codemirror/view";

const lists = {
  // Keywords of OpenSCAD's editor that are plain names to the grammar
  // (the language's own keywords are tokens already).
  keyword: "import projection render return",
  transformation:
    "translate rotate scale linear_extrude rotate_extrude resize mirror " +
    "multmatrix color offset hull minkowski children anchor " +
    "fillet_edges chamfer_edges",
  boolean: "union difference intersection intersection_for",
  function:
    "abs sign rands min max sin cos asin acos tan atan atan2 round ceil " +
    "floor pow sqrt exp len log ln str chr ord concat lookup search " +
    "version version_num norm cross parent_module dxf_dim dxf_cross " +
    "is_undef is_list is_num is_bool is_string is_function is_object " +
    "child_anchors child_bounds child_measure child_distance",
  model: "sphere cube cylinder polyhedron square polygon text circle surface roof",
  value: "PI",
};

/// The sketch vocabulary: its entities and constraint statements.
export const sketchVocabulary = new Set(
  (
    "point line arc circle coincident on horizontal vertical parallel " +
    "perpendicular tangent distance length radius diameter angle equal " +
    "midpoint symmetric fix fillet chamfer"
  ).split(" "),
);

/// Whether `node` is inside the child of a `sketch(...)` call (its body,
/// not its arguments).
function inSketchBody(state, node) {
  for (let n = node.parent; n; n = n.parent) {
    if (n.name !== "ModuleCall") continue;
    const name = n.firstChild;
    const args = name?.nextSibling;
    if (
      name?.name === "ModuleName" &&
      state.doc.sliceString(name.from, name.to) === "sketch" &&
      args &&
      node.from >= args.to
    ) {
      return true;
    }
  }
  return false;
}

/// A builtin's kind by name: "keyword", "transformation", "boolean",
/// "function", "model" or "value".
export const builtinKind = new Map();
for (const [kind, words] of Object.entries(lists)) {
  for (const w of words.split(" ")) builtinKind.set(w, kind);
}

const marks = {};
for (const kind of [...Object.keys(lists), "sketch"]) {
  marks[kind] = Decoration.mark({ class: `cm-scad-${kind}` });
}

/// Each builtin name in `from..to` of `state`, in order: `{from, to, kind}`
/// (`kind` "sketch" for the sketch vocabulary inside a sketch body).
export function builtinNames(state, from = 0, to = state.doc.length) {
  const out = [];
  syntaxTree(state).iterate({
    from,
    to,
    enter(node) {
      const name = node.name;
      if (name !== "VariableName" && name !== "ModuleName") return;
      // A `$` name is a special variable, never a builtin.
      if (node.node.firstChild?.name === "SpecialVariable") return false;
      const word = state.doc.sliceString(node.from, node.to);
      const kind =
        sketchVocabulary.has(word) && inSketchBody(state, node.node)
          ? "sketch"
          : builtinKind.get(word);
      if (kind) out.push({ from: node.from, to: node.to, kind });
      return false;
    },
  });
  return out;
}

function decorations(view) {
  const builder = new RangeSetBuilder();
  for (const { from, to } of view.visibleRanges) {
    for (const n of builtinNames(view.state, from, to)) {
      builder.add(n.from, n.to, marks[n.kind]);
    }
  }
  return builder.finish();
}

/// Colours builtin names in the visible part of the document.
export const builtinHighlighter = ViewPlugin.fromClass(
  class {
    constructor(view) {
      this.decorations = decorations(view);
    }
    update(update) {
      // The parser works in the background, so the tree can change with
      // no change to the document.
      if (
        update.docChanged ||
        update.viewportChanged ||
        syntaxTree(update.startState) !== syntaxTree(update.state)
      ) {
        this.decorations = decorations(update.view);
      }
    }
  },
  { decorations: (v) => v.decorations },
);
