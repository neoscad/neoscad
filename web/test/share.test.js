// Models in the address (src/share.js): the payload's two forms, its
// bounds, bad payloads, file names, the fragment's parameters, and the
// framing check.

import assert from "node:assert/strict";
import { test } from "node:test";
import { Duplex } from "node:stream";
import zlib from "node:zlib";
import {
  DEFAULT_NAME,
  LINK_EXTENSIONS,
  MAX_SOURCE_BYTES,
  codeHash,
  ShareError,
  decodeSource,
  encodeSource,
  exampleHash,
  fileName,
  framing,
  fromBase64url,
  parseShare,
  shareHash,
  stripShare,
  toBase64url,
} from "../src/share.js";

// node:zlib's raw DEFLATE as web streams: Node 18 (the unit tests' floor)
// has CompressionStream but not its "deflate-raw" format.
const streams = {
  compress: () => Duplex.toWeb(zlib.createDeflateRaw()),
  decompress: () => Duplex.toWeb(zlib.createInflateRaw()),
};
const opts = { streams };

const SOURCE = `// A shared model: ünïcödé, emoji 🧊, and a tab\t.
difference() {
  cube(20, center = true);
  sphere(r = 13, $fn = 64);
}
`.repeat(4);

test("base64url round-trips any bytes, without padding or + and /", () => {
  const bytes = new Uint8Array(256).map((_, i) => i);
  const s = toBase64url(bytes);
  assert.match(s, /^[A-Za-z0-9_-]+$/);
  assert.deepEqual(fromBase64url(s), bytes);
  assert.deepEqual(fromBase64url(`${toBase64url(new Uint8Array([1]))}==`), new Uint8Array([1]));
  for (const bad of ["a+b", "a/b", "ab cd", "%41", "a"]) assert.throws(() => fromBase64url(bad), ShareError, bad);
});

test("a source round-trips, compressed when that is shorter", async () => {
  const payload = await encodeSource(SOURCE, opts);
  assert.ok(payload.startsWith("z:"), payload);
  assert.equal(await decodeSource(payload, opts), SOURCE);
  // A tiny source stays plain: compressing it would make it longer.
  const tiny = await encodeSource("cube(1);", opts);
  assert.equal(tiny, toBase64url(new TextEncoder().encode("cube(1);")));
  assert.equal(await decodeSource(tiny, opts), "cube(1);");
  assert.equal(await decodeSource("", opts), "");
});

test("without compression the plain form is used", async () => {
  const none = {
    compress: () => {
      throw new TypeError("no deflate-raw");
    },
    decompress: () => {
      throw new TypeError("no deflate-raw");
    },
  };
  const payload = await encodeSource(SOURCE, { streams: none });
  assert.ok(!payload.startsWith("z:"));
  assert.equal(await decodeSource(payload, { streams: none }), SOURCE);
  // A compressed payload then says why it cannot be opened.
  await assert.rejects(decodeSource(await encodeSource(SOURCE, opts), { streams: none }), /cannot decompress/);
});

test("the size is bounded on both sides", async () => {
  const big = "x".repeat(MAX_SOURCE_BYTES + 1);
  await assert.rejects(encodeSource(big, opts), ShareError);
  assert.ok((await encodeSource("x".repeat(MAX_SOURCE_BYTES), opts)).startsWith("z:"));
  // Plain: over the bound before anything is decoded, or just over it.
  await assert.rejects(decodeSource("A".repeat(200000), opts), /longer than 64 KB/);
  await assert.rejects(decodeSource(toBase64url(new TextEncoder().encode(big)), opts), /longer than 64 KB/);
  // Compressed: a 64 MB "bomb" a few KB long stops at the bound.
  const bomb = `z:${toBase64url(zlib.deflateRawSync(Buffer.alloc(64 * 1024 * 1024)))}`;
  assert.ok(bomb.length < 100000, `${bomb.length}`);
  await assert.rejects(decodeSource(bomb, opts), /longer than 64 KB/);
});

test("bad payloads fail with a ShareError, not a crash", async () => {
  const cases = {
    "not base64url": "%%%",
    "damaged deflate": `z:${toBase64url(new Uint8Array([0xff, 0xff, 0xff, 0xff]))}`,
    "not UTF-8": toBase64url(new Uint8Array([0xc3, 0x28])),
  };
  for (const [why, payload] of Object.entries(cases)) {
    await assert.rejects(decodeSource(payload, opts), ShareError, why);
  }
});

