# NeoSCAD web demo plan

A browser demo of NeoSCAD running in WebAssembly. The code lives in this
repo; it is presented as a public page (`/try`) on the website
(`../neoscad-website`, GitHub Pages at neoscad.org). The UI matches the
macOS app. Planned 2026-09-28.

## Firm ground

- **The pipeline already runs on wasm32.** `crates/wasm-check` exercises
  parse, eval, geometry, preview, `Session` edits, check, measure, fmt,
  test and the LSP through raw exports and an in-memory FS, with MCAD at
  `/neoscad/libraries`.
- **Parallelism is compiled out on wasm32.** The renderer builds for
  WebGPU but has never run in a browser.
- **The editor bundle talks to its host only via `post()` and
  `lspTransport`,** so the host is swappable.
- **Licences:** OpenSCAD's `examples/` are CC0; BOSL2 is BSD-2-Clause.
- **Single-threaded is good enough,** so there are no threads and no
  coi-serviceworker in v1 (it would force a reload on first visit and
  block third-party resources).

Node wasm timings, one run on a shared machine:

| Model | Web (wasm) | Native |
|---|---|---|
| CSG.scad | 14 ms | 7 ms |
| BOSL2 helical spur gear | 71 ms | 47 ms |
| Menger sponge | 315 ms | 126 ms |
| Threaded ring (render) | 4.4 s | 2.8 s |
| Hero gearbox | 4.4 s (670 MB wasm memory) | 3.66 s (threaded) |

**Size:** 37.6 MB of the 48.6 MB `wasm_check.wasm` is DWARF debug info.
Code is 5.3 MB (1.75 MB gzipped) and data 4.8 MB (fonts and MCAD, 2.56 MB
gzipped). The stripped core is about 4.3 MB gzipped, and BOSL2 is 1.0 MB
gzipped. The whole bundle is about 6 MB, far under Pages' limits. Pages
serves `.wasm` as `application/wasm` with `max-age=600`.

## Architecture

- **`crates/client`** (new library, host-neutral): `crates/ffi` can't
  build for wasm32 (uniffi, mimalloc, `std::fs`, `Instant`). Its
  host-neutral glue moves here (document runs, console lines with UTF-16
  ranges, parameter groups, check and measure shaping), as serde structs.
  `ffi` wraps them for Swift, `web` as JSON. This is a pure move; ffi and
  Xcode tests stay green.
- **`crates/web`**: the worker core (wasm-bindgen): one `Session` under
  `Limits::AGENT` (memory 1 GiB), `lsp::Server` with `host_diagnostics`,
  a `MemFs` plus assets, and an 8 MiB stack.
- **`crates/web-view`**: the main-thread WebGPU viewer (surface, camera,
  presets, settings, annotations, `ray_at`, colour schemes).
- **`render::packed`**: `Scene::pack()` produces transferable buffers, and
  `Gpu::upload_packed` consumes them, with a native equivalence test.
- **`web/`**: the esbuild front end, examples, tests and dev server.
- **Threading:** one module worker for evaluation, geometry and the LSP,
  with meshes passed as transferables.
  - Edits are debounced and coalesced (one run in flight).
  - Cancel, a long stale run, a panic or OOM terminates and respawns the
    worker, which gets its buffers back ("engine restarted").
- **No WebGPU:** try wgpu's WebGL2 backend (spike first). If that fails
  too, run without the 3D view; everything else still works.
- **Libraries:** fonts and MCAD are embedded. BOSL2 is a lazy
  `bosl2.tar.gz` fetched on first `include <BOSL2/…>` (included, with its
  licence).
- **Persistence:** `localStorage` for edited examples, settings and
  customizer values, with "Reset example".

## UI (as `DocumentView.swift`: editor over console | viewport | inspector)

In scope:
- the CodeMirror editor, reused with an injectable host, with LSP in the
  worker and go-to-definition into read-only library tabs;
- the viewport with orbit/pan/zoom, the View menu presets and toggles, and
  colour schemes;
