# Phase-0 audit

Date: 2026-09-25. Reference checkout: `.reference/openscad` at
`28fe66bc` (2026-09-23). Nothing was built; Rust is not installed. The only thing run was the OpenSCAD nightly, once, to confirm number formatting (B4).
Outside-world claims come from primary sources (GitHub repos and READMEs
via `gh api`, the crates.io API, docs.rs and the gpuweb wiki), retrieved on
the date above. A library's claims about itself (benchmarks, test parity)
are marked **self-reported**: we read them but did not reproduce them.

## Summary

**On firm ground**

- OpenSCAD master defaults to the Manifold backend
  (`src/glview/RenderSettings.h:12`: `DEFAULT_RENDERING_BACKEND_3D = RenderBackend3D::ManifoldBackend`)
  and ships Clipper2 and Manifold as submodules (`.gitmodules`). Both
  kernel bets therefore converge with OpenSCAD's own output.
- Manifold's C API (`bindings/c/manifoldc.cpp`, CMake flag
  `MANIFOLD_CBIND`) is maintained upstream: there were commits on 2026-09-22
  and 2026-09-13. Upstream is at v3.5.4 (released 2026-09-25).
- wgpu 30.0.1 covers all three rendering uses (details in A3).
- Every major browser now ships WebGPU by default, including Safari 26.
- A current OpenSCAD (nightly 2026.09.23, Manifold backend) is installed
  at `/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD`. It can be
  reinstalled with `brew install --cask openscad@snapshot`.

**Where `docs/architecture.md` is wrong or needs changing (by importance)**

1. **Tier 3 ("Geometry: volume, bbox, Hausdorff…") has almost no stock
   ground truth.** OpenSCAD's suite checks geometry by rendering it to PNG.
   The expected outputs are 1,251 PNGs out of 1,761 files. The only
   exact-geometry files are 7 STL, 4 OBJ, 3 3MF, 1 OFF and 10 POV. All of
   them come from tests marked `EXPERIMENTAL` with
   `--enable=predictible-output` (`tests/CMakeLists.txt:972-996`), and they
   are compared as exact text, not geometrically. Tier 3 therefore needs
   reference meshes produced by a pinned OpenSCAD snapshot (differential
   testing), or it has to be folded into tier 4. See B3.
2. **Tier 4 needs OpenSCAD's renderer, not just a tolerant diff.**
   `image_compare.py` fails the test if even one 3×3 block has every pixel
   off by 8 or more in the same direction (`tests/image_compare.py:7,28-37,70-74`).
   To pass, NeoSCAD must reproduce OpenSCAD's camera, lighting, colour
   scheme and 512×512 framing almost exactly. **Decision for the owner:**
   either build an "OpenSCAD-compatible" render mode in wgpu, or run the
   stock image tests by exporting our mesh and rendering it with the pinned
   OpenSCAD. That is how OpenSCAD's own `render-stl`/`render-off` tests
   work (`tests/export_import_pngtest.py`). The second option separates
   geometry conformance from renderer work and unblocks tier 4 before
   phase 6.
3. **Tier 1 is mostly about diagnostics, not numbers.** 75 of the 122
   expected `.echo` files contain `WARNING`, `ERROR` or `TRACE` lines:
   1,965 WARNING lines against 3,566 ECHO lines. The wording has to match
   byte for byte, including `in file X, line N`. That conflicts with the
   "diagnostics that say how to fix, with stable codes" goal unless
   diagnostics have an **OpenSCAD-compatible text mode** separate from the
   rich/JSON mode. **Decision for the owner.**
4. **Use `harfrust`, not `rustybuzz`.** The rustybuzz README says: "This
   project is not developed further, unmaintained, and archived… switch to
   HarfRust". Its last crate release was 0.20.1 on 2024-11-12.
5. **A pure-Rust Manifold port exists and is linked from Manifold's own
   README:** `manifold-rust` 0.13.1. The "benchmark pure-Rust ports" item
   should name it rather than boolmesh or csgrs. See A1.
