# Validation corpus

Test cases for the sketch solver translated from the test suites of two
established constraint solvers, so that ours is checked against what
theirs are tested to do. `tests/corpus.rs` solves every case and checks
it; `scripts/sketch-oracle.py` (repository root) also solves them with
SolveSpace's solver and compares.

**Licensing.** These files are derived from FreeCAD (LGPL-2.1-or-later)
and SolveSpace (GPL-3.0-or-later) test sources, under those licences. The
solver itself is `MIT OR Apache-2.0`, so this directory, `tests/corpus.rs`,
`tests/support/` and `examples/` are excluded from its published package
(`exclude` in `Cargo.toml`; `cargo package --list` shows what ships). They
live in NeoSCAD's repository, whose own licence position is in the
top-level README ("Licence").

| File | Origin | Upstream files | Commit | Licence |
|---|---|---|---|---|
| `solvespace/regression.json` | SolveSpace | `test/constraint/*/*.slvs`, `test/request/*/*.slvs` | `6909d19753817cd8b0b2a2f44acc225835232bc5` | GPL-3.0-or-later (`solvespace/COPYING.txt`) |
| `solvespace/python-binding.json` | SolveSpace | `python/tests/test.py` | `6909d19753817cd8b0b2a2f44acc225835232bc5` | GPL-3.0-or-later (`solvespace/COPYING.txt`) |
| `freecad/sketcher-solver.json` | FreeCAD | `src/Mod/Sketcher/SketcherTests/TestSketcherSolver.py` | `e326ee2f07df04d4293035d65a98c96c3eb23380` | LGPL-2.1-or-later (`freecad/LICENSE`) |
| `translate.py` | NeoSCAD | writes the two hand-translated files above (it contains their numbers) | — | as the files it writes |

`solvespace/regression.json` is written by
`scripts/sketch-corpus-slvs.py` from a SolveSpace checkout; each case's
`from` names its `.slvs` file. Each case of the other two names its
upstream test in `test`.

## Format

A file has `origin`, `upstream`, `commit`, `licence`, `cases` and
`skipped` (upstream tests not translated, with the reason). A case:

```json
{
  "name": "freecad/testBoxCase",
  "from": "src/Mod/Sketcher/SketcherTests/TestSketcherSolver.py",
  "test": "TestSketcherSolver.testBoxCase",
  "note": "how the translation departs from the upstream test, if it does",
  "entities": [
    ["p", "point", [x, y]],
    ["l", "line", "p", "q"],
    ["a", "arc", "centre", "start", "end"],
    ["c", "circle", "centre", radius_guess]
  ],
  "construction": ["l"],
  "constraints": [
    ["fix", "p"], ["fix", "p", [x, y]], ["coincident", "p", "q"], ["on", "p", "curve"],
    ["horizontal", "l"], ["horizontal", "p", "q"], ["vertical", ...],
    ["parallel", "l1", "l2"], ["perpendicular", "l1", "l2"], ["tangent", "x", "y"],
    ["distance", "a", "b", d], ["distance_x", "a", "b", d], ["distance_y", "a", "b", d],
    ["length", "l", d], ["radius", "c", r], ["diameter", "c", d],
    ["angle", "l1", "l2", degrees], ["sweep", "arc", degrees],
    ["equal", "x", "y"], ["midpoint", "p", "l"], ["symmetric", "p", "q", "about"]
  ],
  "solved": {"p": [x, y], "c": r},
  "expect": {
    "status": "solved", "dof": 0, "tol": 1e-9,
    "points": {"p": [x, y]}, "radii": {"c": r},
    "checks": [["x", "p", v], ["x<", "p", v], ["dx", "p", "q", v], ["cw", "a", "b", "c"]],
    "allow_flipped": ["ArcSweep"]
  },
  "stages": [{"set": [[constraint_index, value]], "expect": {...}}],
  "informational": {"points": {"p": [x, y]}}
}
```

- Points are drawn at their coordinates; the solver starts there.
- `solved` is upstream's own solution (SolveSpace's regression files are
  saved solved). The test checks that a solve started there stays there
  (our equations hold at their solution), and that a solve from a
  perturbed drawing converges, back to it when the sketch has no free
  degree of freedom.
- `expect` is what the upstream test asserts, translated. `dof` is
  upstream's figure where it states one (SolveSpace's `test_pydemo`);
  FreeCAD's tests state none, so their `dof` values are counted by hand
  (and agree with SolveSpace's count wherever the oracle can run them). A solution must not flip (see `Orientation`) unless
  `allow_flipped` says which kind may, and the case's comment in
  `translate.py` says why.
- `stages` change constraint values (FreeCAD's `setDatum`) and solve
  again, each time from the original drawing.
- `informational` values are upstream's for under-constrained points,
  where any solver's answer is one of many; they are not checked.

## Coverage

Only the v1 vocabulary of `docs/language-extensions.md` (section 4.3) is
translated: points, lines, arcs and circles. Ellipses, B-splines, cubics,
text, 3D and workplane geometry, external geometry, and constraints
outside the vocabulary (equal angle, length ratio and difference,
point-plane and point-face distance, same orientation, arc-length
equality, circle-to-line and circle-to-circle distance, FreeCAD's Block)
are listed under `skipped` in each file.

FreeCAD's planegcs GoogleTest cases (`tests/src/Mod/Sketcher/App/planegcs`)
are one B-spline tangency test and one bookkeeping test, and its
SketchObject GoogleTests exercise editing and naming, not solving; none
translates.
