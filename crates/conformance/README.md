# conformance

Runs OpenSCAD's own regression suite against `neoscad` and records progress.
The suite is read from the reference checkout at `.reference/openscad`
(see the root `CLAUDE.md` for how to create it).

## Quick start

    cargo build --release
    ./target/release/conformance run            # all runnable tiers
    ./target/release/conformance run --tier 1 --filter echo-tests -v
    ./target/release/conformance run --record   # also write a progress snapshot
    ./target/release/conformance grid --all     # render snapshot images

`run` prints pass/fail/skip/pending counts per tier and exits non-zero if
any test listed in `conformance/baseline.json` no longer passes.

## Commands

| Command | What it does |
|---|---|
| `manifest` | Regenerates `conformance/manifest.json` from `tests/CMakeLists.txt`. `--check` only verifies it is current. Run it after updating the reference checkout. |
| `run` | Runs the manifest's `text`, `geometry` and `script` cases in parallel (`--jobs`, `--timeout` seconds per process, default 30). `--tier N` (repeatable) and `--filter SUBSTR` narrow the run; `-v` prints the first differing lines of each failure. `--renderer PATH` sets the OpenSCAD that draws tier 3 meshes (default: the pinned nightly). |
| `run --update-baseline` | Rewrites `conformance/baseline.json` from the current passes. A narrowed run only updates the ids it ran. |
| `run --record` | Writes `progress/<UTC>-<sha>[-dirty]/` (`meta.json`, `scoreboard.json`) and appends to `progress/index.jsonl`. Needs a full run. `--grid` also writes `grid.png`. |
| `grid [DIR...]` | Renders `grid.png` for snapshot directories (a path or a name under `progress/`) from their recorded data. `--all` takes every snapshot in `index.jsonl`; existing images are skipped unless `--force`. `--out PATH` writes elsewhere (one snapshot only). |
| `run --binary PATH` | Runs another binary. Pointing it at the OpenSCAD nightly checks the harness itself: all tier 0-2 cases should pass. |
| `showcase` | Checks that every model in `conformance/showcase.json` and its expected image exist. |
| `image-compare EXPECTED ACTUAL` | Compares two PNGs with the port of OpenSCAD's `tests/image_compare.py`; exit 0 when they match. |
| `images` | Surveys neoscad's own renderer: draws every render-mode PNG case (tier 3's direct `--render` images and tier 4's image cases) with neoscad and scores it under tier 4's rules (see "Tier 4" below), printing how many pass each rule and the distribution of the perceptual score. `--previews` adds tier 4's pending OpenCSG previews, drawn from the rendered geometry (they stay pending in `run`). A diagnostic only: it touches neither the baseline nor tier 3's results. Images go to `target/conformance/images/`. |
| `diff [PATHS...]` | Differential test: runs a reference OpenSCAD (`--binary-ref`, default the pinned nightly) and neoscad on every `.scad` under `PATHS` (default: the reference's `tests/data/scad`, `examples`, `libraries/MCAD`) and compares exit status, the output (`--format ast`, `echo` or `csg`; an `.echo` file holds every message, so it is compared even when both runs fail; `csg` ignores `timestamp = N` and owns no stderr messages, which `ast` and `echo` already cover) and the diagnostics that format covers. `--library-path DIR` (repeatable) puts a library directory before the reference's `libraries/` in `OPENSCADPATH` for both binaries, so a library's own files and examples run unmodified (`--library-path .reference` for `include <BOSL2/...>`). Prints the match rate and mismatches by category; the full list goes to `target/conformance/diff-<format>.json`. |
| `bench` | Times neoscad against the reference binaries on `conformance/bench.json`; see "Benchmarks" below. |
| `bench-chart [FILE\|--latest] [--out PATH]` | Draws a benchmark result as a 1920x1080 PNG (default: next to the result). |

## Benchmarks

    ./target/release/conformance bench                 # everything (about 40 minutes)
    ./target/release/conformance bench --quick         # neoscad and the nightly (Manifold) only
    ./target/release/conformance bench --only ex_menger,cold_start --refs neoscad,nightly-cgal
    ./target/release/conformance bench-chart --latest  # progress/bench/<same name>.png

`conformance/bench.json` lists the models (the engine milestone audit's
14), the reference binaries and the method's constants. Every model is
exported to ASCII STL by each reference, one run after another: neoscad
(`target/release/neoscad`, or `--binary`), the nightly with
`--backend=manifold` and with `--backend=cgal`, and OpenSCAD 2021.01
(`/Applications/OpenSCAD-2021.01.app`, x86_64 only, so it runs under
Rosetta 2 on Apple silicon). A missing reference binary other than
neoscad is skipped with a note.

- **Timing:** best wall time of 3 runs (`--runs`); one run once a run
  takes over 60 s; 300 s timeout per run (`--timeout`). A run's CPU time
  (user + system, from `getrusage` of the child) is recorded too.
- **Sanity:** the last run's STL is measured (vertices, triangles,
  volume, area, bounding box), and every reference's mesh is compared
  with neoscad's. A volume or area more than 0.1% apart, or a bounding
  box off by more than 0.1% of its size, is flagged (`mesh_flags`, a `!`
  in the table and on the chart), so a fast but wrong result shows.