6. **Pure-Rust Clipper2 exists:** `clipper2-rust` 1.2.0. i_overlay's offset
   geometry is not Clipper2's, and OpenSCAD's `offset()` depends on
   Clipper2's exact arc-step formula. See A2.
7. Minor: the architecture doc's counts are right for `tests/data/scad`
   (522 `.scad`) and `tests/regression` (1,761 files). The registration
   count is 165 `add_cmdline_test` + 7 `add_failing_test` + 14
   `add_output_file_test`. Expanded per file, these produce roughly 3,000
   ctest cases (B1).

**Unverified**

- Every performance and parity number from manifold-rust, clipper2-rust,
  boolmesh and manifold-csg (all self-reported).
- Whether headless Metal/wgpu works on GitHub-hosted macOS runners.
- 120 Hz presentation via wgpu on a SwiftUI-hosted layer.
- How much FreeType hinting changes OpenSCAD's glyph outlines (A5).

---

## A. Stack bets

### A1. 3D kernel: Manifold from Rust

| Option | Status (retrieved) | WASM | Notes |
|---|---|---|---|
| Upstream C API `manifoldc` | In the repo at `bindings/c`, built with `MANIFOLD_CBIND=ON`. Active: last commits 2026-09-22 "Remove tolerance and simplify epsilon (#1836)" and 2026-09-13 | Emscripten is first-class upstream. README: `MANIFOLD_NO_IOSTREAM` "useful for freestanding/embedded builds (e.g., `wasm32-unknown-unknown`)" | The C API is still changing (tolerance APIs were removed in September), so pin a version |
| `manifold-csg` / `manifold3d` crates (zmerlynn) | 0.4.1, 2026-09-09. Wraps the full C API. Manifold's README lists it as the Rust binding. 20 stars, one maintainer | README: "Tested on … `wasm32-unknown-emscripten`, and `wasm32-unknown-unknown`". The bare-wasm target is "**provisional**": it needs the `unstable-wasm-uu` feature, patches to manifold and Clipper2, a libc++ shim (`wasm-cxx-shim`) and LLVM 20+. It has **no exception runtime ("implicit STL throws abort")** and runs single-threaded | Needs cmake and a C++ toolchain at build time |
| `manifold-rs` (WilstonOreo) | 0.7.0, 2026-02-24 | — | Older, less active; not recommended |
| **`manifold-rust`** (larsbrubaker / MatterHackers) | 0.13.1, 2026-08-10. Repo created 2026-03-29; 25 stars. Linked from Manifold's README as a "native Rust port … passes Manifold's full test suite with identical results" | Pure Rust, so plain wasm-bindgen works. Live WASM demo. "WASM build is single-threaded" | **Self-reported:** a port of C++ **v3.5.0** targeting "exact numerical match … same triangle topology"; 730 tests passing; "at parity with the sequential C++ build" (sphere-minus-sphere at 2M triangles: 2.61 s C++ vs 2.57 s Rust; union of a 7,999-sphere grid: 13.5 s vs 14.5 s; machine not stated). Adds a robust exact-arithmetic engine for soup input. Risks: one maintainer, five months old, already behind upstream (v3.5.4) |
| `boolmesh` | 0.1.10, 2026-09-18, 31 stars. "Inspired by" Manifold and written from scratch | Pure Rust | Self-reported: depth-4 Menger sponge takes about 8 s single-threaded on an M4. No comparison with Manifold, no hull/minkowski/extrude, and it recently *removed* primitives. Not a replacement |
| `csgrs` | crates.io 0.20.1 (2025-07); the repo is being rewritten on `hyperreal` exact scalars (pushed 2026-09-26). 255 stars | Pure Rust | BSP-based, API in flux, not Manifold-compatible output. Not suitable |

**Verdict: go, with a caveat.** Put the kernel behind a `geom::Kernel`
trait. Start with **manifold-rust** because it is pure Rust, gives a clean
`wasm32-unknown-unknown` build and claims bit-parity with C++. Keep
**manifold-csg (C API)** as the reference and differential oracle on
native targets: run both in CI and diff volume and triangle count. If
manifold-rust stalls, switch the native build to the C API and the web
build to emscripten or manifold-csg. Avoid building the C++ for
`wasm32-unknown-unknown` in the main path: it is provisional upstream and
lacks exceptions.

