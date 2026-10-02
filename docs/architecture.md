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
| Evaluator | Tree-walking interpreter → node (CSG) tree, with names resolved ahead of time; per-statement reuse across edits in the session | One engine, exact to OpenSCAD's run-time scoping; see "Evaluator performance" |
| 3D kernel | `manifold-rust` (pure-Rust port of Manifold v3.5.0; OpenSCAD pins v3.5.2), vendored with patches (`vendor/README.md`) | Same algorithm as OpenSCAD's default backend, with a clean `wasm32` build (the C++ binding needs patches for WASM) |
| 2D kernel | `clipper2-rust` 1.2.0 (pure-Rust Clipper2), vendored with a rounding patch (`vendor/README.md`) | OpenSCAD's `offset()` depends on Clipper2's exact arc steps and join types; `i_overlay` differs visibly |
| Text | `crates/text`: `harfrust` (shaping, with `hb_ft`-exact font functions) + `skrifa` (hinted outlines), plus a port of fontconfig name matching over an in-memory font database | No FreeType or fontconfig; portable to WASM. Glyph outlines are byte-identical to the nightly |
| I/O | `crates/io`: STL, OFF, OBJ, 3MF (zip + quick-xml), DXF, SVG (a port of OpenSCAD's libsvg), PDF | `usvg` turns arcs into f32 Béziers and keeps strokes as paint, so it can't reproduce OpenSCAD's `$fn`-dependent flattening or its stroke outlines |
| Renderer | wgpu — Metal on macOS, WebGPU on web, offscreen for snapshots | One renderer for the GUI, the web and agent snapshots |
| Allocator | `mimalloc` (crate `=0.1.52`, which builds mimalloc 3.3.2's C sources through `libmimalloc-sys` 0.1.49 and `cc`) as the global allocator of `neoscad` and the app's core; wasm32 keeps Rust's allocator | 7–15% faster on allocation-heavy models, about a fifth less peak memory (performance audit, O1). OpenSCAD ships it too (`USE_MIMALLOC`, on by default). The one C dependency outside the system frameworks |

### Crates

The workspace is every directory under `crates/` (`Cargo.toml`,
`members = ["crates/*"]`), bottom up:

- **Libraries** (no `std::fs`, `std::env` or clock; WASM-compatible):
  `lang` (lexer, lossless CST, AST, diagnostics, the `FileSystem` trait,
  include fragments) · `io` (import/export formats; below `eval`, which
  reads DXF and imports) · `text` (fonts, shaping, glyph outlines) ·
  `eval` (values, builtins, scoping, name resolution, resource limits,
  the statement memo → node tree) · `geom` (primitives, kernels, CSG
  evaluation, the geometry cache) · `render` (wgpu: OpenSCAD's images,
  previews, snapshots, the app's viewport) · `docs` (builtin reference
  and doc comments) · `fmt` (the formatter; package `neoscad-fmt`, lib
  `scadfmt`) · `assets` (bundled Liberation fonts and MCAD) · `session`
  (the long-lived core every client drives) · `lsp` (the language
  server over a session, transport-agnostic).
- **Hosts** (may touch the platform): `cli` (the `neoscad` binary:
  OpenSCAD's flags plus `serve`, `mcp`, `lsp`, `snapshot`, `check`,
  `measure`, `test`, `fmt`, `docs` and `bench`) · `ffi` (the app's UniFFI
  bridge) · `linux-app` (package `neoscad-linux-app`: the GTK desktop
  app, `docs/linux-app.md`; the window needs its `gtk` feature, the rest
  builds everywhere) · `conformance` (test harness, benchmarks, progress
  video) ·
  `bench-core` (package `neoscad-bench-core`: the benchmark timing, the
  bench kit, the community result schema and the official-release check
  that `neoscad bench` and `conformance bench` share;
  `docs/community-bench.md`).
- **Tooling:** `wasm-check` (a wasm32 build of the pipeline, run in node
  by `scripts/wasm-check.sh`) · `uniffi-bindgen` (the Swift bindings
  generator pinned to `ffi`'s UniFFI, run by
  `scripts/apple/build-core.sh`).

The web demo (phase 9) adds `client` (host-neutral document, check,
measure and export glue shared by `ffi` and `web`), `web` (the worker
core, wasm-bindgen) and `web-view` (the browser viewer, wasm-bindgen);
its front end is `web/` (`docs/web-demo-plan.md`,
`docs/web-protocol.md`).

`client` is the port boundary: whatever a desktop or web front end does
that is not drawing widgets lives there, and each host only wraps it
(`docs/audits/shared-core.md`). Besides the document, check, measure and
export glue it holds the panels' tables and sentences (`present.rs`:
console summary and filter groups, export formats, printer presets and
the check summary, the customizer's snap/clamp/`%g` edit rules, untitled
and parameter-set paths, colour scheme names), the editor's UTF-16 edits
(`text.rs`, converting only through `lang::source`), the 3D view's
overlay for findings, sections, distances and picks (`overlay.rs`), the
document loop's state machine (`document_loop.rs`: debounce, supersede,
in-sync, last mode, parameter pruning; the host passes "now" and keeps
one timer), the file-manager preview's notes and page (`preview.rs`) and
the examples, embedded from `web/examples/` (`examples.rs`). `ffi`
exports them to UniFFI as free functions, synchronous objects
(`EditorText`, `DocumentController` with a `with_foreign`
`DocumentObserver`) and records, which Swift and `uniffi-bindgen-cs`
(the Windows app's C#, pinned to its uniffi 0.32 build;
`docs/windows-app.md`, "Bindings") both take; the web worker sends the tables in `defaults` and the
summary with each run (`docs/web-protocol.md`). What stays in a host:
windows, menus, persistence keys, file watching, the GPU surface, the
editor's web view, timers and watchdogs, and locale-formatted numbers.

