# Community benchmark

Anyone can time an official NeoSCAD release on their own machine with
`neoscad bench`, ideally alongside their own OpenSCAD, and submit the
result. Results are collected in the public repository
[`neoscad/benchmarks`](https://github.com/neoscad/benchmarks) and shown on
neoscad.org per released version. Every release also benchmarks itself on
GitHub's runners (the *release baseline*), so each version has results
from the same four machine types.

## Running it

```sh
neoscad bench                  # this release's kit; finds OpenSCAD and asks
neoscad bench --quick          # 7 fast models, one run each
neoscad bench --openscad /path/to/openscad
neoscad bench --no-openscad    # neoscad alone
neoscad bench --submit         # then submit the result (asks first)
```

| Flag | Meaning |
|---|---|
| `--kit FILE\|DIR` | The bench kit, as its `.tar.gz` or unpacked. Default: this release's kit, downloaded from the GitHub release, checked against its published `.sha256` and cached (`~/Library/Caches/neoscad/bench-kit` on macOS, `$XDG_CACHE_HOME/neoscad` or `~/.cache/neoscad` on Linux, `%LOCALAPPDATA%\neoscad\cache` on Windows). |
| `--openscad PATH` | The OpenSCAD to compare with. Its `--version` line and backend are recorded, never its path. A build with `--backend` runs with `--backend=manifold`; older ones use CGAL. |
| `--no-openscad` | Don't look for OpenSCAD. |
| `--quick` | Only the kit's quick models, one run each, 5 cold-start runs. |
| `--runs N` | Runs per model, keeping the best (default 3, from the kit). |
| `--json FILE` | Write the result there. Without it, the result is kept in the cache directory's `bench-results/` and its path printed. |
| `--submit` | Show the exact payload, ask, and submit it (below). |

Without `--openscad` or `--no-openscad`, `neoscad bench` looks for
OpenSCAD in the usual places (`/Applications/OpenSCAD*.app` and
`~/Applications` on macOS; `/usr/bin`, `/usr/local/bin` and `/snap/bin`
on Linux; `Program Files\OpenSCAD` and `OpenSCAD (Nightly)` on Windows;
then `PATH`) and asks whether to include the one it found. Results
without OpenSCAD are accepted, but a comparison on the same machine is
what makes a result most useful, so including one is encouraged. With no
terminal to ask, it is left out and the command says how to include it.

While it runs it prints one row per model (neoscad's best time, and
OpenSCAD's and the speedup when there is a reference), then a summary:
models finished, the total of neoscad's best times, the cold start, and
the geometric mean speedup over the models both finished.

Close other work first: the load average before and after is recorded,
and a busy or battery-powered machine gives slower times.

Network access (the kit, the release's checksum list) uses `curl`, which
macOS, Windows 10 and later and nearly every Linux install ship; the
`neoscad` binary carries no HTTP client of its own.

## Only official releases

Results are accepted only from **official release binaries**: the
`neoscad` executable inside one of a GitHub release's archives, as
installed by the archives, the installers, Homebrew, Scoop, the `.deb`
and `.rpm` packages or the Docker image. A self-built binary may use other
flags, another toolchain or local patches, and its times would be filed
under a version whose release they do not describe.

Each release carries `neoscad-executables.sha256sums`: the SHA-256 of the
executable in every target's archive, one `HASH  TARGET/neoscad[.exe]`
line each (written by `publish-packages.yml`'s `bench-kit` job, which
first checks each archive against cargo-dist's own checksum). `neoscad
bench` hashes its own executable (`std::env::current_exe`) and compares it
with its target's line before anything runs:

| `official_check` | Meaning |
|---|---|
| `matched` | the release's executable for this target: `official: true` |
| `mismatch` | this target is listed with another hash: self-built or modified |
| `target-not-listed` | the release has no executable for this target |
| `unavailable` | the list could not be fetched (offline, or not a released version) |

Anything but `matched` is `official: false`. Such a run still works and
writes its JSON, but `--submit` refuses before timing anything and says
why. The benchmarks repository repeats the check on every submission,
since a client can be made to say anything.

The macOS app's bundled CLI (`NeoSCAD.app/Contents/MacOS/neoscad`) is a
separately signed, universal build and is not in the list; use the
Homebrew formula or the archive to submit.

## What is collected

The result is JSON, schema 1: `crates/bench-core/src/result.rs` writes it,
[`bench/result.schema.json`](../bench/result.schema.json) (JSON Schema
2020-12) describes it, and a unit test checks the two against each other.

| Field | Contents |
|---|---|
| `schema` | `1` |
| `source` | `user`, or `ci-baseline` for the release workflow's own runs |
| `neoscad` | `version`, `target` (Rust triple), `sha256` of the executable, `official`, `official_check` |
| `kit` | `version`, `archive_sha256` (null for an unpacked kit), `content_sha256` (digest of the unpacked files), and the neoscad, BOSL2 and OpenSCAD commits it was built from |
| `method` | `version` (the timing method's version, `bench_core::timing::METHOD_VERSION`), `runs`, `cold_start_runs`, `single_run_over_s`, `timeout_s`, `quick` |
| `machine` | `os`, `os_version`, `arch`, `cpu` (marketing name), `hardware_model` (a Mac's model identifier), logical, physical, performance and efficiency core counts, `memory_bytes`, `on_battery`, `load_before`/`load_after` (1/5/15-minute load averages), `translated` (Rosetta 2) |
| `threads` | the worker threads neoscad uses |
| `openscad` | `{version, backend, args}`, or null |
| `cold_start`, `models.<id>` | `{neoscad, openscad}`, each `{rc, timed_out, runs_s, best_s, cpu_s}`; `openscad` null without a reference |
| `skipped` | models not run, and why |
| `started_at`, `finished_at` | UTC, to the second |

Fields the OS does not answer cheaply are null: battery and load
averages on Windows, physical cores on most Linux aarch64 kernels.

**Privacy.** No hostname, user name, file path, IP address, serial number
or MAC address is read, so none can be in a result; the OpenSCAD path is
used to run it and never recorded. A test runs `neoscad bench` and checks
that the result contains neither the machine's hostname, the user name,
the home or temporary directory, nor the kit's or binary's path. A
submission is public: `--submit` prints the exact JSON before asking.

## How a model is timed

Every model is exported to ASCII STL (`neoscad -o out.stl model.scad`,
the same for OpenSCAD) with `OPENSCADPATH` set to the kit's `libraries`
and `NEOSCAD_NO_SERVER=1`, so a running `neoscad serve` never answers. Up
to `runs` runs one after another keep the best wall time; after a run
over `single_run_over_s` (60 s) there are no more, and a failure or a
`timeout_s` (300 s) timeout ends the series (a failed model has no
time). The cold start is `cube(1);`, 20 runs (5 with `--quick`). This is
the method of `conformance bench`, and it is the same code
(`crates/bench-core/src/timing.rs`); `method.version` changes whenever
that code does, and results with different versions are not compared.

Files a model imports (`import_stl`'s 21 MB sphere) are exported by the
neoscad under test before the first run, and both programs import the
same file.

## The bench kit

`neoscad-bench-kit-<version>.tar.gz` (about 1 MB, with a `.sha256`) is
attached to every release (including prereleases that publish, see
`docs/release.md`). `scripts/release/bench-kit.sh` builds it from
`conformance/bench.json`:

- `kit.json`: runs, limits, the quick set, and each model's file,
  libraries and inputs;
- `models/`: every model's source (bench.json's inline sources written
  out; OpenSCAD's `examples/Basics/CSG.scad` and `examples/Old/example024.scad`;
  BOSL2's `examples/fractal_tree.scad` and `examples/spring_handle.scad`);
- `inputs/`: sources of imported files;
- `libraries/BOSL2/`: BOSL2's `.scad` files at the commit pinned in the
  script (`BOSL2_COMMIT`), with its licence;
- `licenses/`: BOSL2's BSD 2-Clause licence and OpenSCAD's examples'
  CC0 1.0 dedication.

BOSL2 must be at the pinned commit and OpenSCAD's examples at
`conformance/manifest.json`'s commit, or the script stops; moving either
changes the models, so it is a deliberate edit. The archive is
reproducible: the same commit and inputs give the same bytes with the
same `tar` (entries sorted, owned by root, stamped with the commit time,
modes normalised, `gzip -n`); a test builds it twice and compares.

To benchmark a development build, build a kit and pass it:
`scripts/release/bench-kit.sh --out /tmp/kit` then `neoscad bench --kit
/tmp/kit/neoscad-bench-kit-<version>.tar.gz`. The result is marked
`official: false` and cannot be submitted.

## Submitting

`--submit` prints the exact result JSON, asks for confirmation, then files
an issue on `neoscad/benchmarks`:

- with the GitHub CLI installed and logged in (`gh auth status`),
  through `gh issue create`, with a body in the issue form's layout;
- otherwise by opening a pre-filled new-issue link in the browser (and
  printing it). When the JSON is too long for a link (over about 8 KB,
  as a full run with OpenSCAD can be), the link carries only the
  title and the command names the saved JSON file to paste into the
  form's *Result JSON* field.

### Submission format (the contract with neoscad/benchmarks)

`crates/bench-core/src/submit.rs` holds these constants; the benchmarks
repository's issue form and validation must match them.

| | |
|---|---|
| Issue form | `.github/ISSUE_TEMPLATE/submit.yml` (URL `?template=submit.yml`) |
| Field `result` | label **Result JSON**, a `textarea` with `render: json`, required |
| Field `notes` | label **Notes**, a `textarea`, optional |
| Title | `neoscad <version> on <os_version>, <cpu>[, vs <openscad version> (<backend>)]` |
| Label | `benchmark` (the form applies it; a `gh` submission has none, so the validation should key on the body, not the label) |

The issue body, from the form or from `gh`, is:

````
### Result JSON

```json
{ ... }
```

### Notes

_No response_
````

The validation should, for each opened or edited issue: take the fenced
JSON under `### Result JSON`; validate it against
`bench/result.schema.json` at the tag of `neoscad.version`; require
`source: "user"` (baselines come only from the release workflow's
commits); fetch that release's `neoscad-executables.sha256sums` and
require `neoscad.sha256` on the `neoscad.target` line; require
`kit.archive_sha256` to be the release's
`neoscad-bench-kit-<version>.tar.gz.sha256` (or `kit.content_sha256` to
match the digest of that kit unpacked); require `method.version` to be a
known one; and then commit it.

### Results layout

In `neoscad/benchmarks`:

```
results/<version>/ci-baseline-<target>.json   the release baseline (below)
results/<version>/<issue number>.json          accepted submissions
```

The repo's `summary.json` (rebuilt on every change; format in its
README) is what https://neoscad.org/community.html shows per version:
the baselines as a table per target; for submissions, one row per result with OS, CPU,
cores and memory, neoscad's geometric mean over the models, and, where
the result has OpenSCAD, the geometric mean speedup over the models
both finished (`BenchResult::summary` computes it the same way), with the
OpenSCAD version and backend. Only results whose `kit.version` is the
version shown and whose `method.version` matches are compared, and speedup
aggregates are kept apart per OpenSCAD backend (Manifold, CGAL) and per
quick or full run. Validation (the `validate` workflow there) re-checks
the release, the executable's hash, the kit and the schema at the
release's tag, and rejects identifying strings.

## The release baseline

`publish-packages.yml` (a cargo-dist publish job, run for every release
and for prereleases when `publish-prereleases` is set):

1. **`bench-kit`** builds the kit (after `scripts/release/fetch-reference.sh`),
   writes `neoscad-executables.sha256sums` from the release's archives,
   and attaches both, with the kit's `.sha256`.
2. **`baseline`** downloads the release's own archive and kit on
   `ubuntu-22.04` (x86_64 Linux), `ubuntu-22.04-arm` (aarch64 Linux),
   `macos-15` (arm64 macOS) and `windows-2025` (x86_64 Windows), checks
   both against their `.sha256`, and runs `neoscad bench --no-openscad
   --source ci-baseline --kit <kit> --json …`; a result that is not
   `official` fails the job.
3. **`baseline-submit`** commits the results that finished to
   `neoscad/benchmarks` as `results/<version>/ci-baseline-<target>.json`.
   It needs the `BENCHMARKS_TOKEN` secret (a fine-grained token, resource
   owner `neoscad`, repository access only `neoscad/benchmarks`,
   Contents: read and write); without it the job warns and skips.

Both baseline jobs are `continue-on-error`: a benchmark failure never
holds back a release. GitHub's runners are shared virtual machines, so
the baseline is a like-for-like series across versions rather than a
statement of any machine's best.