- **Extra metrics:** `cold_start` (`cube(1);`, best of 20) and
  `eval_only` (BOSL2's test suite, 976 tests from `tests/*.scadtest`,
  each exported to `.echo` by its own process; the summed wall time and
  the pass count, for neoscad and the nightly).
- **Environment:** the working directory is `target/conformance/bench`,
  `OPENSCADPATH` is `.reference` (which holds BOSL2), and
  `NEOSCAD_FONT_DIR`/`OPENSCAD_FONT_PATH` are unset, so each binary uses
  its own bundled fonts. Inputs a model imports (the 21 MB STL of
  `import_stl`) are generated by neoscad on the first run.
- **Output:** `progress/bench/<UTC>-<sha>[-dirty].json`, in the schema of
  `docs/audits/engine-milestone-bench.json` plus the commit, subject,
  dirty flag and `geomean_speedup`, and a line in
  `progress/bench/index.jsonl`. `progress/` is gitignored.
- **Geometric mean:** of reference time / neoscad time over the models
  both finished; timeouts and failures are left out and listed.

Models from BOSL2 need a clone at `.reference/BOSL2`; without it they
(and `eval_only`) are skipped with a note:

    git clone https://github.com/BelfrySCAD/BOSL2.git .reference/BOSL2

Run benchmarks on AC power with nothing else running: every number is a
wall time.

## Progress snapshots

A snapshot stores only small data (about 5 KB); images are derived later.

- `meta.json`: commit, branch, subject, timestamps, dirty flag, reference
  commit, binary version.
- `scoreboard.json` (minified, `"schema": 2`): `manifest_sha256` (the
  manifest as run), `status` with one character per manifest test in
  manifest order (`P` pass, `F` fail, `S` skip, `-` pending), per-tier
  counts and summed process ms, total wall time, and failure reasons as
  `{reason: count}`. `failure_ids` lists the ids behind reasons shared by at
  most 10 tests.
- `index.jsonl`: one line per snapshot with per-tier counts.

`conformance grid` needs each test's tier, which comes from the manifest.
It finds that manifest by hash: first at the recorded commit
(`git show <sha>:conformance/manifest.json`), then in the working tree.
When a run uses a manifest that differs from HEAD's, `--record` cannot rely
on that lookup, so it embeds the test list (`embedded.ids` and a one-digit
`embedded.tiers` string). That makes the snapshot about 115 KB instead of 5.

The grid has one cell per test at a position fixed by the manifest, so
images of successive snapshots can be stitched into a video.

## How the manifest is built

OpenSCAD computes its test list in CMake, from globs, list arithmetic and
the helpers in `tests/cmake/TestFunctions.cmake`. `src/cmake.rs` evaluates
that file directly: generic commands are interpreted, and the registration
helpers (`add_cmdline_test`, `add_failing_test`, `set_test_config`, ...) are
native ports. It evaluates as an in-tree macOS build with Manifold, lib3mf
and `EXPERIMENTAL` on. An unsupported CMake construct is an error, so an
upstream change that needs new support fails loudly.

Each case gets a tier (see `src/manifest.rs`) and a runner:

- `text`: tiers 0-2 (`.ast`, `.echo`, `.csg`, `.term`), compared exactly,
  and the tier 3 `export-param` JSON, compared as parsed JSON
  (`compare_json`).
- `geometry`: tier 3 PNG tests (direct `--render` and
  `export_import_pngtest.py`) and `stlexportsanitytest.py`; see "Tier 3"
  below.
- `script`: tests driven by a Python script or a raw command, ported in
  `src/script.rs`; see "Script cases" below.
- `image`: tier 4 images drawn by neoscad's renderer in render mode (a
  PNG of the input with `--render` and no `--view`); see "Tier 4" below.
  `relative-output_png_*` (re-tiered from 5 because it needs PNG export)
  runs as a `script` case.
- `pending`, with a `pending_reason`: tier 4 cases that need phase 6b,
  the OpenCSG preview (no `--render`), the throwntogether preview
  (`--preview=throwntogether`) or `--view` options (axes, scales, edges,
  crosshairs).
- `skip`, with a reason: experimental features, the CGAL backend, tests
  disabled upstream, tests tagged `Bugs`, and OpenSCAD's harness self-test.

In `args`, `{REF}` stands for the reference checkout and `{OPENSCAD}` for
the binary under test.

## How a case runs

This follows `tests/test_cmdline_tool.py`:

- The binary runs with the input's absolute path, the manifest args, and
  `-o <actual>`, or with stdin/stdout for `stdio` cases.
- The working directory is `.reference/openscad/build/tests`, where ctest
  ran when the goldens were made. Diagnostics print input paths relative to
  it (`in file ../../tests/...`).
