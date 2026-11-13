// The package's pure parts: result shaping (positions and dedup, against
// what the real core sends), the assets and cross-origin decisions, and
// the element's attributes.

import assert from "node:assert/strict";
import { test } from "node:test";
import { blobWorkerSource, needsBlobWorker, resolveAssets } from "../src/assets.js";
import { flag, inlineSource, nameFromSrc, parseAttributes } from "../src/attributes.js";
import { sceneBBox, shapeResult, spanPosition, utf16Column } from "../src/result.js";

// A real run (crates/web, 0.7.0) of a model whose include has a syntax
// error: one Diagnostic and the console line it duplicates.
const SYNTAX = {
  render: {
    exitCode: 1,
    diagnostics: [
      {
        code: "syntax-error",
        file: "/doc/m1/lib/p.scad",
        hints: [{ message: "unexpected `;` at line 2, column 16", replacement: null }],
        line: 2,
        message: "Parser error: syntax error",
        severity: "error",
        span: { startLine: 2, startColumn: 16, endLine: 2, endColumn: 17 },
        text: "ERROR: Parser error: syntax error in file lib/p.scad, line 2",
        trace: [],
      },
    ],
  },
  console: [
    {
      kind: "error",
      location: { path: "/doc/m1/lib/p.scad", startLine: 1, startCharacter: 15, endLine: 1, endCharacter: 16 },
      text: "ERROR: Parser error: syntax error in file lib/p.scad, line 2",
    },
    { kind: "info", location: null, text: "Can't parse file '/doc/m1/main.scad'!\n" },
    { kind: "error", location: null, text: "ERROR: Assertion 'false' failed" },
  ],
  scene: null,
  fileView: null,
};

test("errors come from diagnostics and console lines, deduplicated, in one position convention", () => {
  const texts = new Map([["/doc/m1/lib/p.scad", "module p() {}\nfunction f() = ;"]]);
  const r = shapeResult(SYNTAX, { dir: "/doc/m1/", texts });
  assert.equal(r.ok, false);
  assert.equal(r.errors.length, 2, "the console's copy of the diagnostic is dropped");
  const [d, c] = r.errors;
  assert.deepEqual(
    { path: d.path, line: d.line, column: d.column, endColumn: d.endColumn, code: d.code, message: d.message },
    { path: "lib/p.scad", line: 1, column: 15, endColumn: 16, code: "syntax-error", message: "Parser error: syntax error" },
  );
  assert.equal(c.message, "Assertion 'false' failed");
  assert.equal(c.line, null);
  assert.equal(r.bbox, null);
});

test("byte columns become UTF-16 columns through the file's text", () => {
  // "é" is two bytes and one UTF-16 unit; "𝒳" four bytes and two units.
  assert.equal(utf16Column('echo("é", yy);', 12), 10);
  assert.equal(utf16Column("𝒳 = yy;", 8), 5);
  assert.deepEqual(spanPosition({ startLine: 1, startColumn: 12, endLine: 1, endColumn: 14 }, 'echo("é", yy);'), {
    line: 0,
    column: 10,
    endLine: 0,
    endColumn: 12,
  });
  assert.deepEqual(spanPosition({ startLine: 3, startColumn: 2, endLine: 3, endColumn: 4 }, null), { line: 2, column: 1, endLine: 2, endColumn: 3 });
});

test("the box comes from the packed scene's meta", () => {
  assert.deepEqual(sceneBBox({ meta: JSON.stringify({ bbox: [[0, 0, 0], [1, 2, 3]] }) }), { min: [0, 0, 0], max: [1, 2, 3] });
  assert.equal(sceneBBox({ meta: "{}" }), null);
  assert.equal(sceneBBox({ meta: "not json" }), null);
});

test("assets resolve to a directory URL; another origin needs the blob worker", () => {
  const base = new URL("https://cdn.example/npm/@neoscad/viewer@0.7.0/dist/assets/");
  assert.equal(resolveAssets(null, base).href, base.href);
  assert.equal(resolveAssets("/static/neoscad", base, "https://site.example/blog/post").href, "https://site.example/static/neoscad/");
  assert.equal(resolveAssets("assets?x=1#y", base, "https://site.example/a/b").href, "https://site.example/a/assets/");
  assert.equal(needsBlobWorker(new URL("core/worker.js", base), "https://site.example"), true);
  assert.equal(needsBlobWorker("https://site.example/a/core/worker.js", "https://site.example"), false);
  assert.equal(needsBlobWorker("https://site.example/a/core/worker.js", "null"), true, "an opaque origin (a sandboxed iframe)");
  assert.equal(blobWorkerSource("https://cdn.example/x/worker.js"), 'import "https://cdn.example/x/worker.js";\n');
});

test("the element's attributes", () => {
  const attrs = (o) => (n) => o[n] ?? null;
  const d = { axes: true, edges: false, scheme: "Cornfield" };
  const p = parseAttributes(attrs({ src: "parts/bracket.scad", mode: "render", enable: "sketch, part", edges: "", axes: "false", "color-scheme": "Metallic", view: "Top" }), d);
  assert.equal(p.mode, "render");
  assert.deepEqual(p.enable, ["sketch", "part"]);
  assert.deepEqual(p.settings, { axes: false, edges: true, scheme: "Metallic" });
  assert.equal(p.view, "top");
  assert.equal(p.backend, "auto");
  const q = parseAttributes(attrs({ mode: "bogus", view: "sideways", backend: "WEBGL" }), d);
  assert.equal(q.mode, "preview");
  assert.equal(q.view, null);
  assert.equal(q.backend, "webgl");
  assert.equal(flag(null, true), true);
  assert.equal(flag("off", true), false);
  assert.equal(flag("", false), true);
  assert.equal(nameFromSrc("parts/bracket.scad?v=2"), "bracket.scad");
  assert.equal(nameFromSrc("https://x.example/model"), "main.scad");
  assert.equal(nameFromSrc(null), "main.scad");
  assert.equal(inlineSource("\n    cube(1);\n      sphere(2);\n  "), "cube(1);\n  sphere(2);\n");
});
