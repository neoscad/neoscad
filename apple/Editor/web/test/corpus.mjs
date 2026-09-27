// The grammar against the real parser, over whole corpora:
//
//   npm run corpus [-- DIR...]    default: BOSL2, OpenSCAD's tests,
//                                 examples and MCAD under .reference
//
// Every file NeoSCAD's parser accepts (`neoscad fmt --check --format json`
// reports no error for it; build it with `cargo build --release`) must
// parse here without an error node, or the editor would mark valid code
// as broken. Files the parser rejects are counted apart: error nodes are
// expected there. Files that are not UTF-8 are skipped (the app refuses
// to open them). Prints the counts and the error-node rate, and exits 1 if
// an accepted file has error nodes.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { relative } from "node:path";
import { parser } from "../src/lang/openscad.js";
import { errorNodes, repo } from "./util.js";

const dirs = process.argv.slice(2).length
  ? process.argv.slice(2)
  : [
      ".reference/BOSL2",
      ".reference/openscad/tests/data/scad",
      ".reference/openscad/examples",
      ".reference/openscad/libraries/MCAD",
    ]
      .map(repo)
      .filter(existsSync);

const neoscad = repo("target/release/neoscad");
if (!existsSync(neoscad)) {
  console.error(`corpus: ${neoscad} is missing; run cargo build --release`);
  process.exit(2);
}

const decoder = new TextDecoder("utf-8", { fatal: true });
let accepted = 0;
let rejected = 0;
let skipped = 0;
let bytes = 0;
let ms = 0;
let rejectedWithErrors = 0;
const failures = [];

for (const dir of dirs) {
  let report;
  try {
    report = execFileSync(neoscad, ["fmt", "--check", "--format", "json", dir], {
      maxBuffer: 1 << 28,
      stdio: ["ignore", "pipe", "ignore"],
    });
  } catch (e) {
    // Exit 1 means "some files would change", with the report on stdout.
    report = e.stdout;
  }
  for (const file of JSON.parse(report).files) {
    let src;
    try {
      src = decoder.decode(readFileSync(file.path));
    } catch {
      skipped++;
      continue;
    }
    const t0 = performance.now();
    const tree = parser.parse(src);
    ms += performance.now() - t0;
    bytes += src.length;
    const errors = errorNodes(tree, src);
    if (file.error) {
      rejected++;
      if (errors.length) rejectedWithErrors++;
    } else {
      accepted++;
      if (errors.length) failures.push(`${relative(repo("."), file.path)}: ${errors[0]}`);
    }
  }
}

for (const f of failures) console.log(`error nodes in an accepted file: ${f}`);
const rate = accepted ? ((100 * failures.length) / accepted).toFixed(2) : "0";
console.log(
  `${accepted} files the parser accepts: ${failures.length} with error nodes (${rate}%)\n` +
    `${rejected} files it rejects: ${rejectedWithErrors} with error nodes\n` +
    `${skipped} not UTF-8, skipped\n` +
    `${(bytes / 1e6).toFixed(2)} MB parsed in ${ms.toFixed(0)} ms ` +
    `(${(bytes / 1e3 / ms).toFixed(1)} MB/s)`,
);
process.exit(failures.length ? 1 : 0);
