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
| `run` | Runs the manifest's `text` cases in parallel (`--jobs`, `--timeout` seconds per case, default 30). `--tier N` (repeatable) and `--filter SUBSTR` narrow the run; `-v` prints the first differing lines of each failure. |
| `run --update-baseline` | Rewrites `conformance/baseline.json` from the current passes. A narrowed run only updates the ids it ran. |
| `run --record` | Writes `progress/<UTC>-<sha>[-dirty]/` (`meta.json`, `scoreboard.json`) and appends to `progress/index.jsonl`. Needs a full run. `--grid` also writes `grid.png`. |
| `grid [DIR...]` | Renders `grid.png` for snapshot directories (a path or a name under `progress/`) from their recorded data. `--all` takes every snapshot in `index.jsonl`; existing images are skipped unless `--force`. `--out PATH` writes elsewhere (one snapshot only). |
| `run --binary PATH` | Runs another binary. Pointing it at the OpenSCAD nightly checks the harness itself: all tier 0-2 cases should pass. |
| `showcase` | Checks that every model in `conformance/showcase.json` and its expected image exist. |
| `diff [PATHS...]` | Differential test: runs a reference OpenSCAD (`--binary-ref`, default the pinned nightly) and neoscad on every `.scad` under `PATHS` (default: the reference's `tests/data/scad`, `examples`, `libraries/MCAD`) and compares exit status, the output (`--format`, currently `ast`) and the diagnostics that format covers. Prints the match rate and mismatches by category; the full list goes to `target/conformance/diff-<format>.json`. |

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

- `text`: tiers 0-2 (`.ast`, `.echo`, `.csg`, `.term`), compared exactly.
- `pending`: tiers 3-5, which this harness does not compare yet.
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
- The environment sets `OPENSCAD_FONT_PATH` and `OPENSCADPATH`.
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
