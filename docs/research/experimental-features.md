# Experimental OpenSCAD features: triage for NeoSCAD

Reference: the manifest at `.reference/openscad` 28fe66bc and the
2026.09.23 nightly (2026-09-28). This was research only; nothing was built
or benchmarked.

## Facts

- **115 skipped cases, not 117.** 107 are "experimental feature (…)" in
  `conformance/manifest.json` `skip_reasons`, and 8 are "experimental
  registration": `offcolorpngtest`/`3mfcolorpngtest`, which upstream
  registers as experimental with no `--enable` flag
  (`tests/CMakeLists.txt:1075-1076`).
- **Several features have no cases:** `python` (compiled only under
  `ENABLE_PYTHON`), `input-driver-dbus`, `vertex-object-renderers-indexing`
  and `ai-features`.
- **Everything is off by default in the nightly.** Without `--enable`,
  `textmetrics("a")` warns and returns undef.
- **NeoSCAD implements none of them, by design.** `--enable` warns "not
  supported by neoscad" (`crates/cli/src/main.rs`); the experimental
  builtins return `Undef`; `roof` is registered as disabled.
  `docs/architecture.md` lists experimental features as deferred.
- **There is no object value type** (`crates/eval/src/value.rs`).
  `textmetrics`, `fontmetrics`, `is_object`, `object`, `has_key` and
  `import()` all need one.

## Per feature

| Feature | Cases (tiers) | Unlocks | Effort | Notes | Recommendation |
|---|---|---|---|---|---|
| predictible-output | 27 (3) | 26 (27 with lazy-union) | **S** | Sorts vertices and faces before export (`src/io/export.cc:317-372`), in the STL/OBJ/3MF/POV/OFF/WRL writers. A Python port matched the expected STLs triangle for triangle (STL only checked) | **Now** |
| textmetrics (+`is_object`) | 9 (1–4) | 6 (3 are CGAL-skipped) | M with objects | BOSL2 `path_text(textmetrics=true)`; `crates/text` can supply the metrics | **Now** |
| object-function | 2 (1) | 2 | S after objects | `object()`, `has_key()` | **Now, with textmetrics** |
| import-function | 2 (1) | 2 | S after objects | `import()` of JSON: data-driven models, useful for agents | **Now, with textmetrics** |
| vector-swizzle | 1 (1) | 1 | S | `v.xy`-style member access (`Expression.cc:385-410`) | **Now** |
| discretization-by-error | 4 (2–3) | 4 | S–M | The `$fe` circle formula, a helix-slice rule, and `$fe` in dumps | Later |
| unicode-identifiers | 2 (1) | 2 | S–M | UAX #31 tables plus NFC | Later |
| lazy-union | 53 (2–4) | 53 | M–L | Top-level children are not unioned: exports carry overlapping shells and the CSG dump changes. Overlaps NeoSCAD's `part()` | Later, **owner decision** |
| roof | 7 (2–4) | 4 | **L** | Voronoi (`boostvoronoi` crate, BSL-1.0) plus a straight skeleton (only thin Rust crates, otherwise a port of CGAL's algorithm); matching CGAL closely enough for the image tests is the risk | Later / maybe never |
| experimental registration (colour OFF/3MF round trip) | 8 (3) | 8 | S? | No flag; colour OFF/3MF I/O exists and may already pass | **Check now** |
| dbus, VBO indexing, ai-features, python | 0 | 0 | — | GUI, renderer internals, or stubs | Never |

## Other skips worth a second look

- **CGAL-only cases with backend-neutral goldens:** 117 cases (42 distinct
  inputs; 80 cases have no active case on the same input). Some were
  CGAL-only because Manifold mode couldn't import `.nef3`, which NeoSCAD
  now can (`ff9ae48`). A per-input audit may find cases worth running.
- **Upstream Bugs (63) and disabled-upstream (37):** not examined.

## Not verified

Whether any feature is close to stabilising upstream (shallow checkout, no
issue search); library use beyond BOSL2 and MCAD; WASM and determinism for
the candidate crates; predictible-output for OBJ, 3MF and POV.
