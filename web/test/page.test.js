// The page's HTML (src/index.html) for search engines and link previews,
// and early.js, the classic script that picks the embed layout before the
// first paint.

import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import { parseShare } from "../src/share.js";

const html = readFileSync(new URL("../src/index.html", import.meta.url), "utf8");
const early = readFileSync(new URL("../src/early.js", import.meta.url), "utf8");
const attr = (re) => html.match(re)?.[1];

test("head: description, canonical, Open Graph and Twitter tags", () => {
  const description = attr(/<meta\s+name="description"\s+content="([^"]*)"/);
  assert.ok(description, "a meta description");
  // Search results cut a description at about 155 characters.
  assert.ok([...description].length <= 155, `description is ${[...description].length} characters`);
  assert.equal(attr(/<link rel="canonical" href="([^"]*)"/), "https://neoscad.org/try/");
  assert.equal(attr(/<meta property="og:url" content="([^"]*)"/), "https://neoscad.org/try/");
  assert.equal(attr(/<meta\s+property="og:description"\s+content="([^"]*)"/), description);
  assert.match(attr(/<meta property="og:image" content="([^"]*)"/), /^https:\/\/neoscad\.org\/assets\/[^/]+\.(jpg|png)$/);
  assert.equal(attr(/<meta name="twitter:card" content="([^"]*)"/), "summary_large_image");
  // Absolute URLs in the head are the site's own, never a local path.
  for (const [, url] of html.matchAll(/(?:href|content)="(\w+:[^"]*)"/g)) assert.match(url, /^https:\/\/neoscad\.org\//);
});

test("JSON-LD: a WebApplication that parses", () => {
  const blocks = [...html.matchAll(/<script type="application\/ld\+json">([\s\S]*?)<\/script>/g)].map((m) => JSON.parse(m[1]));
  assert.equal(blocks.length, 1);
  const [app] = blocks;
  assert.equal(app["@context"], "https://schema.org");
  assert.equal(app["@type"], "WebApplication");
  assert.equal(app.url, "https://neoscad.org/try/");
  assert.equal(app.applicationCategory, "DesignApplication");
  assert.equal(app.operatingSystem, "Web browser");
  assert.equal(app.isAccessibleForFree, true);
  assert.equal(app.offers.price, "0");
});

test("one h1, visually hidden; early.js before the module", () => {
  const h1s = [...html.matchAll(/<h1\b([^>]*)>/g)];
  assert.equal(h1s.length, 1);
  assert.match(h1s[0][1], /class="visually-hidden"/);
  const earlyAt = html.indexOf('<script src="early.js"></script>');
  assert.ok(earlyAt > 0 && earlyAt < html.indexOf("</head>"), "early.js is a classic script in <head>");
  assert.ok(earlyAt < html.indexOf('src="app.js"'));
});

test("early.js marks the embed view exactly when parseShare() does", () => {
  for (const hash of ["", "#embed=1&code=abc", "#embed=1&example=gear", "#example=gear", "#embed=0", "#code=x&embed=1", "#embed=1", "#embed=10", "#embed=0&embed=1", "embed=1"]) {
    const classes = new Set();
    const context = { location: { hash }, URLSearchParams, document: { documentElement: { classList: { add: (c) => classes.add(c) } } } };
    vm.runInNewContext(early, context);
    assert.equal(classes.has("embed"), parseShare(hash).embed, hash);
  }
});