Rule: no rendering or app logic lives in a UI layer. The renderer is Rust;
the app core API is the `session` API, which `neoscad serve` exposes as
JSON-RPC (`docs/serve-protocol.md`).

### Clients and the session

One `session::Session` (documents with unsaved text, parse and fragment
caches, geometry caches per renderer, each document's last CSG products
and statement memo, cancellation) sits under every long-lived client:

| Client | Where | Transport | Limits |
|---|---|---|---|
| `neoscad serve` | `crates/cli/src/serve.rs` | JSON-RPC over stdio or a per-user Unix socket (`docs/serve-protocol.md`) | `Limits::AGENT`, `--limit` |
| One-shot CLI | `crates/cli/src/client.rs`, `delegate.rs` | Hands exports, snapshots, `check` and `measure` to a running socket server as `cli.*` requests unless `--no-server`/`NEOSCAD_NO_SERVER`; otherwise runs in-process (`run.rs`, which does not use `Session` for plain exports) | none, as OpenSCAD; `--limit` opts in |
| `neoscad mcp` | `crates/cli/src/mcp/` | MCP over stdio (`docs/mcp.md`); calls serve's `Local` in-process, not a socket | `Limits::AGENT`, `--limit` |
| `neoscad lsp --stdio` | `crates/cli/src/lsp.rs` over `crates/lsp` | LSP over stdio, its own session, debounced diagnostics | `Limits::AGENT`, `--limit` |
| macOS app | `crates/ffi` | UniFFI; the `lsp` server runs in-process per window over the app's session | `Limits::AGENT` (`ffi/src/host.rs`); Quick Look 5 s / 512 MiB |
| Windows app | `crates/ffi` (as a DLL) | UniFFI through uniffi-bindgen-cs's C#; the `lsp` server in-process per window | `Limits::AGENT` (`ffi/src/host.rs`) |
| Linux app | `crates/linux-app` | Rust calls to `client` in-process, runs on a worker thread (`gio::spawn_blocking`, which also catches a panicking run); no language server yet | `Limits::AGENT` (`linux-app/src/host.rs`) |

