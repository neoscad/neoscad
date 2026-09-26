# NeoSCAD architecture

NeoSCAD reimplements OpenSCAD — its language, features and test suite — on a
modern stack. It has two first-class users with the same need, a fast
edit→see loop:

- **Humans** in a macOS app first, then a WebAssembly web app.
- **AI coding agents** iterating on models through a headless CLI, a
  long-lived server and an MCP server, with structured, token-cheap output.

OpenSCAD's *behaviour* is the spec (see "Validation"); its code structure is
not a template.

Library choices were checked in `docs/audits/phase0.md`.

## Stack

| Layer | Choice | Reason |
|---|---|---|
| Core | Rust | Native speed, first-class WASM, safe parallelism; one core for CLI, app, web, LSP |
| Parser | Hand-written recursive descent, lossless CST | Error recovery and good diagnostics; the same tree serves the formatter and LSP |
| Evaluator | Tree-walking interpreter → CSG tree, content-hash cache per subtree | A one-line edit re-evaluates only the subtrees it touched |
| 3D kernel | `manifold-rust` (pure-Rust port of Manifold; its parity claims must be checked against the Manifold C API in native test builds) | Same algorithm as OpenSCAD's default backend, with a clean `wasm32` build (the C++ binding needs patches for WASM) |
| 2D kernel | `clipper2-rust` (pure-Rust Clipper2) | OpenSCAD's `offset()` depends on Clipper2's exact arc steps and join types; `i_overlay` differs visibly |
| Text | `crates/text`: `harfrust` (shaping, with `hb_ft`-exact font functions) + `skrifa` (hinted outlines), plus a port of fontconfig name matching over an in-memory font database | No FreeType or fontconfig; portable to WASM. Glyph outlines are byte-identical to the nightly |
| I/O | `crates/io`: STL, OFF, OBJ, 3MF (zip + quick-xml), DXF, SVG (a port of OpenSCAD's libsvg), PDF | `usvg` turns arcs into f32 Béziers and keeps strokes as paint, so it can't reproduce OpenSCAD's `$fn`-dependent flattening or its stroke outlines |
| Renderer | wgpu — Metal on macOS, WebGPU on web, offscreen for snapshots | One renderer for the GUI, the web and agent snapshots |

### Crates

`lang` (lexer, parser, CST/AST, diagnostics) · `eval` (values, builtins,
modules → CSG tree) · `geom` (kernels, extrude, hull, minkowski, offset) ·
`text` (fonts, shaping, outlines) · `io` (import/export; sits below `eval` and `geom`) · `render` (wgpu) · `cli` (the `neoscad`
binary; accepts OpenSCAD's CLI flags so OpenSCAD's tests can drive it,
and hosts `neoscad serve`) · `session` (the long-lived core holding
documents and warm caches; the one API every client uses) · `lsp` ·
`mcp` · `wasm` (wasm-bindgen package) · `conformance` (test harness).

Rule: no rendering or app logic lives in a UI layer. The renderer is Rust;
the app core API is the `session` API, which `neoscad serve` exposes as
JSON-RPC (`docs/serve-protocol.md`).

## Agent surface

- **Fast one-shot CLI.** No GUI toolkit; cold start targets milliseconds.
- **`neoscad serve`** keeps the geometry cache warm so a one-line edit
  re-renders in milliseconds. The CLI, GUI and MCP server are its clients.
  Implemented in 7a: `crates/session` (documents, parse and geometry
  caches with budgets, cancellation) behind JSON-RPC over stdio or a
  per-user Unix socket (`docs/serve-protocol.md`); the command line's
  exports and snapshots use a running server automatically.
- **`--format json` everywhere:** diagnostics with spans and stable codes,
  echo output, timings, geometry stats (volume, bbox, manifold, triangle
  count, component count). Terse by default — never an unrequested mesh dump.
- **Diagnostics that say how to fix,** not only what failed.
- **`neoscad snapshot`:** one contact-sheet PNG (iso/front/top/right) with a
  scale grid, axes and optional dimensions; can highlight parts or show a
  diff against a previous version. Implemented in 6b: `render::snapshot`
  draws the sheet, the CLI adds `--diff` (real booleans) and a JSON
  summary (`docs/cli-json.md`). Since 7a the sheet lives in
  `session::snapshot` and is lit by a camera-relative headlight
  (`render::Lighting`), so faces turned away from OpenSCAD's fixed light
  stay legible; PNG export keeps OpenSCAD's lighting.
- **`neoscad check`** (manifold, minimum wall, overhangs, floating or
  intersecting parts), **`measure`** (bbox, distances, cross-sections),
  **`test`** (assert-based model tests), **`fmt`**, **`docs <builtin>`**.
  `check` and `measure` are implemented in 7b-1 (`session::check`,
  `session::measure` on the analysis mesh and BVH of `session::mesh`;
  JSON in `docs/cli-json.md`), as session operations the server exposes
  as the `check` and `measure` methods; `snapshot --issues` marks the
  findings on the sheet. `test`, `fmt` and `docs` are implemented in
  7b-2, each a session operation and a server method: `neoscad test`
  runs each `module test_*()` of `*_test.scad`/`test_*.scad` files as
  its own model, with `// @expect` lines (volume, bbox, manifold,
  components, check, parts; `docs/model-tests.md`) checked on the
  rendered result by `measure`'s and `check`'s code; `neoscad fmt`
  (`crates/fmt`) lays out the lossless CST and proves on every file that
  only whitespace changed and the `.ast` dump, customizer annotations
  included, is identical; `neoscad docs` answers from
  `crates/docs/builtins.toml` (written for neoscad, kept in step with the
  evaluator's builtins by a test) and from the comment blocks of user
  and library code, BOSL2's structured ones included.
- **MCP server** exposing evaluate, snapshot, check, measure, diff and docs.
- **Named parts:** a `part("lid") { … }` extension behind a flag, so checks
  and measurements can refer to parts. A deliberate divergence from
  OpenSCAD, off by default. Implemented in 7b-1 (`--enable part`,
  `eval::Options::parts`): a part node is a union whose faces keep its
  dotted name through rendering by Manifold original IDs
  (`ManifoldGeometry::tag_part`), and each part's own solid is rendered
  from the warm cache (`session::parts`); `snapshot --highlight` ghosts
  the other parts.

## Apps

- **macOS: hybrid native.** A SwiftUI/AppKit shell (NSDocument autosave and
  versions, window tabs, Quick Look and thumbnails, App Intents, native
  gestures) calls the Rust core through UniFFI. The Rust wgpu viewport draws
  into a `CAMetalLayer`: meshes go to the GPU with no copy across a bridge,
  and the view can run at 120 Hz. The editor is CodeMirror 6 in an embedded
  WKWebView, the same component the web app uses.
- **Web:** the same core compiled to WASM and run in a worker, the same wgpu
  renderer on WebGPU, and CodeMirror 6.
- **Project definition:** the Xcode project is generated from a committed
  declarative spec: Xcode's own JSON project format if Xcode 27 really
  supports one (verify first), otherwise XcodeGen's `project.yml`. The
  `.xcodeproj` is a build output, never hand-edited.
- **Panics:** the core runs inside the app's process, so release builds
  unwind (`panic = "unwind"`) and requests catch panics. A bug in one render
  must not take down the app and its unsaved work. This costs 5–8% on
  evaluation-heavy models.
- **Written twice:** only the thin UI around the editor and viewport (panels,
  customizer, console).

## Validation

### Conformance against OpenSCAD's own suite

OpenSCAD's `tests/CMakeLists.txt` registers 3,258 ctest cases over 522
`.scad` inputs (1,309 are CGAL-only and skipped). The harness evaluates that
CMake file directly into `conformance/manifest.json` and runs the cases in
tiers that follow the build order:

| Tier | OpenSCAD tests | Comparison |
|---|---|---|
| 0 Parse | `astdump`, customizer | Exact text |
| 1 Evaluate | `echo` | Exact text, including number formatting |
| 2 Tree | `dump`, `csgterm` | Exact text |
| 3 Geometry | All 3D/2D render tests (about 1,250 expected PNGs), plus about 90 exact SVG/JSON/export files | We export our mesh and have the pinned nightly render it to PNG, then compare with OpenSCAD's own image tolerance. This checks our geometry against every image test without needing to match OpenSCAD's renderer. It is backed by reference meshes the nightly generates, compared geometrically (volume, area, bbox, topology, Hausdorff distance) |
| 4 Image | Our own wgpu renderer | Showcase set and camera/colour-scheme tests, with a looser perceptual comparison. Matching OpenSCAD's renderer pixel-for-pixel is not a goal |

Echo and warning text must match OpenSCAD word for word: 75 of the 122 echo
files contain warnings. Every diagnostic therefore has a stable code and
OpenSCAD-compatible message text; the richer agent/IDE output (spans, fix
hints) wraps that text rather than replacing it. Numbers print like
`double-conversion` with 6 significant digits (`src/core/Value.cc:62`).

Tier 5 holds the rest (exit-code and harness tests).

`conformance/baseline.json` lists the passing test ids. Every id in it must
keep passing; a change may only add to it.

### Beyond the stock suite

- **Comparison against a current OpenSCAD build** on fuzzed inputs and on
  corpora: `examples/`, MCAD, and BOSL2 (whose own tests are assert-based).
- **Benchmarks:** full render time, CLI cold start, and the time to
  re-render after a one-line edit through `serve`. Every benchmark records
  the same cases on each OpenSCAD build installed, as reference series:
  2021.01 stable (CGAL), and the nightly with both `--backend=cgal` and
  `--backend=manifold`. Results carry the version strings and the machine.
- **Agent-loop metrics:** token size of the default outputs, and later an
  eval where agents perform modeling tasks, recording success rate,
  iterations and tokens compared with OpenSCAD.

### Progress recording

Each `conformance run --record` run (at least once per milestone commit) writes
a snapshot into `progress/`. That directory lives in the repo tree but is
gitignored, so the images never bloat history.

- **Directory name:** `progress/<UTC timestamp>-<short sha>[-dirty]/`, e.g.
  `20260925T184210Z-1220a01/`. It sorts chronologically and ties the snapshot
  to the commit it measured. `-dirty` marks a run on uncommitted changes.
- **`meta.json`:** the full sha, branch, commit subject and timestamp, a dirty
  flag, and the OpenSCAD reference commit. A snapshot is self-describing even
  if it is renamed.
- **`progress/index.jsonl`:** one line appended per snapshot, so the timeline
  can be listed without walking the directories.
- **`scoreboard.json`:** about 5 KB: one status character per manifest test,
  per-tier counts and timings, and failure reasons deduplicated with counts.
  It is pinned to the manifest by its SHA-256.
- **Images are generated on demand,** not at record time:
  `conformance grid [DIR…|--all]` redraws the test grid (one cell per test,
  a 1920×1080 video frame) from the scoreboard plus the committed manifest.
  `--record --grid` writes one immediately.
- **Later:** a fixed showcase set of about 25 models, each rendered as
  OpenSCAD's output, ours and a diff side by side, also generated on demand.

A later script stitches the snapshots in order into a progress video with
ffmpeg.

## Scope

- In: the stable OpenSCAD language, builtins, import/export formats and the
  customizer.
- Deferred: PythonSCAD and OpenSCAD's experimental features.

## Build order

0. **Audit** (done: `docs/audits/phase0.md`).
1. **Skeleton** (done): workspace, the `conformance` harness, and progress
   recording.
2. **`lang`** (done): tier 0.
3. **`eval`** (done): tier 1.
4. **CSG tree** (done): tier 2.
5. **`geom`, `io`, `text`** (done): tiers 3 and 5, 1,103 runnable cases in all. Audited in `docs/audits/engine-milestone.md`.
6. **`render` + `snapshot`** (done): tier 4. 6a: the `render` crate
   (wgpu, OpenSCAD's camera, colour schemes and lighting) and `--render`
   PNG export. 6b: previews (OpenCSG from real booleans on the CSG
   products, throwntogether, `%`/`#`), view options and `neoscad
   snapshot`.
7. **`serve`, JSON output, MCP.** 7a (done): `crates/session`,
   `neoscad serve`, `--format json` everywhere, incremental re-render,
   snapshot lighting and the `edit_loop` benchmark. 7b: `check`,
   `measure`, `test`, `fmt`, `docs`, `part()` (7b-1 done: `part()`,
   `check`, `measure`, snapshot parts and issues, and a server that
   survives a panicking request). 7c: the MCP server and an
   agent-loop eval.
8. **macOS app.**
9. **WASM web app** (deferred by the owner, 2026-09-26). The library crates
   stay WASM-compatible, checked by `scripts/wasm-check.sh`, so it can be
   picked up later.

The agent CLI (phases 1–7) comes before any GUI, because the conformance
harness drives it anyway.