### A2. 2D kernel: Clipper2 vs `i_overlay`

What OpenSCAD needs, from its source:

- `offset(r)` uses the Round join; `offset(delta)` uses Miter with
  `miter_limit` fixed at 1e6; `chamfer=true` uses Square
  (`src/core/OffsetNode.cc:57-69`, `OffsetNode.h:27`).
- The arc tolerance is `|delta|·(1−cos(180°/n))`, where `n` comes from
  `$fn/$fa/$fs`, which reproduces Clipper's step formula
  (`src/geometry/GeometryEvaluator.cc:617-621`).
- Coordinates are scaled to int64 by `scaleBitsFromPrecision()`
  (`src/geometry/ClipperUtils.cc:337-350`).
- 2D `minkowski()` also goes through Clipper (`ClipperUtils.cc:355-374`).

The number and placement of vertices on rounded corners follows directly
from Clipper2's algorithm.

| Option | Status | Offset | WASM |
|---|---|---|---|
| `clipper2` (tirithen; wraps C++) | 0.6.0, 2026-05 | yes | needs a C++ toolchain |
| **`clipper2-rust`** (larsbrubaker) | 1.2.0, 2026-09-18. Self-reported: "complete, pure Rust port", "Exact behavioral match with C++ on all test cases", 457 tests, `#![forbid(unsafe_code)]` | Miter, Square, Bevel and Round; Minkowski sum/diff | pure Rust; WASM demo |
| `i_overlay` | 9.0.0, 2026-09-19. Very widely used (8.7M downloads) | "Bevel, clipped miter, and round joins". Round arcs use **CORDIC subdivision**, and integer mode clips miters sharper than **5°**. There is no chamfer-square join, and arcs are not parameterised the way Clipper's `ArcTolerance` is | pure Rust |

**Verdict: swap to `clipper2-rust`**, with the same trait-plus-oracle
pattern as A1: test it against the C++ `clipper2` crate on native targets.
i_overlay is robust for booleans but would give different rounded-offset
vertices and has no `chamfer` join, so `offset()` output would diverge
visibly at low `$fn`.

### A3. wgpu (30.0.1, 2026-08-22)

- **(a) Rendering into a `CAMetalLayer` in a SwiftUI/AppKit view: go.**
  The supported path is to pass the `NSView*` across the FFI as a
  `RawWindowHandle::AppKit` and call `Instance::create_surface_unsafe`.
  wgpu-hal then attaches a layer using
  `raw_window_metal::Layer::from_ns_view` (`wgpu-hal/src/metal/mod.rs:159-168`).
  There is also `metal::Instance::create_surface_from_layer(&CAMetalLayer)`
  (`mod.rs:144`) if the Swift side owns the layer. Neither is exposed as a
  `SurfaceTargetUnsafe` variant (docs.rs 30.0.1 lists only `RawHandle` and
  `Drm`). "120 Hz" (driving frames from `CADisplayLink`) is unverified.
- **(b) Headless rendering to PNG on macOS: go.** The upstream example
  `examples/features/src/render_to_texture` renders with no surface, then
  runs `copy_texture_to_buffer`, `map_async` and writes a PNG. The caveat is
  that it needs a Metal device. Whether GitHub-hosted macOS runners provide
  a usable one is **unverified**; plan a Linux CI lane on Vulkan
  (lavapipe) for snapshots.
- **(c) WebGPU in browsers: go.** From the gpuweb wiki "Implementation
  Status" (edited 2026-08-13):
  - Chrome/Edge 113+ on macOS, Windows and ChromeOS; Android 121+; Linux
    only for Intel Gen12+ (144) and NVIDIA on Wayland (147).
  - Firefox 141 on Windows, and on Apple-silicon Macs from 145 (macOS 26)
    or 147 (all macOS versions).
  - **Safari 26 on macOS, iOS, iPadOS and visionOS**, enabled by default.
  - Gaps: most Linux Chrome setups, and Firefox on Linux and Android.

  wgpu also targets WebGL2 as a fallback (wgpu README).

