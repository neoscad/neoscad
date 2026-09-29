// The examples: every manifest entry has its file and a licence, the
// manifest checker drops bad entries, and the copies of the icon models
// match their sources in apple/Icon.

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { test } from "node:test";
import { checkManifest } from "../src/examples.js";

const dir = new URL("../examples/", import.meta.url);
const manifest = JSON.parse(readFileSync(new URL("manifest.json", dir), "utf8"));

test("every example exists, with a licence and a unique id", () => {
  const ids = new Set();
  for (const e of manifest.examples) {
    assert.ok(existsSync(new URL(e.file, dir)), e.file);
    assert.ok(e.license, `${e.id} has a licence`);
    assert.ok(!ids.has(e.id), `${e.id} is unique`);
    ids.add(e.id);
  }
  assert.ok(ids.has(manifest.default));
  // BOSL2 examples declare the library the page must fetch first.
  for (const e of manifest.examples) {
    const text = readFileSync(new URL(e.file, dir), "utf8");
    assert.equal(/include\s*<BOSL2\//.test(text), (e.libraries ?? []).includes("BOSL2"), e.id);
  }
});

test("the checker fills defaults and drops bad entries", () => {
  const m = checkManifest({
    default: "nope",
    examples: [
      { id: "a", title: "A", file: "a.scad" },
      { id: "b", title: "B", file: "../escape.scad" },
      { id: "c", file: "c.scad" },
      { id: "h", title: "H", file: "h.scad", heavy: true },
    ],
  });
  assert.deepEqual(m.examples.map((e) => e.id), ["a", "h"]);
  assert.equal(m.default, "a");
  assert.equal(m.examples[0].autorun, true);
  assert.equal(m.examples[1].autorun, false, "heavy examples wait for Preview");
  const shipped = checkManifest(manifest);
  assert.equal(shipped.examples.find((e) => e.id === "threaded-ring").autorun, false);
});

test("the icon models are unchanged copies", () => {
  for (const [copy, source] of [
    ["threaded-ring.scad", "../../apple/Icon/concept-c.scad"],
    ["gearbox.scad", "../../apple/Icon/hero.scad"],
  ]) {
    assert.equal(readFileSync(new URL(copy, dir), "utf8"), readFileSync(new URL(source, dir), "utf8"), copy);
  }
});
