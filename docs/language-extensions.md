# Language extensions: constrained sketches and geometry queries

Status: complete. Every stage of section 11 is built: 0 (the flags), 1
(the solver crate), 2 (the language binding of sketches, `--enable
sketch`), 3 (diagnostics with hints, strict mode, the unknowns limit), 4
(the sketch surfaces: `check`, `measure --sketch`, the language server,
MCP, the apps' setting, `docs/sketch.md`), 5 (render-free queries:
`anchor()` and `child_anchors()`, `--enable query`), 6 (rendering
queries: `child_bounds()`, `child_measure()`, the geometry oracle,
`docs/geometry-queries.md`) and 7 (the fast path for bounds,
`child_distance()`, `snapshot --sketch`, line–arc fillets, and the
apps' settings for both extensions). Sections 11.1 to 11.6 record how
stages 2 to 7 were built and where they depart from this text; what is
left is in `docs/followups.md` ("Constrained sketches", "Geometry
queries").
Written 2026-10-07 against the tree at
`4aa80a4` and the reference checkout in `.reference/openscad`. Every
claim about this codebase cites `path:line`; claims about OpenSCAD cite
the reference checkout; claims about other projects cite what was
retrieved, or say "unverified".

The design covers two NeoSCAD extensions to the OpenSCAD language:

1. **Constrained 2D sketches** (`--enable sketch`): points, lines, arcs
   and circles tied together by geometric and dimensional constraints,
   solved numerically into an ordinary 2D shape.
2. **Geometry queries** (`--enable query`): bounding boxes,
   measurements and anchors of a module's children, available as values
   inside the module.

## Contents

1. OpenSCAD compatibility
2. How extensions are named, flagged and labelled
3. Comparison with FreeCAD, CadQuery, build123d and BOSL2
4. Constrained sketches
5. Geometry queries
6. Worked examples
7. Grammar changes
8. Changes by crate
9. Test plan
10. User-facing documentation outline
11. Staged implementation plan
12. Alternatives considered
13. Decisions
14. The questions as first asked

## 1. OpenSCAD compatibility

NeoSCAD's language is meant to be a strict superset of OpenSCAD's. For
these extensions that means:

- **Flags off, the behaviour is identical.** Without `--enable sketch`
  or `--enable query`, none of the new names exist. `sketch(...)` is an
  unknown module with OpenSCAD's own "Ignoring unknown module" warning,
  and `child_bounds()` is an unknown function with its warning, exactly
  as in the 2026.09.23 nightly. This is how `part()` already works:
  `part` is left out of the builtin table entirely when it is off, not
  registered as a disabled experiment
  (`crates/eval/src/builtins/modules.rs:117-127`).
- **The parser does not change.** Every construct below is written in
  OpenSCAD's existing syntax: module instantiations, child blocks with
  assignments (`child_statements` in `.reference/openscad/src/core/parser.y:300-313`)
  and function calls. A file parses into the same tree, prints the same
  `.ast`, and formats the same, whatever the flags. The parser takes no
  options today (`crates/lang/src/syntax/parser.rs:217`), and this design
  keeps it that way.
- **Flags on, existing files still behave the same** unless they call a
  new name they do not define. A program's own definitions shadow the new
  builtins as they shadow every builtin. This is checked, not assumed:
  `.reference/openscad/examples/Basics/roof.scad:17` defines its own
  `module sketch()`, and with `--enable sketch` it must still render its
  own module (see the test plan).
- **The output is plain OpenSCAD.** A solved sketch is a polygon and a
  query result is a number, so the `.csg` export of a model that uses
  either extension is an ordinary OpenSCAD file that the stock nightly
  renders to the same shape (section 9 tests this).

## 2. How extensions are named, flagged and labelled

A reader should always be able to tell an OpenSCAD feature from a NeoSCAD
extension:

- **Flags.** The extensions are NeoSCAD's own `--enable` names, alongside
  `part` (`crates/cli/src/main.rs:459-468`). OpenSCAD's experiments are
  `eval::Feature` (`crates/eval/src/features.rs:14-31`, mirroring
  `.reference/openscad/src/Feature.cc:28-57`). NeoSCAD's own names are a
  separate set, so `--enable all`, which in OpenSCAD means "every
  OpenSCAD experiment" (`.reference/openscad/src/openscad.cc:1084-1091`),
  still leaves them off. This is the rule `part` already follows
  (`crates/cli/src/main.rs:459-462`). The proposed names are `sketch`
  and `query`. `enable_warnings` (`crates/cli/src/main.rs:483`) skips
  them as it skips `part`. A fourth, `exact`, came later
  (`docs/audits/exact-geometry-rust.md`): it adds no names to the
  language, only the `.step`/`.stp` output suffixes (STEP with exact
  surfaces), which without it stay OpenSCAD's "Invalid suffix" error. It
  follows the same rules: not in `--enable all`, checked against
  `Feature.cc` by `crates/eval/tests/extensions.rs`, and its messages say
  "STEP export:" so they are never mistaken for OpenSCAD's. Its user
  reference is `docs/step-export.md`; the apps, `serve`, MCP and the web
  page offer it behind the same switch.
  A fifth, `fillet` (`docs/fillets.md`), adds `fillet_edges()` and
  `chamfer_edges()` on the same terms, with diagnostic codes starting
  `fillet-`; it is not advertised by `serve` until its geometry exists
  (`Extension::implemented`).
- **One extension set in the evaluator.** `eval::Options::parts: bool`
  (`crates/eval/src/lib.rs:226-230`) becomes `extensions: Extensions`,
  a bit set like `Features` (`crates/eval/src/features.rs:93-154`) with
  `part`, `sketch` and `query`. Every host passes it the way it passes
  `features` today: `session::Config`/`Run`, `serve` requests
  (`"enable": [...]`), `neoscad mcp --enable`, and the apps'
  `RunOptions`.
- **Documentation label.** Each extension builtin's entry in
  `crates/docs/builtins.toml` gets a structured field
  `extension = "sketch"` (or `"query"`, or `"part"`) instead of the
  prose prefix `part` has now (`crates/docs/builtins.toml:371-376`).
  Every surface then renders one label from that field: "NeoSCAD
  extension (`--enable sketch`); not in OpenSCAD". The surfaces are
  `neoscad docs`, LSP hover and completion detail, the MCP `docs`
  resource, and the web and app help.
- **Diagnostics.** New diagnostic codes for the extensions start with
  `sketch-` or `query-`. With the flag off, the unknown-module warning
  keeps OpenSCAD's exact text, and the JSON hint (which is not part of
  that text; `crates/session/src/diag.rs:1-9`) adds "`sketch()` is a
  NeoSCAD extension; enable it with `--enable sketch`".
- **Editor.** The editor colours sketch vocabulary only inside a sketch
  body (section 7), so a highlighted `arc` means "sketch arc", not
  BOSL2's `arc()`.

## 3. Comparison with FreeCAD, CadQuery, build123d and BOSL2

| Need | FreeCAD | CadQuery / build123d | BOSL2 | NeoSCAD (this design) |
|---|---|---|---|---|
| Constrained 2D profile | Sketcher with the planegcs solver | CadQuery `Sketch.constrain(...).solve()`, marked experimental; build123d: not checked | none (profiles are computed paths) | `sketch() { ... }` in source, solved at evaluation |
| Constraint set | `ConstraintType`: Coincident, Horizontal, Vertical, Parallel, Tangent, Distance, DistanceX, DistanceY, Angle, Perpendicular, Radius, Equal, PointOnObject, Symmetric, InternalAlignment, SnellsLaw, Block, Diameter, Weight, … | FixedPoint, Coincident, Angle, Length, Distance, Radius, Orientation, ArcAngle | — | coincident, horizontal, vertical, parallel, perpendicular, tangent, distance (with x/y), length, radius, diameter, angle, equal, on, midpoint, symmetric, fix |
| Reference to geometry | integer GeoIds picked in a GUI | string tags (`"s1"`) | named anchors | variables (`base = line(a, b)`), with names kept for messages |
| Fillet in a sketch | `SketchObject::fillet`, an edit that adds an arc and constraints, optionally keeping the corner point | `Sketch.vertices().fillet(r)` (not checked) | `round_corners()` on paths | `fillet(corner, r)`, applied after the solve; the sharp corner stays the dimensioned point |
| Driving / reference dimensions | `isDriving` flag per constraint | — | — | all constraints drive; solved values are reported by `measure` |
| Parameters | Spreadsheet and expression engine | Python variables | OpenSCAD variables | OpenSCAD variables and the customizer |
| Placement relative to other geometry | Attachment (`AttachExtension`: support, map mode, offset) | selectors (`faces(">Z")`, `edges("\|Z")`) | `attachable()`, `attach()`, `position()`, `named_anchor()` | `child_anchors()`, `child_bounds()`, `child_measure()` |
| Output | B-rep (OpenCASCADE), STEP export | B-rep (OpenCASCADE), STEP export | mesh (OpenSCAD) | mesh, as OpenSCAD. No B-rep and no STEP |

Sources for the table: FreeCAD `src/Mod/Sketcher/App/Constraint.h:52-77`
(constraint types) and `:240` (`isDriving`), `SketchObject.h:441-475`
(fillet with `preserveCorner`), `planegcs/GCS.h:62-67` (BFGS,
Levenberg–Marquardt, DogLeg), `src/Mod/Part/App/AttachExtension.h:92-95`,
and `src/Mod/Spreadsheet/App/Sheet.h:76`, all from FreeCAD `main`,
retrieved 2026-10-07. CadQuery: the Sketch and Selectors pages of
cadquery.readthedocs.io, retrieved the same day. BOSL2:
`.reference/BOSL2/attachments.scad:517` (`position`), `:961` (`attach`),
`:2433` (`attachable`), `:2675` (`named_anchor`). Unverified: build123d's
sketch constraints, CadQuery's sketch fillet API, and FreeCAD's GUI-level
"Lock" (believed to be DistanceX plus DistanceY).

What NeoSCAD matches and where it differs:

- **FreeCAD Sketcher.** Same model: points, lines, arcs and circles;
  the same core geometric and dimensional constraints; a numerical
  least-squares solver; and reports of degrees of freedom, redundant
  constraints and conflicting constraints (planegcs's
  `System::diagnose`, GCS.cpp:4773). The differences:
  - The sketch is code. Entities are named by variables, and the
    "drawing" is the guess coordinates written in the source, not
    positions stored by a GUI.
  - There are no ellipses, B-splines, Snell's law, external geometry
    or construction-mode toggling. Construction geometry is a flag on
    the entity instead.
  - Fillets are a declarative step after the solve, not an edit to the
    sketch.
  - Solving is part of evaluation, so the same source always gives the
    same profile (section 4.6). FreeCAD keeps the last solution as the
    next starting point.
- **CadQuery Sketch.** Close in spirit: constraints in code, and string
  tags as references (`.segment((0, 0), (0, 3.0), "s1")` …
  `.constrain("s1", "a1", "Coincident", None)`). NeoSCAD uses variables
  rather than strings, and statements rather than a method chain.
- **BOSL2 attachments.** BOSL2 attaches by geometry each shape declares
  analytically (`attachable(...)` takes `size`, `r`, `vnf` and so on).
  It never measures rendered geometry, and its `bounding_box()`
  approximates the box with `hull`, `projection` and `minkowski`
  (`.reference/BOSL2/miscellaneous.scad:365-400`). NeoSCAD's queries
  measure the actual child, exactly. `child_anchors()` reads anchors a
  model declares, much like `named_anchor()`, and it needs no rendering.
- **Upstream OpenSCAD.** OpenSCAD has asked for child bounds for over a
  decade:
  - issue #1088, `bounds(index)`/`$bounds` for children, closed as
    stalled;
  - issue #586, `get_center`;
  - issue #4520, `sizeof(children(1))`, closed;
  - PR #1713, `probe()`: it defined `bbsize`, `bbcenter` and `volume`
    for the later children and was closed in 2016. The maintainer's
    objections were that it needs a render (preview computes no
    geometry) and "how to fit this into the language in a way that is
    not based on magic variables".

  This design answers both objections (section 5).

## 4. Constrained sketches

### 4.1 The construct

```openscad
sketch(name = "slot") {
  c1 = point([0, 0]);                 // entities: assignments
  c2 = point([30, 0]);
  axis = line(c1, c2, construction = true);
  fix(c1);                            // constraints: statements
  horizontal(axis);
  length(axis, slot_len);
}
```

- `sketch(name, strict = false, convexity = 1)` is a builtin module. Its
  child block is the *sketch body*. `$fn`, `$fa` and `$fs` at the call
  apply to arcs and circles as they apply to `circle()`.
- **Entities are assignments.** `point`, `line`, `arc` and `circle` are
  functions that return an *entity handle*. Each call creates a new
  entity, numbered in evaluation order within the sketch. The variable
  name is kept as the entity's label for messages, and the call's span
  as its location. OpenSCAD evaluates all of a block's assignments
  before its instantiations, in order, so the numbering is fixed by the
  source.
- **Constraints are statements.** They are module instantiations that
  add equations; they produce no geometry.
- **The body is declarative.** Before the solve an entity has no
  coordinates, so the body cannot read `a.x`. Dimensions are ordinary
  expressions over parameters. Solved values come out through anchors
  (section 5.3) and `measure` (section 4.8).

Why this form:

- Nothing in it is new syntax.
- Helper code composes. A user module whose body is a `sketch()`
  contributes to the enclosing sketch when it is called from a sketch
  body, so `module rounded_corner(...) sketch() { ... }` is a reusable
  constraint pattern. Section 12 covers the alternatives that were
  rejected.

**Vocabulary scoping.** BOSL2 defines `function arc`, `module arc`,
`function circle` and `module circle`
(`.reference/BOSL2/drawing.scad:780,914`,
`.reference/BOSL2/shapes2d.scad:275,310`) and `module fillet`
(`shapes3d.scad:5175`). MCAD defines `function distance` and
`function angle` (`.reference/openscad/libraries/MCAD/utilities.scad:10,24`)
and `module chamfer` (`metric_fastners.scad:92`). Global builtins with
these names would be shadowed in any model that includes those
libraries. So the sketch vocabulary is **lexically scoped**: inside the
body of a `sketch` instantiation that resolves to the builtin, the
vocabulary names are bound before any user or library definition,
exactly as if they were declared in a scope between the body and its
surroundings. Outside sketch bodies the vocabulary does not exist (with
the flag on or off). That leaves no collisions with existing code, and
an `arc` in a sketch body always means a sketch arc. (The cost is that a
sketch body cannot call BOSL2's `arc()` function by that name. A
dimension computed with BOSL2 can be assigned to a variable outside the
body.)

### 4.2 Entities

| Constructor | Meaning | Sub-handles |
|---|---|---|
| `point([x, y])` | A free point. The coordinates are its *guess* (the drawing). If omitted, the solver places it deterministically (section 4.6), with an info diagnostic | — |
| `line(p, q, construction = false)` | A segment. `p` and `q` are point handles (shared, so coincident by identity) or `[x, y]` (a new point with that guess) | `.start`, `.end` |
| `arc(center, start, end, cw = false, construction = false)` | A circular arc, counter-clockwise from `start` to `end` unless `cw`. Adds the equation \|start − center\| = \|end − center\| | `.center`, `.start`, `.end` |
| `circle(center, r, d, construction = false)` | A full circle. `r`/`d`, if given, is a radius constraint (sugar for `radius()`) | `.center` |

Handles are a new value kind, printed as `<sketch line "base">`, with type
name "sketch entity" in messages. Two handles are equal when they name
the same entity. A handle used outside the sketch that made it is an
error (`sketch-foreign-entity`).

### 4.3 Constraints

| Statement | Applies to | Equation(s); notes |
|---|---|---|
| `coincident(a, b)` | point, point | merge, or 2 equations if a merge would cross helper boundaries |
| `on(p, c)` | point, line/arc/circle | point on the infinite line, or on the circle |
| `horizontal(l)`, `vertical(l)` | line, or two points | Δy = 0 / Δx = 0 |
| `parallel(l1, l2)`, `perpendicular(l1, l2)` | lines | cross product = 0 / dot product = 0 |
| `tangent(x, y)` | line–arc/circle, arc–arc, circle–circle | signed distance = ±r, or \|c1 − c2\| = r1 ± r2. The side and internal/external choice come from the drawing. Where the two curves share an end point (the same point, or coincident ones), the equation is written at that point instead: the radius there is perpendicular to the line, or the two radii are parallel (FreeCAD's endpoint tangency, SolveSpace's `ARC_LINE_TANGENT`). The distance form holds there only to second order, so a fully constrained slot (section 6.2) would report a free DOF |
| `distance(a, b, d, along)` | point–point, point–line, line–line (parallel) | `along = "x"`/`"y"` for FreeCAD's DistanceX/DistanceY |
| `length(l, d)` | line | |
| `radius(c, r)`, `diameter(c, d)` | arc, circle | |
| `angle(l1, l2, deg)` | lines | signed, counter-clockwise from `l1` to `l2`; also `angle(a, deg)` for an arc's sweep |
| `equal(x, y)` | lines (lengths), arcs/circles (radii) | |
| `midpoint(p, l)` | point, line | |
| `symmetric(p, q, about)` | points, about a line or a point | |
| `fix(p, at)` | point, or a line's two points | `at` defaults to the guess |
| `fillet(corner, r)`, `chamfer(corner, d)` | a point shared by exactly two lines | applied after the solve (section 4.5) |
| `anchor(name, e)` | any entity | export a solved point for `child_anchors()` (section 5.3) |

Every residual is written with `+`, `−`, `×`, `÷` and `sqrt` alone.
`angle` multiplies by `cos θ` and `sin θ` of its parameter, computed
once per solve. There is no `atan2` in the solve loop (section 4.6).

### 4.4 From solution to shape

1. **The profile is every non-construction curve.** Curves are joined
   at shared points into a graph. Each closed loop is a path: a chain of
   lines and arcs whose every point is used by exactly two profile
   curves, or a circle on its own.
2. **Loops become polygon paths and fill even-odd**, the way OpenSCAD
   fills `polygon()` with several paths. OpenSCAD sanitizes with an
   even-odd union (`.reference/openscad/src/geometry/ClipperUtils.cc:153-169`),
   as NeoSCAD does (`crates/geom/src/clipper.rs:118-129`). So a circle
   inside an outline is a hole, whatever the direction of either. A
   loop that crosses another loop gets a `sketch-self-intersection`
   warning, because even-odd then gives a shape the author probably did
   not mean.
3. **Arcs and circles are tessellated like `circle()`.** The segment
   count is `circular_segments_for_angle(r, sweep)`
   (`crates/geom/src/fragments.rs:24-50`), which is
   `getCircularSegmentCount(r, angle)`, so a sketch circle has the same
   vertices as `circle(r)`. An arc's two endpoints are the solved point
   coordinates themselves, not recomputed with trigonometry, so the
   shared vertex between an arc and the next line is bit-identical and
   every loop closes exactly. The inner vertices are taken at evenly
   spaced angles, as `circle()` takes them
   (`crates/geom/src/primitives.rs:252-260`).
4. **The node prints as a polygon.** It is a new
   `NodeKind::Sketch(Box<SketchNode>)` holding exactly what a `polygon`
   node holds (points, paths, convexity) plus an
   `Arc<sketch::Report>` for `check`, `measure` and the LSP. The `.csg`
   dump and the geometry key (`crates/eval/src/dump.rs:16-35`) write it
   as `polygon(points = ..., paths = ..., convexity = ...)`, so it shares
   cache entries with an identical polygon, and a `.csg` export runs in
   stock OpenSCAD. `geom` builds it through the polygon path.

The result is an ordinary 2D shape. `linear_extrude`, `rotate_extrude`
(profile at x ≥ 0, as always), `offset` and the 2D booleans take it as
they take any polygon.

### 4.5 Fillets and chamfers

`fillet(corner, r)` is not solved. After the solve, for a corner point
`P` joining lines with unit directions `u` and `v` (pointing away from
`P`):

- the trim distance is `t = r·(1 + u·v)/|u × v|`;
- the arc runs from `P + t·u` to `P + t·v`, around the centre
  `P + t·u + r·n`, where `n` is the inward normal.

All of this is plain arithmetic, with no trigonometry.

- If `t` is longer than either line, the result is an error
  (`sketch-fillet-too-large`) that gives the needed and available
  lengths.
- The corner point stays in the sketch as the *virtual sharp*, which is
  what dimensions refer to. FreeCAD's `preserveCorner` option keeps such
  a point too (`SketchObject.h:441-475`).
- Doing this after the solve keeps fillets from adding unknowns, which
  is the most common source of solver trouble in GUI sketchers.
- `chamfer(corner, d)` works the same way with a straight cut.
- Fillets between a line and an arc came in stage 7 (section 11.6),
  and between two arcs after it (section 11.7).

### 4.6 The solver

**The problem.** Each point contributes two unknowns, x and y. Arcs and
circles add no unknowns beyond their points, except a circle's radius.
Residuals are evaluated in entity order and Jacobians are analytic.
Sketches are small: tens to a few hundred unknowns.

**Survey.**

| Option | Licence | Vs GPL-2.0-or-later | Rust | WASM (`wasm32-unknown-unknown`) |
|---|---|---|---|---|
| FreeCAD planegcs | LGPL-2.1-or-later (GCS.cpp SPDX line) | Compatible: LGPL code can be combined into a GPL work | No Rust port. C++ with Eigen, Boost.Graph (`GCS.cpp:106-109`), `std::async` threads in `diagnose` (`GCS.cpp:4938`) and FreeCAD `Base` headers; standalone extractions exist (PlaneGCS, `@salusoft89/planegcs` via Emscripten) | Only through Emscripten. NeoSCAD's WASM build is `wasm32-unknown-unknown` with no C++ runtime, and the workspace's one C dependency is mimalloc, natively only (`docs/architecture.md`, Stack) |
| SolveSpace (libslvs) | GPLv3 per the SolveSpace site; "or later" unverified | Linking makes the combined binary GPLv3. Allowed by "or later", but it changes what NeoSCAD's binaries are licensed under | `slvs` crate 0.6.0, GPL-3.0, bindgen (needs libclang) | Not in that crate; unverified |
| Young Rust crates | `solverang` 0.1.0 Apache-2.0; `arael-sketch-solver` 0.8.3 MIT; `vcad-kernel-constraints` 0.10.0 MIT; `brepkit-sketch` 4.1.37 **AGPL-3.0-only** (incompatible) | the permissive ones are fine | yes | not checked |
| Own solver | GPL-2.0-or-later | — | — | yes, plain Rust |

Crate licences and versions are from the crates.io API, 2026-10-07. The
permissive crates were created in 2026 and have few downloads; none was
checked for determinism, diagnosis quality or maintenance.

**Recommendation: write our own** in a new library crate,
`crates/sketch`, with no dependencies except possibly the `libm` crate
(below). The reasons:

- Determinism has to hold bit for bit on aarch64, x86_64 and wasm32.
  That rules out solvers with threads (planegcs's `std::async`), SIMD
  runtime dispatch (`faer`-style kernels), or reductions whose order
  depends on the CPU.
- The diagnostics must speak in source terms: spans, variable names,
  and fix hints.
- The problem sizes make dense linear algebra fine. A 200 × 200 QR is
  about 5 million flops per iteration.
- planegcs's design is the reference for the approach, which is
  well-trodden: Levenberg–Marquardt/dogleg on the least-squares
  residual, and QR of the Jacobian for DOF and dependency analysis
  (`System::diagnose`, GCS.cpp:4773). We port the behaviour, not the
  code.

As built (stage 1), the crate is the package `sketch-solver`, licensed
`MIT OR Apache-2.0` (the owner's decision of 2026-10-07) so that it can
be published on its own and NeoSCAD can depend on the published crate.
It depends on no NeoSCAD crate. Its diagnostics are plain data keyed by
entity and constraint ids (`src/report.rs` in the solver's repository);
the `eval` binding (stage 2) maps the ids to spans, names and hints. The
validation corpus translated from FreeCAD and SolveSpace tests is kept
out of the published package (`corpus/README.md` there).

**Where the solver lives.** Since 0.1.0 (2026-10-07) the crate is
developed in its own repository, github.com/neoscad/sketch-solver, and NeoSCAD depends
on the release published on crates.io (`sketch-solver` in the root
`Cargo.toml`'s `[workspace.dependencies]`, used by `eval` and
`wasm-check`). The solver's unit, worked-example, completion and fuzz
tests and the GPL/LGPL-derived validation corpus with its SolveSpace
differential oracle (`corpus/`, a never-published workspace member)
live and run in CI there. A solver change therefore flows:

1. change, test and review it in github.com/neoscad/sketch-solver (its CI runs the
   unit tests, the corpus and a wasm32 build on every platform);
2. publish a new version to crates.io from there;
3. bump `sketch-solver` here (`cargo update -p sketch-solver`, and the
   version in `[workspace.dependencies]` for a minor or major release),
   then run `cargo test` (the sketch goldens in
   `crates/session/tests/sketch.rs` and the eval tests) and
   `scripts/wasm-check.sh`.

The wasm-check case `sketch-solver-determinism`
(`crates/wasm-check/cases.json`, `src/sketches.rs`) stays here: it
solves 200 generated sketches natively and in wasm32 and compares a
digest of every solved bit, so a release that changes any solution
shows as a changed digest. A bump that changes the digest on purpose
updates the expected digest in `cases.json` in the same commit and
says why; one that changes it unexpectedly is a solver regression to
fix upstream.

**Algorithm.**

1. **Decompose.** Split the unknowns and constraints into connected
   components of their bipartite graph, ordered by lowest entity id, and
   solve each component separately.
2. **Clean the drawing.** Solve with every dimensional constraint's
   target replaced by its value measured on the guesses, so only the
   geometric constraints (horizontal, tangent, and so on) move points.
   Use minimal-norm Gauss–Newton steps, so unconstrained directions keep
   their guessed values. Record the drawing's *orientation signature*:
   which way each corner between two lines turns, which side of 180°
   each arc sweeps, which side of a line an endpoint-tangent arc lies on
   (and whether two endpoint-tangent arcs touch inside or outside), that
   angle constraints are met on their own branch rather than 180° away,
   and that circle radii are positive. Point–line distances and tangents
   written as distances need no entry: their residuals are signed, with
   the side read from the drawing. If the cleaning does not converge, or
   itself flips the signature, start the next steps from the drawing
   instead: the drawn dimensions need not be consistent with the
   geometric constraints when the drawing does not meet them, and the
   least-squares compromise is a poor start (found by the differential
   oracle, below).
3. **Solve.** Run damped Gauss–Newton (Levenberg–Marquardt with a
   fixed damping schedule) from the cleaned drawing to the real targets.
   Stop when the scaled residual is at most 1e-10 times the sketch size,
   or at a fixed iteration cap (100). Then take up to four undamped steps
   while each halves the cost, which brings the result from 1e-10 of the
   size to rounding level.
4. **Check the branch.** If the result's orientation signature differs
   from the drawing's, step 3 did not converge, or step 3 had to take
   back a step (the linear model failed on the way, which is where a
   solve can jump branches without any recorded sign changing), use
   **continuation** instead: move the dimension targets from the measured
   values to the real ones in 2ᵏ steps, halving on failure down to a
   fixed floor (1/1024, and 500 iterations in all). Each step starts from
   the previous one and must converge without flips. This tracks the
   branch the drawing is on through large parameter changes, which is
   the "flip" problem. If continuation fails, the direct result stands,
   with its flips reported.
5. **Exactify.** Snap what has a closed form, in constraint order: a
   horizontal line's second y becomes the first's exactly, coincident
   points become the same bits, and fixed points become their values.
   Then re-check the residual. Without this, profiles carry `1e-17`
   noise into the `.csg` and the echo output. (As built: the equal
   coordinates form classes, each taking a fixed value if one of its
   members has one, else its lowest unknown's; a snap that would leave a
   residual over the tolerance is undone.)

**DOF and diagnosis.** Run Householder QR on the Jacobian at the
solution, with a rank tolerance relative to the largest pivot. (As
built: one QR of Jᵀ, its rows normalised to unit length and taken in
constraint order without pivoting, gives the rank, the null space and the
dependent rows at once; taking them in order means a dependent row is
always reported against earlier ones.)

- **Free DOF** is the number of unknowns minus the rank. The null space
  maps back to entities, so the report can say "g2 can still move along
  y".
- **Dependent rows** come from QR of Jᵀ. If the residual converged, they
  are *redundant*. If it did not, they are a *conflict* set; the
  smallest dependent group is reported. (As built: a dependent row whose
  residual is not the same combination of the earlier rows' residuals is
  a conflict, the others are redundant, whether or not the solve
  converged.)
- A sketch with nothing fixed has three rigid DOF, which is not an
  error: minimal-norm steps leave it where it was drawn.

**Determinism.** Output is byte-identical at any thread count and the
same on every platform for the solve:

- The solver is single-threaded. Each sketch is one call, inside
  single-threaded evaluation (`docs/architecture.md`, "Determinism").
- It uses only IEEE `+ − × ÷ sqrt`, which are correctly rounded
  everywhere. It never calls `eval::fma`, whose `mul_add` fuses on
  aarch64 only, to match OpenSCAD's builds
  (`crates/eval/src/fma.rs:1-29`; `docs/followups.md:683-699`). Sketches
  have no OpenSCAD output to match, so they take the platform-neutral
  choice. Rust does not contract floating-point operations on its own.
- The `cos θ`/`sin θ` of angle parameters, and the guess placement for
  points without guesses, use the pure-Rust `libm` crate on every target
  instead of the platform's maths library. Rust's wasm32 maths differs
  from macOS libm in the last bit (`docs/followups.md:1663`), and a
  last-bit difference in an angle target would move every downstream
  coordinate.
- Termination depends only on these values, so the iteration count is
  the same everywhere.
- **No history.** The solver never starts from a previous run's
  solution, not even in a warm session. Starting from history would make
  a warm session's output depend on what it rendered before, and that
  breaks "warm equals cold" (`docs/architecture.md`, "Determinism";
  `crates/session/tests/warm_export.rs`). Stability under parameter
  changes comes from the guesses in the source, plus continuation (step
  4), plus "pin the drawing" (section 4.8).
- Tessellation (section 4.4) uses `io::trig` like `circle()`, so its
  inner arc vertices share `circle()`'s platform behaviour, no more and
  no less.

**Limits.** The solve polls the evaluator's interrupt flag between
iterations. A new count limit, `Limits::sketch_unknowns` (default 5,000
under `Limits::AGENT`), stops a generated sketch before an O(n³)
factorisation. Time and memory are covered by the existing limits
(`docs/architecture.md`, "Resource limits").

### 4.7 Diagnostics

Each diagnostic has a stable code, NeoSCAD's own text in OpenSCAD's
`WARNING: ... in file X, line N` form, the span of the constraint or
entity it is about, and hints in the existing JSON shape (`hints[]`, with
`replace` where a concrete edit is known;
`crates/session/src/diag.rs:41-71`). The codes are new `DiagCode`
variants (`crates/lang/src/diag.rs:54`).

| Code | Severity | Example text and hint |
|---|---|---|
| `sketch-conflict` | error | "Sketch 'slot': constraints conflict: length(axis, 30) at line 6, distance(c1, c2, 25) at line 9". Hint: "remove one, or make the values agree" |
| `sketch-redundant` | warning | "Sketch 'slot': horizontal(top) at line 8 is implied by the other constraints". Hint: delete it (`replace` with empty text) |
| `sketch-underconstrained` | info; error with `strict = true` | "Sketch 'gusset' has 1 free degree of freedom: g2 can move along y". Hint: "add `distance(o, g2, …, along = "y")` or `fix(g2)`" (from the null-space direction) |
| `sketch-no-convergence` | error | "Sketch 'slot' did not converge (residual 3.2e-4 after continuation); closest constraints: tangent(e2, top)". Hint: "check the guesses: the drawing may be far from any solution" |
| `sketch-flipped` | warning | "Sketch 'slot': tangent(e1, top) solved on the other side than drawn". Hint: "update the guesses (pin the drawing)" |
| `sketch-open-profile` | error | "Sketch 'gusset': line 'hyp' end is not joined to another profile curve". Hint: "share the point, or mark the line `construction = true`" |
| `sketch-fillet-too-large` | error | "fillet(g1, 30) needs 41.2 along 'top_a', which is 22 long" |
| `sketch-unknown-entity` | error | a constraint given something that is not a handle. "Did you mean" suggests entity variables in scope |
| `sketch-foreign-entity` | error | a handle used outside its sketch |
| `sketch-geometry-in-body` | error | `circle(5);` as a statement in a body. Hint: "inside a sketch, write `c = circle(center, r = 5);`" |

A failed sketch emits an empty shape and the error, so the rest of the
model still evaluates. An evaluation error elsewhere stops evaluation as
OpenSCAD's errors do.

### 4.8 Presentation: check, measure, LSP, MCP, snapshot

- **`check`.** Sketch findings appear among the diagnostics. A
  `sketches` array in the JSON (`docs/cli-json.md`) lists, per sketch,
  its name, span, unknowns, rank, free DOF, iterations, final residual,
  whether continuation ran, and its status.
- **`measure --sketch NAME`** (and the MCP `measure` tool's `sketch`
  argument) returns every named entity's solved values: point
  coordinates, line lengths and angles, arc radii and sweeps. These are
  the reference dimensions FreeCAD shows as non-driving constraints.
  This is how an agent reads a solved sketch without rendering it.
- **LSP.**
  - The server's `Options` has no feature set today
    (`crates/lsp/src/lib.rs:82-92`). It gains the `Extensions` set from
    the host.
  - *Completion* offers the vocabulary only inside sketch bodies, with
    snippets (`horizontal(${1:line});`), and entity variables for
    handle parameters.
  - *Hover* on an entity variable shows the solved values from the
    document's last run. On a constraint it shows satisfied, redundant
    or conflicting, and its residual. On `sketch(` it shows the DOF
    summary. The values travel with the run's diagnostics, keyed by
    span, through the same path as `lsp::Options::host_diagnostics`.
  - The *code action* "Pin drawing" rewrites each `point(...)` guess
    (and line endpoint literals) to the solved coordinates, rounded to
    the sketch's precision. After that, later parameter edits start from
    the current shape. Since the change lives in the source it is
    deterministic, which a hidden warm start would not be.
- **MCP.** Sketch diagnostics come back from `check`; the `docs`
  resource has the vocabulary with the extension label, and `recipes`
  gains a sketch recipe. Agents benefit most from the structured
  "free DOF → suggested constraint" hints and from `measure --sketch`.
- **Snapshot** (stage 7, section 11.6): `snapshot --sketch NAME` draws
  the solved sketch flat, with entity labels, construction lines dashed,
  and under-constrained entities highlighted.

## 5. Geometry queries

### 5.1 Why it is hard, and the evaluation order

Values exist before geometry:

- `eval` turns the program into a node tree (`crates/eval/src/lib.rs`
  module docs).
- `geom` renders that tree afterwards, with a cache keyed by each
  subtree's Merkle hash (`crates/geom/src/evaluate.rs:19-35`).
- `session` and the one-shot CLI compute the keys only after evaluation
  (`crates/session/src/lib.rs:1716`, `crates/cli/src/run.rs:653`).
- `geom` depends on `eval`, and not the other way round
  (`crates/geom/Cargo.toml`). The evaluator cannot call the kernel.

A query therefore needs **staged evaluation**: evaluation pauses,
instantiates a subtree, has it rendered, and continues with numbers.

### 5.2 The construct: queries about a module's own children

```openscad
module plate_for(margin = 4) {
  b = child_bounds(0);           // [[x0, y0, z0], [x1, y1, z1]] of children(0)
  ...
  children(0);
}
```

- `child_bounds(i)` is the axis-aligned box `[[min], [max]]` of what
  `children(i)` would produce at this point. It is 2D or 3D to match
  the child, has the same format as BOSL2's `pointlist_bounds`, and is
  `undef` with a warning when the child is empty. With no index it is
  the box of all children, as `children()` is all of them.
- `child_measure(i)` is an object: `dim`, `empty`, `bounds`, `size`,
  `center`, plus `area` for 2D or `volume` and `surface_area` for 3D.
- `child_anchors(i)` is an object mapping anchor names to `[point,
  direction]` (section 5.3).
- `child_distance(i, j)` (stage 7, section 11.6): the exact distance
  between two children, reusing `measure`'s BVH search
  (`crates/session/src/measure.rs:1-17`).

These are functions, valid only inside a user module body (anywhere
else they warn and return `undef`, like `children()` at the top level).
They answer PR #1713's objections:

- **No magic variables.** They are ordinary functions whose results go
  in ordinary variables. They are scoped to the module, which is
  OpenSCAD's unit of abstraction (an "operator module"), and they need
  nothing new in the grammar.
- **A render is fine, and has the same semantics in preview.** A query
  always measures the child as F6 renders it, in preview too: `%`
  children are excluded and `#` included, as in a render. Otherwise a
  model would change shape between preview and render.

**Semantics.** The child is instantiated as `children(i)` would be at
the query's position: the module's frame (transforms in the body around
a later `children()` do not apply) and the `$` variables in effect at
that point.

- **The instantiation is a sandbox.** Its echo and warning output is
  held back; `children(i)` prints it when the child is really
  instantiated. The `rands()` state and the node index counter
  (`crates/eval/src/node.rs:217-218`) are restored afterwards, so adding
  a query never changes anything else's output.
- **The result is kept for reuse.** A later `children(i)` in the same
  context reuses it rather than evaluating the child again, using the
  call memo's "same inputs, same `$` reads" rule
  (`crates/eval/src/callmemo.rs`). A nested query therefore costs one
  evaluation per level, not 2ⁿ.
- **Errors propagate.** An error in the child stops evaluation as it
  would in `children(i)`.

### 5.3 Anchors: queries that need no rendering

`anchor(name, point, dir = undef)` is a statement valid anywhere with
`--enable query`. Inside a sketch body it takes an entity. It records a
named point in the current frame.

- **Storage.** Anchors are kept in a side field on the node of the
  enclosing instantiation: `Option<Box<[Anchor]>>`, which is `None` for
  every node OpenSCAD can produce. They are never a child node, because
  an empty child changes 2D unions (the "empty sibling" rule in
  `docs/architecture.md`, "Determinism"). `geom`, `dump` and `Keys`
  never read the field, so geometry, `.csg` and cache keys are
  untouched by construction.
- **Lookup.** `child_anchors(i)` walks the child's node subtree,
  composing transform matrices (the same `eval::fma` matrix products
  the tree already uses), and collects the anchors. No rendering is
  involved; the cost is the size of the subtree.
- **Sketches.** A sketch exports, as anchors, every entity bound by an
  assignment directly in its own body, under the variable name (a
  line's anchor is its midpoint and direction), plus any explicit
  `anchor()`. This is how solved sketch coordinates become values
  elsewhere in the model.

Anchors are the part of the BOSL2 attachment model that fits plain
OpenSCAD: named points that move with their geometry. Attaching by
anchor (`attach()`-style orientation) can then be written as an
ordinary user module.

### 5.4 Computing the answer: the geometry oracle

`eval::Options` gains
`geometry: Option<Arc<dyn GeometryOracle + Send + Sync>>`, a trait
defined in `eval` and implemented in `session` over the session's
`geom::Renderer`. This is the same pattern as `Options::fs`
(`crates/eval/src/lib.rs:214-217`). The oracle takes a node subtree and
the evaluation's limits and returns facts or an error. With no oracle
(a host that does not render), a query warns `query-unavailable` and
returns `undef`. Every NeoSCAD host renders, so this happens only in
library tests.

- **Cache.** The oracle computes the subtree's `Keys` and renders
  through the shared `Renderer`. Because the key is a Merkle hash of the
  subtree alone (`crates/eval/src/dump.rs:16-35`), the final render
  finds the same subtree in the cache and does not compute it twice.
- **IDs and warm equals cold.** Original-ID blocks are reserved per
  subtree key (`crates/geom/src/evaluate.rs:204-222`), and a cache hit's
  IDs are rebased (`docs/architecture.md`, "Determinism"). Query values
  are bounds and measures that do not depend on IDs. Whether a query
  render's cached entries keep the final export byte-identical is to be
  *proved* by tests: cold, warm, and with the query cache disabled
  (section 9). If they do not, the oracle renders into a scratch
  `Renderer`, at the cost of computing the subtree twice.
- **Values are deterministic.** Bounds are min and max over the
  rendered mesh's vertices. Area and volume are serial sums in mesh
  order over the canonical mesh `session::mesh` builds, not a parallel
  reduction.
- **Fast path.** For a subtree of primitives, transforms, unions and
  hulls, the box of the leaves' generated vertices under their matrices
  is the result's box. The fast path may be used only where a
  differential test shows it bit-identical to the rendered answer.
  Otherwise the oracle renders. (Built in stage 7, more narrowly than
  this: hulls in 2D only, and no transform above a union; section 11.6.)
- **Limits.** Query renders count against the request's time, memory
  and triangle limits like any render. A new count,
  `Limits::queries` (default 10,000 per evaluation under
  `Limits::AGENT`), stops runaway patterns with a `resource-limit`
  diagnostic. Recursion is bounded by the existing depth limit, since
  queries are evaluated inside module calls.
- **Memoisation.** A call containing a query is a pure function of its
  inputs, because geometry is a function of the subtree. Imports inside
  the queried child already mark the call untracked
  (`crates/eval/src/eval.rs:1267`, `callmemo.rs:51-52`), so the
  statement and call memos need no new rule beyond including
  `extensions` in their fingerprint.
- **WASM.** The worker renders on its single thread. Query renders are
  ordinary renders under the same memory probe
  (`docs/architecture.md`, "Resource limits"). Nothing is
  platform-specific.
- **Errors.**
  - `query-empty`: the child has no geometry; the result is `undef`.
  - `query-outside-module`: the query is not inside a user module.
  - `query-index`: the index is out of range, with `children()`'s
    text.
  - `query-unavailable`: no oracle.
  - A render failure inside the child is reported at the query's span.
    Its own geometry warnings are printed once, by the final render.

## 6. Worked examples

None of these has been run, because nothing is implemented. The DOF
counts were checked by hand.

### 6.1 L-bracket gusset profile with tangent fillets

```openscad
// [Bracket]
leg_a  = 40;   // [20:80]
leg_b  = 30;   // [20:80]
t      = 4;    // [2:8]
gusset = 18;   // [8:30]
r      = 3;    // [1:6]
width  = 12;

linear_extrude(height = width)
sketch(name = "gusset", $fn = 48) {
  o  = point([0, 0]);
  a  = point([leg_a, 0]);
  a2 = point([leg_a, t]);
  g1 = point([t + gusset, t]);      // gusset foot on the horizontal leg
  g2 = point([t, t + gusset]);      // gusset foot on the vertical leg
  b2 = point([t, leg_b]);
  b  = point([0, leg_b]);

  bottom = line(o, a);
  end_a  = line(a, a2);
  top_a  = line(a2, g1);
  hyp    = line(g1, g2);
  in_b   = line(g2, b2);
  end_b  = line(b2, b);
  back   = line(b, o);

  fix(o);
  horizontal(bottom); length(bottom, leg_a);
  vertical(end_a);    length(end_a, t);
  horizontal(top_a);
  vertical(in_b);
  horizontal(end_b);  length(end_b, t);
  vertical(back);     length(back, leg_b);
  distance(o, g1, t + gusset, along = "x");
  distance(o, g2, t + gusset, along = "y");

  fillet(g1, r);      // tangent arcs where the gusset meets each leg
  fillet(g2, r);
}
```

There are 14 unknowns (7 points) and 14 equations: `fix` (2), six
horizontal/vertical (6), four lengths (4) and two distances (2). Each
point follows from the ones before it, so the rank is 14 and the sketch
is **fully constrained** (0 DOF). The sign choices (whether `a` lies at
+x or −x, and so on) come from the drawing. Delete `length(end_b, t)`
and the report becomes "1 free degree of freedom: b2 and g2 can move
along x", with the hint `distance(o, g2, t, along = "x")`, which is the
under-constrained case section 4.7 describes.

### 6.2 Slot with explicit tangent arcs

```openscad
slot_len = 30;
slot_w   = 8;

linear_extrude(3)
difference() {
  square([50, 20], center = true);
  translate([-slot_len / 2, 0])
  sketch(name = "slot") {
    c1   = point([0, 0]);
    c2   = point([slot_len, 0]);
    axis = line(c1, c2, construction = true);
    top  = line([0, slot_w / 2], [slot_len, slot_w / 2]);
    bot  = line([slot_len, -slot_w / 2], [0, -slot_w / 2]);
    e1   = arc(c1, top.start, bot.end);   // left cap, counter-clockwise through 180°
    e2   = arc(c2, bot.start, top.end);   // right cap, counter-clockwise through 0°

    fix(c1);
    horizontal(axis); length(axis, slot_len);
    tangent(e1, top); tangent(e1, bot);
    tangent(e2, top); tangent(e2, bot);
    diameter(e1, slot_w); equal(e1, e2);
  }
}
```

Counting: `c2` is fixed by the axis. The remaining 8 unknowns (the four
line endpoints) meet 8 equations: two arc-internal equalities, the
diameter, `equal`, and four tangencies. Each tangency at a shared
endpoint has two solutions (outer or crossing tangent). The drawing
picks the outer ones, and the orientation signature check (section 4.6,
step 4) keeps them when `slot_w` changes.

### 6.3 Gear plate: a hole positioned from a child's bounding box

```openscad
use <gears.scad>   // any module producing a gear

// A base plate under its child, with a fixing hole 6 mm beyond the
// child's +x edge, centred in y.
module plate_for(margin = 4, thick = 3, hole_d = 5) {
  b  = child_bounds(0);                 // forces the child's geometry
  lo = b[0];
  hi = b[1];
  cy = (lo[1] + hi[1]) / 2;
  difference() {
    translate([lo[0] - margin, lo[1] - margin, -thick])
      cube([hi[0] - lo[0] + 2 * margin + 12, hi[1] - lo[1] + 2 * margin, thick]);
    translate([hi[0] + 6, cy, -thick - 1])
      cylinder(d = hole_d, h = thick + 2, $fn = 32);
  }
  children(0);                          // reuses the queried instance
}

plate_for() translate([10, 5, 0]) gear(teeth = 17);
```

The `.csg` export contains a `cube` and a `cylinder` with literal
numbers, so it renders in stock OpenSCAD. With a sketch child,
`child_anchors(0)` would give named solved points instead of the box.

## 7. Grammar changes

**None are required, for either feature.** The brief mentions a
tree-sitter grammar; the editor's grammar is actually Lezer
(`apple/Editor/web/src/lang/openscad.grammar`), and it needs no rule
changes either. What does change:

- `apple/Editor/web/src/lang/builtins.js` colours builtins by kind with
  decorations over the syntax tree (`builtins.js:1-10`). It gains a
  `sketch` list that applies only to names inside a `ModuleCall` whose
  name is `sketch`, and the query functions under "function".
  `npm test` and the corpus run (0 error nodes) cover it.
- The formatter, the `.ast` dump, fragments, and the parse caches are
  untouched.

If the owner later wants queries at the top level without a wrapper
module (question 3), that needs real syntax, such as a geometry
expression. It would then touch `crates/lang/src/syntax/parser.rs`,
`ast.rs` (a new `ExprKind`), `dump.rs` (the `.ast` form), `crates/fmt`,
the Lezer grammar and its corpus test, and the LSP's syntax walks. It
would also have to keep OpenSCAD's exact parser error with the flag
off, which pushes the flag into the parser and its caches. That cost is
why this design avoids it.

## 8. Changes by crate

| Crate | Change |
|---|---|
| `sketch` (new library, package `sketch-solver`, `MIT OR Apache-2.0`; the solver core, built in stage 1, is everything up to exactify and the diagnosis, and fillets, loops and tessellation may live in `eval` instead so that the published crate stays a solver) | Entity and constraint model; residuals and analytic Jacobians; LM/Gauss–Newton with continuation; pivoted QR, DOF, redundancy and conflict sets; exactify; fillet and chamfer; loop finding; arc tessellation; a `Report`. No `std::fs`, env or clock; wasm-clean; optional `libm` dependency |
| `io` | `circular_segments[_for_angle]` moves here from `geom/src/fragments.rs` (with a re-export in `geom`), so `sketch` and `geom` share one rule. Degree trigonometry already lives in `io` for the same reason (`crates/eval/src/lib.rs:65-67`) |
| `lang` | New `DiagCode`s (`sketch-*`, `query-*`). No syntax change |
| `eval` | `Extensions` set replacing `Options::parts`; the `sketch` builtin module and its lexically scoped vocabulary (a body region in `resolve` that binds vocabulary names first); an entity-handle `Value` variant (type name, printing, equality, members `.start`/`.end`/`.center`); `NodeKind::Sketch`, dumped and keyed as `polygon`; an anchor side field on `Node`; `child_bounds`/`child_measure`/`child_anchors`; the `GeometryOracle` trait and `Options::geometry`; sandboxed child instantiation with reuse; `Limits::sketch_unknowns` and `Limits::queries`; the extension bits in memo and callmemo fingerprints |
| `geom` | Treat `NodeKind::Sketch` as a polygon. Nothing else: the oracle lives in `session` |
| `session` | The oracle over the session's `Renderer`; sketch facts in `check` JSON; `measure --sketch`; run results carry sketch facts by span for hover |
| `cli` | `--enable sketch`/`query` (skipped by `enable_warnings`, left out of `all`); `run.rs` creates its `Renderer` before evaluation (now at `crates/cli/src/run.rs:609`) and passes an oracle; `measure --sketch` |
| `lsp` | `Extensions` in `Options`; context-aware completion and snippets; hover with solved values; the "Pin drawing" code action |
| `docs` | `extension = "..."` field and entries for every new builtin; the docs test (`crates/session/tests/docs.rs`) evaluates their examples with the flag on |
| `client`, `ffi`, `web`, `linux-app` | No new plumbing: they already pass `enable` lists. The panels show the sketch report later |
| `apple/Editor/web` | Decorations (section 7) |
| `conformance` | New golden suite (section 9) |

## 9. Test plan

1. **Flags off, nothing changes.** `conformance run` passes
   `baseline.json` unchanged. Every stage's diff runs the full suite.
2. **Flags on, nothing changes either.** The full suite is run again
   with `--enable sketch,query,part` added, and must give the same
   results. The harness gains an `--extra-enable` option for this.
   `examples/Basics/roof.scad` defines its own `module sketch`, but all
   7 of its manifest cases are skipped because they need `roof`
   (`conformance/manifest.json`). So a dedicated test evaluates its
   `sketch` module with and without `--enable sketch` and requires the
   same `.csg`.
3. **Golden tests** in a new `conformance/extensions/` (inputs and
   expected `.csg`, `.echo` and diagnostics JSON), run by `conformance
   run --tier ext`, not mixed into OpenSCAD's manifest:
   - every constraint kind, alone and combined;
   - holes (circles in loops, nested loops), and arcs at many
     `$fn`/`$fa`/`$fs` values (vertex counts equal to `circle()`'s);
   - every diagnostic code, with spans;
   - the three worked examples;
   - parameter sweeps that cross former flip points (the slot width,
     the gusset size) with no branch change;
   - queries on 2D and 3D children, empty children, `%`/`#` children,
     nested queries, recursive modules, and anchors through transforms.
4. **Byte-identical `.csg` in stock OpenSCAD.** The `.csg` export of
   each golden model is rendered by the nightly
   (`--backend=manifold`), and its mesh must equal NeoSCAD's from the
   source (the existing tier 3 comparison).
5. **Determinism.**
   - Solver outputs are hashed for 200 generated sketches and must be
     identical on aarch64, the x86_64 CI job, and wasm32
     (`scripts/wasm-check.sh`).
   - Query models are rendered at 1, 2 and 8 threads.
   - Warm and cold exports are compared (`warm_export.rs`): a model with
     queries rendered cold, after unrelated edits, and with the query
     cache disabled.
   - Incremental evaluation is compared with fresh evaluation
     (`crates/eval/tests/incremental.rs` gains extension programs).
6. **Solver unit tests** in the solver crate (now github.com/neoscad/sketch-solver): known closed-form
   configurations; rank and DOF on textbook cases; conflict-set
   minimality; exactify; and fillet geometry, checked against
   analytical arcs.
7. **Fuzzing, bounded.** Random sketches with an entity cap and
   `Limits` on every evaluation, under the 2 GB process guard. It must
   never panic, and must either converge or report a diagnosis.
8. **Fast path vs render.** A differential test over generated trees.
   The fast path is enabled only for node kinds where every case agrees
   bit for bit.
9. **Surfaces.** LSP completion and hover tests (`crates/lsp`), MCP
   `check`/`measure` JSON tests, the docs examples test, and the editor
   corpus.

## 10. User-facing documentation outline

This is plain reference documentation. The headings use the terms
readers look for.

- **"NeoSCAD extensions to the OpenSCAD language"**
  (`docs/extensions.md`): the compatibility statement (section 1); the
  list of extensions with their `--enable` names (`part`, `sketch`,
  `query`); how to tell them apart (the label, `neoscad docs`, hover);
  and how to export a plain OpenSCAD file (`.csg`).
- **"Constrained sketches (2D constraint solver)"** (`docs/sketch.md`):
  - Quick start;
  - Entities;
  - Constraints, one heading per constraint name (coincident,
    horizontal, vertical, parallel, perpendicular, tangent, distance,
    length, radius, diameter, angle, equal, point on curve, midpoint,
    symmetric, fix);
  - Fillets and chamfers;
  - Profiles, holes and construction geometry;
  - Parameters and the customizer;
  - Degrees of freedom and diagnostics, one entry per code;
  - Guesses, flips and "Pin drawing";
  - "Comparison with the FreeCAD Sketcher": a factual table of
    constraint names and behaviour differences, from section 3;
  - Comparison with CadQuery sketches.
- **"Geometry queries: bounding box, measurements and anchors"**
  (`docs/geometry-queries.md`): `child_bounds`, `child_measure`,
  `child_anchors` and `anchor`; evaluation order and cost; preview vs
  render; limits; "Comparison with BOSL2 attachments"; and a note on the
  upstream OpenSCAD requests (#1088, #4520, PR #1713).
- **`neoscad docs <name>`** for every builtin, with its extension label.
- **`docs/cli-json.md`**: the `sketches` field of `check`, and
  `measure --sketch`.
- **`docs/mcp.md`**: sketch recipe and the `measure` `sketch` argument.

## 11. Staged implementation plan

Rough effort is for one builder working serially.

| Stage | Content | Effort |
|---|---|---|
| 0 | `Extensions` set replacing `Options::parts`; CLI, serve, MCP, LSP and ffi plumbing; docs `extension` field and labels; the conformance `--extra-enable` run | S, 1–2 days |
| 1 | `crates/sketch` solver core with no language attached: the model, residuals, LM, QR diagnosis, exactify, `libm`; the cross-platform determinism test in wasm-check. (Built with continuation and the flip check too, which are solver behaviour; stage 3 keeps their diagnostics and hints.) | L, 1.5–2 weeks |
| 2 | Language binding: the `sketch` module, the scoped vocabulary in `resolve`, entity handles, loops, tessellation (moving fragments to `io`), `NodeKind::Sketch` as a polygon; fillet and chamfer; the first goldens | L, 1–1.5 weeks |
| 3 | Diagnostics with hints; continuation and the flip check; strict mode; `Limits::sketch_unknowns` | M, 1 week |
| 4 | Sketch surfaces: `check` JSON, `measure --sketch`, LSP completion, hover and "Pin drawing", MCP recipe, editor decorations, `docs/sketch.md` | M, 1 week |
| 5 | Queries A (render-free): `anchor`, the node side field, `child_anchors`, sketch anchors, sandboxed and reused child instantiation | M, 4–5 days |
| 6 | Queries B: `GeometryOracle`, the session implementation, `child_bounds` and `child_measure`; `Limits::queries`; warm/cold and thread tests; `docs/geometry-queries.md` | L, 1.5 weeks |
| 7 | Fast path for bounds (behind the differential test); `child_distance`; `snapshot --sketch`; line–arc fillets (built, with the apps' settings for both extensions: section 11.6) | M, 1 week |

Stages 1 to 4 deliver sketches without queries; 5 and 6 can follow
independently. The `docs/followups.md` entries go in as each stage
lands.

### 11.1 Stage 2 as built

The binding is `crates/eval/src/sketch.rs`; the rest of the evaluator
changed only where the design says (resolve, heap driver, values, node,
dump). What it does, and where it departs from sections 4 and 9:

- **Vocabulary scoping.** The resolver puts a link that binds nothing at
  run time between a sketch body and the scope around the `sketch()`
  call (`Env::vocab` in `crates/eval/src/resolve.rs`), but only when that
  `sketch` can resolve to nothing but the builtin. A function or module
  lookup reaching the link stops at the vocabulary, so a definition
  outside the body (BOSL2's `arc`, MCAD's `chamfer`) is never found for a
  vocabulary name; only a function literal assigned to that name in the
  body itself would come first. A program's own `module sketch`
  (roof.scad) gets no vocabulary at all. The vocabulary is per namespace:
  `distance` is a statement, so MCAD's `function distance` can still be
  called inside a body. OpenSCAD's grammar allows only assignments and
  instantiations in a child block (`child_statements`,
  `.reference/openscad/src/core/parser.y:300-313`), so helper modules are
  defined outside the body.
- **Parameter names**, which section 4.2 did not fix: `point(at)`,
  `line(p, q, construction)`, `arc(center, start, end, cw,
  construction)`, `circle(center, r, d, construction)`; the statements'
  names are those of section 4.3 (`on(p, c)`, `fix(p, at)`, `distance(a,
  b, d, along)`, `angle(l1, l2, deg)`).
- **Labels.** After a body's assignments run, each entity a call made
  directly in an assignment takes the variable's name; the points a
  named line, arc or circle made from `[x, y]` become `top.start` and so
  on. A handle prints as `<sketch line "top">`, or `<sketch point>`
  before it has a name.
- **`distance(l1, l2, d)` implies parallel**, as section 4.3 says: the
  binding adds the solver's `Parallel` before its `Distance`, unless an
  earlier `parallel()` of the two lines already states it. The solver's
  own constraint does not impose it (`src/model.rs` in github.com/neoscad/sketch-solver,
  `Constraint::Distance`). Nothing in the solver's corpus disagrees: no
  case has a distance between two lines (`corpus/data/*/*.json` there),
  and the differential oracle cannot check one (`docs/followups.md`,
  "Constrained sketches").
- **Diagnostics.** Built: `sketch-conflict`, `sketch-redundant`,
  `sketch-underconstrained` (a new `Severity::Info`, printed `INFO:` and
  listed in the JSON diagnostics; an error with `strict = true`),
  `sketch-no-convergence`, `sketch-flipped`, `sketch-open-profile` (also
  for a point joining more than two profile curves),
  `sketch-fillet-too-large`, `sketch-unknown-entity`,
  `sketch-foreign-entity` and `sketch-geometry-in-body`. Wrong entity
  kinds, bad values and a fillet at a point that is not a corner of two
  lines are `invalid-argument`. Messages name a constraint by its source
  text (`length(axis, slot_len)`), not by evaluated values as section
  4.7's examples do. Every error leaves the sketch an empty polygon; the
  model goes on. Stage 3 added `sketch-self-intersection`, the info for
  points placed without a guess and the hints (section 11.2).
  `sketch-foreign-entity` guards an invariant
  rather than a case found in practice: every way found of reaching a
  handle outside its sketch (helpers, children, `$` variables, function
  literals) runs inside the sketch, which merges.
- **Profile.** Points made one by `coincident()` are one vertex; each
  vertex must join exactly two profile curves. Fillets and chamfers cut
  corners of two lines only. A single loop is written as `polygon()`
  without `paths`. Besides the polygon, `NodeKind::Sketch` holds an
  `Arc<eval::node::SketchReport>` (name, counts, residual, status), the
  evaluator's own summary rather than the solver's `Solution`, so the
  node stays plain data; stage 4 adds what `check`, `measure` and hover
  need.
- **Sharing code with `circle()`.** The segment rule moved to
  `io::fragments` (section 8), with `geom::fragments` as its form over a
  node's `Discretizer`.
- **Limits.** Under any resource limit a sketch may have at most 5,000
  unknowns (a constant until stage 3 made it `Limits::sketch_unknowns`);
  the solve polls the interrupt flag and the time limit.
- **Docs.** `sketch` and the vocabulary have `builtins.toml` entries
  labelled `extension = "sketch"`; `neoscad docs` lists the vocabulary on
  a line of its own, and LSP completion leaves it out until stage 4 makes
  completion aware of sketch bodies.
- **Tests.** The goldens (section 9, item 3) are in
  `conformance/extensions/sketch` (inputs, `.echo`, `.csg`), run by
  `crates/session/tests/sketch.rs` rather than by a `conformance run
  --tier ext`: the worked examples, holes, scoping and helpers, handles,
  and every diagnostic. The same file checks the flag off, roof.scad's
  module, the real MCAD and BOSL2 names, the `.csg` export evaluated
  without the flag, and a warm export equal to a cold one;
  `crates/geom/tests/sketch.rs` checks that a sketch renders and keys as
  its polygon, that a sketch circle is `circle()` at any `$fn`, `$fa`,
  `$fs`, and the same STL at 1, 2 and 8 threads; `crates/wasm-check`
  renders the two worked examples on wasm32.

### 11.2 Stage 3 as built

Stage 3 is in `crates/eval/src/sketch.rs` (the diagnosis and its hints),
the solver's `src/solve.rs` (two additions to the solver, now in
github.com/neoscad/sketch-solver) and the
limits plumbing. What it does, and where it departs from sections 4.6
and 4.7:

- **Hints are edits.** A diagnostic's hints go out in the existing JSON
  shape (`hints[]` with `replace`; `crates/session/src/diag.rs`), which
  the language server already turns into code actions
  (`crates/lsp/src/diagnose.rs`, `fixes`). One hint carries one edit, so
  a fix that needs several is either one edit of a larger span (pinning
  the drawing replaces the whole `sketch()` call) or several hints. An
  edit is only attached where it is exactly right; otherwise the hint is
  advice:
  - *Under-constrained*: constraints to add, as statements inserted
    before the body's closing brace (on their own lines, indented like
    the body), each measured on the solution so that it holds there:
    `horizontal(l)` for a line solved level, `length`, `radius`, `angle`
    between lines sharing a point, `fix(p)` (with `at` when the solve
    moved it from its guess), and last a coordinate measured from a point
    that cannot move (`distance(o, p, 12, along = "y")`). The solver picks
    which of these candidates to keep: `Sketch::completion` takes them in
    that order and keeps each whose equations all add to the rank of the
    Jacobian at the solution, until no freedom is left. So each suggestion
    on its own removes freedom and none is redundant, and with several
    there is first an "add all N" hint that leaves the sketch fully
    constrained. Only entities the outermost body names can be suggested
    (a helper module's variables are not in scope there); sketches over
    400 unknowns get advice only, as the rank updates cost O(n²) per
    candidate equation.
  - *Redundant*: delete the statement, with its line when nothing else is
    on it. *Conflict*: one hint per statement in the conflicting set
    (the later first, at most four) deleting it, and advice to make the
    values agree. A statement that ran more than once (in a loop, or in a
    helper called twice) or that is not written as a statement (the
    radius of `circle(c, r = 5)`) gets advice instead of an edit.
  - *Flipped*: "pin the drawing", the textual form of section 4.8's code
    action: the `sketch()` call with every literal `[x, y]` guess in it
    rewritten to the solved coordinates (6 significant digits), so that
    the next solve starts on the solved branch. Guesses computed from
    parameters keep their expressions. And advice for the other case,
    that the drawing should move towards the shape meant.
  - *No convergence*: the message names the equations still unmet, worst
    first (the solver's new `Solution::unmet`), and the hint the points
    whose guesses to move. There is no edit: no solution is known.
  - *Fillet too large*: the size argument replaced with the largest that
    fits, rounded down so that it does fit.
- **New diagnostics.** `sketch-self-intersection` (warning) for profile
  loops that cross each other or themselves, found on the tessellated
  loops (proper crossings only; touching loops are not reported), naming
  the two curves and where. `sketch-no-guess` (info) for a point written
  `point()`, saying where it solved, with the edit that writes that
  position in as its guess. Section 4.7 has no code for the latter; it is
  the "info diagnostic" of section 4.2.
- **The implied parallel.** `distance(l1, l2, d)` adds a parallel
  constraint (section 11.1). Stage 2 reported it as redundant, under the
  distance's name, whenever other constraints already made the lines
  parallel (two `horizontal` edges), and the obvious fix, deleting the
  distance, would have lost a dimension. It is no longer reported as
  redundant; in a conflict it is named as "(which makes the lines
  parallel)".
- **Messages.** A statement in a loop is many constraints with one text:
  identical messages are printed once, and a message lists at most six
  statements ("and N more"). The other texts are stage 2's.
- **Labels from values.** Besides a call that is an assignment's whole
  expression, an entity takes the name of the variable that holds it
  (`p = f(point(...))`, a conditional), and a list of entities names its
  elements (`pts[0]`, one level of nesting deeper too). Entities made in
  a statement's arguments still have no name (`point #3`).
- **Strict mode** is unchanged from stage 2 (Decision 3): the
  under-constrained message is info, an error with `strict = true`, and
  its hints are the same either way.
- **Limits.** `Limits::sketch_unknowns` (`crates/eval/src/limits.rs`) is
  a limit like the others: `--limit sketch_unknowns=N`, the `limits`
  object of `serve` and MCP requests, `sketch_unknowns` in the apps'
  `ResourceLimits`, 5,000 under `Limits::AGENT`, none by default. Past
  it, `sketch()` stops evaluation with a `resource-limit` error before
  the solve, as `rands()` does. The solve's interrupt callback is the
  request's cancel flag and its time limit, polled once per
  Levenberg–Marquardt iteration and per equation in
  `Sketch::completion`.
- **Fillets** between a line and an arc stay in stage 7 (section 11).
- **Tests.** `conformance/extensions/sketch/hints.scad` has every new
  hint and diagnostic, with its JSON (`hints.json`; `diagnostics.json`
  for the stage 2 model) checked by `crates/session/tests/sketch.rs`,
  which also applies every edit a hint carries and checks that its
  problem is gone, checks the unknowns limit, and stops a 300-unknown
  solve by cancellation and by the time limit. The solver's
  `completion` and `unmet` have tests of their own
  (`tests/completion.rs` in github.com/neoscad/sketch-solver), and the edit placement
  (deletion, insertion) unit tests in `crates/eval/src/sketch.rs`.

### 11.3 Stage 4 as built

Stage 4 is in `crates/session/src/sketches.rs` (the facts as JSON),
`check.rs` and `measure.rs`, `crates/lsp/src/sketch.rs` and the server's
resolution (`world.rs`), the MCP tools, `client`/`ffi` and the apps, and
`docs/sketch.md`. What it does, and where it departs from section 4.8:

- **Where the facts travel.** The evaluator's `SketchReport` gained each
  entity (label, kind, construction, span, solved values: a line's
  length and angle, an arc's radius and sweep), the "pin the drawing"
  edit, now computed for every solved sketch rather than only with a
  flip (`pin_drawing`), and the codes of its diagnostics. After
  evaluation the session walks the tree (only with the extension on)
  and keeps each sketch as JSON in `Log::sketches`, the first 100 with
  the count, so every host gets them with the run's diagnostics through
  the same path, as section 4.8 asks. Values are rounded to 1e-9 there.
- **`check`** has `sketches` (name, place, `status`, DOF, unknowns,
  equations, rank, iterations, residual, continuation, `empty`, codes)
  only when the model has a sketch, so other checks read as before, and
  a line per sketch in the text. **`measure --sketch NAME`** reports the
  first sketch of that name with every entity's solved values and
  `instances`; an unknown name is an error listing the names. It still
  renders the model, as every `measure` does.
- **Language server.** `Options::extensions` (and `set_extensions`, for
  the apps' setting) together with the session's own make sketch bodies
  bind the vocabulary in resolution, as the evaluator's `Env::vocab`
  does: the child block of a `sketch` call that resolves to the builtin
  (scopes now record their instantiation's reference). Completion offers
  the vocabulary only there, with snippets; hover and signature help
  find the vocabulary's entries there and only there (outside, `circle`
  is OpenSCAD's module and `point` unknown), with the extension label
  from `docs`. Hover adds an entity variable's solved values and a
  `sketch` call's state from the last run of exactly the current text:
  facts are kept with the text the run read, from the server's own
  evaluation or from a host's `Server::supply_log` (the macOS, Windows,
  Linux and web hosts now hand over the log rather than its diagnostics
  alone). "Pin drawing" is a `refactor.rewrite` code action anywhere in
  a sketch, and also a fix on the sketch's own diagnostics, since the
  apps' editor offers fixes from diagnostics only. Every hint with an
  edit already was a quick fix (`diagnose::fixes`); info diagnostics are
  published as `Information`. Go to definition on a handle's member
  (`top.start`, `e1.center`) goes to the point it names: the variable
  given as that argument, through further members, or the `[x, y]`
  literal. Hover on a constraint (satisfied, redundant, its residual) is
  not built.
- **Info severity in the apps.** `client::Severity` and the ffi's gained
  `Info`, and `diagnostic_of` maps it. The shared editor bundle already
  mapped LSP severity 3 to CodeMirror's `info` marker, so the macOS,
  Windows and Linux editors show it once the server publishes it.
- **Editor colours.** `builtins.js` colours the vocabulary as `sketch`
  inside the child of a `sketch(...)` call only (section 7). The editor
  cannot tell the builtin from a program's own `module sketch`, so it
  colours inside that too.
- **MCP.** `measure` takes `sketch`; `check`'s structured content lists
  the sketches (name, status, DOF, unknowns, codes, line). The sketch
  recipe (`crates/cli/src/mcp/recipe_sketch.scad`) is not in the
  instructions: with the printing recipes they are 2,038 of Claude
  Code's 2,048 characters, and a server without the extension should not
  steer agents to `sketch()`. With `--enable sketch`, `docs` for
  `sketch` ends with it, the index says so in a line, and
  `neoscad://recipes` appends it.
- **Apps' setting.** The macOS app has Settings > Language >
  "Constrained sketches (sketch)", passed to the document loop (a new
  `set_enable`, so every run, check, measure and export takes it) and to
  the window's language server (`LanguageServer::set_enable`); changing
  it runs open documents again. Linux and Windows have no such setting
  yet (`docs/followups.md`).
- **Snapshot.** `snapshot --sketch` stays in stage 7, as the table in
  section 11 has it.
- **Tests.** `crates/session/tests/sketch.rs` (the `check` listing on
  the diagnostics golden, `measure` on the slot, and every example of
  `docs/sketch.md` with the diagnostics its fence names);
  `crates/lsp/tests/sketch.rs` (scoped completion, hover and resolution,
  roof.scad's own module, info markers, quick fixes and "Pin drawing"
  applied, solved values on hover and their invalidation, a host's run,
  definitions through members); `crates/cli/tests/mcp.rs` (the recipe
  renders fully constrained; `check` and `measure` through MCP);
  `crates/client` (info diagnostics, `enable` through the loop); the
  editor's `language.test.js` (the colours).

### 11.4 Stage 5 as built

The queries are `crates/eval/src/query.rs`; the rest of the evaluator
changed only where the sandbox needs it (the heap driver, messages, the
call memo's recording, the node, the sketch binding). What it does, and
where it departs from sections 5.2 to 5.4:

- **`anchor(name, point, dir = undef)`** is a builtin module with
  `--enable query`, in the table only then, as `part` and `sketch` are.
  It makes no node, so a model's tree, `.csg`, geometry keys and node
  numbering are those of the same model without it. It puts an
  `eval::node::Anchor` (name, point, direction) on the node being filled
  (the innermost instantiation around it: a module call, a transform, a
  loop, `children()`'s own node). The side field is
  `Node::anchors: Option<Box<Vec<Anchor>>>` rather than
  `Option<Box<[Anchor]>>`, because statements add to it one at a time; it
  costs one word per node. An `echo` or `assert` whose only children were
  anchors leaves no node, and its anchors go to the node around it. At
  the top level there is no node and nothing could ask, so they are
  dropped. A point is always three coordinates (`z = 0` for `[x, y]`).
  Bad arguments are `invalid-argument` warnings and the anchor is
  ignored. Inside a sketch body `point` may be an entity (below).
- **Placement.** `child_anchors()` walks the child's nodes from a heap
  stack, composing each transform's matrix with plain sums of products,
  as `geom::csg`'s own walk composes them, so the answers are the same
  on every platform (section 5.3 said `eval::fma`, which rounds
  differently on aarch64). A direction is moved by the linear part and
  scaled to unit length. `linear_extrude(center = true)` moves its
  profile's anchors to its base (half the height vector down), and
  `projection()` flattens them onto `z = 0` (a direction it flattens to
  nothing is dropped). `resize()` and `rotate_extrude()` hide their
  children's anchors, since placing them needs the geometry (`resize`
  scales by the child's size) or a choice of angle; so does a transform
  with a NaN or infinite entry, whose children OpenSCAD removes.
- **`child_anchors(index)`** takes `index` as `children()` does (none for
  all the children, a number, a list, a range) and returns an object
  from each name to `[point, dir]`, `dir` `undef` when none was given, in
  pre-order (a node's own anchors before its children's). The first of a
  name wins. It is resolved like `children()`, lexically: a function
  defined outside the module cannot ask, even when the module calls it.
- **The sandbox.** The child is instantiated by a nested run of the
  statement driver (`Evaluator::instantiate_children_into`) into a group
  node standing in for `children()`'s own, from inside the expression
  that asked. Held back or restored around it: its messages
  (`Evaluator::hold`; the error path prints them before the error), the
  node counter, the limit-check counter, the `rands()` state, the
  deprecations and part names already printed, and the sketch number. A
  sketch being built around the query is set aside while it runs, so the
  child cannot add to it. Each level of such nesting holds native stack,
  so it is charged to the frame budget like a nested expression loop
  (`recursion::HEAP_LOOP_FRAMES`) and stops with OpenSCAD's recursion
  error where the stack would run out; a child that asks about itself
  (through a `$` function) is the same error at once.
- **Reuse.** What the child read from outside itself is noted by the
  call memo's own hooks: the sandbox is a recording on its stack
  (`CallMemo::begin_sandbox`, `end_sandbox`) that keeps no entry but
  passes its reads to any recording around it. The instance is kept for
  the rest of the module call; a later `children()` of the same call and
  indices takes it (renumbered from the counter, its messages printed,
  its node indices and limit checks counted) when every `$` variable it
  read has the same value there, and a second query reads it again
  under the same test. It is never reused after something no key covers
  (`rands()`, file reads, `import()`, `part()`, a deprecation, a `$`
  function found outside it, a passed limit), with a sketch set aside,
  or under `--hardwarnings`, whose stop at a warning a replay would not
  make; the child is then instantiated again by `children()`, which is
  always correct. A sketch's own work does not count against reuse
  (`Evaluator::untracked_sketch`): the memos still never replay a sketch,
  but a sandboxed instance holds the whole sketch, solved. The held
  nodes count in the memory estimate.
- **Sketch anchors** (with `--enable sketch` too): once a sketch solves
  without an error, every entity its outermost body names becomes an
  anchor of that name (`top`, `top.start`, `pts[0]`; a helper module's
  variables are not the sketch's): a point where it solved, a line's
  midpoint with its direction from start to end, an arc's or circle's
  centre. `anchor(name, entity)` in the body adds one at that entity's
  solved position, and replaces a named entity's anchor of the same
  name. `anchor(name, [x, y])` in a sketch body, a helper's included, is
  kept on the sketch's node. A sketch that fails exports no solved
  anchors.
- **Diagnostics.** `query-outside-module` and `query-index` (with
  `children()`'s text) as section 5.4 says, warnings, the answer
  `undef`; and `query-duplicate-anchor` (new), a warning naming the
  duplicated anchors. An error in the child stops evaluation at the
  query, with the child's messages before it. `query-empty` and
  `query-unavailable` belong to stage 6, which renders.
- **Surfaces.** `anchor` and `child_anchors` have `builtins.toml` entries
  labelled `extension = "query"`, `serve` lists `query` among its
  features, and with the flag off the JSON hints of their unknown-module
  and unknown-function warnings name `--enable query`.
- **Tests.** `conformance/extensions/query` (inputs, `.echo`, `.csg`),
  run by `crates/session/tests/query.rs`, which also checks the flag off,
  the `.csg` exports evaluated without the flag (`plate.csg`,
  `sketch.csg` and `reuse.csg` were also rendered by the stock nightly,
  by hand, to the vertex and facet counts NeoSCAD renders them to), and
  a warm export equal to a cold one.
  `crates/eval/tests/query.rs` checks that removing every `anchor()`
  leaves the `.csg`, the keys and the numbering unchanged, that the dump
  and the keys never read the field, that replacing every query with a
  constant changes nothing but the lines printing the answers (call memo
  on and off), and the native budget. `crates/eval/tests/incremental.rs`
  runs a query model through the statement memo, `crates/geom/tests/query.rs`
  renders at 1, 2 and 8 threads, and `crates/wasm-check` runs two of
  the models on wasm32. The design's section 6.3 is `plate.scad`, with
  the child's extent read from anchors it declares rather than from its
  bounding box (stage 6).

### 11.5 Stage 6 as built

`child_bounds()` and `child_measure()` are `crates/eval/src/query.rs`
(`child_geometry`) over the stage 5 sandbox; the oracle's trait is
`crates/eval/src/oracle.rs`, its implementation
`crates/session/src/oracle.rs`. The user reference is
`docs/geometry-queries.md`. Where it departs from sections 5.2 to 5.4:

- **The trait.** `eval::GeometryOracle::measure(subtree, interrupt,
  guard)` returns `eval::Facts` (`Empty`, `Flat { min, max, area }`,
  `Solid { min, max, volume, surface_area }`) or an `OracleError`
  (`Interrupted`, `Unsupported`). `Options::geometry` holds it as
  `Option<Arc<dyn GeometryOracle>>` (the trait is `Send + Sync`). The
  subtree is the sandbox's group standing in for `children()`'s node;
  the instance is now renumbered from 1 under it (the group is 0)
  whatever the counter was, because `dump::Keys` and the renderer size
  their tables by the largest node index, and a query late in a big
  model would otherwise allocate for every node before it. A reuse
  renumbers from there as before.
- **The hosts.** `session` builds an oracle per request when `query` is
  on, over the renderer the build will use (`Session::renderer_for`
  with the request's colour scheme and fonts; `evaluate_loaded` now
  takes the scheme, and `Session::evaluate`, which builds nothing, uses
  the default one) and the build's render settings, `replay:
  Some(epoch)` included. The command line (`crates/cli/src/run.rs`,
  `query_oracle`) makes one per frame over the export's renderer, now an
  `Arc`, and for `.csg`, `.term`, `.echo` and `.param` exports over a
  renderer of their own. The LSP evaluates through the session, so it
  answers queries too; `.ast` export never evaluates. With no oracle (a
  library test) a query is `query-unavailable`.
- **One cache, proved.** The oracle renders into the shared renderer.
  The proof the design asked for is
  `crates/session/tests/query.rs`, `a_query_changes_no_exported_byte`:
  the §6.3 model with `child_bounds()` exports the same STL bytes as its
  twin with the answers written in as literals (no query at all), cold,
  and warm in both orders in one session; `a_warm_query_export_equals_a_cold_one`
  repeats stage 5's warm test with rendering queries. So the scratch
  `Renderer` fallback (Decision, question 6) was not needed.
- **Messages.** A query render's messages are dropped. For the child's
  geometry warnings still to be printed once, by the final render, that
  render must replay what cached nodes printed when computed; the
  session always does (`replay: Some(epoch)`), and the command line,
  whose rule is OpenSCAD's silent cache hit, does so (epoch 0) only for
  a frame whose evaluation rendered a query (`Oracle::asked`).
  `a_queried_childs_warnings_print_once` compares the log with that of
  the model without the query.
- **Answers.** Bounds are the result's own box (`Polygon2d::bounds`,
  `PolySet::bounds` over the vertices faces use, Manifold's bounding
  box: minima and maxima, which no order changes). Area is the shoelace
  sum over a 2D result's outlines; volume and surface area are serial
  sums over a mesh result's face fans or over a solid's triangles
  (`session::mesh::Mesh::mass`), never Manifold's parallel reductions.
  The oracle keeps its answers per request by result key, so the same
  child asked about twice is measured once. `force` is ignored: a query
  measures what a render makes before any conversion.
- **Empty children.** `child_bounds()` warns `query-empty` and is
  `undef`, as section 5.4 says; `child_measure()` does not warn: its
  object has `empty = true` and `dim`, `bounds`, `size` and `center`
  `undef` (and no `area` or `volume`), which a model can test.
- **Limits.** `Limits::queries` (`--limit queries=N`, the `limits`
  object of `serve` and MCP, `queries` in the apps' `ResourceLimits`;
  10,000 under `Limits::AGENT`, none by default) counts the
  `child_bounds()` and `child_measure()` calls that reach an oracle, in
  one evaluation; past it the call stops evaluation with a
  `resource-limit` error at the query. `child_anchors()` does not count.
  A query render runs under the evaluation's interrupt flag and guard,
  so a cancellation or the time limit stops it at its next node, and a
  count or memory limit it passes is reported at the node that passed
  it (the guard's location), as in a cold render, after which
  evaluation stops.
- **Memoisation.** Section 5.4 expected no new memo rule. But the count
  is in no key, and a replayed statement or call would skip its
  queries, so a warm evaluation could pass where a cold one stops at
  the limit. So a statement or module call that asked a geometry query
  is never recorded by the statement memo or the call memo
  (`Evaluator::untracked_sketch`, which leaves the sandbox's own reuse
  alone, so nested queries still cost one evaluation per level). The
  renders still come from the cache. A followup could carry the count in
  the entries instead.
- **Unsupported children.** A child the renderer cannot build (a
  feature of a later phase) is a `query-unavailable` warning naming it,
  and `undef`; the final render then reports it as usual.
- **Not built.** The fast path for bounds and the apps' setting for the
  flag; both came in stage 7 (section 11.6).
- **Tests.** `conformance/extensions/query/plate-bounds.scad` is
  section 6.3 as written, beside stage 5's `plate.scad`, and
  `bounds.scad` covers 2D and 3D children, booleans, `%` and `#`,
  mixed dimensions, empty children, indices, nesting, a recursion and
  the warnings. `crates/session/tests/query.rs` adds the byte-identity
  and warm tests above, preview and render giving the same answers, 1,
  2 and 8 threads, the queries and triangles limits, the flag off with
  its JSON hints, and the examples of `docs/geometry-queries.md`.
  `crates/eval/tests/query.rs` checks with a fake oracle that geometry
  queries change nothing but the printed answers (call memo on and off),
  what the oracle is shown, `query-unavailable`, a cancelled or
  limit-stopped query render, and the count; `crates/eval/tests/incremental.rs`
  that statements with queries are evaluated anew and still match a
  fresh evaluation. `crates/wasm-check`'s `query-bounds` case renders
  queries on wasm32, its volume and area included. The `.csg` exports
  of `plate-bounds` and `bounds` were rendered by the stock nightly:
  `plate-bounds.csg` gives the same vertex and facet counts (582, 1,164)
  in the nightly as in NeoSCAD, and the same volume as the source to
  the six digits the `.csg` keeps; `bounds.csg` the same volume and box,
  with a different triangulation that a plain file without queries
  reproduces (`docs/followups.md`, "Geometry queries").

### 11.6 Stage 7 as built

Stage 7 is `crates/geom/src/fastbounds.rs` (the fast path),
`crates/eval/src/query.rs` (`child_distance`) with the oracle's two new
methods (`crates/eval/src/oracle.rs`, `crates/session/src/oracle.rs`),
the line–arc corners and the per-constraint facts in
`crates/eval/src/sketch.rs`, `crates/session/src/snapshot_sketch.rs` and
the overlay in `crates/render/src/snapshot.rs`, and the apps' settings.
Where it departs from sections 4.5, 4.8, 5.2 and 5.4:

- **The fast path for bounds.** `GeometryOracle` gained `bounds()`,
  whose default renders (`measure()` and its box); `child_bounds()` asks
  it, `child_measure()` still asks `measure()`. The session's oracle
  tries `geom::fastbounds::bounds` first (`fastbounds.rs:92`). Section
  5.4 described it as "the box of the leaves' generated vertices under
  their matrices". It is built more narrowly, so that it can be exact:
  - Rather than reasoning about vertices, it *builds* what the renderer
    builds where that is cheap, with the renderer's own functions: the
    primitives, the extrusions (`linear_extrude`, `rotate_extrude` of a
    single 2D child), the 2D hull, and each transform applied node by
    node with `evaluate::transform` (OpenSCAD applies a transform per
    node, not a composed matrix, so composing them would round
    differently). Only one step is replaced by reasoning: a 3D union of
    several solids is the union of its operands' boxes.
  - The differential test (`crates/session/tests/fastbounds.rs`)
    renders every subtree of generated models (primitives, transforms
    with arbitrary angles and singular or near-singular matrices,
    unions, hulls, extrusions, `%` and `#`, scales of 1e-5 and 1000,
    and the operations the fast path must leave alone) and requires the
    fast answer to equal the rendered box bit for bit wherever it
    answers. Searches of 100,000 models (about 600,000 subtrees
    answered) found three things the obvious rule gets wrong, each now
    declined: a 3D hull (QuickHull drops a point one bit outside a face:
    a revolved vertex at 2.666…67 beside a cube's 8/3), an operand
    flattened by a singular transform, which a union drops as having no
    volume (`scale([1, 1, 0])`, and a shear whose determinant rounds to
    6e-17; `singular`, `fastbounds.rs:399`), and two operands' extremes
    closer than Manifold's tolerance, one of which the union merges away
    (`union_box`, `fastbounds.rs:413`, with a clearance of 1e-6 relative
    to the coordinates). 2D unions of more than one child are declined
    outright: Clipper rounds to its 2^-27 grid and simplifies. The cargo
    test runs 300 models (about 1,700 answered subtrees, under a second
    in release);
    `NEOSCAD_FASTBOUNDS_MODELS` asks for a longer search.
  - It declines (and the oracle renders) for everything else: booleans
    other than union, `minkowski`, `offset`, `projection`, `resize`,
    `fill`, imports, text, parts, a transform above a union of solids
    (only the union's box is known), trees deeper than 256 levels, more
    than 2^20 vertices, and a primitive past the fragment or triangle
    limit (the render then reports it, as before).
  - A fast answer renders nothing into the cache, so the final render
    still builds the child; the gain is a query that does not wait for
    a Manifold union, and a preview, which never runs the union, that
    does not run it either. The queries limit counts it all the same.
- **`child_distance(a, b)`** (`query.rs:342`) takes two indices as
  `children()` does and is one query against `Limits::queries`. Both
  children are instantiated in the sandbox (the first copied out before
  the second, whose evaluation may end module calls and so move the
  kept instances) and reused by later `children()` calls as the other
  queries' are. The oracle's `distance()` (`oracle.rs:237` in
  `session`) renders both and measures as `measure --between` does:
  0 when their intersection's volume (a serial sum, not Manifold's) is
  over `between`'s floor, so one inside the other is 0, which surfaces
  alone would not give; otherwise the BVH's exact smallest distance. 2D
  children are compared in the plane (overlap by a Clipper
  intersection's area, distance between their triangulations at z = 0).
  The answer is the unrounded distance, where `measure` prints six
  digits. Empty children are `query-empty`; a 2D child with a 3D one is
  an `invalid-argument` warning; both answers `undef`. Distances are
  kept per request by the two subtrees' keys.
- **Per-constraint facts.** `SketchReport` gained `constraints` (one per
  statement that ran, with its kind, text, span, entities, dimension,
  state and, when unmet, its residual: `sketch.rs:446`) and each entity
  `free` (a point or radius whose coordinate the diagnosis lists as
  moving, mobility at least 0.01 as the under-constrained message uses,
  or a curve with such a point). The state is the worst of the
  statement's solver constraints: conflicting (in a conflict, as source
  or as one of its set), unmet, redundant, or satisfied; the parallel
  `distance(l1, l2, d)` implies does not count. The session's facts
  carry them (`constraints`, `free`) and the solved `profile` (at most
  20,000 points, else `profile_omitted`); `check`'s summary leaves all
  three out, `measure --sketch` the profile.
- **Hover on a constraint** (`crates/lsp/src/sketch.rs:259`): on a
  constraint statement's name in a sketch body, the last run's state,
  with the residual when unmet; a statement that ran more than once
  shows its worst run. Section 4.8 asked for this and stage 4 left it
  out.
- **`snapshot --sketch NAME`** (CLI, `serve`, MCP `snapshot`'s
  `sketch`): the first sketch of that name, from the run's facts, drawn
  flat in its own plane (the top view unless views are asked for): the
  profile filled in a pale colour without a render's 2D outline, every
  entity as a line on top (bold; construction dashed; free to move
  orange; in a conflicting or unmet constraint red), points as dots,
  the names the source gives (not members such as `top.start`), and a
  boxed glyph per constraint in its state's colour (`H`, `V`, `||`,
  `_|_`, `T`, `=`, `F`, `o`, `L 30`, `R 5`, `D 8`, `d 12`, `< 45`,
  `r 3` for a fillet, `c 2` for a chamfer; Hershey has ASCII only), with
  a key line in the header. The overlay is drawn on the CPU after the
  panels (`render::snapshot::SketchOverlay`, `sketch_overlay` at
  `crates/render/src/snapshot.rs:599`), clipped to each panel, labels
  stepping aside from each other as markers do, so the same sketch gives
  the same pixels. The header grows with its lines. The summary's
  `sketch` counts constraints by state and names the free entities. It
  cannot be combined with `--diff`, `--highlight` or `--issues`.
- **Line–arc fillets and chamfers** (`LineArc::cut` in `sketch.rs`;
  the function was `cut_line_arc` in stage 7, and is now
  `cut_with_arc`, shared with the arc–arc corners of section 11.7).
  Section 4.5's arithmetic, extended: the fillet's centre is `r` from
  the line on the corner's inside and `R + r` from the arc's centre, or
  `R − r` when the line runs into the arc's circle (`u · (C − P) > 0`);
  that is a quadratic along the line's offset, whose root nearest the
  corner is taken, with square roots only. The arc is shortened on its
  own circle to the tangency point; its direction and centre are kept.
  A chamfer cuts the line `d` along and the arc where the circle of
  radius `d` about the corner crosses it (also square roots only).
  `sketch-fillet-too-large` now has four texts (more of the line or of
  the arc than there is, a fillet inside the arc not smaller than it, or
  no fitting arc), and its hint's size is found by bisection, since the
  fit is not linear in the size; the golden test applies that edit and
  checks the error is gone. A corner where the line runs along the arc's
  tangent has no corner to cut (`invalid-argument`), and a corner of two
  arcs was still `invalid-argument` ("not supported yet") until section
  11.7.
- **The apps.** macOS Settings > Language has "Geometry queries
  (query)" beside the sketch toggle (`LanguageSettings.queries`).
  Linux has Preferences > Language with both
  (`crates/linux-app/src/extensions.rs`, kept in `language.json`; each
  window's loop and language server take the names, and changing one
  runs every window again). Windows has Design > NeoSCAD Extensions
  with both (`NeoSCAD.Host/LanguageSettings.cs`,
  `DocumentSession.SetEnable`); its windows are processes, so a window
  already open kept its setting until toggled there (until section 11.7,
  which watches the file). The panels' runs
  (check, measure, export) on Linux and Windows now pass the names too;
  they passed none before.
- **Editor.** `builtins.js` colours `child_anchors`, `child_bounds`,
  `child_measure` and `child_distance` as functions and `anchor` as a
  module everywhere, with or without the extension (as for the sketch
  vocabulary, the decorations do not know the setting).
- **Tests.** `crates/session/tests/fastbounds.rs` (the differential
  test, under a 2 GB resident guard and `Limits::AGENT`, and the
  session's `child_bounds()` through the fast path); the goldens
  `conformance/extensions/query/distance.scad` and
  `conformance/extensions/sketch/fillets.scad` (with the hint edit
  applied in `every_hint_edit_fixes_its_problem`);
  `a_sketch_snapshot_shows_entities_and_constraint_states` in
  `crates/session/tests/sketch.rs` (the overlay's colours, glyphs and
  summary, and the request's errors; the PNG itself needs a GPU);
  `a_sketch_overlay_draws_its_strokes_and_labels` in `render`;
  `hover_shows_a_constraints_state` in `crates/lsp/tests/sketch.rs`;
  the apps' settings (`theQuerySettingRunsQueries` on macOS, the Linux
  settings' round trip, and the Windows session and settings tests);
  the editor's colouring; and `crates/wasm-check`'s `query-distance`
  case (a fast-path box, a rendered box and distances on wasm32).

### 11.7 After stage 7 as built

Small items from `docs/followups.md` ("Constrained sketches", "Geometry
queries"), done after stage 7:

- **Arc–arc fillets and chamfers** (`ArcArc::cut`, `cut_with_arc` in
  `crates/eval/src/sketch.rs`). The fillet's centre is `r1 ± r` from the
  first arc's centre and `r2 ± r` from the second's, minus where the
  other arc's leaving tangent runs into that circle (`u · (C − P) > 0`,
  the line–arc rule with the other arc's tangent for the line), so it is
  where those two circles cross: along the line of centres by
  `(ρ1² − ρ2² + d²) / 2d`, then across by the rest of `ρ1`, square roots
  only. Of the two crossings the one nearer the corner is taken; the
  other belongs to the circles' other crossing, and a cut that lands
  past an arc's sweep (measured from the corner) is too large. A chamfer
  cuts each arc where the circle of radius `d` about the corner crosses
  it, as on the arc of a line–arc corner. The texts and the bisected
  hint are the line–arc ones, naming the arc the cut runs out of; "the
  two arcs at … are tangent there" when the arcs meet smoothly. The
  golden is `conformance/extensions/sketch/arc-corners.scad` (a lens, a
  crescent's tip inside one circle and outside the other, a snowman's
  waist outside both, the widest fillet the lens takes, too large with
  the hint's edit applied by `every_hint_edit_fixes_its_problem`, and a
  tangent join), with
  `arc_arc_corners_are_cut_tangent_to_both_arcs` in `eval` checking the
  tangency (centre distances, fillet radius, the cut on each circle near
  the corner) for each case at three sizes.
  - The fillet arc of a corner with an arc (line–arc too) now turns from
    its first cut towards the corner rather than "the short way round".
    They agree except at the bound the hint gives for a lens: the widest
    fillet touches both arcs at the ends of a diameter, and the short way
    was a rounding error that took the half circle away from the corner
    (the area by hand: half the lens plus a half disc, 100.68 at
    `$fn = 360` against 100.69 exact). The line–arc goldens did not
    change.
- **Completion of the query names** (`World::visible`,
  `crates/lsp/src/world.rs`): `anchor`, `child_anchors`, `child_bounds`,
  `child_measure` and `child_distance` are offered only when the host or
  the session has `query`, and the `child_*` functions only inside a
  module body (any enclosing module scope), where they answer; outside
  one they are a warning and undef. Hover and navigation still find
  their documentation without the extension, labelled as an extension's
  (`docs::extension_label`). Tests: `crates/lsp/tests/query.rs`.
- **Windows: the extensions setting across windows**
  (`NeoSCAD.Host/LanguageSettingsWatch.cs`). Each window watches
  `language.json` as `AgentConnection` watches `agents.json`, and a
  change another window saved sets the menu and runs the document with
  the new names. Its own save is remembered first and so comes back as
  no change; a file that fails to read or parse (another process
  mid-write, a damaged file) is ignored rather than read as everything
  off. Test: `LanguageSettingsSavedElsewhereReachTheWatchingWindow`.

## 12. Alternatives considered

- **Sketches as data with string names**, CadQuery-style:
  `s = sketch_solve([pt("a", [0, 0]), line("l", "a", "b"),
  horizontal("l")])`.
  - For: it is pure, it needs no scoped vocabulary, and solved values
    are directly available (`s.points`).
  - Against: string references get no LSP navigation and no
    "unknown variable" check, and they are easy to mistype. Global
    function names collide with BOSL2 and MCAD (section 4.1). It is also
    noisier to write.
  - It could be added later as a function form over the same solver if
    the owner wants values without anchors (question 2).
- **Entity identity by structure** (two `point([0, 0])` calls are one
  point). This is wrong, because separate points often share a guess.
- **Vocabulary as ordinary global builtins.** It is shadowed by
  BOSL2/MCAD names (section 4.1).
- **A dedicated `sketch { }` keyword syntax.** It breaks `sketch` as an
  identifier in existing files, and it would need a flag in the parser
  (section 7).
- **Queries through `$` variables** (PR #1713's `probe()`). This is the
  "magic variables" upstream rejected, and functions over the module's
  children are more explicit.
- **Render-free bounds only** (bounding boxes of primitives under
  transforms). They are exact only without `difference`, `intersection`,
  `minkowski` with concave input, `offset` and similar operations. That
  is kept as a fast path, not as the semantics.
- **Warm-starting the solver from the previous solution.** It breaks
  "warm equals cold" (section 4.6). The source-level "Pin drawing"
  replaces it.
- **planegcs through C++.** It cannot build for `wasm32-unknown-unknown`
  without a C++ runtime, it brings Eigen and Boost, and its `diagnose`
  is threaded (section 4.6).

## 13. Decisions

Settled by the owner (2026-10-07):

1. **Flag names:** unprefixed, `--enable sketch` and `--enable query`,
   checked against OpenSCAD's `Feature.cc` at every reference update.
2. **Solved values:** anchors (and `measure --sketch`) for v1; a function
   form only if it is missed.
3. **Strictness:** an under-constrained sketch is an info message, as in
   FreeCAD; `strict = true` makes it an error.
4. **Licence:** the source stays GPL-2.0-or-later; distributed binaries
   are already effectively GPLv3 through Apache-2.0-only dependencies
   (README, "Licence"), so GPLv3-compatible dependencies are acceptable.
   The solver is still NeoSCAD's own, in Rust, for WASM and determinism,
   and it is validated against the established solvers' test suites
   (FreeCAD's Sketcher and planegcs tests, SolveSpace's constraint tests)
   and, as a differential oracle outside the shipped code, against
   SolveSpace's solver.
5. **The solver crate** (added during stage 1): it may be published on
   its own (its own repository under the neoscad organisation, and
   crates.io), with NeoSCAD depending on it. It is licensed
   `MIT OR Apache-2.0`, depends on no NeoSCAD crate, and its published
   package contains no file derived from FreeCAD or SolveSpace.

Still open, with the recommendation taken unless the owner says
otherwise: top-level query syntax (not now), the `libm` crate (use it in
the solver now), a scratch `Renderer` as the warm/cold fallback (yes),
and the v1 vocabulary (section 4.3 as written).

## 14. The questions as first asked

1. **Flag names.** Are `sketch` and `query` unprefixed (matching
   `part`), or `neoscad-sketch`/`neoscad-query` so a future upstream
   experiment with the same name cannot clash? The recommendation is
   unprefixed, checked against `Feature.cc` at every reference update.
2. **Solved values as values.** Are anchors (plus `measure --sketch`)
   enough, or should there also be a function form returning solved
   coordinates (section 12)? That form would need string names or a
   second solve.
3. **Queries at the top level.** Is the module-children form enough, or
   should a geometry expression syntax be considered later (section 7)?
4. **Strictness default.** Is an under-constrained sketch an info
   message (the recommendation, like FreeCAD) or a warning?
5. **The `libm` crate.** Use it in the solver now (recommended), or
   settle the wider "one libm everywhere" question in
   `docs/followups.md:1663` first?
6. **Query renders and the shared cache.** If the warm/cold proof fails
   with shared ID blocks, is a scratch `Renderer` (double work) an
   acceptable fallback?
7. **The vocabulary set for v1.** Is the table in section 4.3 right?
   Ellipses, B-splines, external references to other geometry (FreeCAD's
   external geometry), and symbolic relations between dimensions
   (FreeCAD's expressions referencing other constraints) are all out.
8. **Licence posture.** The own-solver recommendation avoids planegcs
   (LGPL, compatible) for technical reasons, and SolveSpace (GPLv3)
   partly for licence reasons. Please confirm that NeoSCAD's binaries
   should stay GPL-2.0-or-later compatible.
