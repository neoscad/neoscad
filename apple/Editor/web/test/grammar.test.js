// The grammar: every construct parses cleanly, the trees have the expected
// shape, numbers and names split as OpenSCAD's lexer splits them, and real
// libraries (BOSL2, OpenSCAD's own tests and MCAD, when the reference
// checkouts are present) parse without error nodes.

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { test } from "node:test";
import { parser } from "../src/lang/openscad.js";
import { errorNodes, fixture, repo, scadFiles } from "./util.js";

/// The tree's named nodes, without punctuation and operator tokens (whose
/// names are their text), in Lezer's `Node(child,...)` notation.
function tree(src) {
  const print = (node) => {
    const kids = [];
    for (let c = node.firstChild; c; c = c.nextSibling) {
      if (/^[A-Za-z$]/.test(c.name)) kids.push(print(c));
    }
    return kids.length ? `${node.name}(${kids.join(",")})` : node.name;
  };
  return print(parser.parse(src).topNode);
}

test("every construct parses without an error node", () => {
  const src = fixture("constructs.scad");
  assert.deepEqual(errorNodes(parser.parse(src), src), []);
});

test("statements have the expected shape", () => {
  assert.equal(
    tree("translate([1,0,0]) cube(2);"),
    "Program(ModuleCall(ModuleName,ArgList(Vector(Number,Number,Number)),ModuleCall(ModuleName,ArgList(Number))))",
  );
  assert.equal(
    tree("module m(a=1) cube(a);"),
    'Program(ModuleDefinition(module,DefName,ParamList(Param(ParamName,Number)),ModuleCall(ModuleName,ArgList(VariableName))))',
  );
  assert.equal(
    tree("function f(x) = x;"),
    'Program(FunctionDefinition(function,DefName,ParamList(Param(ParamName)),VariableName))',
  );
  assert.equal(
    tree("if (a) cube(); else sphere();"),
    "Program(IfStatement(if,ParenthesizedCondition(VariableName),ModuleCall(ModuleName,ArgList),else,ModuleCall(ModuleName,ArgList)))",
  );
  assert.equal(
    tree("#!cube();"),
    "Program(ModifiedStatement(Modifier,ModifiedStatement(Modifier,ModuleCall(ModuleName,ArgList))))",
  );
  assert.equal(tree("include <a/b.scad>"), "Program(IncludeStatement(include,IncludePath))");
  assert.equal(
    tree("for (i=[0:2]) cube(i);"),
    'Program(ModuleCall(for,ArgList(NamedArgument(ArgName,Range(Number,Number))),ModuleCall(ModuleName,ArgList(VariableName))))',
  );
});

test("operators bind as in OpenSCAD", () => {
  // `^` binds tighter than unary minus and to the right (parser.y's
  // `exponent: call '^' unary`).
  assert.equal(
    tree("x = -2 ^ 3 ^ 2;"),
    'Program(Assignment(VariableDefinition,UnaryExpression(ArithOp,BinaryExpression(Number,ArithOp,BinaryExpression(Number,ArithOp,Number)))))',
  );
  assert.equal(
    tree("x = 1 + 2 * 3;"),
    'Program(Assignment(VariableDefinition,BinaryExpression(Number,ArithOp,BinaryExpression(Number,ArithOp,Number))))',
  );
  assert.equal(
    tree("x = a || b && c;"),
    'Program(Assignment(VariableDefinition,BinaryExpression(VariableName,LogicOp,BinaryExpression(VariableName,LogicOp,VariableName))))',
  );
  // A `let` body runs to the end of the expression.
  assert.equal(
    tree("x = let (a = 1) a + 1;"),
    'Program(Assignment(VariableDefinition,LetExpression(let,ArgList(NamedArgument(ArgName,Number)),BinaryExpression(VariableName,ArithOp,Number))))',
  );
  // `assert(...)` with no body.
  assert.equal(
    tree("x = assert(true);"),
    'Program(Assignment(VariableDefinition,AssertExpression(assert,ArgList(true))))',
  );
});

test("numbers and names split as lexer.l splits them", () => {
  const kinds = (src) => {
    const t = parser.parse(`x = ${src};`);
    const out = [];
    t.iterate({
      enter(n) {
        if (n.name === "Number" || n.name === "VariableName") out.push(n.name);
      },
    });
    return out.join(" ");
  };
  // flex's longest match, a tie going to the number.
  for (const n of ["10", "1e5", "1E-5", "1.5", ".5", "1.", "0x1F", "08", "1e+5"]) {
    assert.equal(kinds(n), "Number", n);
  }
  for (const n of ["8bit", "3d", "1e", "2e3x", "0x", "0x1Fg", "12ptStar", "$fn", "é"]) {
    assert.equal(kinds(n), "VariableName", n);
  }
});

test("special variables are names everywhere", () => {
  const src = "$fn = 8; cube(1, $fn = $fn); function f($a) = $a; x = o.$fs;";
  assert.deepEqual(errorNodes(parser.parse(src), src), []);
});

test("unterminated strings and comments run to the end without error cascades", () => {
  assert.match(tree('x = "abc'), /String/);
  assert.equal(parser.parse("/* never closed\ncube();").toString(), "Program(BlockComment)");
});

test("real libraries parse without error nodes", (t) => {
  // BOSL2's library, tests and examples; not examples_x or tests_x, which
  // are generated from its documentation and hold fragments that OpenSCAD
  // itself rejects (`npm run corpus` checks those against the real parser).
  const corpora = [
    { name: "BOSL2", dir: repo(".reference/BOSL2"), flat: true },
    { name: "BOSL2 tests", dir: repo(".reference/BOSL2/tests") },
    { name: "BOSL2 examples", dir: repo(".reference/BOSL2/examples") },
    { name: "MCAD", dir: repo(".reference/openscad/libraries/MCAD") },
    { name: "OpenSCAD examples", dir: repo(".reference/openscad/examples") },
  ];
  for (const { name, dir, flat } of corpora) {
    if (!existsSync(dir)) {
      t.diagnostic(`${name}: not checked out, skipped`);
      continue;
    }
    let bytes = 0;
    const bad = [];
    const files = scadFiles(dir, !flat);
    for (const file of files) {
      const src = readFileSync(file, "utf8");
      bytes += src.length;
      const errors = errorNodes(parser.parse(src), src);
      if (errors.length) bad.push(`${file}: ${errors[0]}`);
    }
    t.diagnostic(`${name}: ${files.length} files, ${(bytes / 1e6).toFixed(2)} MB, ${bad.length} with error nodes`);
    assert.deepEqual(bad, [], name);
  }
});
