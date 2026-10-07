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
| `run --extra-enable NAMES` | Adds `--enable NAME` for each comma-separated name (`sketch,query`) to every run of the binary under test (not the renderer). NeoSCAD's extensions must leave OpenSCAD's files unchanged, so the results must equal a plain run's and the baseline still gates. Not with `--record` or `--update-baseline`. |
| `run --binary PATH` | Runs another binary. Pointing it at the OpenSCAD nightly checks the harness itself: all tier 0-2 cases should pass. |
| `depth` | The recursion-depth guard (`src/depth.rs`): runs a built binary (`--binary`, default `target/release/neoscad`; any build, PGO or a release archive's) on OpenSCAD's recursion tests (`recursion-test-module`, `-vector`, `-function3`) and on `eval/src/recursion.rs`'s two plain recursions, and exits 1 unless a neoscad binary reports exactly the depths of the counted recursion limit (the same in every build, plain or PGO; constants in the source), or another binary goes at least 1.25 times as deep as the OpenSCAD nightly 2026.09.23 does. `issue4172` is reported, not gated. `--json PATH` also writes the numbers; `--timeout` per run (default 120 s). The regular suite cannot see depth: OpenSCAD's expected files cut the trace to one frame. |
| `showcase` | Checks that every model in `conformance/showcase.json` and its expected image exist. |
| `image-compare EXPECTED ACTUAL` | Compares two PNGs with the port of OpenSCAD's `tests/image_compare.py`; exit 0 when they match. |
| `images` | Surveys neoscad's own renderer: draws every PNG case neoscad draws itself (tier 3's direct `--render` images and all of tier 4: render mode, OpenCSG previews, throwntogether, `--view`) and scores it under tier 4's rules (see "Tier 4" below), printing per kind how many pass each rule and the distribution of the perceptual score. Takes `--filter`, `-v` (list cases failing both rules), `--jobs`, `--timeout` (default 30) and `--binary`. A diagnostic only: it touches neither the baseline nor tier 3's results. Images go to `target/conformance/images/`. |
| `diff [PATHS...]` | Differential test: runs a reference OpenSCAD (`--binary-ref`, default the pinned nightly) and neoscad on every `.scad` under `PATHS` (default: the reference's `tests/data/scad`, `examples`, `libraries/MCAD`) and compares exit status, the output (`--format ast`, `echo` or `csg`; an `.echo` file holds every message, so it is compared even when both runs fail; `csg` ignores `timestamp = N` and owns no stderr messages, which `ast` and `echo` already cover) and the diagnostics that format covers. `--library-path DIR` (repeatable) puts a library directory before the reference's `libraries/` in `OPENSCADPATH` for both binaries, so a library's own files and examples run unmodified (`--library-path .reference` for `include <BOSL2/...>`). `--binary` sets neoscad, `--jobs` the parallelism, `--timeout` the per-run limit (default 60 s); `-v` lists every mismatch. Prints the match rate and mismatches by category; the full list goes to `target/conformance/diff-<format>.json`. |
| `bosl2-corpus` | Writes BOSL2's documentation examples and tests out as `.scad` files for `diff`; see "BOSL2 corpus" below. `--bosl2 DIR` (default `.reference/BOSL2`), `--check` (compare only; exit 1 on a difference). |
| `bench` | Times neoscad against the reference binaries on `conformance/bench.json`; see "Benchmarks" below. `--only IDS`, `--refs IDS`, `--quick`, `--runs N`, `--timeout S`, `--binary PATH`, and the cache flags `--fresh-refs`, `--fresh-ref ID`, `--refs-max-age DAYS`, `--seed-refs FILE`. |
| `bench-chart [FILE\|--latest] [--out PATH]` | Draws a benchmark result as a 1920x1080 PNG (default: next to the result). |
| `video` | Renders the progress video from `progress/`; see "Progress video" below. |

## BOSL2 corpus

    git clone https://github.com/BelfrySCAD/BOSL2.git .reference/BOSL2
    ./target/release/conformance bosl2-corpus
    ./target/release/conformance diff --format echo --library-path .reference .reference/BOSL2

BOSL2 documents itself in comments and tests itself with
`tests/*.scadtest` tables, neither of which OpenSCAD can run directly.
`bosl2-corpus` writes them as files beside the library (`src/bosl2_corpus.rs`):

- `examples_x/<file>__NNN.scad`: every `// Example` block of the top-level
  library files, except `NORENDER` ones and blocks with no code, numbered
  per file, after the file's `// Includes:` lines and an include of the
  file itself; `ex__<name>.scad`, each file of `examples/`; and
  `meta.json`, each block's tags and title.
- `tests_x/<file>__<test>.scad`: every `[[test]]` script (a repeated name
  gets `_2`), and `meta.json` with each test's name and flags.

Includes become `<../...>`, so the files run in place. The rules
reproduce the corpus the engine-milestone audit extracted by hand, byte for
byte: at BOSL2 `9948313`, 2,516 examples, the 10 `examples/` files and 976
tests. Files already in those directories that it doesn't generate are
listed, never deleted. The `diff` above then runs every `.scad` under
`.reference/BOSL2`: the corpus, the library's own files and `examples/`.

## Benchmarks

    ./target/release/conformance bench                 # everything (about 1 minute cached, 40 uncached)
    ./target/release/conformance bench --quick         # neoscad and the nightly (Manifold) only
    ./target/release/conformance bench --only ex_menger,cold_start --refs neoscad,nightly-cgal
    ./target/release/conformance bench --fresh-ref nightly-cgal   # re-time one reference
    ./target/release/conformance bench-chart --latest  # progress/bench/<same name>.png

`conformance/bench.json` lists the models (the engine milestone audit's
14), the reference binaries and the method's constants. The same models,
packed by `scripts/release/bench-kit.sh`, are the community benchmark's
kit, and `neoscad bench` times them with the same code
(`crates/bench-core/src/timing.rs`; `docs/community-bench.md`). Every model is
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
- **Extra metrics:** `cold_start` (`cube(1);`, best of 20),
  `eval_only` (BOSL2's test suite, 976 tests from `tests/*.scadtest`,
  each exported to `.echo` by its own process; the summed wall time and
  the pass count, for neoscad and the nightly) and `edit_loop`, the
  agents' headline: for each case in `bench.json` (a BOSL2 part and
  OpenSCAD's `examples/Basics/CSG.scad`), a one-line edit that gives a
  line a new value every time, then a re-render or a snapshot, best and
  median of 10 edits in ms, timed four ways (`src/edit_loop.rs`):
  `serve` (neoscad serve on stdio driven over JSON-RPC: `update` +
  `render`, `update` + `snapshot`), `cli_via_serve` (the edited file on
  disk, then `neoscad FILE -o out.stl` and `neoscad snapshot FILE` as
  clients of `neoscad serve --socket`; `served_requests` confirms every
  run went through it), `cli_cold` (the same commands, no server) and
  `nightly_cold` (the nightly exporting STL and a 1024x1024 `--render`
  PNG, one view where a snapshot draws four). `--only edit_loop` runs it
  alone. Where it cannot run (no GPU adapter for the snapshots, as in a
  CI container) it is listed under `skipped_models` with the reason and
  the rest of the result is still written.
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

### Reference-result cache

A reference's time on a model changes only when the binary, the model,
the method or the machine does, yet timing the references is almost all
of a full run: about 38 of its 40 minutes, including 300 s CGAL and
2021.01 timeouts. So reference results are cached in
`progress/bench/ref-cache.json` (`src/bench_cache.rs`), one entry per
(reference, model), and reused while the key matches. **neoscad is always
measured fresh**: it is what the benchmark is for.

- **Key:** everything a result depends on, compared field by field.
  - The binary: its resolved path, its whole `--version` output, and the
    size and SHA-256 of the executable (about 0.1 s for the 44 MB
    nightly; size plus mtime would be cheaper but would miss a same-size
    rebuild), plus the app bundle's `Info.plist`.
  - The model, by content: the SHA-256 of its text and of each generated
    input (`import_stl`'s STL is neoscad's output, so a rebuilt neoscad
    that writes it differently is a miss). A model that `requires` a
    library, or has an `include`/`use`, also keys on the `OPENSCADPATH`
    corpus: the entries of `.reference`, and each library's git commit
    plus a hash of its uncommitted changes to tracked files. `eval_only`
    keys on every split test script and its flags.
  - The arguments and environment: backend flag, working directory,
    `OPENSCADPATH`, the variables set and removed, and any inherited
    `OPENSCAD*`/`NEOSCAD*` variable.
  - The method: runs, `single_run_over_s`, the timeout (exact, so a longer
    timeout is a miss), and `METHOD_VERSION`, which is bumped whenever the
    timing code changes and so invalidates everything.
  - The machine: `machine` as the result file records it (hardware model
    and CPU, cores, memory, OS version and build, AC or battery power).
- **Stored:** the result exactly as a run records it (runs, best, CPU,
  mesh stats, `rc`, including `"timeout"`, so a 300 s timeout is paid
  once), with `measured_at` and the tree (`measured_sha`, `measured_dirty`)
  that measured it. Failures other than timeouts are not stored: they are
  cheap to repeat and more likely a broken setup than a fact about the
  binary.
- **In the output:** every reference result carries `cached: true|false`
  and `measured_at`; the file's `ref_cache` counts hits and misses. The
  schema is otherwise unchanged, so `bench-chart` and `video` read it as
  before. The mesh check still compares each reference's (cached) mesh
  stats with this run's neoscad. A miss prints its cause, such as
  `key differs: model.text_sha256`.
- **Flags:** `--fresh-refs` re-measures every reference, `--fresh-ref ID`
  one (repeatable), `--refs-max-age DAYS` treats older entries as misses
  (no limit by default). A fresh result replaces the entry. Deleting the
  file is always safe.
- **Seeding:** `--seed-refs FILE,...` fills the cache from earlier result
  files and stops. A result file records the binaries' paths and
  versions, the method, the machine and the library commits, but not the
  executables' or model files' contents. Those are taken from disk now and
  accepted only where the file's ctime (which, unlike mtime, no tool can
  set back) predates the run's start. The run's start is bounded by its
  recorded run times plus an hour. A run is used only if its tree was
  clean, at or after `METHOD_SINCE`, on this machine and OS, in this
  checkout. Anything unprovable is listed as not seeded. The first seeding
  took all 46 reference results of `20260927T072506Z-df6731d.json`. Every
  other file then in `progress/bench/` was from a dirty tree and was
  refused.

