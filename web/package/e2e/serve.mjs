// The e2e tests' two origins, as a third-party page meets the package:
//
//   page origin    (--port, default 8124)    the test pages (e2e/pages),
//                  the package at /pkg/ (self-hosted), and /app/, a small
//                  app bundled with esbuild from e2e/app (a bundler user
//                  that copies the assets and passes assetsUrl)
//   assets origin  (--cdn-port, default 8125) the package again at /pkg/,
//                  as a CDN serves it: Access-Control-Allow-Origin: *,
//                  and .wasm as application/octet-stream, so the loaders'
//                  fallback for a wrong MIME type is exercised too
//
//   node e2e/serve.mjs [--dir BUILD] [--port N] [--cdn-port N]
//
// BUILD is build.mjs's output (default build-mock). Local use only: it
// binds 127.0.0.1.

import * as esbuild from "esbuild";
import { createReadStream, existsSync, mkdtempSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { dirname, extname, join, normalize, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : fallback;
}

const dir = resolve(arg("dir", join(here, "../build-mock")));
const port = Number(arg("port", 8124));
const cdnPort = Number(arg("cdn-port", 8125));

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json",
  ".wasm": "application/wasm",
  ".scad": "text/plain; charset=utf-8",
  ".gz": "application/gzip",
  ".txt": "text/plain; charset=utf-8",
};

// The bundler user: e2e/app/main.js imports "@neoscad/viewer" and is bundled
// against BUILD, as an app's bundler resolves it from node_modules.
const app = mkdtempSync(join(tmpdir(), "neoscad-e2e-app-"));
await esbuild.build({
  entryPoints: [join(here, "app/main.js")],
  bundle: true,
  format: "esm",
  outfile: join(app, "main.js"),
  alias: { "@neoscad/viewer": join(dir, "dist/index.js") },
  logLevel: "warning",
});

function file(base, rel) {
  const p = normalize(join(base, decodeURIComponent(rel)));
  if (!p.startsWith(base)) return null;
  if (existsSync(p) && statSync(p).isDirectory()) return file(p, "index.html");
  return existsSync(p) ? p : null;
}

function handler(cdn) {
  return (req, res) => {
    const url = new URL(req.url, "http://localhost");
    let path = null;
    if (url.pathname.startsWith("/pkg/")) path = file(dir, url.pathname.slice(5));
    else if (!cdn && url.pathname.startsWith("/app/")) path = url.pathname === "/app/" ? join(here, "app/index.html") : file(app, url.pathname.slice(5));
    else if (!cdn) path = file(join(here, "pages"), url.pathname.slice(1));
    const headers = {};
    if (cdn) headers["access-control-allow-origin"] = "*";
    if (!path) {
      res.writeHead(404, headers);
      return res.end("not found");
    }
    let type = TYPES[extname(path)] ?? "application/octet-stream";
    if (cdn && extname(path) === ".wasm") type = "application/octet-stream";
    res.writeHead(200, { ...headers, "content-type": type, "cache-control": "no-store" });
    createReadStream(path).pipe(res);
  };
}

createServer(handler(false)).listen(port, "127.0.0.1");
createServer(handler(true)).listen(cdnPort, "127.0.0.1");
console.log(`package e2e: pages on http://127.0.0.1:${port}/, the package as a CDN on http://127.0.0.1:${cdnPort}/pkg/ (${dir})`);