Every host catches a panicking request (`serve`, `mcp` and `lsp` per
request, `ffi`'s `guarded` per call) and keeps its session; see "Panics".

## Agent surface

- **Fast one-shot CLI.** No GUI toolkit; cold start targets milliseconds.
  The renderer's frameworks (Metal, QuartzCore, CoreGraphics, Foundation)
  are linked delay-init (`crates/cli/build.rs`), so a run that draws
  nothing does not initialize them; this needs a macOS 15 deployment
  target, which `neoscad` and the app both use.
- **`neoscad serve`** keeps the caches warm so a one-line edit
  re-renders in milliseconds. The command line and the MCP server are its
  clients; the app drives the same session API through `crates/ffi`
  (see "Clients and the session"). Across edits the session replays each
  top-level statement whose inputs did not change (`eval::memo`), so an
  edit to one part of a heavy model evaluates only that part, with output
  identical to a full run.
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
  Implemented in 7c: `neoscad mcp` (`docs/mcp.md`), MCP 2026-07-28 over
  stdio with the legacy `initialize` handshake too, eight tools
  (`evaluate`, `render`, `snapshot` with `diff_against`, `check`,
  `measure`, `test`, `format`, `docs`; `test` and `format` listed only
  with `--tool`) on the warm session of `serve`,
  inline source or files, and file access fenced to allowed roots.
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
  WKWebView, the same component the web app uses: `apple/Editor/web`, with
  a Lezer grammar for OpenSCAD, bundled by esbuild and served offline
  through a custom URL scheme. CodeMirror owns editing (selection, undo);
  each change crosses the bridge at once, into the document's copy (which
  NSDocument saves) and the core's (`Core.edit`, in UTF-8 offsets).
  Language features come from `crates/lsp` in-process: the editor's
  `@codemirror/lsp-client` talks JSON-RPC over the same bridge to a
  server per window (sharing the session and its cache of analysed
  library files). Go to definition opens the user's own files as documents
  and library files (BOSL2, the bundled MCAD) read-only in a tab.
- **Windows: the same hybrid** (`windows/`, `docs/windows-app.md`). A
  WinUI 3 shell in C# (Windows App SDK, Fluent controls, Mica) calls the
  same core through UniFFI's C# binding (uniffi-bindgen-cs), loaded as
  `neoscad_ffi.dll`. The wgpu viewport draws with Direct3D 12 into a XAML
  `SwapChainPanel` (`Viewport::attach_swap_chain_panel`, `ffi/src/layer.rs`);
  the editor is the same CodeMirror bundle in WebView2, served under the
  same `neoscad-editor:` scheme and speaking the same bridge protocol;
  the document loop is the core's `DocumentController` with a
  `DispatcherQueueTimer`. Its non-UI host logic (`windows/NeoSCAD.Host`)
  is plain .NET and tested on Linux too. Milestone 1: one window, edit,
  preview, render, export; packaging and the panels are next.
- **Linux: GTK 4 and libadwaita** (`crates/linux-app`,
  `docs/linux-app.md`): a Rust host that calls `client`, `session` and
  `render` directly (no UniFFI). The editor is the macOS app's CodeMirror
  bundle in a WebKitGTK 6 web view, over the same message protocol; the
  document loop is `client::DocumentLoop` with one GLib timer; the wgpu
  viewport (Vulkan, else GL) draws into a texture that GTK composites,
  because GTK 4 gives a widget no native window to put a surface in.
  Milestone 1: editing with live preview, the view, the console, files,
  examples, STL and PNG export, light and dark.