What the cache doesn't replace: comparing neoscad with neoscad (a change
against its parent, as the performance audits do) still needs an
**interleaved A/B**, both builds alternating run by run in one session.
The cache holds reference times from another session, and the machine's
background load differs between sessions: `dasd` alone took most of a
core during `docs/audits/performance.md`, which moves absolute times by a
few percent. That is fine for a 3x-or-more gap to a reference, but it
swamps the 1-5% a neoscad change is judged on. Only runs of both builds
under the same load show a difference that size. The edit loop's
`nightly_cold` series is not cached either; it takes seconds.

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

## Progress video

    ./target/release/conformance video [--out FILE.mp4] [--fps 30] [--hold 2] [--frames-dir DIR]
                                       [--progress DIR] [--ffmpeg PATH] [--agent-eval]
                                       [--commit-map FILE]

Draws one 1920x1080 scene per snapshot in `progress/index.jsonl` (the grid,
a caption with time, sha and subject, per-tier pass counts and a chart of
total passes over time, interpolated between snapshots), benchmark
interludes, and title and end cards, then encodes H.264
(`yuv420p`, CRF 20) with ffmpeg. The default output is
`progress/video/progress.mp4`; frames go to a temporary directory unless
`--frames-dir` keeps them. `--progress` reads another checkout's
`progress/` (from a worktree, the main tree's); no reference checkout is
needed. `--agent-eval` adds the agent-eval results interlude; it is off
by default because those results are not published. `progress/` records
commit ids from before the history was rewritten for publication;
`--commit-map` (default `.git/filter-repo/commit-map` when present) reads
them as the rewritten commits, for the manifests and the captions. The
same data gives the same file. Details, and the unbuilt
`--showcase` mode, are in `docs/progress-video.md`.

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
  the tier 3 `export-param` JSON, compared as parsed JSON
  (`compare_json`), and the tier 3 exact mesh files of the
  `predictible-output` export tests (STL, OBJ, 3MF, POV), compared
  exactly; a 3MF is first reduced to its model XML as
  `post_process_3mf` does.
- `geometry`: tier 3 PNG tests (direct `--render` and
  `export_import_pngtest.py`) and `stlexportsanitytest.py`; see "Tier 3"
  below.
- `script`: tests driven by a Python script or a raw command, ported in
  `src/script.rs`; see "Script cases" below.
- `image`: tier 4 images drawn by neoscad's renderer (a PNG of the
  input): render mode (`--render`), the OpenCSG preview (no `--render`),
  the throwntogether preview (`--preview=throwntogether`), with any
  `--view` options (axes, scales, edges, crosshairs); see "Tier 4" below.
  `relative-output_png_*` (re-tiered from 5 because it needs PNG export)
  runs as a `script` case.
