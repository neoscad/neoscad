// Builds the @neoscad/viewer package directory (docs/js-package.md):
//
//   node build.mjs [--out DIR] [--version V] [--mock]
//                  [--core DIR] [--view DIR] [--view-webgl DIR]
//                  [--fonts FILE] [--bosl2 FILE]
//
// DIR (default web/package/build) gets package.json (this directory's,
// with the version set and the development fields dropped), README.md,
// LICENSE (the GPL with the Classpath exception, LICENSE-EXCEPTION.txt),
// and dist/: index.js and element.js (esbuild ESM bundles of src/ and the
// demo's engine and view modules they import), the hand-written types,
// and assets/ with whichever of the core, viewers, fonts and BOSL2 were
// given. scripts/web/build-package.sh calls this with the real ones, then
// adds THIRD-PARTY-LICENSES.txt and SOURCE.txt and packs it.
//
// `--mock` puts the demo's mock worker (web/src/engine/mock-worker.js) at
// assets/core/worker.js instead of a core, for the e2e tests in CI, which
// build no wasm. Such a build must never be published; build-package.sh
// refuses to pack without a real core.

import * as esbuild from "esbuild";
import { copyFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));
const repo = resolve(root, "../..");

function arg(name, fallback = null) {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : fallback;
}
const mock = process.argv.includes("--mock");
const out = resolve(arg("out", join(root, "build")));
const version = arg("version", workspaceVersion());
const dist = join(out, "dist");
const assets = join(dist, "assets");

/// `[workspace.package] version` in Cargo.toml: the package's version is
/// the engine's.
function workspaceVersion() {
  const toml = readFileSync(join(repo, "Cargo.toml"), "utf8");
  const m = toml.match(/^\[workspace\.package\][^[]*?^version\s*=\s*"([^"]+)"/m);
  if (!m) throw new Error("no [workspace.package] version in Cargo.toml");
  return m[1];
}

rmSync(out, { recursive: true, force: true });
mkdirSync(assets, { recursive: true });

const common = {
  bundle: true,
  format: "esm",
  platform: "browser",
  // The demo's targets (web/build.mjs): module workers and top-level await.
  target: ["es2022", "chrome111", "safari16.4", "firefox114"],
  // Not whitespace: esbuild drops the import()'s webpackIgnore and
  // @vite-ignore comments when it minifies whitespace (checked below), and
  // an app's bundler then tries to bundle the viewer's URL.
  minifySyntax: true,
  minifyIdentifiers: true,
  sourcemap: false,
  legalComments: "none",
  logLevel: "warning",
  define: { __NEOSCAD_VERSION__: JSON.stringify(version) },
};

// element.js imports the entry by its dist name, not its sources, so a
// page that loads both gets one copy and one shared engine.
const entryExternal = {
  name: "entry-external",
  setup(build) {
    build.onResolve({ filter: /^\.\/index\.js$/ }, (a) =>
      a.importer.endsWith(join("src", "element.js")) ? { path: "./index.js", external: true } : undefined,
    );
  },
};

await esbuild.build({ ...common, entryPoints: { index: join(root, "src/index.js") }, outdir: dist });
await esbuild.build({ ...common, entryPoints: { element: join(root, "src/element.js") }, outdir: dist, plugins: [entryExternal] });
copyFileSync(join(root, "src/index.d.ts"), join(dist, "index.d.ts"));
copyFileSync(join(root, "src/element.d.ts"), join(dist, "element.d.ts"));

// The bundles must leave the viewer's run-time import to the browser, also
// when an app's bundler reads them (webpack and Vite honour these).
const index = readFileSync(join(dist, "index.js"), "utf8");
if (!index.includes("webpackIgnore") || !index.includes("@vite-ignore")) {
  throw new Error("dist/index.js lost the dynamic import's webpackIgnore / @vite-ignore comments");
}

// --- Assets ------------------------------------------------------------------
const core = arg("core");
if (mock) {
  mkdirSync(join(assets, "core"));
  await esbuild.build({
    ...common,
    entryPoints: [join(repo, "web/src/engine/mock-worker.js")],
    outfile: join(assets, "core/worker.js"),
  });
} else if (core) {
  mkdirSync(join(assets, "core"));
  // package.json in the core's directory only lets node import the glue.
  for (const f of ["worker.js", "neoscad_web.js", "neoscad_web_bg.wasm"]) copyFileSync(join(core, f), join(assets, "core", f));
}
for (const [opt, dir] of [
  ["view", "view"],
  ["view-webgl", "view-webgl"],
]) {
  const from = arg(opt);
  if (!from) continue;
  mkdirSync(join(assets, dir));
  for (const f of ["web_view.js", "web_view_bg.wasm"]) copyFileSync(join(from, f), join(assets, dir, f));
}
for (const [opt, name] of [
  ["fonts", "fonts.tar.gz"],
  ["bosl2", "bosl2.tar.gz"],
]) {
  const from = arg(opt);
  if (from) copyFileSync(from, join(assets, name));
}
if (mock) writeFileSync(join(assets, "MOCK.txt"), "A mock engine (web/src/engine/mock-worker.js), for tests. Not for publishing.\n");

// --- Package files -----------------------------------------------------------
const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
pkg.version = version;
delete pkg.private;
delete pkg.scripts;
delete pkg.devDependencies;
writeFileSync(join(out, "package.json"), JSON.stringify(pkg, null, 2) + "\n");
copyFileSync(join(root, "README.md"), join(out, "README.md"));
writeFileSync(
  join(out, "LICENSE"),
  readFileSync(join(repo, "LICENSE"), "utf8").trimEnd() + "\n\n" + readFileSync(join(root, "LICENSE-EXCEPTION.txt"), "utf8"),
);

console.log(`@neoscad/viewer ${version} -> ${out}${mock ? " (mock engine)" : ""}`);