- **The document loop** (8f, `crates/ffi/src/document.rs`,
  `apple/App/Document/DocumentLoop.swift`): each pause in typing (and
  each customizer edit, or change on disk to a file the model read) runs
  the document once, and that one run feeds the whole window. Its
  evaluation's diagnostics go to the language server as soon as it ends
  (`lsp::Options::host_diagnostics`: the server then never evaluates,
  and publishes a run's diagnostics for the editor's version with the
  run's text), before the geometry is built; the geometry stage's
  warnings follow; the model is swapped into the viewport unless a newer
  run started; and the console's lines (with editor positions to jump
  to), the customizer's parameters and the files to watch come back with
  the result. Customizer values run as `-D` assignments after the text,
  which never changes; parameter sets are OpenSCAD's JSON beside the
  model.
- **Panels, export and Shortcuts** (8i, `crates/ffi/src/inspect.rs`,
  `apple/App/Document/Inspect.swift`, `Export.swift`,
  `apple/App/Intents`): the inspector's check and measure panels,
  File > Export and the App Intents call `check`, `measure`, exports and
  snapshots as requests detached from the document loop (no superseding;
  a cancel token of their own), so a check or an export neither cancels
  the live preview nor is cancelled by typing. A measurement keeps its
  solids in the core for sections, distances and picking; findings,
  section outlines and picked points are drawn over the model as the
  viewport's annotations.
- **Web** (phase 9): the same core compiled to WASM and run in a module
  worker (`crates/web`), the same wgpu renderer on WebGPU in the page
  (`crates/web-view`, with a lazily loaded WebGL2 build), and the app's
  CodeMirror 6 editor (`web/`).
- **Project definition:** XcodeGen `project.yml`; the generated `.xcodeproj`
  is a build output, never hand-edited. Xcode's JSON format (`.xcproj`) only
  becomes the default in Xcode 27.2 (beta), and XcodeGen can't emit it yet.
  Revisit when both land (`docs/audits/macos-prep.md`).
- **Written twice:** only the thin UI around the editor and viewport (panels,
  customizer, console).

## Resource limits

OpenSCAD has no limits, so the one-shot command line has none either
unless `--limit NAME=VALUE` is given. Every host that runs code it did
not write — `serve`, `mcp`, `lsp --stdio` and the app — starts from
`Limits::AGENT` (`crates/eval/src/limits.rs`): 60 s, 4 GiB of estimated
memory, 10,000 fragments per primitive and slices per extrusion, 10
million list elements, 64 MiB strings, 10 million `rands()` numbers
and 10 million triangles per result. `session::Config::limits` sets a
session's, and a request's `limits` its own. A trip is an ordinary
result with a `resource-limit` diagnostic, not a crash.

- **Counts** are checked before the allocation they guard, so
  `sphere(10, $fn=1e5)` fails before it builds a vertex.
- **Memory** is an estimate, not a measurement (the workspace forbids
  the `unsafe` a counting allocator needs). Since `88de26e` every list,
  string, range and function value the evaluator creates is charged
  when made and credited when dropped, on one thread-local counter; the
  charge that passes the limit raises the interrupt flag the evaluator
  already polls, and element-wise operators and the printer stop early
  once over, instead of growing to gigabytes. Nodes, messages and in-flight geometry results are
  counted too; kernel working memory and the caches (own budgets) are
  not. A host that can measure also hands the session a `MemoryProbe`
  (`session::Config::memory_probe`), checked against the same limit
  wherever the time is: the web core's is its counting allocator's peak
  for the request (`crates/web/src/heap.rs`, the one `unsafe` there, as
  `GlobalAlloc` requires), so kernel growth the estimate misses stops at
  the next node with a `resource-limit` error instead of trapping the
  instance. Native hosts measure the process (`crates/cli/src/memory.rs`,
  shared by `cli` and `ffi`: the footprint on macOS, the resident set on
  Linux, private bytes on Windows), so in `serve`, `mcp` and the app the
  limit is a budget the process shares. The session reads a probe at
  most every 10 ms, and before a reading over the limit fails a request
  it evicts cached geometry and has the allocator return freed pages,
  then measures again (`session::memory`); the request that fails is the
  one that checked, not necessarily the one that grew. Under the limit a
  probe changes nothing.
