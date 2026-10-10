// The adapter (web/src/engine/protocol.js) against the wire of
// docs/web-protocol.md: the requests it builds, and the results it turns
// into the panels' shapes.

import assert from "node:assert/strict";
import { test } from "node:test";
import {
  EXPORT_FORMATS,
  LANGUAGE_EXTENSIONS,
  Requests,
  applyEdits,
  enabledExtensions,
  checkReport,
  exportOutcome,
  consoleLines,
  control,
  editorEdits,
  fileURI,
  fileViewChanged,
  offsetAt,
  overrides,
  parameterGroups,
  plainValue,
  positionAt,
  runResult,
  sectionResult,
  uriPath,
  usesLibrary,
  usesText,
} from "../src/engine/protocol.js";

test("controls: the wire's kinds, spinBox as the panel's spinbox", () => {
  assert.deepEqual(control({ kind: "slider", min: 0, max: 10, step: 0.5 }), { kind: "slider", min: 0, max: 10, step: 0.5 });
  assert.deepEqual(control({ kind: "checkbox" }), { kind: "checkbox" });
  assert.deepEqual(control({ kind: "spinBox", min: null, max: 5, step: null }), { kind: "spinbox", min: null, max: 5, step: null });
  assert.deepEqual(control({ kind: "text", maxLength: 8 }), { kind: "text", maxLength: 8 });
  assert.deepEqual(
    control({ kind: "dropdown", options: [{ label: "One", value: { kind: "number", value: 1 } }] }),
    { kind: "dropdown", options: [{ label: "One", value: 1 }] },
  );
  // An unknown control degrades to a text field rather than failing.
  assert.equal(control({ kind: "knob" }).kind, "text");
});

test("parameter values: tagged on the wire, plain in the page", () => {
  assert.equal(plainValue({ kind: "bool", value: true }), true);
  assert.deepEqual(plainValue({ kind: "vector", value: [1, 2] }), [1, 2]);
  assert.deepEqual(overrides({ b: 1, a: [1, 2], c: "x", d: false }), [
    { name: "a", value: { kind: "vector", value: [1, 2] } },
    { name: "b", value: { kind: "number", value: 1 } },
    { name: "c", value: { kind: "text", value: "x" } },
    { name: "d", value: { kind: "bool", value: false } },
  ]);
});

test("parameter groups keep order and read defaultValue", () => {
  const g = parameterGroups([
    { name: "A", parameters: [{ name: "x", description: "d", control: { kind: "checkbox" }, defaultValue: { kind: "bool", value: false } }] },
    { name: "B", parameters: [{ name: "y", description: "", control: { kind: "text", maxLength: null }, defaultValue: { kind: "text", value: "s" } }] },
  ]);
  assert.deepEqual(
    g.map((x) => [x.name, x.parameters.map((p) => [p.name, p.defaultValue, p.control.kind, p.description])]),
    [
      ["A", [["x", false, "checkbox", "d"]]],
      ["B", [["y", "s", "text", ""]]],
    ],
  );
});

test("console lines keep their locations; unknown kinds are info", () => {
  const location = { path: "/doc/a b.scad", startLine: 1, startCharacter: 2, endLine: 1, endCharacter: 5 };
  const lines = consoleLines([
    { kind: "warning", text: "W", location },
    { kind: "echo", text: "E", location: null },
    { kind: "mystery", text: "?", location: null },
  ]);
  assert.deepEqual(lines[0], { kind: "warning", text: "W", location });
  assert.equal(lines[1].kind, "echo");
  assert.equal(lines[2].kind, "info");
});

