// The mock engine and the small formats around it: the tar reader for the
// lazy libraries, the mock's exports, and the site contract's resolution.

import assert from "node:assert/strict";
import { test } from "node:test";
import { MockCore } from "../src/engine/mock-core.js";
import { docPath } from "../src/engine/protocol.js";
import { tar, untar } from "../src/engine/tar.js";
import { unpack } from "../src/view/canvas2d.js";
import { DEFAULT_SITE, resolveSite } from "../src/site.js";

const enc = new TextEncoder();

test("tar round trip, and a pax path for a long name", () => {
  const files = [
    { path: "BOSL2/std.scad", bytes: enc.encode("include <x.scad>\n") },
    { path: "BOSL2/LICENSE", bytes: enc.encode("BSD") },
    { path: "BOSL2/empty.scad", bytes: new Uint8Array() },
  ];
  const back = untar(tar(files));
  assert.deepEqual(
    back.map((f) => [f.path, new TextDecoder().decode(f.bytes)]),
    files.map((f) => [f.path, new TextDecoder().decode(f.bytes)]),
  );
  const long = `BOSL2/${"d/".repeat(60)}f.scad`;
  const pax = enc.encode(`${long.length + 10} path=${long}\n`);
  const archive = tar([{ path: "PaxHeader", bytes: pax }, { path: "short", bytes: enc.encode("x") }]);
  // Turn the first entry into a pax header (type 'x').
  archive[156] = "x".charCodeAt(0);
  assert.deepEqual(untar(archive).map((f) => f.path), [long]);
});

const ready = () => {
  const core = new MockCore();
  core.handle({ type: "init", seed: 0 });
  return core;
};

test("the mock answers in the wire's shapes", () => {
  const core = new MockCore();
  assert.throws(() => core.handle({ type: "run", path: "/doc/a.scad", mode: "preview" }), (e) => e.kind === "invalidArgument");
  core.handle({ type: "init", seed: 0 });
  const path = docPath("t.scad");
  core.handle({ type: "open", path, text: 'cube(1);\n  echo("a", 1);\n' });
  const { result, transfer } = core.handle({ type: "run", path, mode: "preview", overrides: [], parts: false });
  const echo = result.console.find((l) => l.kind === "echo");
  assert.deepEqual(echo.location, { path, startLine: 1, startCharacter: 2, endLine: 1, endCharacter: 15 });
  assert.equal(result.render.geometry, null, "a preview has no statistics");
  assert.equal(result.render.exitCode, 0);
  // The packed scene: whole vertices, a draw over all of them, a box.
  const meta = JSON.parse(result.scene.meta);
  assert.equal(result.scene.faces.byteLength, meta.draws[0].count * 44);
  assert.equal(meta.bbox.length, 2);
  assert.deepEqual(transfer, [result.scene.faces, result.scene.edges]);
  const unpacked = unpack(result.scene);
  assert.equal(unpacked.count, 12);
  assert.deepEqual(unpacked.tris[0].n, [0, 0, -1]);
});

test("the mock's edits, measurement handles and exports", () => {
  const core = ready();
  const path = docPath("t.scad");
  core.handle({ type: "open", path, text: "cube(1);" });
  core.handle({ type: "edit", path, edits: [{ start: { line: 0, character: 5 }, end: { line: 0, character: 6 }, text: "2" }] });
  assert.equal(core.files.get(path), "cube(2);");
  core.handle({ type: "run", path, mode: "render", overrides: [], parts: false });
  const m = core.handle({ type: "measure", path, run: {} }).result.measurement;
  assert.equal(core.handle({ type: "pick", measurement: m, origin: [0, 0, 50], direction: [0, 0, -1] }).result.point.length, 3);
  core.handle({ type: "measure", path, run: {} });
  assert.throws(() => core.handle({ type: "section", measurement: m, axis: "z", offset: 1 }), (e) => e.kind === "invalidArgument");
  const exp = (f) => core.handle({ type: "export", path, format: f, run: {} }).result;
  const text = (f) => new TextDecoder().decode(exp(f).data);
  assert.match(text("stl"), /^solid /);
  assert.equal(exp("stl").mime, "model/stl");
  assert.match(text("off"), /^OFF\n/);
  assert.match(text("svg"), /<svg/);
  const zip = new Uint8Array(exp("3mf").data);
  assert.deepEqual([...zip.subarray(0, 4)], [0x50, 0x4b, 0x03, 0x04]);
  assert.throws(() => exp("dxf"), (e) => e.kind === "invalidArgument");
});

test("the mock unpacks addFiles' tar under its root", () => {
  const core = ready();
  const archive = tar([{ path: "BOSL2/std.scad", bytes: enc.encode("// std") }]);
  const r = core.handle({ type: "addFiles", tar: archive.buffer, root: "/neoscad/libraries" }).result;
  assert.equal(r.added, 1);
  assert.equal(core.handle({ type: "readFile", path: "/neoscad/libraries/BOSL2/std.scad" }).result.text, "// std");
});

test("site.json resolves against its own URL", () => {
  const base = "https://neoscad.org/site.json";
  const site = resolveSite(
    { schema: 1, name: "NeoSCAD", home: "/", theme: "/theme.css", nav: [{ label: "Try it", href: "/try/" }, { label: 3 }] },
    base,
  );
  assert.equal(site.home, "https://neoscad.org/");
  assert.equal(site.theme, "https://neoscad.org/theme.css");
  assert.deepEqual(site.nav, [{ label: "Try it", href: "https://neoscad.org/try/" }]);
  assert.equal(resolveSite({ schema: 2 }, base), DEFAULT_SITE);
  assert.equal(resolveSite(null, base), DEFAULT_SITE);
});
