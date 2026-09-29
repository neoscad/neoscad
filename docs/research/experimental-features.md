# Experimental OpenSCAD features: triage for NeoSCAD

Reference: the manifest at `.reference/openscad` 28fe66bc and the
2026.09.23 nightly (2026-09-28). This was research only; nothing was built
or benchmarked. Status lines added since then are marked **Status**.

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
- **NeoSCAD implemented none of them, by design.** `--enable` warns "not
  supported by neoscad" (`crates/cli/src/main.rs`); the experimental
  builtins return `Undef`; `roof` is registered as disabled.
  `docs/architecture.md` lists experimental features as deferred.
  **Status:** several are now implemented; see the Status sections.
- **The object value type now exists** (`Value::Object` in
  `crates/eval/src/value.rs`), for `textmetrics`, `fontmetrics`,
  `is_object`, `object`, `has_key` and `import()`; see Status.

## Per feature

| Feature | Cases (tiers) | Unlocks | Effort | Notes | Recommendation |
|---|---|---|---|---|---|
| predictible-output | 27 (3) | 25 (26 with lazy-union; `export-3mf-stdio` is also disabled upstream) | **S** | Sorts vertices and faces before export (`src/io/export.cc:317-372`), in the STL/OBJ/3MF/POV/OFF/WRL writers. A Python port matched the expected STLs triangle for triangle (STL only checked). **Status: done**, `io::mesh::sorted`; all 25 pass, as exact-file text cases (3MF via `post_process_3mf`). Re-exporting the nightly's own OFF of 65 feature models, neoscad's sorted OFF/WRL/POV match the nightly's byte for byte, OBJ/3MF 63 of 65 (two meshes the tessellator already treats differently, flag or not), STL the same except facet-normal last bits (`docs/followups.md`) | **Done** |
| textmetrics (+`is_object`) | 9 (1–4) | 6 (3 are CGAL-skipped) | M with objects | BOSL2 `path_text(textmetrics=true)`; `crates/text` can supply the metrics | **Done** |
| object-function | 2 (1) | 2 | S after objects | `object()`, `has_key()` | **Done** |
| import-function | 2 (1) | 2 | S after objects | `import()` of JSON: data-driven models, useful for agents | **Done** |
| vector-swizzle | 1 (1) | 1 | S | `v.xy`-style member access (`Expression.cc:385-410`) | **Done** |
| discretization-by-error | 4 (2–3) | 4 | S–M | The `$fe` circle formula, a helix-slice rule, and `$fe` in dumps | Later |
| unicode-identifiers | 2 (1) | 2 | S–M | UAX #31 tables plus NFC | Later |
| lazy-union | 53 (2–4) | 53 | M–L | Top-level children are not unioned: exports carry overlapping shells and the CSG dump changes. Overlaps NeoSCAD's `part()` | Later, **owner decision** |
| roof | 7 (2–4) | 4 | **L** | Voronoi (`boostvoronoi` crate, BSL-1.0) plus a straight skeleton (only thin Rust crates, otherwise a port of CGAL's algorithm); matching CGAL closely enough for the image tests is the risk | Later / maybe never |
| experimental registration (colour OFF/3MF round trip) | 8 (3) | 8 | S? | No flag; colour OFF/3MF I/O exists and may already pass. **Status: done**, all 8 pass unchanged once the manifest runs them | **Done** |
| dbus, VBO indexing, ai-features, python | 0 | 0 | — | GUI, renderer internals, or stubs | Never |

## Other skips worth a second look

- **CGAL-only cases with backend-neutral goldens:** 117 cases (42 distinct
  inputs; 80 cases have no active case on the same input). Some were
  CGAL-only because Manifold mode couldn't import `.nef3`, which NeoSCAD
  now can (`505c10c`). A per-input audit may find cases worth running.
- **Upstream Bugs (63) and disabled-upstream (37):** not examined.

## Not verified

Whether any feature is close to stabilising upstream (shallow checkout, no
issue search); library use beyond BOSL2 and MCAD; WASM and determinism for
the candidate crates. (predictible-output for OBJ, 3MF and POV has since
been checked; see its row.)

## Status: objects, textmetrics, object-function, import-function, vector-swizzle

Built behind their `--enable` flags exactly as the nightly gates them
(`is_object` goes with `textmetrics`); off, each builtin still warns
"Experimental builtin function '...' is not enabled" and returns undef,
and `v.xy` is undef. The flag set is `eval::Features`
(`crates/eval/src/features.rs`), carried by `eval::Options::features`,
`session::Config`/`Run::features`, the command line and `serve`
requests (`"enable": [...]`), `neoscad mcp --enable`, and the app's
`DocumentRequest`/`RunOptions` `enable`.

- All 11 runnable cases pass (tiers 1–4); 3 more are CGAL-only.
- `crates/eval/tests/experimental.rs` compares echo and warning output
  line for line with the nightly for objects (formatting, equality,
  methods and `this`, every `object()` warning), text and font metrics
  across fonts, alignments and directions, JSON (values, key order,
  numbers, nlohmann's error texts), swizzles, and the features off.
- BOSL2 `path_text(..., textmetrics=true)` dumps the same `.csg` as the
  nightly.
- Differences left are in `docs/followups.md` (the `FONT-WARNING` line,
  `import()` dependency tracking, how methods are bound).

## Status: predictible-output and the colour round trips

`predictible-output` is `eval::Feature::PredictibleOutput`, on through the
same `--enable` set as the features above; the exporters read it from the
request's features (`session::Session::export`, `run::encode_settings`).
`io::mesh::sorted` ports `createSortedPolySet`. All 25 runnable cases pass
as exact-file text cases (3MF via `post_process_3mf`), and the eight colour
OFF/3MF round-trip cases, registered EXPERIMENTAL with no flag, pass
unchanged now that the manifest runs them. The manifest skips 67
cases as "experimental feature (…)" and none as "experimental
registration".