- `pending`, with a `pending_reason`: a case with no runner yet (none at
  present).
- `skip`, with a reason: experimental features neoscad does not
  implement (a case runs when every feature it enables is in
  `SUPPORTED_FEATURES`, `src/manifest.rs`; the colour round trips
  registered EXPERIMENTAL with no `--enable` run too), the CGAL backend, tests
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
  It also sets `NEOSCAD_NO_SERVER=1` for every neoscad it runs (here, in
  `diff` and in `bench`'s cold runs), so a `neoscad serve` the developer
  has running never answers for the binary under test.
- A non-zero exit fails the case.
- Otherwise the outputs are normalised as in `src/normalize.rs` and must
  match line for line.

Actual outputs and each run's stderr go to `target/conformance/actual/`.

Before running, the harness creates the inputs that OpenSCAD's configure
step would generate: `include-tests.scad`, `use-tests.scad`, the
`import_*-tests.scad` files, `issue2342.scad` and the fourteen SVGs of
the `svgviewbox-*` images (`gen_svg_viewbox_tests.py`, ported in
`src/prepare.rs`). It writes them into the reference checkout, where
OpenSCAD's own `.gitignore` covers them.

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

Previews are held to the same rule. OpenSCAD draws them with OpenCSG's
image-space CSG; neoscad draws the same CSG products from real booleans
(`crates/render/src/preview.rs` says what that cannot reproduce), so a
pass means the preview's shapes, colours, `%` and `#` objects and view
options agree with OpenSCAD's to within the tolerance. At phase 6b on an
Apple M4 Pro: OpenCSG previews 340 of 345 (image_compare 339,
perceptual 328), throwntogether 265 of 267 (265, 250), `--view` 5 of 8
(5, 4); the failures are listed in `docs/followups.md`, "Rendering".