test("file names are made safe", () => {
  assert.equal(fileName(null), DEFAULT_NAME);
  assert.equal(fileName(""), DEFAULT_NAME);
  assert.equal(fileName("gear"), "gear.scad");
  assert.equal(fileName("Gear Box.SCAD"), "Gear Box.SCAD");
  assert.equal(fileName("../../etc/passwd"), "passwd.scad");
  assert.equal(fileName("a\\b\\c.scad"), "c.scad");
  assert.equal(fileName(".."), DEFAULT_NAME);
  assert.equal(fileName(".hidden"), DEFAULT_NAME);
  assert.equal(fileName("a\u0000b<c>.scad"), "abc.scad");
  const long = fileName("x".repeat(200));
  assert.equal(long.length, 64);
  assert.ok(long.endsWith(".scad"));
});

test("the fragment's parameters, and taking them out", () => {
  assert.deepEqual(parseShare("#embed=1&code=z:abc&name=gear.scad"), { code: "z:abc", name: "gear.scad", example: null, embed: true, enable: [] });
  assert.deepEqual(parseShare("#example=csg"), { code: null, name: DEFAULT_NAME, example: "csg", embed: false, enable: [] });
  // Extensions: known names only, once each, in one order, however the
  // link wrote them (URLSearchParams' %2C included).
  assert.deepEqual(parseShare("#code=abc&enable=fillet,part,bogus,fillet").enable, ["part", "fillet"]);
  assert.deepEqual(parseShare("#code=abc&enable=query%2Csketch%2C%20exact").enable, ["sketch", "query", "exact"]);
  assert.deepEqual(parseShare("#code=abc&enable=").enable, []);
  assert.deepEqual(parseShare("#code=abc&enable=all").enable, [], "`all` is OpenSCAD's experiments, never an extension");
  assert.equal(stripShare("#code=abc&enable=fillet&example=csg"), "#example=csg");
  // URLSearchParams' own encoding of the marker is read the same.
  assert.equal(parseShare("#code=z%3Aabc").code, "z:abc");
  assert.equal(parseShare("").code, null);
  assert.equal(stripShare("#code=abc&name=x.scad"), "");
  assert.equal(stripShare("#embed=1&code=abc&example=csg"), "#example=csg");
  assert.equal(stripShare("#agent"), "#agent");
});

test("shareHash and exampleHash build links that parse back", async () => {
  const hash = await shareHash(SOURCE, { name: "my part", streams });
  const p = parseShare(hash);
  assert.equal(p.name, "my part.scad");
  assert.equal(p.embed, false);
  assert.equal(await decodeSource(p.code, opts), SOURCE);
  assert.ok(!(await shareHash("cube(1);", { streams })).includes("name="));
  assert.ok((await shareHash("cube(1);", { embed: true, streams })).startsWith("#embed=1&code="));
  // A link without extensions is the same as one made before links had
  // them; with them it lists the known ones in LINK_EXTENSIONS's order.
  assert.ok(!(await shareHash("cube(1);", { enable: [], streams })).includes("enable="));
  const on = await shareHash("cube(1);", { name: "f", enable: ["fillet", "bogus", "part", "sketch"], streams });
  assert.match(on, /&name=f\.scad&enable=part,sketch,fillet$/);
  assert.deepEqual(parseShare(on).enable, ["part", "sketch", "fillet"]);
  assert.equal(await decodeSource(parseShare(on).code, opts), "cube(1);");
  assert.deepEqual(LINK_EXTENSIONS, ["part", "sketch", "query", "exact", "fillet"]);
  assert.equal(codeHash("abc", { embed: true, enable: ["query"] }), "#embed=1&code=abc&enable=query");
  assert.equal(codeHash("abc"), "#code=abc");
  assert.equal(exampleHash("csg"), "#example=csg");
  assert.equal(exampleHash("a b", { embed: true }), "#embed=1&example=a%20b");
});

test("framing: top, same-origin and cross-origin frames", () => {
  const win = (origin, parent = null) => {
    const w = { location: { origin } };
    w.self = w;
    w.parent = parent ?? w;
    w.top = parent ? parent.top : w;
    return w;
  };
  const top = win("https://neoscad.org");
  assert.equal(framing(top), "top");
  assert.equal(framing(win("https://neoscad.org", top)), "same");
  assert.equal(framing(win("https://neoscad.org", win("https://neoscad.org", top))), "same");
  assert.equal(framing(win("https://neoscad.org", win("https://evil.example"))), "cross");
  // A cross-origin parent's location throws when read.
  const opaque = { self: null, top: null };
  Object.defineProperty(opaque, "location", {
    get() {
      throw new Error("SecurityError");
    },
  });
  opaque.self = opaque;
  opaque.top = opaque;
  assert.equal(framing(win("https://neoscad.org", opaque)), "cross");
  // Chrome's and Safari's list of ancestors.
  const listed = win("https://neoscad.org", top);
  listed.location.ancestorOrigins = ["https://evil.example"];
  assert.equal(framing(listed), "cross");
});
