// Builds the web demo's front end into a directory:
//
//   node build.mjs [--out DIR] [--engine mock|wasm] [--view none|wasm]
//                  [--version V] [--sha S]
//
// DIR (default web/dist) gets index.html, app.js, app.css, the mock
// worker (for `--engine mock` only), the examples, build.json and the
// front end's third-party licences. scripts/web/build.sh calls this, then adds the wasm core and
// viewer, BOSL2 and the release files; `npm run build` alone gives a
// mock build for development (`npm run serve`).
//
// The editor is the macOS app's CodeMirror bundle source
// (apple/Editor/web/src), bundled here with its own pinned node_modules,
// so its grammar must be generated there first (`npm ci` in that
// directory; this script runs its grammar step when the parser is missing).

import * as esbuild from "esbuild";
import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const repo = resolve(root, "..");
const editorDir = join(repo, "apple/Editor/web");

function arg(name, fallback) {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : fallback;
}

const out = resolve(arg("out", join(root, "dist")));
const build = {
  schema: 1,
  version: arg("version", "dev"),
  sha: arg("sha", "unknown"),
  engine: arg("engine", "mock"),
  view: arg("view", "none"),
};

if (!existsSync(join(editorDir, "node_modules"))) {
  throw new Error(`${editorDir}/node_modules is missing: run \`npm ci\` there first`);
}
if (!existsSync(join(editorDir, "src/lang/parser.js"))) {
  execFileSync(process.execPath, ["build.mjs", "--grammar"], { cwd: editorDir, stdio: "inherit" });
}

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });

const common = {
  bundle: true,
  format: "esm",
  // Browsers with module workers and top-level await; WebGPU needs newer
  // ones still, and the page falls back without it.
  target: ["es2022", "chrome111", "safari16.4", "firefox114"],
  minify: true,
  sourcemap: false,
  legalComments: "none",
  metafile: true,
  logLevel: "warning",
};

const app = await esbuild.build({
  ...common,
  // The mock worker only in a mock build: a release bundle has the wasm
  // core's own worker (core/worker.js) and nothing canned.
  entryPoints: {
    app: join(root, "src/app.js"),
    ...(build.engine === "mock" ? { "mock-worker": join(root, "src/engine/mock-worker.js") } : {}),
  },
  outdir: out,
});

for (const f of ["index.html", "app.css", "favicon.svg"]) copyFileSync(join(root, "src", f), join(out, f));
mkdirSync(join(out, "examples"));
for (const f of readdirSync(join(root, "examples"))) {
  if (/\.(scad|json|txt)$/.test(f)) copyFileSync(join(root, "examples", f), join(out, "examples", f));
}
writeFileSync(join(out, "build.json"), JSON.stringify(build, null, 2) + "\n");
writeFileSync(join(out, "THIRD-PARTY-LICENSES-web.txt"), licenses(app.metafile));

/// The licence of every npm package bundled into app.js (the minified
/// bundle keeps no licence comments), found in whichever node_modules
/// esbuild resolved it from.
function licenses(metafile) {
  const packages = new Map();
  for (const input of Object.keys(metafile.inputs)) {
    const m = input.match(/^(.*node_modules)\/((?:@[^/]+\/)?[^/]+)\//);
    if (m) packages.set(m[2], resolve(process.cwd(), m[1], m[2]));
  }
  const parts = [];
  for (const name of [...packages.keys()].sort()) {
    const dir = packages.get(name);
    const { version, license } = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
    const file = ["LICENSE", "LICENSE.md", "LICENSE.txt"].map((f) => join(dir, f)).find(existsSync);
    const text = file ? readFileSync(file, "utf8").trim() : `License: ${license}`;
    parts.push(`${name} ${version}\n\n${text}\n`);
  }
  return parts.join("\n" + "-".repeat(72) + "\n\n");
}

console.log(`web front end -> ${out} (${build.engine} engine, ${build.view} view)`);