- The environment sets `OPENSCAD_FONT_PATH` and `OPENSCADPATH` (the
  reference checkout's `libraries/`, as ctest does, so MCAD comes from
  there rather than from neoscad's bundled copy). It removes
  `NEOSCAD_FONT_DIR`, so neoscad uses the Liberation fonts compiled into
  it, OpenSCAD's `<resources>/fonts`; they are byte-identical to the
  reference checkout's `fonts/`, which a test in `crates/assets` checks.
- A non-zero exit fails the case.
- Otherwise the outputs are normalised as in `src/normalize.rs` and must
  match line for line.

Actual outputs and each run's stderr go to `target/conformance/actual/`.

Before running, the harness creates the inputs that OpenSCAD's configure
step would generate: `include-tests.scad`, `use-tests.scad`, the
`import_*-tests.scad` files and `issue2342.scad`. It writes them into the
reference checkout, where OpenSCAD's own `.gitignore` covers them.

Several tier 0-2 inputs use MCAD (`include-tests`, `use-tests`,
`text-search-test`, `example023`). A shallow clone does not fetch that
submodule, and `run` warns when it is missing. To fetch it:

    git -C .reference/openscad submodule update --init libraries/MCAD

## Script cases

These need no renderer: the binary under test does every OpenSCAD step.

- **SVG re-export** (`export-svg*`, `export_import_pngtest.py` with an SVG
  result): export as SVG, import that in a wrapper, export again; the
  second SVG is compared as text. Both steps get the test's `-O` options.
- **PDF** (`export-pdf*`, `export_pngtest.py`): the PDF is rasterised at
  300 dpi and compared with OpenSCAD's image comparator. Upstream uses
  Ghostscript (`gs`), used here when it is on `PATH`; otherwise poppler's
  `pdftoppm` (with `pdfinfo` for the page size), cropped to Ghostscript's
  page size. With neither, the cases fail with a message saying so. On
  macOS: `brew install poppler` (or `ghostscript`).
- **Exit codes** (`shouldfail.py`): the test's arguments plus
  `--export-format=<suffix> -o -`; the exit code must equal `--retval`.
- **Relative output** (`relative-output_*`): `_run` writes
  `relative-output.<format>` into the working directory, `_check` passes
  when the file is there. Checks run after all other cases, as ctest's
  `DEPENDS` orders them.

## Tier 3: geometry through images

OpenSCAD checks geometry by rendering PNGs. A tier 3 case passes when the
geometry of the binary under test, drawn by the pinned nightly with the
test's own arguments, matches the expected PNG under OpenSCAD's own
comparator (`src/image_compare.rs`, a port of `tests/image_compare.py`
checked against the Python original on 119 image pairs). `src/geometry.rs`
has the details:

- **Direct renders** (`render-manifold`, `render-force-manifold`, ...): the
  binary exports `in.off` with the test's arguments; OFF keeps the per-face
  colours a Manifold render shows (scheme colour, green cut faces,
  `color()`). The nightly then renders a one-line wrapper,
  `import(file);`, with `-D file="…"` and the same arguments. A 2D result
  is exported as SVG instead, an empty one renders an empty wrapper, and
  `$vp*` assignments in the input are copied to the wrapper.
- **`export_import_pngtest.py`** is ported step for step, with the binary
  under test doing the export.
- **`stlexportsanitytest.py`** exports an STL and checks it as
  `validatestl.py` does.

The nightly's PNGs are cached in `target/conformance/image-cache`, keyed by
the renderer, its arguments and the mesh bytes, so a rerun only renders
meshes that changed. Run the suite with `--binary` set to the nightly to
measure the pipeline's ceiling: whatever fails then is an artefact of
export and re-import, not a geometry bug. Known artefacts are listed with a
reason in `conformance/tier3-limits.json`; such a case is reported as
skipped when it fails, and the run names any listed case that passed.

## Tier 4: neoscad's own images

A tier 4 `image` case runs neoscad with the test's arguments and `-o
<actual>.png`; it must exit 0, and the image must meet tier 4's rule
(`run_image` in `src/run.rs`):

- **either** OpenSCAD's own `image_compare` accepts it (the tier 3
  comparator: no 3x3 block whose nine samples all differ, in the same
  direction, by 8 or more);
- **or** at most 0.1% of its pixels have a channel 8 or more apart from
  the expected image (`PERCEPTUAL_LIMIT_PERCENT` in
  `src/image_compare.rs`).

The second rule is the architecture's "looser perceptual comparison": the
image comes from neoscad's renderer, not OpenSCAD's, so a pass should not
hinge on rasterisation details. It is deliberately tight. 0.1% of a
512x512 image is 262 pixels, a 16x16 patch: a missing feature, a wrong
colour or a shifted camera fails it, while scattered edge pixels pass.
`image_compare` already tolerates most rasterisation differences, since
they come as one-pixel lines rather than 3x3 blocks, so in practice the
second rule rarely decides: when it was introduced, `conformance images`
drew 320 render-mode images (316 tier 3 direct renders, 4 tier 4 cases),
318 passed `image_compare`, the same 318 passed either rule, and 206
were pixel-identical to OpenSCAD's. The run prints how many images pass
each rule and the score's distribution.

The expected images come from OpenSCAD's offscreen renderer, which draws
without multisampling into an 8-bit RGBA framebuffer object; neoscad
matches both (see `crates/render/src/gpu.rs`).