test("a run result passes the packed scene through and reads render's fields", () => {
  const faces = new ArrayBuffer(44 * 3);
  const r = runResult({
    render: {
      exitCode: 0,
      diagnostics: [],
      echo: [],
      console: "",
      geometry: { dimensions: 3, bboxMin: [0, 0, 0], bboxMax: [1, 1, 1], area: 6 },
      cacheEntries: 1,
      timings: { parseMs: 0.5, evaluateMs: 1, geometryMs: 0.5, totalMs: 2 },
    },
    console: [],
    files: [],
    language: ['{"jsonrpc":"2.0"}'],
    scene: { faces, edges: new ArrayBuffer(0), meta: "{}" },
    fileView: { vpr: [0, 0, 0] },
  });
  assert.equal(r.scene.faces, faces);
  assert.deepEqual(r.geometry.bboxMax, [1, 1, 1]);
  assert.equal(r.timings.totalMs, 2);
  assert.deepEqual(r.fileView, { vpr: [0, 0, 0] });
  assert.equal(r.language.length, 1);
  assert.equal(r.fontsWanted, false);
  assert.equal(runResult({ render: { exitCode: 0 }, fontsWanted: true }).fontsWanted, true);
});

test("fileView changes are what moves the view", () => {
  assert.ok(fileViewChanged({ vpr: [1, 2, 3] }, null));
  assert.ok(!fileViewChanged({ vpr: [1, 2, 3] }, { vpr: [1, 2, 3] }));
  assert.ok(fileViewChanged({ vpr: [1, 2, 3] }, { vpr: [1, 2, 4] }));
});

test("check reports and sections", () => {
  const r = checkReport({ findings: [{ id: 1, severity: "warning", bboxMin: [0, 0, 0] }], minWall: 0.6 });
  assert.equal(r.findings[0].severity, "warning");
  assert.deepEqual(r.truncated, []);
  const s = sectionResult({ plane: "z=1", outline: [[0, 0, 1, 1, 0, 1, 1, 1, 1]] });
  assert.deepEqual(s.outline, [[[0, 0, 1], [1, 0, 1], [1, 1, 1]]]);
});

test("requests carry the protocol's fields", () => {
  const run = Requests.run({
    path: "/doc/a.scad",
    mode: "render",
    values: { n: 3 },
    parts: true,
    camera: { vpt: [0, 0, 0], vpr: [55, 0, 25], vpd: 140, vpf: 22.5 },
    colorScheme: "Metallic",
  });
  assert.deepEqual(run, {
    type: "run",
    path: "/doc/a.scad",
    mode: "render",
    overrides: [{ name: "n", value: { kind: "number", value: 3 } }],
    parts: true,
    enable: [],
    camera: { vpt: [0, 0, 0], vpr: [55, 0, 25], vpd: 140, vpf: 22.5 },
    colorScheme: "Metallic",
  });
  // The page's language extensions: the apps' four, on in the settings
  // or by a link (`extra`), sent with every run in one order.
  assert.deepEqual(Object.keys(LANGUAGE_EXTENSIONS), ["sketch", "query", "exact", "fillet"]);
  assert.equal(LANGUAGE_EXTENSIONS.sketch.label, "Constrained sketches (sketch)");
  assert.equal(LANGUAGE_EXTENSIONS.query.label, "Geometry queries (query)");
  assert.equal(LANGUAGE_EXTENSIONS.exact.label, "Exact STEP export (exact)");
  assert.equal(LANGUAGE_EXTENSIONS.fillet.label, "Edge fillets and chamfers (fillet)");
  assert.deepEqual(enabledExtensions({ fillet: true, exact: true }), ["exact", "fillet"]);
  assert.deepEqual(enabledExtensions({ fillet: false, sketch: "yes" }), [], "only true turns one on");
  assert.deepEqual(enabledExtensions(undefined), []);
  assert.deepEqual(enabledExtensions({ query: true }, ["fillet", "sketch", "part", "bogus"]), ["sketch", "query", "fillet"]);
  assert.deepEqual(enabledExtensions({}, null), []);
  assert.equal(EXPORT_FORMATS.step.extension, "exact");
  assert.deepEqual(Requests.lsp("{}"), { type: "lsp", message: "{}" });
  assert.deepEqual(Requests.lsp("{}", []), { type: "lsp", message: "{}", enable: [] });
  assert.deepEqual(Requests.run({ path: "/doc/a.scad", mode: "preview", enable: enabledExtensions({ fillet: true }) }).enable, ["fillet"]);
  assert.deepEqual(Requests.section(4, "z", 2), { type: "section", measurement: 4, axis: "z", offset: 2 });
  assert.deepEqual(Requests.section(4, "z", 2, "lid").part, "lid");
  const tar = new ArrayBuffer(8);
  assert.deepEqual(Requests.addFiles({ tar, root: "/neoscad/libraries" }), { type: "addFiles", files: [], tar, root: "/neoscad/libraries" });
  assert.equal(Requests.readFile("/x").type, "readFile");
  assert.match(Requests.export("/doc/a.scad", "stl", {}).creationDate, /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/);
});