- a console with click-to-jump;
- the customizer;
- the check and measure panels, with annotations and picking;
- preview (F5) and render (F6), with the browser's F5 reload prevented;
- STL/3MF/OFF/SVG downloads;
- an example picker.

Deferred: multiple documents, parameter-set files, PNG/DXF/PDF export,
user file import, PWA, a separate LSP worker, threads.

## Examples

| File | Why | Web time |
|---|---|---|
| `examples/Basics/CSG.scad` (default) | booleans | 14 ms |
| `examples/Parametric/sign.scad` | customizer + text | 23 ms |
| `examples/Advanced/GEB.scad` | 2D→3D, projection | 75 ms |
| `examples/Old/example024.scad` | Menger recursion | 315 ms |
| BOSL2 helical spur gear | a library, lazy BOSL2 | 71 ms + 1 MB fetch |
| `web/examples/box-lid.scad` (from the agent-task box/lid) | minkowski, parts, check/measure | – |
| `apple/Icon/concept-c.scad` | colour, sweep, showpiece | 4.4 s |
| `apple/Icon/hero.scad` (labelled heavy) | BOSL2 gearbox + isosurface | 4.4 s, 670 MB |

Excluded: BOSL2 `fractal_tree.scad`, which hits the wasm32 recursion budget.

## Build and hosting

- **Build:** `cargo build --profile web` (no debug, stripped), then
  `wasm-bindgen --target web` (CLI pinned to the lockfile, 0.2.129), then
  `wasm-opt -O3`.
- **Size targets:** core ≤ 4 MB gzipped, view ≤ 2 MB.
- **`scripts/web/build.sh`** writes a self-contained
  `dist/web/neoscad-web-<version>-<sha>/`:
  - index, JS/CSS, both wasm files and their glue, the worker, examples,
    `bosl2.tar.gz`;
  - `THIRD-PARTY-LICENSES.txt`, `SOURCE.txt` (GPL source offer) and
    `build.json`;
  - plus a tarball and `SHA256SUMS`.
- **Every URL is relative** (`new URL('./…', import.meta.url)`), so the
  bundle works under `/try/`.
- **The website** pins a bundle (tag plus sha256) and unpacks it into
  `/try/`. It supplies its own `site.json` (nav) and `theme.css` (CSS
  variables) for a design fit; the bundle ships a thin top bar with a
  link back to neoscad.org.

## Testing

- Node tests for the worker protocol, the examples (summary plus a time
  budget) and respawn.
- Playwright (Chromium and WebKit, served under `/try/`) for the console,
  customizer, downloads, a non-blank canvas or the fallback message, and a
  worker recursion-depth probe.
- `scripts/wasm-check.sh` and the editor tests are kept.

## Risks

- **Worker stack depth** is unmeasured: probe it per browser, curate the
  examples.
- **Long runs block cancel and the LSP:** coalesce and respawn.
- **Panics and OOM trap the instance:** respawn; the memory limit is
  1 GiB.
- **WebGPU missing or the WebGL2 fallback failing:** spike first,
  otherwise no 3D view.
- **Pages compression of `.wasm`** is unverified: fall back to `.gz` plus
  `DecompressionStream`.
- **Wasm maths differs from native:** don't claim output identical to the
  desktop.
- **Wasm memory never shrinks:** respawn after heavy examples.
- **Extracting `client` could regress the Mac app:** keep it a pure move,
  with the tests green.

## Work breakdown (disjoint files)

- **B, render/viewer:** `crates/render/**`, `crates/web-view/**`,
  `scripts/web/build-view.sh`. `packed.rs` goes first.
- **A, core:** `crates/client/**` (new), `crates/ffi/**`,
  `crates/web/**`, `scripts/web/build-core.sh`, `docs/web-protocol.md`
  (written first).
- **C, front end:** `web/**`, the editor host injection
  (`apple/Editor/web/src/{editor.js,bridge.js}`), `scripts/web/build.sh`,
  and the website sync template. It uses a mock worker until A lands.
