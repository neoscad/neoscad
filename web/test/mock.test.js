// The mock engine and the small formats around it: the tar reader for the
// lazy libraries, the mock's exports, and the site contract's resolution.

import assert from "node:assert/strict";
import { test } from "node:test";
import { MockCore } from "../src/engine/mock-core.js";
import { docPath } from "../src/engine/protocol.js";
import { tar, untar } from "../src/engine/tar.js";
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

test("the mock's run points echo lines at their source", () => {
  const core = new MockCore();
  const path = docPath("t.scad");
  core.handle({ type: "open", path, text: 'cube(1);\n  echo("a", 1);\n' });
  const { result } = core.handle({ type: "run", path, mode: "preview", overrides: [], parts: false });
  const echo = result.console.find((l) => l.kind === "echo");
  assert.deepEqual(echo.location, { path, start_line: 1, start_character: 2, end_line: 1, end_character: 15 });
  assert.equal(result.render.geometry, null, "a preview has no statistics");
});

test("the mock's edits and exports", () => {
  const core = new MockCore();
  const path = docPath("t.scad");
  core.handle({ type: "open", path, text: "cube(1);" });
  core.handle({ type: "edit", path, edits: [[5, 6, "2"]] });
  assert.equal(core.files.get(path), "cube(2);");
  core.handle({ type: "run", path, mode: "render", overrides: [], parts: false });
  const text = (f) => new TextDecoder().decode(core.handle({ type: "export", path, format: f }).result.bytes);
  assert.match(text("stl"), /^solid /);
  assert.match(text("off"), /^OFF\n/);
  assert.match(text("svg"), /<svg/);
  const zip = new Uint8Array(core.handle({ type: "export", path, format: "3mf" }).result.bytes);
  assert.deepEqual([...zip.subarray(0, 4)], [0x50, 0x4b, 0x03, 0x04]);
  assert.throws(() => core.handle({ type: "export", path, format: "dxf" }), /unknown export format/);
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