test("editor edits become LSP positions, each against the text before it", () => {
  // The bridge sends the last change first, so earlier offsets hold.
  const { edits, text } = editorEdits("cube(1);", [[5, 6, "22"], [0, 4, "sphere"]]);
  assert.equal(text, "sphere(22);");
  assert.deepEqual(edits, [
    { start: { line: 0, character: 5 }, end: { line: 0, character: 6 }, text: "22" },
    { start: { line: 0, character: 0 }, end: { line: 0, character: 4 }, text: "sphere" },
  ]);
  assert.equal(applyEdits("a😀b", [[3, 4, "c"]]), "a😀c");
  // UTF-16 columns across lines, and a column past a line's end clamps.
  const t = "a\n😀x\n";
  assert.deepEqual(positionAt(t, 4), { line: 1, character: 2 });
  assert.equal(offsetAt(t, { line: 1, character: 2 }), 4);
  assert.equal(offsetAt(t, { line: 1, character: 99 }), 5);
  assert.equal(offsetAt(t, { line: 9, character: 0 }), t.length);
});

test("library use is detected in include and use", () => {
  assert.ok(usesLibrary("include <BOSL2/std.scad>", "BOSL2"));
  assert.ok(usesLibrary("use<BOSL2/gears.scad>", "BOSL2"));
  assert.ok(!usesLibrary("include <MCAD/gears.scad>", "BOSL2"));
  assert.ok(!usesLibrary("// BOSL2 is nice", "BOSL2"));
});

test("text drawing is guessed from the source, to fetch the fonts first", () => {
  assert.ok(usesText('linear_extrude(2) text("Hi");'));
  assert.ok(usesText("m = textmetrics(s);"));
  assert.ok(usesText("fontmetrics ()"));
  assert.ok(!usesText("context(1); my_text = 2; subtext(3);"));
  assert.ok(!usesText("cube(1);"));
});

test("file URIs round-trip with spaces", () => {
  const p = "/doc/my model.scad";
  assert.equal(fileURI(p), "file:///doc/my%20model.scad");
  assert.equal(uriPath(fileURI(p)), p);
});

test("an export with failed fillets is downloaded and still fails", () => {
  const data = new ArrayBuffer(4);
  const step = { ok: true, error: null, summary: "STEP: 6 of 6 faces exact (100%)." };
  assert.deepEqual(exportOutcome({ exitCode: 0, written: true, bytes: 4, data, step }, "m.step"), {
    download: true,
    kind: "done",
    text: "Exported m.step (4 bytes). STEP: 6 of 6 faces exact (100%).",
  });
  // Decision 2 (docs/fillets.md, section 18): written, sharp, failed.
  const failure = "The file was written, but 1 fillet_edges() call failed and its edges are sharp: fillet_edges(): too large";
  const sharp = exportOutcome({ exitCode: 1, written: true, filletErrors: ["fillet_edges(): too large"], failure, bytes: 4, data, step }, "m.step");
  assert.deepEqual(sharp, { download: true, kind: "failed", text: `m.step: ${failure} STEP: 6 of 6 faces exact (100%).` });
  // Not written: the reason, and nothing to download.
  const refused = exportOutcome({ exitCode: 1, written: false, data: null, failure: "ERROR: x", step: { ok: false, error: "a fin" } }, "m.step");
  assert.deepEqual(refused, { download: false, kind: "failed", text: "Export failed: STEP export refused: a fin." });
  assert.equal(exportOutcome({ exitCode: 1, written: false, data: null, failure: "Current top level object is empty." }, "m.stl").text,
    "Export failed: Current top level object is empty.");
});
