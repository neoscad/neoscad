// Builds the editor bundle for the macOS app's web view.
//
//   node build.mjs            grammar, then dist/editor.js and dist/editor.html
//   node build.mjs --grammar  only the parser (src/lang/parser.js), for the tests
//
// The Lezer grammar compiles to src/lang/parser.js and parser.terms.js
// (gitignored, like dist/): generated code is rebuilt from
// src/lang/openscad.grammar rather than reviewed. They sit beside the
// grammar because the parser imports the grammar's external tokenizer
// (src/lang/tokens.js) by a path relative to itself.
//
// esbuild then bundles everything into one IIFE with no imports left,
// because the page loads exactly one script and the app serves it offline
// (apple/App/Editor/EditorSchemeHandler.swift).

import { buildParserFile } from "@lezer/generator";
import * as esbuild from "esbuild";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(fileURLToPath(import.meta.url));

function writeIfChanged(path, text) {
  let old = null;
  try {
    old = readFileSync(path, "utf8");
  } catch {}
  // Rewriting an unchanged file would bump its time stamp and make the
  // Xcode phase (which compares time stamps) think the output is stale.
  if (old !== text) writeFileSync(path, text);
}

function buildGrammar() {
  const grammar = readFileSync(join(root, "src/lang/openscad.grammar"), "utf8");
  const warnings = [];
  const { parser, terms } = buildParserFile(grammar, {
    fileName: "openscad.grammar",
    moduleStyle: "es",
    warn: (message) => warnings.push(message),
  });
  // A warning is a conflict resolved by accident or an unused rule: treat
  // it as an error, so the grammar stays exactly what it says.
  if (warnings.length) {
    throw new Error("grammar warnings:\n" + warnings.join("\n"));
  }
  writeIfChanged(join(root, "src/lang/parser.js"), parser);
  writeIfChanged(join(root, "src/lang/parser.terms.js"), terms);
}

async function buildBundle() {
  const dist = join(root, "dist");
  mkdirSync(dist, { recursive: true });
  const result = await esbuild.build({
    entryPoints: [join(root, "src/editor.js")],
    bundle: true,
    format: "iife",
    // The app's WebKit (macOS 15 and later, the deployment target).
    target: "safari18",
    minify: true,
    sourcemap: false,
    legalComments: "none",
    metafile: true,
    write: false,
    outfile: join(dist, "editor.js"),
    logLevel: "warning",
  });
  for (const file of result.outputFiles) {
    writeIfChanged(file.path, file.text);
  }
  const html = readFileSync(join(root, "src/editor.html"), "utf8");
  writeIfChanged(join(dist, "editor.html"), html);
  writeIfChanged(join(dist, "THIRD-PARTY-LICENSES.txt"), licenses(result.metafile));
}

/// The license of every package bundled (all MIT today), which the app
/// ships beside the bundle: the minified bundle keeps no license comments.
function licenses(metafile) {
  const packages = new Set();
  for (const input of Object.keys(metafile.inputs)) {
    const m = input.match(/node_modules\/((?:@[^/]+\/)?[^/]+)\//);
    if (m) packages.add(m[1]);
  }
  const parts = [];
  for (const name of [...packages].sort()) {
    const dir = join(root, "node_modules", name);
    const { version, license } = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
    const file = ["LICENSE", "LICENSE.md", "LICENSE.txt"].map((f) => join(dir, f)).find(existsSync);
    const text = file ? readFileSync(file, "utf8").trim() : `License: ${license}`;
    parts.push(`${name} ${version}\n\n${text}\n`);
  }
  return parts.join("\n" + "-".repeat(72) + "\n\n");
}

const grammarOnly = process.argv.includes("--grammar");
buildGrammar();
if (!grammarOnly) await buildBundle();
