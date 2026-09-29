// A static server for trying the bundle as the website serves it: under
// a sub-path (/try/ by default), so a URL that is not relative shows up as
// a 404 here rather than on the site.
//
//   node serve.mjs [--dir web/dist] [--prefix /try/] [--port 8123]
//                  [--site DIR]
//
// `--site DIR` serves DIR at the root too (a copy of ../neoscad-website,
// for its site.json and theme.css). Without it the root has a stand-in
// site.json and a theme.css that sets one token (TEST_THEME), so the top
// bar's contract with the site is exercised either way. Local use only: it binds 127.0.0.1.

import { createReadStream, existsSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : fallback;
}

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json",
  ".wasm": "application/wasm",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".scad": "text/plain; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
  ".gz": "application/gzip",
};

export const TEST_SITE = {
  schema: 1,
  name: "NeoSCAD",
  home: "/",
  theme: "/theme.css",
  nav: [
    { label: "Home", href: "/" },
    { label: "Try it", href: "/try/" },
  ],
};

/// A theme that changes one token, so a test can see the site's theme win.
export const TEST_THEME = ":root { --ns-accent: rgb(1, 2, 3); }\n";

/// Resolve `rel` inside `dir`, or null if it escapes it or is missing.
function file(dir, rel) {
  const p = normalize(join(dir, decodeURIComponent(rel)));
  if (!p.startsWith(dir)) return null;
  if (existsSync(p) && statSync(p).isDirectory()) return file(p, "index.html");
  return existsSync(p) ? p : null;
}

export function serve({ dir, prefix = "/try/", port = 8123, site = null }) {
  dir = resolve(dir);
  site = site ? resolve(site) : null;
  const server = createServer((req, res) => {
    const url = new URL(req.url, "http://localhost");
    let path = null;
    if (url.pathname === prefix.slice(0, -1)) {
      res.writeHead(301, { location: prefix });
      return res.end();
    }
    if (url.pathname.startsWith(prefix)) path = file(dir, url.pathname.slice(prefix.length));
    else if (site) path = file(site, url.pathname.slice(1));
    else if (url.pathname === "/site.json") {
      res.writeHead(200, { "content-type": TYPES[".json"] });
      return res.end(JSON.stringify(TEST_SITE));
    } else if (url.pathname === "/theme.css") {
      res.writeHead(200, { "content-type": TYPES[".css"] });
      return res.end(TEST_THEME);
    }
    if (!path) {
      res.writeHead(404, { "content-type": "text/plain" });
      return res.end(`not found: ${url.pathname}\n`);
    }
    res.writeHead(200, { "content-type": TYPES[extname(path)] ?? "application/octet-stream", "cache-control": "no-cache" });
    createReadStream(path).pipe(res);
  });
  return new Promise((ok) => server.listen(port, "127.0.0.1", () => ok(server)));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const port = Number(arg("port", 8123));
  const prefix = arg("prefix", "/try/");
  await serve({ dir: arg("dir", join(root, "dist")), prefix, port, site: arg("site", null) });
  console.log(`http://127.0.0.1:${port}${prefix}`);
}
