// Helpers shared by the tests and the corpus report.

import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

/// A path in the NeoSCAD checkout (four levels up from here).
export function repo(path) {
  return join(here, "../../../..", path);
}

export function fixture(name) {
  return readFileSync(join(here, "fixtures", name), "utf8");
}

/// Every `.scad` file in `dir` (and below it if `recurse`), sorted.
export function scadFiles(dir, recurse = true) {
  const out = [];
  for (const entry of readdirSync(dir).sort()) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) {
      if (recurse) out.push(...scadFiles(path));
    } else if (entry.endsWith(".scad")) out.push(path);
  }
  return out;
}

/// The error nodes of a parse tree, as "line N: ...context...".
export function errorNodes(tree, src) {
  const out = [];
  tree.iterate({
    enter(node) {
      if (!node.type.isError) return;
      const line = src.slice(0, node.from).split("\n").length;
      const context = src.slice(Math.max(0, node.from - 30), node.from + 20);
      out.push(`line ${line}: ${JSON.stringify(context)}`);
    },
  });
  return out;
}
