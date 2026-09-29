# Model tests: `neoscad test`

`neoscad test [PATHS...] [--filter S] [--format json] [--enable part] [-j N]`
runs model tests: tests an agent (or a person) writes before iterating on
a model, in plain OpenSCAD plus comments. No new language: a test is a
module, a check is an `assert()` or an `// @expect` comment.

The implementation is `crates/session/src/modeltest.rs`; the command is
`crates/cli/src/modeltest.rs`; `neoscad serve` answers it as the `test`
method (`docs/serve-protocol.md`). An example suite is `examples/tests/`
(`neoscad test examples/tests`), which `cargo test` runs.

## Discovery

- **Test files** are `*_test.scad` and `test_*.scad`. `PATHS` are files
  (taken whatever their name) or directories, searched recursively for
  test files; hidden directories are skipped. No path: the current
  directory.
- **Tests** are the top-level `module test_*()` definitions of a test file
  (not of the files it includes).
- `--filter S` keeps the tests whose id, `FILE::test_name`, contains `S`.

## Running

Each test runs as its own model: the file with its top-level instances
replaced by one call, `test_name();`. The file's assignments, function
and module definitions, `include`s and `use`s all apply, so a test file
usually starts with `include <model.scad>` (or `use`) and may define
helpers; geometry the file makes at top level is not part of any test.

A test **passes** when

1. evaluation (and rendering, when an expectation needs geometry) reports
   no error: a failed `assert()`, an undefined operation that stops
   evaluation, a recursion limit, a geometry error; and
2. every `@expect` line holds.

Warnings do not fail a test unless it says `@expect no-warnings`. A test
file that does not parse fails as a whole (one result with the file's
name and no test name).

Tests run on `-j N` threads (default: one per CPU). The results are in
file and line order whatever the thread count: the output, human or
JSON, is the same except `timings_ms`. Exit status 0 when every test
passed, 1 when one failed or none was found.

## `@expect`

Expectations are `//` comment lines starting `@expect` in the comment
block directly above the test module (no blank line between). Other
lines of the block are free text.

```openscad
include <box.scad>

// The default tray: 7152 mm³ of plastic.
// @expect volume 7152
// @expect bbox [40, 30, 20]
// @expect manifold
// @expect components 1
module test_tray() tray();
```

The grammar, one expectation per line:

```text
expect      := "@expect" SP what
what        := "volume" SP approx            ; mm³, of a 3D model
             | "area" SP approx              ; mm² surface (3D) or area (2D)
             | "bbox" SP vector [tol]        ; size, [x, y, z] or [x, y] for 2D
             | "bbox" SP "[" vector "," vector "]" [tol]   ; min and max corners
             | "manifold"                    ; a valid closed solid
             | "components" SP INT           ; separate pieces
             | "check" SP ("clean" | "no-error")
             | "parts" SP NAME ("," NAME)*   ; these parts exist
             | "no-warnings"
approx      := NUMBER [tol]
tol         := ("±" | "+-") NUMBER ["%"]     ; bbox: mm only
vector      := "[" NUMBER ("," NUMBER)* "]"
```

- **Tolerances.** `volume 1000±1` allows 999 to 1001, `volume 1000±2%`
  allows 980 to 1020. Without one a number must match to a millionth of
  itself (and bbox coordinates to 1e-6 mm): write a tolerance for
  anything curved, whose value depends on `$fn`.
- **Where the numbers come from.** The model is rendered once (as
  `neoscad measure` renders it) and measured on its mesh: `volume` and
  `area` are `measure`'s, `bbox` the mesh's bounding box,
  `components` the pieces sharing no vertex (`check`'s count), and
  `manifold` requires Manifold to accept the solid and `check` to find no
  `not-manifold` or `not-closed` problem. A 2D model has `area` and a
  2-number `bbox`; a volume expected of a 2D model fails with "got a 2D
  model". An empty model (an `intersection()` whose parts do not meet)
  measures volume 0, area 0 and 0 components, so `@expect volume 0`
  asks "no interference" whether the parts are apart or only touch; a
  failure says `got 0 (an empty model)`, and a zero-volume result (the
  faces where parts only touch) says so after its count. `bbox` and
  `manifold` of an empty model still fail with "got an empty model".
  `no-warnings` does not count the `use-special-variables` hint
  (`docs/cli-json.md`).
- **`check clean`** runs `neoscad check` with its defaults (0.4 mm nozzle,
  0.8 mm minimum wall, 45° overhangs, no bed) and requires no error or
  warning finding; **`check no-error`** allows warnings (overhangs, thin
  but printable walls). Findings are listed in the failure.
- **`parts a,b`** requires parts named `a` and `b` (dotted names for
  nested ones: `lid.hinge`); other parts may exist. A test with it runs
  with neoscad's `part()` extension on (`--enable part` turns it on for
  every test).
- **Mistakes fail.** An unknown expectation or a malformed value
  (`@expect volum 3`) fails the test with "bad @expect: ...", so a typo
  never passes silently.

Expectations that need no geometry (`no-warnings` alone, or none) only
evaluate the test, which is fast: function tests are plain `assert()`s:

```openscad
module test_capacity() {
    assert(capacity([40, 30, 20]) == 36 * 26 * 18);
}
```

## Output

Human-readable, one line per test and the failures under it:

```text
test examples/tests/box_test.scad::test_tray ... FAILED
    @expect volume 7150: expected 7150, got 7152
test examples/tests/box_test.scad::test_capacity ... ok

test result: FAILED. 1 passed; 1 failed; 1 files
```

`--format json` prints one object (keys sorted, compact, a trailing
newline; fields are only ever added):

```json
{"schema": 1, "exit_code": 1,
 "counts": {"tests": 2, "passed": 1, "failed": 1, "files": 1},
 "tests": [TEST, ...], "timings_ms": 12.3}
```

`TEST`:

```json
{"id": "box_test.scad::test_tray", "file": "box_test.scad",
 "name": "test_tray", "line": 7, "ok": false,
 "failures": [FAILURE, ...],
 "expectations": [{"expect": "volume 7150", "ok": false,
                   "expected": {"value": 7150.0, "tolerance": 0.00715},
                   "actual": 7152.0}, ...],
 "diagnostics": [DIAG, ...], "echo": ["ECHO: ..."], "timings_ms": 3.1}
```

- `line` is the test module's line.
- `FAILURE`: `{"kind", "message", ...}`. `kind` is `expect` (with
  `expect`, `expected` and `actual` as in `expectations`), `error` (the
  first error as OpenSCAD prints it, `ERROR: Assertion '...' failed ...
  in file ..., line N`; all of them are in `diagnostics`),
  `expectation-syntax` (with `expect`) or `file` (the file does not
  parse or cannot be read).
- `expected`/`actual` by expectation: `volume`, `area`:
  `{"value", "tolerance"}` (the tolerance in mm³ or mm²) and a number;
  `bbox`: `{"size", "tolerance"}` or `{"min", "max", "tolerance"}` and
  `{"size", "min", "max"}` or `{"min", "max"}`; `manifold`: `true` and a
  bool; `components`: counts; `check`: `[]` and the offending findings
  (`{"severity", "code", "message", "fix"}`); `parts`: the names expected
  and the parts found; `no-warnings`: `[]` and the warnings. When the
  model has nothing to measure `actual` says so: `"an empty model"`
  (for `bbox` and `manifold`), `"a 2D model"`.
- `DIAG` is the diagnostic object of `docs/cli-json.md`.
- A request that could not start (a path that does not exist) is
  `{"schema": 1, "exit_code": 1, "error": "...", "counts": ..., "tests": []}`.
