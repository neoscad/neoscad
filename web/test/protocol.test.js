// The adapter's normalisers: whatever naming the worker settles on
// (serde's snake_case and externally tagged enums, or camelCase and a
// `kind`/`type` tag), the panels get one shape.

import assert from "node:assert/strict";
import { test } from "node:test";
import {
  Requests,
  applyEdits,
  checkReport,
  consoleLines,
  control,
  fileURI,
  overrides,
  parameterGroups,
  plainValue,
  runResult,
  uriPath,
  usesLibrary,
} from "../src/engine/protocol.js";

test("controls from each enum encoding", () => {
  const slider = { kind: "slider", min: 0, max: 10, step: 0.5 };
  assert.deepEqual(control({ Slider: { min: 0, max: 10, step: 0.5 } }), slider);
  assert.deepEqual(control({ kind: "slider", min: 0, max: 10, step: 0.5 }), slider);
  assert.deepEqual(control({ type: "Slider", min: 0, max: 10, step: 0.5 }), slider);
  assert.deepEqual(control("Checkbox"), { kind: "checkbox" });
  assert.deepEqual(control({ SpinBox: { min: null, max: 5, step: null } }), { kind: "spinbox", min: null, max: 5, step: null });
  assert.deepEqual(control({ kind: "spin_box", min: 1 }), { kind: "spinbox", min: 1, max: null, step: null });
  assert.deepEqual(control({ Text: { max_length: 8 } }), { kind: "text", maxLength: 8 });
  assert.deepEqual(
    control({ Dropdown: { options: [{ label: "One", value: { Number: { value: 1 } } }] } }),
    { kind: "dropdown", options: [{ label: "One", value: 1 }] },
  );
  // An unknown control degrades to a text field rather than failing.
  assert.equal(control({ Knob: {} }).kind, "text");
});

test("parameter values are plain JSON either way", () => {
  assert.equal(plainValue({ Bool: { value: true } }), true);
  assert.equal(plainValue({ kind: "text", value: "hi" }), "hi");
  assert.deepEqual(plainValue({ Vector: { value: [1, 2] } }), [1, 2]);
  assert.deepEqual(plainValue([1, 2]), [1, 2]);
  assert.equal(plainValue(3), 3);
});

test("parameter groups keep order and read default_value", () => {
  const g = parameterGroups([
    { name: "A", parameters: [{ name: "x", description: "d", control: "Checkbox", default_value: { Bool: { value: false } } }] },
    { name: "B", parameters: [{ name: "y", control: { kind: "text" }, defaultValue: "s" }] },
  ]);
  assert.deepEqual(
    g.map((x) => [x.name, x.parameters.map((p) => [p.name, p.defaultValue, p.control.kind, p.description])]),
    [
      ["A", [["x", false, "checkbox", "d"]]],
      ["B", [["y", "s", "text", ""]]],
    ],
  );
});

test("console lines: kinds lower-cased, locations camelCased, paths from URIs", () => {
  const lines = consoleLines([
    { kind: "Warning", text: "W", location: { path: "file:///neoscad/work/a%20b.scad", start_line: 1, start_character: 2, end_line: 1, end_character: 5 } },
    { kind: { kind: "echo" }, text: "E", location: null },
    { kind: "Mystery", text: "?" },
  ]);
  assert.deepEqual(lines[0], {
    kind: "warning",
    text: "W",
    location: { path: "/neoscad/work/a b.scad", startLine: 1, startCharacter: 2, endLine: 1, endCharacter: 5 },
  });
  assert.equal(lines[1].kind, "echo");
  assert.equal(lines[2].kind, "info");
});

test("a run result keeps the packed scene's typed arrays untouched", () => {
  const positions = new Float32Array([1, 2, 3]);
  const r = runResult({
    render: { exit_code: 0, geometry: { bbox_min: [0, 0, 0], bbox_max: [1, 1, 1] }, timings: { total_ms: 2 } },
    console: [],
    scene: { meshes: [{ positions }] },
    file_view: { vpt: [0, 0, 0] },
  });
  assert.equal(r.scene.meshes[0].positions, positions);
  assert.deepEqual(r.geometry.bboxMax, [1, 1, 1]);
  assert.equal(r.timings.totalMs, 2);
  assert.deepEqual(r.fileView, { vpt: [0, 0, 0] });
  assert.equal(r.parameters, null);
});

test("check findings' severities are plain strings", () => {
  const r = checkReport({ findings: [{ id: 1, severity: "Warning", bbox_min: [0, 0, 0] }], min_wall: 0.6 });
  assert.equal(r.findings[0].severity, "warning");
  assert.deepEqual(r.findings[0].bboxMin, [0, 0, 0]);
  assert.equal(r.minWall, 0.6);
});

test("requests: check options go out in the ffi's names", () => {
  const r = Requests.check("/p", [], true, { nozzle: 0.4, minWall: 0.8, maxOverhang: 45, bed: null });
  assert.deepEqual(r.options, { nozzle: 0.4, min_wall: 0.8, max_overhang: 45, bed: null });
  assert.equal(Requests.open("/neoscad/work/x.scad", "t").uri, "file:///neoscad/work/x.scad");
});

test("edits apply in order, UTF-16 offsets", () => {
  // The bridge sends the last change first, so earlier offsets hold.
  assert.equal(applyEdits("cube(1);", [[5, 6, "22"], [0, 4, "sphere"]]), "sphere(22);");
  assert.equal(applyEdits("a😀b", [[3, 4, "c"]]), "a😀c");
});

test("library use is detected in include and use", () => {
  assert.ok(usesLibrary("include <BOSL2/std.scad>", "BOSL2"));
  assert.ok(usesLibrary("use<BOSL2/gears.scad>", "BOSL2"));
  assert.ok(!usesLibrary("include <MCAD/gears.scad>", "BOSL2"));
  assert.ok(!usesLibrary("// BOSL2 is nice", "BOSL2"));
});

test("overrides are sorted by name", () => {
  assert.deepEqual(overrides({ b: 1, a: [1, 2] }), [
    { name: "a", value: [1, 2] },
    { name: "b", value: 1 },
  ]);
});

test("file URIs round-trip with spaces", () => {
  const p = "/neoscad/work/my model.scad";
  assert.equal(fileURI(p), "file:///neoscad/work/my%20model.scad");
  assert.equal(uriPath(fileURI(p)), p);
});