### A4. UniFFI (0.32.2, 2026-09-23): go, with a caveat

- The README says "ready for production use, but … a long way from a 1.0
  release"; advanced features may break across upgrades. Pin the version.
- Large buffers: records, sequences and strings are **serialized into a
  `RustBuffer`** in both directions (`docs/manual/src/internals/lifting_and_lowering.md`).
  `Vec<u8>` copies once. `Vec<f32>` is lowered element by element, so avoid
  it for meshes.
- A borrowed `&[u8]` is zero-copy, but only from foreign code to Rust, only
  as an argument, and never nested (`docs/manual/src/types/bytes.md`).
- The architecture already keeps meshes in Rust (the renderer is Rust), so
  only source text, JSON results and PNG thumbnails cross the bridge.
  **Rule:** never return geometry through UniFFI. Return `Vec<u8>` for
  thumbnails, and pass handles, not data.

### A5. Text without FreeType/HarfBuzz

- **Shaping:** use `harfrust` 0.13.3 (2026-08-25). It "Matches HarfBuzz
  v14.3.1" and mostly passes HarfBuzz's own tests, with known issues listed
  in its `HARFBUZZ.md`. rustybuzz is archived. **Swap.**
- **Outlines:** use `skrifa` 0.47.0 (2026-09-08, fontations, which is what
  harfrust builds on). `ttf-parser` is not archived but has not had a
  release since 0.25.1 (2024-11). Use skrifa so there is one parser.
  **Go.**
- **Font discovery:** OpenSCAD uses fontconfig patterns as part of its
  language, for example `"Liberation Sans:style=Bold"`,
  `":charset=76,78"`, `"Amiri:style=Regular"` (found in 36 test and example
  files). Tests point `OPENSCAD_FONT_PATH` at `tests/data/ttf`
  (`test_cmdline_tool.py:340-343`), which holds Liberation 2.00.1, Amiri,
  marvosym and EvenOddTT. NeoSCAD needs its own fontconfig-pattern parser
  and matcher over a `fontdb` (0.24.0) database, and the Liberation fonts
  bundled for the web build. This is not free; budget for it.
- **Fidelity traps to keep in mind:**
  - OpenSCAD calls `FT_Set_Char_Size(face, 0, 1e5, 100, 100)` and
    `FT_Load_Glyph(..., FT_LOAD_DEFAULT)`, so outlines are *hinted* at
    about 2,170 ppem (`src/core/FreetypeRenderer.cc:302,432`).
  - Shaping advances come from `hb_ft_font_create`, which gives FreeType's
    advances (`:378`).
  - Curves are flattened into `fn` segments per Bézier by OpenSCAD itself
    (`src/core/DrawingCallback.cc:107-120`), so that part is portable.
  - The effect of hinting at that ppem should be well under a pixel at
    512², but this is **unverified**.

**Verdict: go, with a caveat** (harfrust + skrifa + fontdb + a custom
fontconfig-pattern matcher).

### A6. A current OpenSCAD on macOS

- `brew install --cask openscad@snapshot` gives version 2026.09.23 from
  `https://files.openscad.org/snapshots/OpenSCAD-2026.09.23.dmg`, requires
  macOS 12+, and links `openscad` onto PATH. It conflicts with the
  `openscad` cask.
- The stable `openscad` cask is 2021.01 and has been **disabled since
  2026-09-01** because it fails Gatekeeper.
- Snapshots are built with `-DSNAPSHOT=ON -DEXPERIMENTAL=ON`
  (`scripts/release-common.sh:132`), so experimental tests can run too.
- **Installed:** a 2026.09.23 nightly is now at
  `/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD`. `--version` prints
  `OpenSCAD version 2026.09.23`, and `--backend=manifold` works (confirmed
  by the coordinator). Use this path as the differential oracle.
- **Recommendation:**
  - Pin the snapshot version in the repo (`tools/openscad-version`).
  - Record `openscad --info` in each scoreboard.
  - Keep the snapshot date and the `.reference/openscad` commit in step.