- **Time** is checked at evaluator calls and loop iterations and before
  each geometry node; one long kernel operation runs to its end.
- **Recursion** ends with OpenSCAD's "Recursion detected" error, not a
  crash, at a counted limit: `--limit depth=N` (`Limits::depth`, default
  100,000, never off) counts the user modules being instantiated plus
  the user function calls in progress. Statements run on a heap stack
  (`crates/eval/src/heap.rs`), and so do function calls past 8 native
  levels with the expressions around them
  (`crates/eval/src/heap_expr.rs`), so a recursion takes no native stack
  per level and stops at the same depth in every build, profile, target
  and browser (`docs/audits/heap-evaluator.md`; `conformance depth`
  checks it). What still recurses natively is bounded separately
  (`crates/eval/src/recursion.rs`): nesting in the source itself ends
  with OpenSCAD's "Parser error: memory exhausted" past a counted limit
  on the syntax tree's depth, weighted by what each level costs
  (`lang::syntax::parser::NESTING_LIMIT` and `nesting_weight`: about
  5,000 levels of `{` natively, 250 on wasm32, sized for WebKit), and
  the few expression shapes that stay native per level (a range's
  bounds, parameter defaults and the like), and printing deeply nested
  values, stop at a native check.
  Natively that check measures the stack (64 MiB on the evaluation's own
  thread); on wasm32, where the engine's stack cannot be seen, it counts
  frames against a budget calibrated for V8.

Details: `docs/cli-json.md` ("Resource limits"); open gaps in
`docs/followups.md` ("Serve and session").

## Panics

Release builds unwind (`panic = "unwind"` in `Cargo.toml`; Cargo cannot
set it per binary). The core runs inside the app's process, and a
server's warm caches outlive requests, so a bug in one request must not
take down the app with its unsaved work, or a server with every client's
caches.

The cost, measured A/B against `abort` in `docs/audits/unwind.md`: 5–7%
on evaluation-bound BOSL2 models, up to 12% on call-heavy code, nothing
at cold start, about 1% on geometry-bound runs, and 2.5 MB of binary.
After D1 (`a948259`, fewer `Rc<Ctx>` clones per call) the gap is 5–9%;
D2 (drop shims) measured 3–7% slower and was reverted. An `abort` build
for the one-shot CLI alone would mean shipping two binaries, since
`serve`, `mcp` and `lsp` are subcommands of `neoscad`; that is an open
product decision. This supersedes the "0–1.5%" in
`docs/audits/performance.md`.

## Determinism

Output is byte-identical at any thread count; anything parallel needs a
determinism test (`CLAUDE.md`).

- **Geometry** (`crates/geom/src/evaluate.rs`): children render on rayon,
  but Manifold original IDs come from blocks reserved in tree order
  (re-rendered with larger blocks on overflow), messages travel with
  results in child order, and runs sharing an ID are ordered by
  geometry. Small chain-shaped trees skip the pool and must match a
  pooled render at 1, 2 and 8 threads (`crates/geom/tests/render.rs`).
- **Warm equals cold**: a render keeps an earlier render's ID block only
  if it still comes after every block placed before it in tree order,
  and a cache hit's IDs are rebased onto this render's blocks, so a
  warm session's exports match a fresh one's byte for byte whatever it
  rendered before (`crates/session/tests/warm_export.rs`).
- **Cache keys** are exact: a node's key is its own result, including
  a group whose empty sibling still sends its child through a 2D union
  (`5e8ef61`).
