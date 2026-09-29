// The customizer's model (web/src/model/customizer.js) and the mock's
// reading of customizer comments against a real example.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { parseParameters } from "../src/engine/mock-params.js";
import { parameterGroups } from "../src/engine/protocol.js";
import { CustomizerModel, clamp, formatNumber, limitText, parseNumber, snap } from "../src/model/customizer.js";

const slider = { name: "r", control: { kind: "slider", min: 0, max: 1, step: 0.1 }, defaultValue: 0.5 };
const spin = { name: "n", control: { kind: "spinbox", min: 1, max: null, step: 1 }, defaultValue: 3 };
const text = { name: "s", control: { kind: "text", maxLength: 3 }, defaultValue: "ab" };
const vector = { name: "v", control: { kind: "vector", min: 0, max: 10, step: null }, defaultValue: [1, 2] };

test("snap keeps the step's decimals, from the minimum", () => {
  assert.equal(snap(0.3000001, 0.1, 0), 0.3);
  assert.equal(snap(7, 5, 1), 6);
  assert.equal(snap(7.3, null, 0), 7.3);
  assert.equal(clamp(12, 0, 10), 10);
  assert.equal(clamp(-1, null, null), -1);
});

test("number fields parse and print", () => {
  assert.equal(parseNumber(" 1. "), 1);
  assert.equal(parseNumber("abc"), null);
  assert.equal(parseNumber(""), null);
  assert.equal(formatNumber(0.1 + 0.2), "0.3");
});

test("text is cut at a character boundary by UTF-8 bytes", () => {
  assert.equal(limitText("abcd", 3), "abc");
  assert.equal(limitText("aé", 2), "a");
  assert.equal(limitText("aé", null), "aé");
});

test("setting a value snaps, clamps and unsets at the text's value", () => {
  const m = new CustomizerModel([{ name: "G", parameters: [slider, spin, text, vector] }]);
  assert.ok(m.set(slider, 0.73));
  assert.equal(m.value(slider), 0.7);
  assert.ok(m.set(spin, -5));
  assert.equal(m.value(spin), 1);
  assert.ok(m.set(text, "abcdef"));
  assert.equal(m.value(text), "abc");
  assert.ok(m.set(vector, [3, 40]));
  assert.deepEqual(m.value(vector), [3, 10]);
  // Back to the default: no longer an override.
  assert.ok(m.set(slider, 0.5));
  assert.ok(!m.isSet("r"));
  assert.ok(!m.set(slider, 0.5), "no change");
  assert.ok(m.unset("n"));
  assert.deepEqual(Object.keys(m.values).sort(), ["s", "v"]);
  assert.ok(m.reset());
  assert.deepEqual(m.values, {});
});

test("new groups drop values for parameters that went away or now match", () => {
  const m = new CustomizerModel([], { r: 0.9, gone: 1, n: 7 });
  m.setGroups([{ name: "G", parameters: [slider, { ...spin, defaultValue: 7 }] }]);
  assert.deepEqual(m.values, { r: 0.9 });
});

test("the mock reads sign.scad's customizer comments", () => {
  const src = readFileSync(new URL("../examples/sign.scad", import.meta.url), "utf8");
  const groups = parameterGroups(parseParameters(src));
  assert.deepEqual(
    groups.map((g) => g.name),
    ["properties of Sign", "Content To be written"],
  );
  const [props, content] = groups;
  const byName = Object.fromEntries([...props.parameters, ...content.parameters].map((p) => [p.name, p]));
  assert.deepEqual(byName.resolution.control.options.map((o) => o.value), [10, 20, 30, 50, 100]);
  assert.deepEqual(byName.radius.control, { kind: "slider", min: 60, max: 200, step: null });
  assert.equal(byName.radius.description, "The horizontal radius of the outer ellipse of the sign.");
  assert.equal(byName.Message.control.kind, "dropdown");
  assert.equal(byName.Message.defaultValue, "Welcome to...");
  assert.equal(byName.To.control.kind, "text");
  assert.equal(byName.$fn, undefined, "not a literal");
});

test("the mock reads box-lid.scad's groups, steps and hidden group", () => {
  const src = readFileSync(new URL("../examples/box-lid.scad", import.meta.url), "utf8");
  const groups = parameterGroups(parseParameters(src));
  assert.deepEqual(
    groups.map((g) => g.name),
    ["Size", "Print"],
  );
  const wall = groups[1].parameters.find((p) => p.name === "wall");
  assert.deepEqual(wall.control, { kind: "slider", min: 0.8, max: 4, step: 0.2 });
});