- The installed `/Applications/OpenSCAD-2021.01.app` predates Manifold.
  Don't use it.

---

## B. Test-suite map

### B1. How tests run

`add_cmdline_test` (`tests/cmake/TestFunctions.cmake`) registers one ctest
per input file, named `<group>_<basename>`. Each one runs:

    python test_cmdline_tool.py --comparator=image_compare -c <py> -s <suffix> [-e <expecteddir>] [-k kernel] [--stdin --stdout] -t <group> -f <basename> <openscad|script> <file.scad> [2D camera] [ARGS] -o <output>/<group>/<basename>-actual.<suffix>

- The output format is chosen by the `-o` extension: `.ast`, `.echo`,
  `.csg`, `.term`, `.png`, `.stl`, `.json`, and so on
  (`test_cmdline_tool.py:333-335`).
- Expected files are
  `tests/regression/<expecteddir or group>/<basename>-expected.<suffix>`
  (`:59-71`).
- Files classed as 2D get
  `--camera=0,0,100,0,0,0 --viewall --autocenter --projection=ortho`
  (`TestFunctions.cmake`, `is_2d`).
- The environment sets `OPENSCAD_FONT_PATH=tests/data/ttf` and
  `OPENSCADPATH=libraries` (MCAD).

Comparison depends on the suffix (`compare_<suffix>`, else
`compare_default`, `:269-273`):

- **Text** (`compare_default`, `:171-184`): exact line equality after these
  steps:
  - strip `, timestamp = N`;
  - normalise CRLF and trim;
  - replace the build source path with the runtime one in the expected
    file;
  - drop lines matching `OPENSCAD_TEST_EXCLUDE_LINE`. Globally this is only
    `^TRACE:\s*\*\*\* Excluding \d+ frames \*\*\*` (`CMakeLists.txt:203`).

  Float truncation is **commented out** (`:118-126`), so numbers must
  match exactly.
- **JSON** (`compare_json`): structural equality.
- **PNG** (`compare_png`, `image_compare.py`):
  - per-channel differences under 8 are zeroed;
  - a 3×3 block counts only if all 9 pixels differ in the same direction;
  - the test **fails if any such block exists** (`perc_diff == 0`
    required);
  - `USE_IMAGE_COMPARE_PY` is ON by default (`CMakeLists.txt:12`).
  - The ImageMagick fallback allows fewer than 32 error pixels after an
    8% threshold and an erode (`test_cmdline_tool.py:236-256`).
- **3MF** output is normalised first: UUIDs, dates and namespaces
  (`:280-296`).
- **Export → import tests** (`export_import_pngtest.py`): export with
  `--render=force`, write `import("file")` into a new `.scad`, render that
  to PNG and compare. **PDF** goes through Ghostscript to PNG
  (`export_pngtest.py`).
- **Should-fail tests** (`shouldfail.py`) check only the return code
  (`--retval=1`).

ctest configurations: tests tagged `Examples`, `Bugs` or `Heavy` are
**not** in the default `ctest` run (`cmake/EnforceConfig.cmake` forces
`Default`). Tests flagged `EXPERIMENTAL` are only registered when the build
has `-DEXPERIMENTAL=ON`, which OpenSCAD CI and snapshots use. There is also
a large list of disabled tests (`CMakeLists.txt:1343-1488` and
`1516-1554`).

### B2. Tiers

Counts come from simulating the file lists in Python (`CMakeLists.txt`
lines 229-723), so they may be off by ±3 because of generated files
(`include-tests`, `use-tests`, `import_{stl,3mf,dxf}-tests`,
`issue2342`). "Ex" means examples in the Examples configuration.