- **Evaluation** is single-threaded. Unseeded `rands()` starts from a
  seed the host passes in (`eval::Options::rng_seed`): the command line
  takes it from the clock and process ID, as OpenSCAD does, unless
  `--seed N` fixes it (PGO training does, so its runs repeat; OpenSCAD
  has no such option). Statement reuse
  (`eval::memo`) must give output identical to a fresh evaluation; a
  randomized harness checks it (`crates/eval/tests/incremental.rs`).
  So must call reuse within an evaluation (`eval::callmemo`), which
  every test in `crates/eval/tests/call_memo.rs` runs with the memo on
  and off and compares.
- **Platform:** multiply-adds fuse on aarch64 only, as OpenSCAD's
  builds do (`eval::fma`). wasm32's maths functions differ from macOS
  libm in the last bit, and PNGs differ between GPUs at edge pixels;
  see `docs/followups.md` ("WASM", "Rendering").

## Evaluator performance

One engine, the tree-walker, made cheaper where profiles showed the
cost: allocation, reference counting and context-chain walks rather
than dispatch (`docs/audits/bytecode-vm.md`, "Recommendation").

- **Name resolution ahead of time** (`b4ec6bd`, `eval::resolve`): each
  scope is a region, ordinary variables live in slots, and each
  reference resolves lazily to candidate (region, slot) pairs or a
  pre-looked-up builtin. Lookups still walk the chain but only compare
  region ids and index slots, which keeps OpenSCAD's run-time scoping exact;
  `$` names stay dynamic. −19% to −27% on BOSL2 evaluation.
- **Include fragments** (`9dbb98b`, `lang::fragment`): in the session an
  included file is parsed and lowered once and spliced into each new
  program; a served BOSL2 edit went from 34 to 23 ms.
- **Statement reuse across edits** (`a1b9179`, `eval::memo`): each
  top-level statement is fingerprinted by its AST and text, the
  top-level names it transitively reads, top-level `$` values and the
  options; a match replays its node subtree and messages. Top-level
  assignments always run; `rands`, imports, errors and limits always
  re-evaluate. Budgeted at 128 MiB per memo and 256 MiB per session;
  the one-shot CLI does not use it. A hero carrier edit re-renders in
  294 ms instead of 1,894.
- **Call reuse within an evaluation** (`eval::callmemo`): a user
  module call without children whose inputs repeat replays the subtree
  and messages its first evaluation recorded. The key is the
  definition, the bound frame after argument binding, and the
  definition's context (the main file's by identity, a used library's
  by its variables); the `$` names the call read from outside are
  recorded as it runs and checked at each later call, by value, or by
  shape when only BOSL2's `$transform = $transform * m` pattern read
  them. `rands`, file reads, `part()`, deprecations, errors, `$`-named
  functions and `parent_module()` reaching a caller keep a call (and
  the calls around it) from being kept; a replay is refused deeper in
  the stack than it was recorded, or where a fresh evaluation could
  pass the memory limit. A key is recorded at its second call; 64 MiB
  of entries per evaluation, freed at its end; on in every host, off
  with `--hardwarnings`. `fractal_tree` renders in 0.61 s instead of
  3.4 (`docs/audits/perf-opportunities.md`, P1).
- **The call path** (`cbcf7ed`, `a948259`, `d137102`): a one-flag limit
  check, linear `concat`/`each` accumulation by moving a uniquely held
  accumulator, borrowed contexts on tail calls (D1), and T1–T5 from the
  VM spike (builtin names skip the context walk, pooled argument
  vectors, a positional binding fast path, a direct builtin call, and
  up to 256 recycled contexts): 1.07–1.10× on BOSL2 models.
- **Registers and pure frames** (`37ca8eb`): the spike's register
  analysis and pure-frame rule, ported into the tree-walker. `let`,
  comprehension variables and positional-only calls need no heap context
  when nothing can capture them: 1.08–1.16× on BOSL2 models (the spike
  estimated 1.2–1.35×; the rest needs a second engine). The prototype
  stays on branch `mr/vm-spike`; a second engine was rejected as a
  standing tax on every semantic change (`docs/audits/bytecode-vm.md`).
