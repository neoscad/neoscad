# NeoSCAD architecture

NeoSCAD reimplements OpenSCAD — its language, features and test suite — on a
modern stack. It has two first-class users with the same need, a fast
edit→see loop:

- **Humans** in a macOS app first, then a WebAssembly web app.
- **AI coding agents** iterating on models through a headless CLI, a
  long-lived server and an MCP server, with structured, token-cheap output.

OpenSCAD's *behaviour* is the spec (see "Validation"); its code structure is
not a template.

Items marked *(to verify)* are bets awaiting the phase-0 audit.

## Stack

| Layer | Choice | Reason |
|---|---|---|
| Core | Rust | Native speed, first-class WASM, safe parallelism; one core for CLI, app, web, LSP |
| Parser | Hand-written recursive descent, lossless CST | Error recovery and good diagnostics; the same tree serves the formatter and LSP |
| Evaluator | Tree-walking interpreter → CSG tree, content-hash cache per subtree | A one-line edit re-evaluates only the subtrees it touched |
| 3D kernel | Manifold via its C API *(pure-Rust ports to be benchmarked)* | OpenSCAD's current default backend, so outputs converge; builds to WASM |
| 2D kernel | Clipper2 *(or pure-Rust `i_overlay`)* | Same as OpenSCAD |
| Text | `rustybuzz` + `skrifa`/`ttf-parser` | No FreeType/fontconfig; portable to WASM |
| I/O | STL, OFF, OBJ, 3MF, SVG (`usvg`), DXF, PDF | |
| Renderer | wgpu — Metal on macOS, WebGPU on web, offscreen for snapshots | One renderer for the GUI, the web and agent snapshots |

### Crates

`lang` (lexer, parser, CST/AST, diagnostics) · `eval` (values, builtins,
modules → CSG tree) · `geom` (kernels, extrude, hull, minkowski, offset,
text) · `io` (import/export) · `render` (wgpu) · `cli` (the `neoscad`
binary; accepts OpenSCAD's CLI flags so OpenSCAD's tests can drive it) ·
`serve` (long-lived process holding caches; the one API every client uses) ·
`lsp` · `mcp` · `wasm` (wasm-bindgen package) · `conformance` (test harness).

Rule: no rendering or app logic lives in a UI layer. The renderer is Rust;
the app core API is the `serve` API.

## Agent surface

- **Fast one-shot CLI.** No GUI toolkit; cold start targets milliseconds.
- **`neoscad serve`** keeps the geometry cache warm so a one-line edit
  re-renders in milliseconds. The CLI, GUI and MCP server are its clients.
- **`--format json` everywhere:** diagnostics with spans and stable codes,
  echo output, timings, geometry stats (volume, bbox, manifold, triangle
  count, component count). Terse by default — never an unrequested mesh dump.
- **Diagnostics that say how to fix,** not only what failed.
- **`neoscad snapshot`:** one contact-sheet PNG (iso/front/top/right) with a
  scale grid, axes and optional dimensions; can highlight parts or show a
  diff against a previous version.
- **`neoscad check`** (manifold, minimum wall, overhangs, floating or
  intersecting parts), **`measure`** (bbox, distances, cross-sections),
  **`test`** (assert-based model tests), **`fmt`**, **`docs <builtin>`**.
- **MCP server** exposing evaluate, snapshot, check, measure, diff and docs.
- **Named parts:** a `part("lid") { … }` extension behind a flag, so checks
  and measurements can refer to parts. A deliberate divergence from
  OpenSCAD, off by default.

## Apps

- **macOS: hybrid native.** A SwiftUI/AppKit shell (NSDocument autosave and
  versions, window tabs, Quick Look and thumbnails, App Intents, native
  gestures) calls the Rust core through UniFFI. The Rust wgpu viewport draws
  into a `CAMetalLayer`: meshes go to the GPU with no copy across a bridge,
  and the view can run at 120 Hz. The editor is CodeMirror 6 in an embedded
  WKWebView, the same component the web app uses.
- **Web:** the same core compiled to WASM and run in a worker, the same wgpu
  renderer on WebGPU, and CodeMirror 6.
- **Written twice:** only the thin UI around the editor and viewport (panels,
  customizer, console).

## Validation

### Conformance against OpenSCAD's own suite

OpenSCAD has 522 `.scad` test inputs, about 1,760 expected-output files and
168 test registrations. The harness reads them from the reference checkout
and runs them in tiers that follow the build order:

| Tier | OpenSCAD tests | Comparison |
|---|---|---|
| 0 Parse | `astdump` | Exact text |
| 1 Evaluate | `echo` | Exact text, including number formatting |
| 2 Tree | `dump`, `csgterm` | Exact text |
| 3 Geometry | `export-*`, `render-*` exports | Geometric: volume, area, bbox, topology, Hausdorff distance within tolerance (triangulation may legitimately differ) |
| 4 Image | `preview-*`, `render-*`, camera and colour-scheme tests | Image diff with tolerance |

A committed scoreboard records pass counts per tier along with an
expected-failures list. Every change must shrink that list and never grow it.

### Beyond the stock suite

- **Comparison against a current OpenSCAD build** on fuzzed inputs and on
  corpora: `examples/`, MCAD, and BOSL2 (whose own tests are assert-based).
- **Benchmarks:** full render time against OpenSCAD, CLI cold start, and the
  time to re-render after a one-line edit through `serve`.
- **Agent-loop metrics:** token size of the default outputs, and later an
  eval where agents perform modeling tasks, recording success rate,
  iterations and tokens compared with OpenSCAD.

### Progress recording

Each `conformance --record` run (at least once per milestone commit) writes
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
- **`scoreboard.json`:** per-test results, per-tier counts and timings.
- **A test-grid image** with one cell per test, which shows progress before
  any renderer exists.
- **A fixed showcase set** of about 25 models, each rendered as OpenSCAD's
  output, ours and a diff side by side.

A later script stitches the snapshots in order into a progress video with
ffmpeg.

## Scope

- In: the stable OpenSCAD language, builtins, import/export formats and the
  customizer.
- Deferred: PythonSCAD and OpenSCAD's experimental features.

## Build order

0. **Audit:** verify the bets marked *(to verify)*, map OpenSCAD's test
   registrations to the tiers, and define the showcase set.
1. **Skeleton:** workspace, the `conformance` harness, and progress
   recording.
2. **`lang`:** tier 0.
3. **`eval`:** tier 1.
4. **CSG tree:** tier 2.
5. **`geom` + `io`:** tier 3.
6. **`render` + `snapshot`:** tier 4.
7. **`serve`, JSON output, MCP.**
8. **macOS app.**
9. **WASM web app.**

The agent CLI (phases 1–7) comes before any GUI, because the conformance
harness drives it anyway.