| Tier | Groups (flags) | Cases | Comparison | Expected dir |
|---|---|---|---|---|
| **0 Parse** | `astdump` (`-o x.ast`): MISC_FILES plus 13 function/syntax files; `astdump-stdio` | 28 + 1 | exact text | `astdump/` (28) |
| **1 Evaluate** | `echo` (`-o x.echo`): ECHO_FILES plus 3 flag variants (`--trace-usermodule-parameters=false`, `--check-parameter-ranges=on`, `--quiet`); `echo-stdio`; `echo-param`/`-paramset` (`-p json -P set`); `echo-cli-view-variables-{6,7}` (`--camera`); `openscad-override` (`-D a=3;`) | about 117 | exact text, **including WARNING/ERROR/TRACE lines** | `echo/` (122), `echo-*` |
| **2 Tree** | `dump` (`-o x.csg`): FEATURES_2D, FEATURES_3D, MISC; `dump-examples` (Ex, 49); `csgterm` (`.term`, 3); `export-csg-nonascii`; `dump-cli-view-variables-{6,7}` | about 126 + 49 + 6 | exact text | `dump/` (167), `dump-examples/` (56), `csgterm/` (3) |
| **3 Geometry** (stock) | `export-stl`, `-binstl`, `-obj`, `-3mf`, `-pov` (**all EXPERIMENTAL, `--enable=predictible-output`**); `export-param` (JSON); `export-svg`, `-fill-stroke`, `-fill-only` (import then re-export SVG, text); `export-stl-sanitytest` | about 20 exact-file, 69 SVG, 2 JSON | exact text (sorted or remeshed by predictible-output) | `export-*/` |
| **4 Image** | `render-{cgal,manifold}` (`--render --backend=…`), `render-force-*` (`--render=force`), `preview-{cgal,manifold}`, `throwntogether-*` (`--preview=throwntogether`), `render-monotone` and the export→import PNG tests (`render-{stl,off,obj,3mf,dxf,svg,csg}-*`, `preview-{stl,off,obj,3mf}`), `export-pdf*`, camera (`--camera`, `--viewall`, `--projection=o`, `--imgsize`; 22), `openscad-colorscheme-*` (7, Ex), view options (`--view axes,scales,edges,crosshairs`; 8), `svgviewbox-*` (14), `svgimport` (2) | about 2,600 (render-manifold 306, preview-manifold 323, throwntogether-manifold 294; the same again for CGAL; render-csg-cgal 306; render-dxf 79; render-svg 71; …) of which about 340 are Ex and about 150 Bugs | `image_compare.py` | `render/` (328), `preview/` (331), `throwntogether/` (288), `render-monotone/` (83), `render-manifold/` (33), `render-cgal/` (32), … |
| **Other** | `customizer` (`.ast`, 6), `customizer-{first,wrong,incomplete,imgset,setNameWithDot}` (`-p … -P …`, 5); `add_failing_test`: `stlfailedtest`, `offfailedtest`, `parsererrors` (4 plus 1 experimental), `hardwarnings`, `export-param{,set}-hardwarnings` (return code only); `relative-output_*` (14 formats × run/check); `pretty_print_logfile` (harness) | 11 / about 10 / 28 | ast text / exit code / file exists | `customizer*/` |

**Mapping tiers to NeoSCAD phases.** Target only the `*-manifold` and
backend-neutral variants.

- `render-manifold` compares against `render/`, except for coloured files
  (`RENDER_DIFFERENT_EXPECTATIONS`, `CMakeLists.txt:607-627`), which use
  `render-manifold/`.
- `preview-manifold` compares against `preview/`.
- `customizer` belongs with tier 0 because its output is AST.

### B3. Things that trip the harness

- **CGAL-only tests (exclude them):**
  - every `*-cgal` group: `render-cgal`, `render-force-cgal`,
    `render-csg-cgal`, `preview-cgal`, `throwntogether-cgal`,
    `render-off-cgal`, `render-3mf-cgal`;
  - `nef3/*.scad` ("Nef3 import not supported in Manifold mode",
    `:651`);
  - `FILES_CGAL_CORNER_CASES` (`:718`);
  - the expected dirs `render-cgal/` and `preview-cgal/`;
  - the `relative-output_{nef3,nefdbg}` formats.
- **Manifold-specific:**
  - `render-force-manifold-hardwarnings` (`--hardwarnings`: no warnings
    allowed, 6 files);
  - `FILES_MANIFOLD_CORNER_CASES` (22 touching or self-touching meshes,
    `:692`);
  - `render-manifold_color-tests` and `_hex-colors-tests` are disabled
    because "Manifold triangle order not stable" (`:1433-1436`).