- Final numbers: `docs/audits/final.md` (about 2.8× the nightly on heavy
  models; 3.7× geometric mean over 14 models including startup).

Geometry-side work is in `docs/audits/performance.md` (O1–O12, each with
its status).

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
  iterations and tokens compared with OpenSCAD. The harness is
  `scripts/agent-eval/run.py` with tasks in `conformance/agent-tasks.json`
  graded by hidden model tests (`docs/agent-eval.md`); 7c ran a pilot.

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
- Deferred: PythonSCAD and most of OpenSCAD's experimental features.
  Implemented behind their `--enable` flags, as in OpenSCAD: objects with
  `textmetrics`, `object-function` and `import-function` (JSON), and
  `vector-swizzle` (`docs/research/experimental-features.md`).

## Build order

0. **Audit** (done: `docs/audits/phase0.md`).
1. **Skeleton** (done): workspace, the `conformance` harness, and progress
   recording.
2. **`lang`** (done): tier 0.
3. **`eval`** (done): tier 1.
4. **CSG tree** (done): tier 2.
5. **`geom`, `io`, `text`** (done): tiers 3 and 5, 1,103 runnable cases
   then. Audited in `docs/audits/engine-milestone.md`; its findings were
   fixed in hardening H1–H3 (`17a31e4`, `f3cd896`, `85eb08d`) and
   `101f8f1`. The baseline is now 1,719 passing cases.
6. **`render` + `snapshot`** (done): tier 4. 6a: the `render` crate
   (wgpu, OpenSCAD's camera, colour schemes and lighting) and `--render`
   PNG export. 6b: previews (OpenCSG from real booleans on the CSG
   products, throwntogether, `%`/`#`), view options and `neoscad
   snapshot`.
7. **`serve`, JSON output, MCP** (done). 7a: `crates/session`, `neoscad
   serve`, `--format json` everywhere, incremental re-render and the
   `edit_loop` benchmark. 7b-1: `part()`, `check`, `measure`, and a
   server that survives a panicking request. 7b-2: `test`, `fmt`,
   `docs`. 7c: the MCP server and an agent-loop eval pilot. Hardened
   after `docs/audits/agent-surface.md` (H4, `3ae32d5`).
8. **macOS app** (done, 8a–8j; plan in `docs/audits/macos-prep.md`).
   8a+8b: XcodeGen project, NSDocument app, `crates/ffi` and the
   `NeoSCADCore` framework. 8c: the Metal viewport. 8d: the CodeMirror
   editor. 8e: the language server (`crates/lsp`, also `neoscad lsp
   --stdio`). 8f: the document loop. 8g: resource limits, already in
   place from H4 (the app runs under `Limits::AGENT`). 8h: Quick Look extensions. 8i:
   panels, export and App Intents. 8j: release plumbing
   (`docs/release.md`; the Developer ID path has not run yet).
9. **WASM web app** (the demo works end to end, 2026-09-29; plan in
   `docs/web-demo-plan.md`). The core in a worker (`crates/web`, protocol
   in `docs/web-protocol.md`), the WebGPU viewer with a lazy WebGL2
   fallback (`crates/web-view`), and the front end (`web/`), bundled by
   `scripts/web/build.sh` and synced into the website's `/try/`. Tested
   by node unit tests and Playwright (Chromium) against the real engine;
   open items in `docs/followups.md` ("Web demo"). The library crates
   stay WASM-compatible, checked by `scripts/wasm-check.sh`.

**Performance** (after phase 8, `docs/audits/performance.md`): its
opportunities O1–O10 are done, O11 and O12 are not (status at the top of
the audit). Evaluator work since: `unwind.md` D1 (done), the VM spike's
T1–T5 (done) and the register/pure-frame port (done, `37ca8eb`); see
"Evaluator performance".

The agent CLI (phases 1–7) comes before any GUI, because the conformance
harness drives it anyway.
