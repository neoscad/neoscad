// The Rust crates compiled into the web bundle's wasm modules, and their
// licences, for THIRD-PARTY-LICENSES.txt (scripts/web/build.sh). There is
// no cargo-about here, so this reads `cargo metadata` itself:
//
//   cargo metadata --format-version 1 --filter-platform wasm32-unknown-unknown \
//     | node scripts/web/rust-licenses.mjs neoscad-web neoscad-web-view
//
// It walks the resolved graph from the named packages along normal
// dependencies (not build or dev ones: those are not in the module),
// lists every crate that is not NeoSCAD's own with its licence
// expression, and then the licence texts a binary distribution must carry:
//
// - a crate that may be used under MIT (alone, or as one choice of an OR)
//   is taken under MIT, whose notice names its copyright holders, so its
//   MIT text is included (identical texts once); one that also offers
//   Apache-2.0 but ships no MIT file is taken under Apache-2.0;
// - Apache-2.0 alone: the licence text once, plus any NOTICE file;
// - anything else (BSL, BSD, Zlib, Unicode, MPL, ...): every licence file
//   the crate ships.
//
// A crate without the file its licence needs is reported on stderr and
// makes the script fail, so a missing notice is caught at build time
// rather than shipped.

import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";

const roots = process.argv.slice(2);
const meta = JSON.parse(readFileSync(0, "utf8"));
const byId = new Map(meta.packages.map((p) => [p.id, p]));
const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));

const seen = new Set();
const stack = meta.packages.filter((p) => roots.includes(p.name)).map((p) => p.id);
if (stack.length !== roots.length) throw new Error(`not all of ${roots.join(", ")} are in the workspace`);
while (stack.length) {
  const id = stack.pop();
  if (seen.has(id)) continue;
  seen.add(id);
  for (const d of nodes.get(id)?.deps ?? []) {
    if (d.dep_kinds.some((k) => k.kind === null)) stack.push(d.pkg);
  }
}

const own = (p) => p.source === null && /GPL-2\.0/.test(p.license ?? "");
const crates = [...seen]
  .map((id) => byId.get(id))
  .filter((p) => !own(p))
  .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));

/// The OR-alternatives of a licence expression, each a list of AND-ed ids.
function alternatives(expr) {
  const e = (expr ?? "").replace(/\//g, " OR ").trim();
  // Only the forms crates use: `A OR B`, `(A OR B) AND C`, `A AND B`.
  const and = e.split(/\s+AND\s+/).map((part) => part.replace(/[()]/g, "").split(/\s+OR\s+/).map((s) => s.trim()));
  // Distribute: every combination of one choice per AND-ed part.
  return and.reduce((acc, choices) => acc.flatMap((a) => choices.map((c) => [...a, c])), [[]]);
}

const LICENSE_FILE = /^(LICEN[CS]E|COPYING|NOTICE|UNLICENSE)/i;
function files(p) {
  const dir = dirname(p.manifest_path);
  return existsSync(dir) ? readdirSync(dir).filter((f) => LICENSE_FILE.test(f)).map((f) => join(dir, f)) : [];
}

const problems = [];
const texts = new Map(); // text -> [crate names]
const add = (text, who) => {
  const t = text.replace(/\r\n/g, "\n").trim();
  if (!texts.has(t)) texts.set(t, []);
  texts.get(t).push(who);
};
let apache = false;

for (const p of crates) {
  const who = `${p.name} ${p.version}`;
  const alts = alternatives(p.license);
  const fs = files(p);
  const mit = alts.some((a) => a.length === 1 && a[0] === "MIT");
  const apacheChoice = alts.some((a) => a.length === 1 && a[0] === "Apache-2.0");
  const mitFile = mit
    ? (fs.find((f) => /MIT/i.test(f)) ?? fs.find((f) => /Permission is hereby granted/.test(readFileSync(f, "utf8"))))
    : null;
  if (mitFile) {
    add(readFileSync(mitFile, "utf8"), who);
  } else if (mit && !apacheChoice) {
    problems.push(`${who} (${p.license}): no MIT licence file`);
  } else if (apacheChoice) {
    // Apache-2.0's text is generic, so a crate that ships no licence file
    // (some publish none) is covered by the one copy below.
    apache = true;
    for (const f of fs.filter((f) => /NOTICE/i.test(f))) add(readFileSync(f, "utf8"), `${who} (NOTICE)`);
  } else {
    if (!fs.length) problems.push(`${who} (${p.license}): no licence file`);
    for (const f of fs) add(readFileSync(f, "utf8"), who);
  }
}
if (apache) {
  const holder = crates.flatMap(files).find((f) => /APACHE/i.test(f));
  if (holder) add(readFileSync(holder, "utf8"), "Apache-2.0 (the licence text, for the crates above under it)");
  else problems.push("no Apache-2.0 licence text found");
}
if (problems.length) {
  console.error(`rust-licenses: ${problems.join("\n  ")}`);
  process.exit(1);
}

const rule = "-".repeat(72);
const out = [
  `${crates.length} Rust crates (name, version, licence as its Cargo.toml declares it):`,
  "",
  ...crates.map((p) => `  ${p.name} ${p.version}  ${p.license ?? "(none declared)"}${p.repository ? `  ${p.repository}` : ""}`),
  "",
  "A crate offering a choice of licences is used under MIT where MIT is one",
  "of them and the crate ships its MIT notice, else under Apache-2.0. The licence texts and notices follow,",
  "each once, with the crates it covers.",
];
for (const [text, who] of texts) out.push("", rule, `${who.join(", ")}:`, "", text);
process.stdout.write(out.join("\n") + "\n");