- **Fonts:** 36 files use `text()`. They need the bundled TTFs and
  fontconfig-style names (A5). Text tests are 2D and so run under the ortho
  top-down camera.
- **Experimental features (deferred by scope; exclude them):**
  - `object-function` (2), `import-function` (2), `textmetrics` (3 files
    across 8 groups), `vector-swizzle`, `unicode-identifiers` (3),
    `discretization-by-error` (2 × 2), `lazy-union` (13 × 6 groups), `roof`
    (`examples/Basics/roof.scad`, 7 groups);
  - **all exact-file geometry exports** (`predictible-output`). Moving
    geometry exports out of tier 3's stock checks is finding 1.
- **Renderer-specific, not geometry:**
  - `throwntogether-*`, `preview-*` (OpenCSG image-space CSG with `#`/`%`
    highlight and transparency), `--view edges` and `axes,scales`;
  - several of these are disabled for z-fighting or the Apple dFdy bug
    (`:1350-1382`).
- **Stateful/CLI:** `$vp*` from `--camera`, `-D` overrides, `-p/-P`
  parameter sets, `--export-format`, stdin/stdout (`-`), `--hardwarnings`,
  `--check-parameter-ranges`.

### B4. Echo number formatting (tier-1 trap)

`src/core/Value.cc:62-179` formats every double with Google
double-conversion `ToPrecision`:

- 6 significant digits (`DC_PRECISION_REQUESTED 6`).
- Flags `UNIQUE_ZERO | EMIT_POSITIVE_EXPONENT_SIGN`.
- `max_leading_padding_zeroes = 5`, `max_trailing_padding_zeroes = 0`,
  exponent character `e`.
- After that, `trimTrailingZeroes` removes trailing fractional zeros and a
  bare `.`, including before an exponent.
- `inf`, `-inf` and `nan` are special-cased.

Consequences, confirmed in `tests/regression/echo`:

| Value | Printed as |
|---|---|
| 100000 | `100000` |
| 1000000 | `1e+6` (not `%g`'s `1e+06`) |
| 1e-5 | `0.00001` |
| 1e-6 | `1e-6` |
| −0.0 | `0` (UNIQUE_ZERO) |
| 2/3 | `0.666667` |
| −4.2/… | `-0.47619` |
| large values | `-1.38143e+307` |

- The exponent has no zero padding.
- This differs from Rust's `{}` and `{:e}` and from C `%g` (padding, and
  the switch point, which is exponent ≥ 6 or < −5 with 5 leading zeros
  allowed).
- Confirmed by running the installed 2026.09.23 nightly on
  `echo(100000, 1000000, 1e-5, 1e-6, -0, 2/3, 1/3*1e22, 123456789, 0.1+0.2);`,
  which printed
  `ECHO: 100000, 1e+6, 0.00001, 1e-6, 0, 0.666667, 3.33333e+21, 1.23457e+8, 0.3`.
- It is **not** used everywhere: some warnings use printf `%f`, for example
  `clamping to 0.010000` (`regression/echo/errors-warnings-included-expected.echo:32`).
  Each message's formatter has to be ported separately.
- Implement it as a dedicated `fmt_openscad_number()` (a Rust port of
  `ToPrecision` with these parameters), and test it against every number in
  `regression/{echo,dump,astdump}`.

---

## C. Showcase set (26 models)

The ground truth is the Manifold-backend render image (`--render`, 512×512,
default camera; 2D files use the ortho top camera). Every path below was
checked to exist. Paths are relative to `.reference/openscad/`. A preview
image also exists at `tests/regression/preview/<name>-expected.png` for all
of them except example009.

| # | Input | Expected PNG | Shows |
|---|---|---|---|
| 1 | `examples/Basics/logo.scad` | `tests/regression/render/logo-expected.png` | CSG difference, the OpenSCAD logo |
| 2 | `examples/Basics/CSG.scad` | `tests/regression/render/CSG-expected.png` | union, intersection, difference |
| 3 | `examples/Basics/CSG-modules.scad` | `tests/regression/render-manifold/CSG-modules-expected.png` | modules, colour |
| 4 | `examples/Basics/hull.scad` | `tests/regression/render/hull-expected.png` | hull |
| 5 | `examples/Basics/LetterBlock.scad` | `tests/regression/render/LetterBlock-expected.png` | text + linear_extrude |
| 6 | `examples/Basics/text_on_cube.scad` | `tests/regression/render-manifold/text_on_cube-expected.png` | text on faces, colour |
| 7 | `examples/Basics/logo_and_text.scad` | `tests/regression/render-manifold/logo_and_text-expected.png` | text, colour |
| 8 | `examples/Basics/rotate_extrude.scad` | `tests/regression/render-manifold/rotate_extrude-expected.png` | rotate_extrude, colour |
| 9 | `examples/Basics/linear_extrude.scad` | `tests/regression/render-manifold/linear_extrude-expected.png` | twist/scale extrude |
| 10 | `examples/Advanced/GEB.scad` | `tests/regression/render-manifold/GEB-expected.png` | text, projection, offset, intersection |
| 11 | `examples/Advanced/offset.scad` | `tests/regression/render/offset-expected.png` | offset r/delta/chamfer |
| 12 | `examples/Advanced/surface_image.scad` | `tests/regression/render-manifold/surface_image-expected.png` | surface() from PNG |
| 13 | `examples/Advanced/children.scad` | `tests/regression/render-manifold/children-expected.png` | children(), colour |
| 14 | `examples/Old/example001.scad` | `tests/regression/render/example001-expected.png` | sphere minus cylinders |
| 15 | `examples/Old/example006.scad` | `tests/regression/render/example006-expected.png` | rounded die (hull) |
| 16 | `examples/Old/example009.scad` | `tests/regression/render/example009-expected.png` | DXF import, extrudes (fan) |
| 17 | `examples/Old/example010.scad` | `tests/regression/render/example010-expected.png` | surface() from .dat |
| 18 | `examples/Old/example011.scad` | `tests/regression/render/example011-expected.png` | polyhedron |
| 19 | `examples/Old/example012.scad` | `tests/regression/render/example012-expected.png` | STL import |
| 20 | `examples/Old/example017.scad` | `tests/regression/render-manifold/example017-expected.png` | large rotate_extrude assembly |
| 21 | `examples/Old/example024.scad` | `tests/regression/render/example024-expected.png` | Menger sponge (boolean stress) |
| 22 | `examples/Parametric/candleStand.scad` | `tests/regression/render/candleStand-expected.png` | parametric, extrude |
| 23 | `examples/Parametric/sign.scad` | `tests/regression/render/sign-expected.png` | text sign |
| 24 | `tests/data/scad/3D/features/minkowski3-tests.scad` | `tests/regression/render/minkowski3-tests-expected.png` | 3D minkowski (none in examples/) |
| 25 | `tests/data/scad/2D/features/text-font-direction-tests.scad` | `tests/regression/render/text-font-direction-tests-expected.png` | 2D text: direction, shaping |
| 26 | `tests/data/scad/2D/features/offset-tests.scad` | `tests/regression/render/offset-tests-expected.png` | 2D offset joins |

Notes:

- `examples/` contains no `minkowski()`, hence #24.
- `examples/Basics/projection.scad` has only a preview image, and
  `roof.scad` is experimental; both were left out.
- The example tests sit in ctest's `Examples` configuration, not `Default`.
  That doesn't matter for recording.

---

## Checked and found fine

- The architecture's 522 `.scad` inputs (`tests/data/scad`) and 1,761
  expected files are correct.
- OpenSCAD's defaults to Manifold and uses Clipper2 are confirmed, so
  aiming to converge on its outputs is sound.
- UniFFI plus a Rust-owned renderer is a sound split: meshes never cross
  the bridge.
- wgpu can do the GUI surface, offscreen snapshots and WebGPU from one
  codebase.
- Safari now ships WebGPU, which removes the main risk to the web plan.
- Number formatting is a closed, portable specification (B4).
- Curve flattening for text is done by OpenSCAD, not FreeType, so it is
  portable.
