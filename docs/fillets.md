# Design: 3D fillets and chamfers (`--enable fillet`)

Status: stages F0 to F4 built (the flag, both builtins, the selector
parser and the node; the plan: the child's B-rep, per-edge facts,
selection, and the reports in `check`, `measure` and `snapshot`; the
blends of straight edges, then of circles and arcs about an axis and
the tangent chains of both, in the mesh and in STEP, with the checks
before and after the boolean; then the surfaces: the language server,
the MCP recipe, the apps' and /try's toggles, editor colouring and the
user reference `docs/fillet-edges.md`; sections 15.1 to 15.5 record
how, and where they depart from this text). The stop rule's corpus
passes (99.7% of supported cases exact, section 15.4); F5 not started.
Written
2026-10-08 against `127be03` and the reference checkouts in
`.reference/openscad` and `.reference/BOSL2`.
Claims about this codebase cite `path:line`; claims about OpenSCAD and
BOSL2 cite the reference checkouts; claims about other projects cite what
was retrieved on 2026-10-08, or say "unverified".

This is stage 4 of `docs/audits/exact-geometry-rust.md` (section 10:
"fillets/chamfers as generated CSG tools on selected B-rep edges, edge
selection language, corner spheres", 8–14 person-weeks), designed as a
NeoSCAD language extension on the pattern of `docs/language-extensions.md`.
The result must render, preview and print through the normal mesh
pipeline, and export to STEP (`--enable exact`, `docs/step-export.md`) with
true cylindrical, toroidal, conical, spherical and planar blend faces.

## Contents

1. Summary
2. OpenSCAD compatibility, flag and names
3. How other tools select edges
4. The language
5. Edge selection
6. Geometry: blend tools
7. Vertices where blends meet
8. Failure modes and diagnostics
9. Exact export
10. Pipeline, determinism, limits, cancellation, WASM, caching
11. Reporting: check, measure, snapshot, LSP, MCP
12. Worked examples
13. Comparison table
14. Prototype evidence
15. Staged plan, effort and stop rule
16. Test plan
17. Alternatives considered
18. Open questions for the owner

## 1. Summary

```openscad
fillet_edges(r = 2, edges = "|z") cube([40, 30, 20]);
chamfer_edges(d = 1, edges = "%circle and >z") difference() { ... }
```

- Two builtin operator modules, `fillet_edges()` and `chamfer_edges()`,
  behind one flag, `--enable fillet`. Each takes its children (one solid,
  their implicit union), selects edges of that solid with a selector, and
  rounds or cuts them. One call handles convex edges (material removed)
  and concave edges (material added).
- Selection runs on the child's **B-rep**, which already exists: the
  STEP export's tagged render and `meshbrep` reconstruction give every
  face its exact surface and every edge its exact curve. Selectors are a
  small string language that is a subset of CadQuery's, plus selectors
  by provenance (which child, which `part()`, which edges a boolean
  created) and by `anchor()`.
- Geometry is the audit's "generated CSG tools": for each selected edge
  a tool solid is computed from the two faces' exact surfaces and
  applied with Manifold (subtracted for convex edges, added for concave
  ones). Two classes of edge reduce to a 2D fillet in a cross-section,
  the problem the constrained sketches already solve: edges whose faces
  are both swept along a line (plane–plane, plane–cylinder parallel) give
  prism tools with **cylinder** blends; edges whose faces are both
  surfaces of revolution about one axis (hole and boss rims, extruded
  rounded rectangles, cone and sphere rims) give revolved tools with
  **torus** blends. Chamfers give planes and cones. Three equal fillets
  at a corner get a **sphere** patch.
- The tools' triangles carry exact surface records, so the STEP export
  reconstructs true blend faces. Five hand-written equivalents of the
  generated tools (section 14) export through today's `neoscad 0.5.0
  --enable exact` as all-exact solids that OCCT 8.0.1 reads back valid,
  with volumes equal to closed forms to 1e-8 or better.
- Out of v1: blends between two curved surfaces that do not share an
  axis (a cylinder meeting a cylinder at a tee), variable radius, and
  corners where unequal radii or convex and concave edges meet. Each is
  a diagnostic with a hint, usually "fillet in two passes", which the
  language expresses by nesting calls.

## 2. OpenSCAD compatibility, flag and names

The rules are those of the existing extensions
(`docs/language-extensions.md`, sections 1 and 2):

- **Flag off, nothing changes.** `fillet_edges` and `chamfer_edges` are
  not in the builtin table; a call is OpenSCAD's own `Ignoring unknown
  module 'fillet_edges'` warning (`.reference/openscad/src/core/Context.cc:127`),
  and the JSON hint names `--enable fillet`. The table is built per
  extension set (`crates/eval/src/builtins/modules.rs:125-150`, where
  `part`, `sketch` and `anchor` are added only when on).
- **The parser does not change.** Both are module instantiations with
  named arguments and a child statement.
- **A program's own definitions win.** A user or library `module
  fillet_edges` shadows the builtin, as for every builtin.
- **The flag name.** `fillet` turns on both modules. It is not an
  OpenSCAD experiment: `Feature.cc` declares `roof`, `input-driver-dbus`,
  `lazy-union`, `vertex-object-renderers-indexing`, `textmetrics`,
  `import-function`, `object-function`, `predictible-output`,
  `vector-swizzle`, `discretization-by-error`, `ai-features`,
  `unicode-identifiers` and `python-engine`
  (`.reference/openscad/src/Feature.cc:28-58`), and
  `crates/eval/tests/extensions.rs:35` fails if a reference update adds a
  clashing name. It is a new `eval::Extension` variant
  (`crates/eval/src/extensions.rs:22-36`), outside `--enable all`, and
  independent of `exact`: fillets render to meshes without `exact`;
  STEP export still needs `exact`.
- **Why not `fillet()` and `chamfer()`.** Those names are taken in the
  libraries NeoSCAD models use: BOSL2 defines `module fillet`
  (`.reference/BOSL2/shapes3d.scad:5175`, an edge mask object, not an
  operation on children) and MCAD `module chamfer`
  (`.reference/openscad/libraries/MCAD/metric_fastners.scad:92`). With
  BOSL2 included, `fillet(r = 2) cube(10);` would silently call BOSL2's
  mask with the cube as an attachment: wrong geometry and no message.
  NeoSCAD's own MCP recipes teach agents a `module fillet(r, l)`
  (`crates/cli/src/mcp/recipes.scad:9`), so models agents have written
  define it too. Inside sketch bodies `fillet(corner, r)` and
  `chamfer(corner, d)` are already the sketch vocabulary
  (`docs/language-extensions.md`, 4.3).
  No module or function named `fillet_edges`, `chamfer_edges`,
  `round_edges` or `edge_fillet` exists in BOSL2, MCAD, OpenSCAD's
  `examples` or `tests` (searched). The sketch design solved the same
  problem by lexical scoping; an operator module is used at any depth
  of a model, so a distinct name is the simpler answer (question 1).
- **Labels and codes.** `crates/docs/builtins.toml` entries carry
  `extension = "fillet"`; diagnostic codes start with `fillet-`; messages
  say "fillet_edges():" or "chamfer_edges():".
- **`.csg` export** prints the node as `fillet_edges(r = 2, edges =
  "|z", ...) { ... }`, as `part` prints `part(name = ...)`
  (`crates/eval/src/dump.rs:304-308`). Unlike a solved sketch, which
  prints as the polygon it is, a fillet's result is geometry the
  evaluator does not have, so the `.csg` of a filleted model is not
  plain OpenSCAD; STL, 3MF and STEP are the interchange formats
  (question 10).

## 3. How other tools select edges

**CadQuery** (`doc/selectors.rst` and `cadquery/selectors.py`, `master`,
retrieved 2026-10-08). Selectors are strings over the current object
list: `|Z` (lines parallel to Z), `#Z` (perpendicular), `>Y`/`<Y`
(farthest along a direction, by the edge's centre: `DirectionMinMaxSelector`
derives from `CenterNthSelector`, tolerance 1e-4, `selectors.py:399-426`),
`>Y[1]` (nth parallel edge), `>>Y[-2]` (nth by centre), `%Line`/`%Circle`
(curve type), user directions `>(-1, 1, 0)`, combined with `and`, `or`,
`not`, `exc` (`selectors.rst:33-42`, `:79-107`, `:131-145`). The rst warns
that non-linear edges are returned only by type and centre selectors
(`:84-88`). `Workplane.fillet(radius)` and `chamfer(length, length2)` act
on the selected edges and raise if none are selected
(`cadquery/cq.py:1219-1275`). Selection is re-run every time the script
runs; there are no stored edge names.

**build123d** (`docs/topology_selection.rst` and
`src/build123d/operations_generic.py`, `dev` branch, retrieved
2026-10-08). Selectors return a `ShapeList` refined by `filter_by` (axis,
plane, `GeomType`, `Convexity`), `sort_by`, `group_by` and operator
shorthands `>`, `<`, `>>`, `<<`, `|` (`topology_selection.rst:83-120`).
`Convexity` classifies an edge as `CONVEX`, `CONCAVE`, `SMOOTH` or
`SADDLE` "by how the material of the shape it was selected from sits
around it" (`:388-404`). History: `Select.LAST` and `Select.NEW`, the
latter "the edges where two objects intersect, the faces of a fillet"
(`:500-516`). `fillet(objects, radius)`: "radius … must be less than 1/2
local width"; "3D fillets propagate along chains of tangent edges …
OpenCascade offers no way to disable this"
(`operations_generic.py:482-507`). `chamfer(length, length2, angle,
reference)` (`:326-366`). Whether `Convexity` is in a released version
was not checked (it is in `dev`).

**FreeCAD PartDesign Fillet/Chamfer** (FreeCAD-documentation `main`,
`wiki/PartDesign_Fillet.md`, `PartDesign_Chamfer.md`,
`Topological_naming_problem.md`, retrieved 2026-10-08). Edges are picked
in the 3D view, or all edges of picked faces, or all edges of a feature;
"for a chain of tangentially connected edges only a single edge needs to
be selected, the fillet will propagate along the chain"
(`PartDesign_Fillet.md:20-24`). The picks are stored as sub-element names
in a `Base` link (`:69`); **Use All Edges** ignores them (`:85`). The page
warns: "Edge numbers are not completely stable … filleted edges would
likely become invalid. When the Use All Edges property is True there is
some protection from this" (`:119`). FreeCAD 1.0 is the first release
with the topological naming algorithm (`Topological_naming_problem.md:13`),
which is "not intended to fix every failure" (`:133-139`). Chamfers are
equal distance, two distances, or distance and angle, with a flip
(`PartDesign_Chamfer.md:50-68`, `:101-109`). FreeCAD fillets are OCCT's
`BRepFilletAPI`; the earlier audit measured it returning an invalid
solid for a radius larger than half an edge
(`docs/audits/brep-feasibility.md:159-180`).

**BOSL2.** Edge sets on attachable primitives: `edges=` and `except=`
take descriptors, each a direction vector naming one edge, the edges
around a face or the edges at a corner of the bounding cuboid, or `"X"`,
`"Y"`, `"Z"`, `"ALL"`, `"NONE"`, or a 3×4 mask
(`.reference/BOSL2/attachments.scad:209-239`). `cuboid(rounding=,
chamfer=, edges=, except=, trimcorners=, teardrop=)`
(`.reference/BOSL2/shapes3d.scad:127-132`); negative roundings make
external fillets but only around the top and bottom faces
(`:118-122`); `trimcorners` "rounds or chamfers corners where three
chamfered/rounded edges meet" (`:131`). For other shapes,
`edge_profile()` and `edge_mask()` attach a mask along the edges of a
parent cuboid, prismoid or cone and `diff()` removes it
(`.reference/BOSL2/masks.scad:1225-1232`, `:2645`). The masks round
their cylinder's segment count up to a multiple of 4
(`$fn=quantup(segs(r),4)`, `masks.scad:2629`), the alignment the exact
export also uses. Edges are those of the declared primitive, not of the
result of booleans.

**OpenSCAD idioms.** `offset(r = r) offset(delta = -r)` rounds a 2D
profile's corners before extrusion (`.reference/openscad/examples/Advanced/offset.scad:16-18`;
NeoSCAD's MCP recipes use it, `crates/cli/src/mcp/recipes.scad:6-7`).
`minkowski()` with a sphere rounds every convex edge and grows the
shape; a minkowski of the complement erodes
(`.reference/openscad/tests/data/scad/3D/features/minkowski3-erosion.scad:1-12`).
Hand-written tools: a block minus a cylinder, unioned or subtracted
along one edge (`crates/cli/src/mcp/recipes.scad:8-12`). Libraries such
as `roundedcube(size, center, radius, apply_to)`, with `apply_to` one of
`"all"`, `"x"`, `"y"`, `"z"`, `"zmax"`, `"zmin"`, `"xmax"`, `"xmin"`,
`"ymax"`, `"ymin"` (danielupshaw.com/openscad-rounded-corners, retrieved
2026-10-08; how it builds the shape: unverified). None of these selects
edges of an arbitrary boolean result; minkowski rounds all of them, at a
cost that grows with both operands.

## 4. The language

```
fillet_edges(r, edges = "all", except = undef, expect = undef) children;
chamfer_edges(d, edges = "all", except = undef, expect = undef) children;
```

| Argument | Meaning |
|---|---|
| `r` / `d` | Fillet radius / chamfer distance, measured along each face from the edge. Positive number; `d` also accepts `r =` as an alias so agents can swap the module name |
| `edges` | A selector (section 5): a string, a BOSL2-style direction vector, or a list of them (their union). Default `"all"` (question 3) |
| `except` | A selector removed from `edges` (BOSL2's `except=`) |
| `expect` | An optional count. If the selection has a different number of edges, the call is an error (`fillet-count`) listing what it matched. This pins a selection against parameter changes (section 5.4) |
| `$fn`, `$fa`, `$fs` | The blend arcs' fragments, as for `circle()`; and, by the `$fn` rule of `docs/step-export.md`, whether STEP export writes them exact (`$fn` unset) or as the polygon (`$fn` set) |

- **Children.** The children are rendered as F6 renders them (`%`
  excluded, `#` included, as the geometry queries do,
  `docs/language-extensions.md` 5.2) and unioned into one solid. A 2D
  child is an error (`fillet-2d`, hint: `offset(r)` or a sketch
  `fillet()`); empty children give nothing, silently.
- **Frame.** The operation runs in the call's own coordinate frame:
  `scale(2) fillet_edges(r = 1) cube(10);` is a 20 mm cube with 2 mm
  fillets, as any operator module behaves under a transform. Directional
  selectors (`>z`) mean the call's axes.
- **One call, both senses.** Selected convex edges lose material,
  selected concave edges gain it. The result is `(child ∪ concave tools)
  − convex tools`; both tool sets come from the same B-rep of the child.
- **Composition.** A second operation on the result is a nested call.
  `fillet_edges(r = 2, edges = ">z") fillet_edges(r = 5, edges = "|z")
  cube([40, 30, 20]);` rounds the vertical edges first, then the top
  outline, which is then a tangent chain of lines and arcs (section 12.2).
  This is how unequal radii and mixed corners are expressed (section 7).
- **Why an operator module.** It is OpenSCAD's own shape for
  "do something to children" (`offset`, `hull`, `minkowski`,
  `resize`). Agents already write it, it nests, and it carries `$fn`.
  The alternatives (arguments on `cube()` and friends, functions that
  return selections) are in section 17.

## 5. Edge selection

### 5.1 What the selector sees

Selection runs on the B-rep of the child, built as the STEP export
builds it: the export render of the subtree (`geom::exact::walk::export_render`,
`crates/geom/src/exact/walk.rs:142-150`, which takes any node and a
segment multiplier), then `meshbrep::reconstruct`
(`crates/meshbrep/src/lib.rs:155`). For each edge the B-rep has its
curve (line, circle, ellipse, B-spline; `crates/meshbrep/src/model.rs:108-145`),
its two faces through the coedges of their loops (`model.rs:233-290`),
and each face's exact surface and orientation (`model.rs:10-87`,
`:249-266`). From those, per edge, at sample points along it:

- **Turn angle and convexity**: from the two faces' outward normals and
  the edge's direction in the first face's loop. *Convex* when the
  material angle is under 180°, *concave* over 180°, *smooth* when the
  faces are tangent (within 1e-9 rad), *saddle* when the sense changes
  along the edge (build123d's four classes).
- **Class** (section 6): translational, rotational, or other.
- **Provenance**: which leaf of the child each face's surface record
  came from. The walk already draws every leaf's surface numbers in tree
  order (`walk.rs:16-23`); it also records, per record, the child index
  of the fillet call it lies under, the innermost enclosing `part()`
  name, and the leaf node. A face merged from several records (coplanar
  tops of two children) has several.
- **Polygon seams**: an edge between two facets of one `$fn` polygon (a
  `$fn = 24` cylinder's 24 vertical edges) or of one faceted region
  (`hull`, `minkowski`, `polyhedron`, imports: `docs/step-export.md`,
  "What is exact").

**Never selected:** smooth edges, polygon seams, and edges with a
faceted face on either side. A selector that names one explicitly gets an
info message (`fillet-skipped`) saying why. Without this rule, `"all"` on
a model with a `$fn = 64` cylinder would put 64 tiny blends on its side.

### 5.2 Selector strings

A selector string is case-insensitive; whitespace separates tokens.

| Atom | Selects | Source |
|---|---|---|
| `all` | every selectable edge | BOSL2 `"ALL"`, FreeCAD "Use all edges" |
| `convex`, `concave` | by sense (5.1) | build123d `Convexity` |
| `%line`, `%circle`, `%ellipse`, `%bspline` | by curve type (an arc of a circle is `%circle`) | CadQuery `%Line` |
| `\|x`, `\|y`, `\|z`, `\|(a, b, c)` | lines parallel to the direction | CadQuery `\|Z` |
| `#x`, `#(a, b, c)` | lines perpendicular to it, and circles whose axis is parallel to it (a circle lies in a plane perpendicular to its axis) | CadQuery `#Z` (lines only there) |
| `>z`, `<z`, `>(a, b, c)` | edges whose centre is farthest along / against the direction, within the tolerance | CadQuery `>Z` |
| `>>z[i]`, `<<z[i]` | the i-th group of edges by centre along the direction (negative from the far end) | CadQuery `>>Z[-2]` |
| `new` | edges whose two faces come from different leaves: the edges booleans made | build123d `Select.NEW` |
| `child(i)`, `child(i, j)` | edges with a face from child `i` of this call; edges between a face of child `i` and a face of child `j` | — |
| `part(name)` | edges with a face from `part(name)` (needs `--enable part`; dotted names as `check` uses them) | — |
| `@name` | edges passing through anchor `name` of the children, and parallel to its direction if it has one (needs `--enable query`) | BOSL2 `named_anchor()` |
| `box(x0, y0, z0, x1, y1, z1)` | edges lying wholly in the box | the earlier audit's "position or region" |

Operators, loosest first: `not`, `exc` (set difference; `except` is the
same), `or`, `and`, and parentheses. This is CadQuery's grammar as its
code builds it (`cadquery/selectors.py`, `_makeExpressionGrammar`, `master`,
retrieved 2026-10-08: `infix_notation` with `and`, then `or`, then
`exc`/`except`, then `not`), not the order this section first gave
(`or` loosest, `not` tightest), which `selectors.rst` does not state
either. So `not convex and |z` is `not (convex and |z)`. CadQuery rejects
a `not` after a binary operator; NeoSCAD accepts it with the same rule,
`not` negating everything to its right up to the closing parenthesis
(`|z and not >x or new` is `|z and not (>x or new)`). So `"|z and >x"` is
the vertical edges on the +x side, `"%circle and >z"` the top hole rims,
`"child(0, 1)"` where child 1 meets child 0, `"all exc <z"` everything but
the bottom outline.

A CadQuery selector string of the atoms above means the same thing here.
CadQuery's `+z`/`-z` on edges depend on the edge's orientation, which in
an OpenSCAD model is not the user's to choose; they are an error with the
hint "use `|z`". `>y[1]` (CadQuery's nth *parallel* edge) is not
supported; `>>y[1]` is.

**Tolerance.** "Farthest" and grouping compare centres within 1e-6 of
the child's bounding-box diagonal (CadQuery: 1e-4 absolute). Parallel
and perpendicular mean within 1e-9 rad.

**Non-string selectors.** A direction vector with entries in {-1, 0, 1}
is BOSL2's descriptor, generalised from a cuboid to the child's bounding
box: one non-zero entry selects the edges lying in that face of the box
(`[0, 0, 1]`: edges in the plane z = max), two the edges lying along that
edge of the box, three the edges touching that corner. On a cube this is
exactly BOSL2's set, so `fillet_edges(r = 2, edges = [[0, -1, 1], [1,
0, 1]]) cube(...)` matches `cuboid(..., rounding = 2, edges = [TOP+FRONT,
TOP+RIGHT])` (with BOSL2's constants defined). The strings `"X"`, `"Y"`,
`"Z"` are aliases of `|x`, `|y`, `|z`, and `"ALL"`/`"NONE"` of `all`/`not
all`. A list is the union of its items.

**Errors in the selector** are evaluation-time errors at the argument's
span, with the column in the string (`fillet-selector`): unknown atom
(with "did you mean"), unbalanced parentheses, `part()` without `--enable
part`, an unknown anchor (with the anchors that exist). The parsed
selector is stored on the node, so geometry never parses strings.

### 5.3 Anchors and parts reach geometry through the node

Anchors are a side field that geometry, `.csg` and cache keys never read
(`docs/language-extensions.md`, 5.3). So `@name` is resolved in the
evaluator when the call is instantiated, by the same walk
`child_anchors()` uses, and the resolved points and directions are
stored on the fillet node, where they are part of its key. Part names
are already nodes (`NodeKind::Part`), which the walk sees.

### 5.4 Stability under parameter changes

The topological naming problem is a stored name (FreeCAD's `Edge12`)
outliving the topology it named. Here nothing is stored: every
evaluation selects again from geometry and provenance, as CadQuery and
build123d do. The failure mode changes from "the reference broke" to
"the selector now matches a different set", for example `>z` picking the
top of a boss instead of the plate once the boss grows. Mitigations, most
stable first:

1. **Provenance**: `child(i, j)`, `part(name)`, `new`. Where a boss meets
   a plate is `child(0, 1)` at any size or position.
2. **Anchors**: `@lip` moves with the geometry that declares it.
3. **Kind and sense**: `convex`, `%circle`, `|z`.
4. **Position**: `>z`, `box(...)`, BOSL2 vectors: right until the shape
   changes which edge is extreme.
5. **`expect = n`** turns any drift in count into an error, and the
   report (section 11) shows what was matched. The language server
   offers a code action "Pin count" that writes `expect = n` from the
   last run, as "Pin drawing" pins a sketch's guesses
   (`docs/language-extensions.md`, 4.8).

## 6. Geometry: blend tools

### 6.1 The rolling-ball cross-section

A constant-radius fillet is the envelope of a ball of radius `r` rolling
in contact with both faces. For the two classes below the ball's centre
moves on a line or a circle, so the problem is a 2D one in a
cross-section, and the blend surface is a cylinder or a torus.

- **Translational class.** Both faces are swept along the edge's
  direction: planes containing it, cylinders whose axis is parallel to
  it. The edge is a line. In the plane perpendicular to it the faces are
  lines and circles; the 2D fillet arc is swept along the edge into a
  **cylinder**, a chamfer line into a **plane**. Covers every plane–plane
  edge (any dihedral angle), plane–cylinder parallel, and cylinder–cylinder
  with parallel axes.
- **Rotational class.** Both faces are surfaces of revolution about one
  axis: a plane perpendicular to it, cylinders, cones, spheres centred on
  it, coaxial tori (`Surface`, `crates/meshbrep/src/model.rs:10-62`). The
  edge is a circle or an arc about the axis. In the meridian half-plane
  the faces are lines and circles; the 2D fillet arc is revolved into a
  **torus** (a cylinder or plane when degenerate), a chamfer line into a
  **cone** (or plane or cylinder). Covers hole and boss rims, the arcs of
  an extruded rounded rectangle's top outline, countersink and
  sphere rims, and the arcs where an earlier fillet meets a face
  perpendicular to its axis. Any plane–sphere edge is in this class,
  about the line through the centre along the plane's normal.
- **Other edges** (cylinder–cylinder crossing at a tee, a plane cutting a
  cylinder obliquely: ellipses and B-splines) are not in v1:
  `fillet-unsupported-edge` (section 15, stage F5).

The 2D problem is the sketch's: a fillet between two lines, a line and an
arc, or two arcs, tangent to both, with square roots only
(`LineArc::cut`, `ArcArc::cut`, `cut_with_arc` in
`crates/eval/src/sketch.rs:3032-3192`; `docs/language-extensions.md`,
11.6 and 11.7). The corner to cut is the edge's trace in the
cross-section; the side is the material's. That arithmetic is the core
of the tool generator (where it lives: question 6).

### 6.2 The tool solid

For an edge of the translational class, with tangent points `T1` and `T2`
on the two faces and fillet centre `C` in the cross-section:

- **Convex edge (subtract).** The 2D region bounded by the corner, `T1`,
  the fillet arc, `T2`, back to the corner, extended outwards past the
  corner by a margin so that none of the tool's own faces is coplanar
  with the child's: the MCP recipe's "block minus cylinder"
  (`crates/cli/src/mcp/recipes.scad:8-12`, which unions it into a
  concave corner) facing the other way, for any angle. Swept along
  the edge and past each end that is open air (section 7.1).
- **Concave edge (add).** The region between the two faces and the arc,
  on the empty side. Its two contact sides lie on the child's faces. A
  contact side on a plane can be coplanar with the face; a contact side
  on a curved face must either use the face's own tessellation (the same
  ring of vertices) or overlap into the material by a margin smaller
  than the wall there (checked on the B-rep). Section 14 measured why:
  coincident curved faces with different tessellations fail exact
  reconstruction.
- **Chamfer.** The triangle (corner, `T1`, `T2`), with `|corner − T1| =
  |corner − T2| = d`, swept the same way.

For the rotational class the same 2D region is revolved about the axis
by the edge's sweep (a whole turn for a full rim, the arc's angle for a
partial one). `meshbrep::primitives` already builds tagged revolved and
swept primitives: `torus(major, minor, segments, tube_segments, angle, t)`
with planar ends for partial sweeps (`crates/meshbrep/src/primitives.rs:379-454`),
`frustum` (`:213`), `sphere` (`:285`), `prism` (`:318`), all tagging each
triangle with its exact surface.

### 6.3 Tessellation of the tools

The tool's mesh must meet the child's mesh cleanly, or the boolean leaves
slivers in the mesh and the reconstruction fails (audit F2):

- **Tangent lines are mesh vertices.** The fillet arc is tessellated from
  `T1` to `T2` with vertices exactly at both ends, by
  `circular_segments_for_angle(r, sweep)` (`crates/geom/src/fragments.rs:24-50`)
  from the call's `$fn`/`$fa`/`$fs`, as a sketch arc is
  (`docs/language-extensions.md`, 4.4).
- **Conforming to the face it runs along.** A revolved tool takes the
  segment count and phase of the cylinder, cone or sphere it blends into,
  as that face is tessellated in the mesh the tool is applied to: in the
  normal render, OpenSCAD's own polygon (13 sides is fine; the tool is
  then 13 revolved sections in the same phase, so its tangent ring is
  the polygon's vertices); in the export render, the aligned count
  (`aligned_segments`, `primitives.rs:134`). The walk records, per
  surface record, the leaf's segment count and phase so the generator
  can read them.
- **Corners** (section 7) are generated patches whose boundary arcs reuse
  the adjoining tools' arc vertices, rather than an independent
  `sphere()`.

### 6.4 Which mesh the tools are applied to

- **Normal render (preview, render, STL, 3MF, `check`):** the child's
  normal geometry, OpenSCAD's tessellation, cached and with its colours
  and parts. The B-rep used for selection comes from a separate export
  render of the child (aligned tessellation), whose exact edges define
  the tools; the tools are then tessellated to conform to the normal
  mesh. So a filleted model's unfilleted faces have exactly the vertices
  they would have without the fillet, and colours and `part()`
  attribution survive (Manifold keeps original IDs through booleans,
  `docs/cli-json.md:451-455`). Tool faces take the colour and part of the
  first of their two faces.
- **Export render (STEP):** the walk meets the fillet node, walks its
  child tagged (aligned), applies the same plan's tools tessellated to
  the aligned mesh and tagged with their exact surfaces, and returns the
  tagged result, transformed by the matrix above the node
  (`meshbrep::primitives::Transform::surface`, `primitives.rs:92-128`;
  a non-uniform scale above it makes the blends facets, as for any
  curved surface).

The plan (selected edges, classes, cross-sections, vertex patches) is
computed once per fillet node and shared by both.

## 7. Vertices where blends meet

### 7.1 An edge's end, no other selected edge there

The tool is swept between the edge's end vertices. At each end the
other faces at that vertex decide:

- **Open end** (the material ends there: a box's vertical edge at its
  top face): a subtracting tool is extended past the end by its own
  cross-section size, so its end cap lies in air. An adding tool is cut
  by the end face (coplanar with it).
- **Closed end** (the edge runs into a wall: the solid continues on the
  tool's side): the tool is cut by the end face's surface, a plane or a
  cylinder in v1. The blend surface then simply ends on that face.
  Whether this is what OCCT produces in the same configuration was not
  checked.

### 7.2 Tangent chains

Edges that meet tangentially (a rounded rectangle's top outline: lines
and arcs alternating) get tools whose cross-sections at the shared vertex
are identical. Each tool is cut by the plane perpendicular to the chain
at that vertex, so their end caps are coplanar and opposite, and the
union has no seam. No special patch is needed; section 14's `box` case
is such a chain (four cylinders, four quarter tori) and reconstructs
exact.

### 7.3 Two selected edges at an angle, the third edge not selected

- **Both convex** (a box's two bottom edges at a corner whose vertical
  edge stays sharp): the tools, each extended past the vertex as in 7.1,
  are subtracted together. The result is the intersection of the two
  singly filleted solids. With equal radii the two cylinders meet in the
  bisecting plane (an ellipse); with unequal radii in a 3D curve, which
  reconstruction writes as a fitted B-spline edge
  (`docs/audits/exact-geometry-rust.md`, 3.1 step 6). Both are exact
  surfaces.
- **Both concave** (a square boss's two base edges at its sharp vertical
  edge): extended tools would add material beyond the boss's side
  planes, so each is cut by the bisecting plane between the two edges (a
  mitre). A true rolling ball would instead wrap around the sharp
  vertical edge on a horn torus; the mitre is simpler and v1 uses it,
  reported in the plan (question 5).

### 7.4 Three selected edges at a vertex

- **Three convex or three concave edges, equal radii, three planar
  faces** (a box corner): the ball touching all three faces is a sphere
  of radius `r` centred `r` from each plane. Each edge tool is cut by the
  plane through the sphere's centre perpendicular to its edge, and a
  corner patch is added: the vertex's corner cut by those three planes,
  minus the sphere (convex), or the sphere's complement in it
  (concave). This is BOSL2's `trimcorners` (`shapes3d.scad:131`) and the
  audit's "corner spheres". Section 14's `corner` case is this
  construction.
- **Anything else** (unequal radii, a mix of convex and concave edges,
  four or more edges, a curved face at the vertex): no quadric patch
  exists. v1 refuses the call with `fillet-unsupported-vertex`, naming the
  vertex and its edges, with a hint that rewrites the call as two nested
  calls where that works: concave edges first, then convex edges of the
  result (`fillet_edges(r, edges = "convex") fillet_edges(r, edges =
  "concave") child`), or the larger radius first. After the first pass
  the second pass's edges at such a vertex form tangent chains (7.2),
  which are supported. Whether one call should do the two passes itself
  is question 5.

## 8. Failure modes and diagnostics

Each has a stable code, NeoSCAD's text in OpenSCAD's `WARNING: ... in
file X, line N` form, the call's span, and hints in the existing JSON
shape (`crates/session/src/diag.rs`), with a `replace` edit where a
concrete fix is known, as the sketch diagnostics do
(`docs/language-extensions.md`, 4.7).

| Code | Severity | Example text and hint |
|---|---|---|
| `fillet-too-large` | error | "fillet_edges(): r = 6 needs 6 on face 'cube, line 3 (top)' beside edge 3, which is 4.2 wide". Hint: the largest radius that fits, found as the sketch's is (closed form where the faces are planar, bisection otherwise), with a `replace` edit of `r` |
| `fillet-overlap` | error | "the blends of edges 2 and 5 overlap on face …: they need 3 + 3 of 5". Same hint |
| `fillet-count` | error | "edges = \"|z\" matched 6 edges, expect = 4" with the six listed (kind, centre, length) |
| `fillet-no-edges` | warning | "edges = \">z and concave\" matched no edge; the child is unchanged" |
| `fillet-skipped` | info | "3 edges named by the selector were skipped: 2 polygon seams of cylinder() at line 7 ($fn = 24), 1 tangent" |
| `fillet-unsupported-edge` | error, or warning under the default `"all"` | "edge 4 joins a cylinder and a cylinder whose axes cross; v1 blends only …" Hint: select fewer edges, or round the profile before the boolean |
| `fillet-unsupported-vertex` | error | section 7.4, with the nested rewrite as a `replace` edit |
| `fillet-no-brep` | error | "the child could not be reconstructed as a B-rep: bodies touch along an edge (…)" with `meshbrep`'s located failure. Hint: overlap touching bodies |
| `fillet-2d` | error | "fillet_edges() needs a 3D child". Hint: `offset(r = …) offset(delta = -…)`, or a sketch `fillet()` |
| `fillet-selector` | error | section 5.2 |
| `fillet-interrupted` | info | blend partly removed by other geometry: "62% of edge 7's blend remains: a hole in child 1 crosses it" |

**Radius too large.** Every tool's tangent curves must lie inside the
face they touch, over the edge's whole length, and no two tools' strips
on one face may overlap except where they meet at a vertex handled in
section 7. Checked before any boolean, on the B-rep in the face's
parameter space (planes: a 2D clip of the offset lines and arcs against
the face's loops; cylinders and cones: their angular and axial extent).
This catches a fillet wider than its face, two fillets on a thin wall,
and a concave fillet that would run past a plate's edge. The earlier
audit's warning that OCCT returned a "done" but invalid solid for a
radius larger than half an edge (`docs/audits/brep-feasibility.md:170-172`)
is why the check is a priori and the result is validated again after.

**After the boolean**, a check per tool: the area of the result's
triangles tagged with the tool's blend surface record against the
tool's analytic blend area. Less means something cut into the blend
(`fillet-interrupted`, info: often intended); none means the selection
and the result disagree, an internal error that is reported, never
silently exported.

**A failed call** leaves its child unchanged (sharp), reports the error
at the call, and the rest of the model renders, so the preview still
shows something and `check` exits with the error (question 4).

## 9. Exact export

- **Exact:** every tool face from a blend or chamfer is tagged with its
  surface: cylinder, torus, cone, plane, sphere. They are surfaces
  `meshbrep` already reconstructs, writes and integrates (`Surface::Torus`
  since exact stage 2, `docs/audits/exact-geometry-rust.md` "stage 2
  built"). The edges between a blend and its faces are tangent contacts:
  a cylinder tangent to a plane along a line, a torus tangent to a
  coaxial cylinder or a perpendicular plane along a circle. The
  generator places mesh vertices on those lines (6.3), which is the
  audit's measured cure for tangency (F2: CSG fillets `f01`, `f02` valid
  with the aligned rule), and section 14 shows plane–cylinder,
  plane–torus and cylinder–torus contacts reconstructed exact.
- **Faceted:** a fillet call with `$fn` set (its arcs are the polygon, by
  the `$fn` rule); blends on edges of faceted regions (not selectable in
  v1); stage F5's blends between unrelated curved surfaces, if built.
- **Report.** Each call is one `exact` substitution ("fillet_edges() is
  exported as 4 exact cylinders and 4 tori, not the 8-segment arcs of
  the mesh"), so the volume cross-check against the normal render
  allows for the arcs' sagitta as it does for any curve made exact
  (`crates/geom/src/exact/check.rs`). Partial faceted fallback applies to
  tool regions as to any source region (`docs/step-export.md`, "Fallbacks
  and the report").
- **Torus pairs** with no closed form fall back to fitted B-spline edges
  (`docs/followups.md`, "Exact geometry"). The v1 cases do not create
  them: a rim torus meets coaxial or perpendicular surfaces only. A
  torus meeting a plane obliquely (a fillet arc cut by a sloped end
  face) would; the surface stays exact.

## 10. Pipeline, determinism, limits, cancellation, WASM, caching

- **Evaluator.** A new `NodeKind::Fillet(Box<FilletNode>)`: kind
  (fillet, chamfer), size, parsed selector and `except`, `expect`,
  resolved anchors, discretizer. The `.csg` label and the key are written
  by one writer (`crates/eval/src/dump.rs:1-35`), so every field is in
  the key.
- **Geometry.** `uses_children` places it with the operations
  (`crates/geom/src/evaluate.rs:597-627`, an exhaustive match). Preview
  treats it as a leaf computed with real geometry, as `hull` and
  `minkowski` are (`crates/geom/src/csg.rs:599`): previewing a filleted
  part renders its child.
- **Caching.** The node's result is cached under its Merkle key like any
  node. The plan is a second cache entry under the same key (selected
  edges, tools, report), so the STEP export and `check` reuse it, and an
  export of an unchanged model recomputes nothing but the tagged tools.
  The export render of the child for selection is keyed by the child's
  key at multiplier 1.
- **Determinism.** Selection sorts edges by (class, curve kind, centre
  lexicographic, length) before anything uses their order; tools are
  generated and combined in that order, in one thread; every sine and
  cosine goes through `libm`, as `meshbrep`'s do
  (`crates/meshbrep/Cargo.toml`, "Transcendental functions in pure
  Rust"; `crates/meshbrep/src/math.rs:3`), so tool vertices are the same
  bits natively and on wasm32. Tools draw original IDs from the node's
  reserved block (`crates/geom/src/evaluate.rs:209`, `:513`), so a warm
  render equals a cold one. Tests at 1, 2 and 8 threads, cold and warm,
  native and wasm32 (section 16).
- **Limits.** Tool triangles count against `Limits::triangles`; the
  child's export render and reconstruction against time and memory like
  any render. A new count, `Limits::fillet_edges` (edges selected per
  evaluation; 100,000 under `Limits::AGENT`, none by default), stops a
  selector on a huge imported-looking model before it generates tools,
  with a `resource-limit` diagnostic. A fillet inside a loop is one
  reconstruction per distinct child key.
- **Cancellation.** The export render and the booleans already run under
  the interrupt flag and guard (`kernel_token`, `walk.rs:166`).
  `meshbrep::reconstruct` takes no stop signal today
  (`crates/meshbrep/src/reconstruct.rs:47-50`: `Options` holds only
  tolerances); stage F1 adds a `should_stop` callback polled per face, so
  a preview of a large filleted child can be cancelled.
- **WASM.** Nothing new: `meshbrep` and Manifold already run in the web
  core (the STEP export does). The /try page gets the toggle with the
  other extensions.
- **Library rules.** No `std::fs`, `std::env` or clock: the plan is a
  pure function of the subtree and the render options.

## 11. Reporting: check, measure, snapshot, LSP, MCP

An agent needs to see what was selected without a STEP viewer.

- **`check`** JSON gains `fillets[]`, one per call: span, kind, size,
  selector, matched count, status, and per edge `{index, curve, sense,
  angle, class, length, center, from, to, faces: [kinds], status}`, with
  vertices patched and skipped edges by reason. The human summary prints
  one line per call: "fillet_edges at line 4: 4 edges (4 line, convex,
  90°), r 2".
- **`measure --fillet N`** (MCP `measure`'s `fillet`) lists the same
  for the N-th call, before and after: the blend faces' radii and areas.
- **`snapshot --fillet N`** draws the child unfilleted with every
  selectable edge thin and the selected ones bold, numbered, in the
  status colour; skipped edges dashed. It is drawn on the CPU over the
  panels like the sketch overlay (`render::snapshot::SketchOverlay`,
  `crates/render/src/snapshot.rs:148`, `:599`), so the same model gives
  the same pixels. This is the "show me the edges" an agent asks for
  before committing a radius.
- **LSP.** Hover on `fillet_edges(` shows the last run's one-line summary;
  completion inside the `edges` string offers the atoms; the "Pin count"
  code action (5.4); `fillet-too-large`'s `replace` edit is a quick fix.
- **MCP.** `check`, `measure` and `snapshot` carry the above; the docs
  resource has both modules labelled as extensions; `recipes` gains a
  fillet recipe for servers started with `--enable fillet`, replacing the
  hand-written tool there.

## 12. Worked examples

These were written in the proposed syntax before anything was built.
Volumes are closed forms; sections 12.1, 12.3 and 12.4 and the top
outline of 12.2 were each built by hand as the tools the design
generates, and exported and read back as section 14 reports. All four
now run as written and are golden models (`l_bracket`, `boss_plate`,
`chamfered_hole`, `box_top` and `lid_lip` in
`conformance/extensions/fillet`, sections 15.3 and 15.4).

### 12.1 L-bracket

```openscad
t = 5; w = 20;
fillet_edges(r = 4, edges = "convex and |y and <x and <z")   // outer heel
fillet_edges(r = 3, edges = "child(0, 1)")                   // inner corner
{
  cube([40, w, t]);
  cube([t, w, 30]);
}
```

The inner call's `child(0, 1)` is the one concave edge where the legs
meet (it stays that edge at any `t`, leg length or width); its tool is
added: a 3 × 3 block minus a cylinder of radius 3 along y. The outer
call's selector names the heel edge at x = 0, z = 0 on the result; its
tool is subtracted. Volume: 6500 + (9 − 9π/4)·20 − (16 − 4π)·20 =
6469.9557. Fully rounding the bracket is
`fillet_edges(r = 1, edges = "convex")` around this: the side faces'
outlines are then tangent chains of lines and the arc where the inner
fillet meets them, a torus about the inner fillet's axis (not
prototyped).

### 12.2 Rounded box with a filleted lid lip

```openscad
L = 40; W = 30; H = 20; R = 5;
module box() fillet_edges(r = 2, edges = ">z")
             fillet_edges(r = R, edges = "|z") cube([L, W, H]);

module lid() fillet_edges(r = 1, edges = "child(0, 1)")   // lip meets lid
{
  translate([-2, -2, 0]) fillet_edges(r = 7, edges = "|z") cube([44, 34, 3]);
  translate([0, 0, 3]) difference() {
    fillet_edges(r = R, edges = "|z") cube([L, W, 4]);
    translate([1.5, 1.5, -1]) fillet_edges(r = R - 1.5, edges = "|z")
      cube([L - 3, W - 3, 6]);
  }
}
```

The box's first call rounds the vertical edges (four cylinder tools);
the second selects the top outline of the result, which is now four
lines and four arcs, tangent at every vertex (7.2): four cylinder tools
and four quarter-torus tools, all exact. Volume, for a box whose
vertical edges are R5 and top edges r2: (L·W − (4 − π)R²)·H − (4 −
π)·(2(L − 2R) + 2(W − 2R)) − 2π·(R − 0.4468)·(4 − π) = 23460.397 (the
spandrel's centroid sits 0.2234·r inside the edge). The lid's call
selects the concave outline where the lip's outer wall meets the lid: a
tangent chain of four added cylinder tools and four quarter tori whose
inner faces lie on the lip's R5 corners; their volume is (1 − π/4)·100 +
2π·(5 + 0.2234)·(1 − π/4) = 28.503.

### 12.3 Boss on a plate (concave, rotational)

```openscad
fillet_edges(r = 2, edges = "child(0, 1)") {
  translate([-20, -20, 0]) cube([40, 40, 4]);
  cylinder(r = 6, h = 14);
}
```

The edge where the boss meets the plate top is a circle, between a plane
perpendicular to the cylinder's axis and the cylinder: rotational class.
The meridian fillet is a quarter circle of radius 2 tangent to both
lines; revolved, an added torus tool (major radius 8, minor 2). Volume:
6400 + 360π + 2π·(6 + 0.4468)·(4 − π) = 7565.744.

### 12.4 Chamfered hole

```openscad
chamfer_edges(d = 1, edges = "%circle and >z")
  difference() {
    cube([20, 20, 10]);
    translate([10, 10, -1]) cylinder(d = 6, h = 12);
  }
```

`%circle and >z` is the hole's top rim (the bottom rim and the cube's
lines are not). The meridian chamfer is a 45° line; revolved, a cone
tool subtracted. Volume: 4000 − 90π − (π/3·(9 + 12 + 16) − 9π) =
3706.785.

## 13. Comparison table

| | FreeCAD PartDesign Fillet/Chamfer | CadQuery `fillet()`/`chamfer()` | build123d `fillet()`/`chamfer()` | BOSL2 rounding | NeoSCAD (this design) |
|---|---|---|---|---|---|
| Kernel | OCCT `BRepFilletAPI` | OCCT | OCCT | mesh: masks and hulls in OpenSCAD | Manifold booleans of generated tools; exact faces by `meshbrep` |
| Edges chosen by | picks in the 3D view, faces, or a whole feature; stored as sub-element names | selector strings on the current result | selectors, filters by axis, type, convexity; history (`Select.NEW`) | `edges=`/`except=` descriptors on the 12 edges of an attachable's bounding cuboid | selector strings (CadQuery subset), provenance (`child`, `part`, `new`), anchors, BOSL2 vectors on the result's bounding box |
| Works on boolean results | yes | yes | yes | no (primitives and attachables only; `edge_profile` masks along a parent's edges) | yes |
| Stored references | yes, with the 1.0 naming algorithm to repair them | none (re-selected each run) | none; history within a run | none | none; `expect=` pins a count |
| Tangent chains | propagates automatically | propagates (OCCT) | propagates; cannot be disabled | — | supported, selected explicitly (a chain is its edges) |
| Convex and concave | both | both | both | convex; negative rounding for external fillets on top/bottom only | both, in one call |
| Corners of three edges | OCCT vertex blends | OCCT | OCCT | `trimcorners` (spherical) | sphere patch, equal radii; otherwise nested calls |
| Variable radius | yes (OCCT) | not in `fillet()` (unverified for lower-level API) | not in `fillet()` | `rounding1`/`rounding2` on masks | no |
| Chamfer forms | equal, two distances, distance and angle, flip | `length`, `length2` | `length`, `length2` or `angle`, `reference` | `chamfer=` | equal distance (asymmetric: question 8) |
| Too-large radius | error, or (measured) an invalid solid reported done | OCCT error | "must be less than 1/2 local width" | silently wrong or a BOSL2 assert (unverified) | a priori check with the largest radius that fits, and a `replace` fix |
| Exact export | STEP | STEP | STEP | mesh only | STEP via `--enable exact`, and mesh |

Sources: section 3. "Unverified" marks what was looked for and not found
in the retrieved sources.

## 14. Prototype evidence

To check that the tools the design generates reconstruct as exact blend
faces, the tools of sections 12.1–12.4 and a box corner were written by
hand as ordinary OpenSCAD CSG (cylinders, cubes, `rotate_extrude` of
"square minus circle", a sphere for the corner) and exported with the
current release, `neoscad 0.5.0 --enable exact -o x.step`, then read back
with the OCCT 8.0.1 oracle (`crates/meshbrep/oracle/build.sh`,
`check.cpp`). The prototype files were throwaway and are not in the tree.

| Case | Faces (ours) | Surfaces | OCCT valid | Volume (OCCT) | Closed form |
|---|---|---|---|---|---|
| L-bracket, 12.1 | 10, all exact | 8 plane, 2 cylinder | yes, 1e-7 tolerance | 6469.9557428756 | 6469.9557 |
| Box top outline, 12.2 (R5 vertical, r2 top) | 18, all exact | 6 plane, 8 cylinder, 4 torus | yes | 23460.3974144645 | 23460.397 |
| Lid lip concave chain, 12.2 | 44, all exact | 24 plane, 16 cylinder, 4 torus | yes | blends add 28.503285 | 28.503 |
| Boss on plate, 12.3 | 9, all exact | 7 plane, 1 cylinder, 1 torus | yes | 7565.7440342951 | 7565.744 |
| Chamfered hole, 12.4 | 8, all exact | 6 plane, 1 cylinder, 1 cone | yes | 3706.7846856650 | 3706.785 |
| Box corner, three r4 edges and a sphere patch | 10, all exact | 6 plane, 3 cylinder, 1 sphere | yes | 7804.6962015903 | 7804.6961 |

(The lid row's figure is the difference between the model with and
without its tools, both exported exact; the closed form of the lid's own
inner `offset(delta)` wall was not derived.)

What the failures taught (all with the same release):

- **Coincident curved faces need one tessellation.** The boss with its
  torus tool's inner face on the boss's cylinder, but with the two at
  different fragment counts (the defaults give the cylinder and the
  revolved profile different counts), did not reconstruct: the export
  fell back to facets in places ("a loop winds 56 times about the
  axis"). The same model with both forced to the same count was all
  exact. The lid's added torus tools on offset-rounded corners failed the
  same way until their inner faces overlapped into the wall instead.
  Hence section 6.2's rule (conform, or overlap into the material) and
  6.3's conforming tessellation.
- **The usual hand-written "0.01 overlap"** around a curved face did not
  reconstruct either (it leaves thin cylinder and plane strips); the
  generator does not use fudge overlaps on curved faces.
- **Not tried:** tools on non-axis-aligned edges (analytic tangency for
  those is unverified in the exact audit, section 11), the nested
  full-rounding of 12.1, and any vertex rule of 7.3.

The normal (mesh) render of every case succeeded; its volumes differ from
the closed forms by the polygons' sagitta, as for any curved model.

## 15. Staged plan, effort and stop rule

Person-weeks, wide bands, in the audit's terms (its stage 4 estimate was
8–14).

| Stage | Scope | Estimate |
|---|---|---|
| F0 | `Extension::Fillet`, the two builtins (absent when off), selector parser with spans and "did you mean", `NodeKind::Fillet`, dump and key, `builtins.toml` labels, flag-off and shadowing tests | 1–1.5 |
| F1 | The plan without geometry: child's export render and reconstruction (cached; `should_stop` in `meshbrep`), per-edge facts (sense, class, provenance, polygon seams), selection, `check`/`measure` reports, `snapshot --fillet`. Ships behind the flag as "selection only" if F2 slips: the call reports its selection and passes the child through with a warning | 2–3 |
| F2 | Translational class: plane–plane (any angle), plane–cylinder and cylinder–cylinder parallel; fillets and equal chamfers; convex and concave; open and closed ends; tangent chains of lines; 7.3 both senses; sphere corners; the too-large and overlap checks with hints; normal render and tagged STEP tools | 3–4 |
| F3 | Rotational class: torus, cone and plane tools; tessellation conforming to the face's polygon; mixed tangent chains (lines and arcs); rims of holes, bosses, cones and spheres | 2–3 |
| F4 | Surfaces: LSP (hover, completion, code actions), MCP recipe and docs, the apps' and /try's toggle, a user reference page (as `docs/step-export.md` is for exact export), editor colouring | 1–2 |
| F5 (optional) | Blends between curved surfaces with no common axis (spine as the intersection of offset surfaces, a faceted pipe, or B-spline surfaces later), unequal-radius and mixed corners, automatic two-pass calls | 3–6, decided after F3 |

F0–F4: 9–13.5 person-weeks.

**Stop rule** (after F3, on the corpus of section 16):

- Continue to F4 if at least 95% of supported-class cases in the
  generated corpus export as valid, fully exact STEP (ours and OCCT), with
  zero silent errors: every case is either a result that passes the
  checks or a diagnostic.
- Between 80% and 95%, the owner decides, with the failure classes.
- Below 80% after two further weeks of fixes, ship fillets as mesh-only:
  the mesh pipeline keeps working, and STEP export writes the tools as
  facets (reported) instead of exact blends, until the classes are fixed.
- Independently, if the *mesh* result fails (non-manifold output, or a
  volume off the closed form by more than the sagitta bound) on more than
  1% of the corpus, stop and fix before any further class.

### 15.1 Stage F0 as built

- **Flag.** `Extension::Fillet`, `--enable fillet`
  (`crates/eval/src/extensions.rs`), outside `--enable all`, checked
  against `Feature.cc` by the existing test. `implemented()` stays false,
  so `serve` does not advertise it until F2 builds geometry.
- **Builtins.** `fillet_edges` and `chamfer_edges` enter the module table
  only with the flag (`crates/eval/src/builtins/modules.rs`, `table`);
  off, a call is OpenSCAD's unknown-module warning and the JSON hint
  names `--enable fillet` (`crates/session/src/diag.rs`). Positional
  order is `r`/`d`, `edges`, `except`, `expect`; `chamfer_edges` also
  takes `r` (giving both `d` and `r` is an error).
- **Selectors.** `eval::fillet::selector` parses the strings into an
  expression tree stored on the node, with byte spans for errors. As
  built against the grammar of 5.2: precedence is CadQuery's (above);
  `except` is `exc` (CadQuery has both); `>>z`/`<<z` without an index
  are index -1 (CadQuery's default); a bare `x`, `y` or `z` is `|x`...
  (CadQuery's bare direction); `xy`/`xz`/`yz`, named views (`top`) and
  `>z[i]` are errors with a hint (`>z[i]` suggests `>>z[i]`); vector
  directions take signed decimals with an optional exponent; part names
  are `[A-Za-z0-9_.-]+` or quoted with `'...'` and anchor names the
  former, both case-sensitive (the rest is case-insensitive); `box()`
  requires each minimum not to exceed its maximum. `part(...)` without
  `--enable part` and `@name` without `--enable query` are selector
  errors.
- **Errors.** `fillet-selector` (a selector string that does not parse,
  or a value that is not a selector) and `invalid-argument` (size,
  `expect`) are `ERROR` lines; the call then becomes a plain `group()`,
  so its children render sharp. A selector error points at the bad
  text inside the string when the argument is a literal written without
  escapes, with a `replace` edit for "did you mean" (edit distance with
  swaps, within a third of the word); otherwise at the expression. The
  message gives the 1-based column in the string.
- **Node.** `NodeKind::Fillet(Box<FilletNode>)` holds the kind, size,
  parsed `edges` and `except`, `expect` and the discretizer. The `.csg`
  prints `fillet_edges(r = 2, edges = "|z and >x", except = undef,
  expect = undef, $fn = 0, $fa = 12, $fs = 2)` with selectors in a
  canonical form (lower case, minimal parentheses) that parses back to
  the same tree; the key is the same label, so `"|Z"` and `"|z"` share
  cache entries. A `.csg` export reads back to itself with the flag.
- **Geometry.** The renderer returns the children's union
  (`crates/geom/src/evaluate.rs`); the preview treats the node as a leaf
  computed with geometry, as `hull()` (`crates/geom/src/csg.rs`); fast
  bounds give up on it; the STEP walk does not descend into it. Each
  valid call warns `fillet-not-built` ("edge rounding is not built yet
  ...; the children are rendered unchanged").
- **Not in F0** (`docs/followups.md`, "Fillets and chamfers"): export
  exit codes for a failed call, `child(i)` range checks and anchor
  resolution, the 2D-child error.

### 15.2 Stage F1 as built

- **The child's B-rep.** `geom::fillet` (`crates/geom/src/fillet/`)
  export-renders the call's children with
  `geom::exact::walk::export_render_traced`, which walks the fillet node
  as the union of its children and records per surface record a
  `Provenance`: the call's child it lies under (an index into the node's
  children, `%` ones counted), the innermost `part()`'s dotted name, the
  leaf instance (numbered in tree order, so a `for` loop's cubes differ),
  and for a facet of a `$fn` polygon which polygon. Nested fillet nodes
  are walked as unions too (their blends are not built), and the STEP
  export still leaves the node to the normal render. Then
  `meshbrep::reconstruct_located`, retried at 2 and 4 times the segments
  after a topology mismatch, as the STEP export does. The facts are
  cached on the `Renderer` (32 entries) by the children's keys and `%`
  flags, so the selector, `expect` and anchors are not part of that key.
  An interrupted request is not cached.
- **Cancellation.** `meshbrep::Options` gained `should_stop`
  (`Option<Arc<dyn Fn() -> bool + Send + Sync>>`), polled between
  reconstruction's stages and once per face in its final face checks,
  ending in the new `Error::Stopped`. `Options` is no longer `Copy`, and
  `Error` has a variant more, so the next meshbrep release is 0.2
  (`docs/followups.md`, "Exact geometry"). The fillet pass sets it from
  the request's interrupt flag and limit guard.
- **Per-edge facts.** For every B-rep edge that is not a periodic
  face's seam: the curve kind; the sense from the two faces' outward
  normals at five points along it (`(n_A × n_B) · d_A > 0` is convex,
  `d_A` the edge's direction in face A's loop), smooth where the normals
  agree within 1e-7, saddle where the sign changes; the material angle at
  the middle; the class (lines between planes or cylinders parallel to
  them; circles whose faces are planes perpendicular to the axis, or
  cylinders, cones, spheres and tori on it); length; centre of mass;
  ends; the two faces' kinds, children, parts and leaves; and a polyline
  for drawing and for `box()`. Normals and points use `libm`'s sines, so
  the facts are the same bits on wasm32. Edges are numbered in the order
  (class, curve, centre, length).
- **Never selected** (5.1): edges with a faceted face, polygon seams (two
  faces sharing a `$fn` polygon: a cylinder's sides but not its caps, a
  sphere's facets, the side planes `linear_extrude` and `rotate_extrude`
  sweep from a `$fn` circle, and every facet of a `$fn` `rotate_extrude`),
  and tangent edges. A `$fn` cylinder's rims stay real edges: the user
  asked for that prism. Seams of `offset(r)` with `$fn` and of other
  polygons with many short sides are not tracked (`docs/followups.md`).
- **Selection.** `all` and `not` range over the selectable edges; every
  other atom tests every edge, so only an atom that names an edge can
  report it skipped (`fillet-skipped`, info, grouped by reason and
  leaf). `>z`, `>>z[i]` follow CadQuery's `CenterNthSelector` as its
  code has it (`cadquery/selectors.py`, `_NthSelector.filter` and
  `cluster`, retrieved 2026-10-08): centres projected on the direction
  and sorted ascending, clustered by distance from each cluster's first
  key (tolerance 1e-6 of the box diagonal), reversed for `<`/`<<`, then
  indexed. So `>>z[0]` is the bottom group, `>>z[-1]` (and `>z`) the top
  one, and `<<z[0]` the top one; an index past the end matches nothing
  (CadQuery raises). This settles the F0 followup. `child(i, j)` with
  `i ≠ j` is narrower than "a face of each": one face must be `i`'s and
  not `j`'s, the other `j`'s and not `i`'s. Coplanar faces of two
  children merge into one face with both, and with the looser rule the
  L-bracket's flush legs (12.1) matched all fifteen of its edges instead
  of the inner corner. `new` likewise needs faces with no leaf in
  common. `part(name)` matches the part and the parts nested in it.
  `@name` matches edges within the tolerance of the anchor's point and,
  with a direction, lines parallel to it or circles whose axis is.
  BOSL2 vectors use the export render's bounding box.
- **Anchors and child indices** are checked when the call's children
  have been instantiated (`Evaluator::fillet_close`): a `child(i)` past
  the children, or an `@name` no child declares, is a `fillet-selector`
  error at the call listing what exists, and the call becomes a group.
  Anchors are looked for among the children and on the call itself (an
  `anchor()` written directly in the call's body lands there). The
  resolved anchors are stored on the node and written into its key, not
  into the `.csg` (which has no anchors), so a `.csg` round trip of a
  call using `@name` fails to resolve it. `fillet-not-built` is now said
  at this point, after the children's own messages.
- **Diagnostics** (section 8), at the call: `fillet-count` (error; the
  selection listed, up to 8 edges, and a hint whose edit writes the
  matched count into `expect`), `fillet-no-edges` (warning, not when
  `expect = 0`), `fillet-skipped` (info), `fillet-unsupported-edge`
  (warning under `edges = "all"`, written or by default, else error;
  class "other" or a saddle), `fillet-no-brep` and `fillet-2d` (errors).
  Empty children say nothing. They print as OpenSCAD-style lines from
  every host: `session::fillets::report` runs after the render in the
  session (`Session::build`, so `check`, `measure`, `snapshot`, preview
  and export requests alike) and on the command line (`run.rs`), only
  for a tree that has a fillet node.
- **Reports.** `check`'s `fillets` (one object per call, the selected
  edges, the skipped ones by reason, `codes`, and `pin`, the "Pin count"
  edit) and a text line per call; `measure --fillet INDEX|SELECTOR`;
  `snapshot --fillet INDEX|SELECTOR`, the selected edges bold and
  numbered over the model through the sketch overlay
  (`render::snapshot::SketchOverlay`, not depth-tested, so hidden edges
  show), skipped ones dashed, the others thin. The same through `serve`
  (the commands' parameters) and MCP (`fillet` on `measure` and
  `snapshot`; `check`'s text and structured content list the calls).
  Shapes are in `docs/cli-json.md`.
- **"Pin count".** The language server offers it as a refactoring on a
  call when a host has supplied a rendered run's log for the document's
  current text (`Server::supply_log`, as the apps do); its own runs only
  evaluate, so they keep a rendered run's reports of the same text
  rather than drop them. On a wrong `expect`, the same edit is the
  `fillet-count` diagnostic's quick fix.
- **Corrections to this text.** Section 12.2's lid: `child(0, 1)` is
  also the lip's *inner* wall where it stands on the lid, so the call
  selects eight concave lines (with the lip's own corner fillets unbuilt)
  where the text describes only the outer outline;
  `"child(0, 1) and not box(...)"` around the inside picks the outer
  four. And until F2 builds blends, a call nested around another (12.1's
  outer call, 12.2's top outline) selects on the inner call's
  *unrounded* child: 12.2's `>z` is four lines, not the chain of lines
  and arcs it will be.
- **Not in F1:** `Limits::fillet_edges`, export exit codes for a failed
  call and `check`'s status (`check` counts its own findings; a
  `fillet-count` error is in `diagnostics` but not in `counts`), LSP hover
  and completion, and the blends (`docs/followups.md`, "Fillets and
  chamfers").

### 15.3 Stage F2 as built

- **The tool generator** is `meshbrep::blend` (`crates/meshbrep/src/blend.rs`,
  MIT OR Apache-2.0, no NeoSCAD dependency): a `BlendSpec` (profile,
  size, edges with their two faces as exact planes or parallel
  cylinders with their outward sides, how each end ends, sphere
  corners) gives closed, oriented `TaggedMesh` tools, each triangle
  tagged with its exact surface, and `blend::section` gives an edge's
  cross-section (fillet centre, tangent points, how far into each face)
  for the checks. The arithmetic is 6.1's in 3D vectors across the
  edge: the ball's centre where the faces offset by `r` meet (two
  planes; a plane and a circle; two circles, the solution nearest the
  edge), the arc between the tangent points with vertices exactly at
  both (`segments(sweep)` from the call's `$fn`/`$fa`/`$fs`, as for
  `circle()`), and a chamfer's line between the points `d` along each
  face (an arc length on a cylinder). Every sine is `libm`'s.
- **The region** (6.2): convex, the corner between the arc and the faces
  pushed out past both faces by a margin (`r`, at most half a
  cylinder's radius), so no tool face is coplanar with the child's;
  concave, the space between the faces and the arc, its side on a
  plane in that plane (tagged with the face's own plane record) and its
  side on a cylinder overlapping into the material by the margin
  instead (section 14's lesson). Swept along the edge and cut by each
  end's plane; caps that cross inside the tool are an edge too short.
- **Ends** (`crates/geom/src/fillet/build.rs`, from the B-rep's
  vertex-edge incidence): past a tangent edge touching one of the
  edge's faces (a line running into an arc), cut across the edge (7.2);
  at a vertex of three faces with a plane third face and no other
  selected edge, convex: extended into the air beyond that face when it
  faces the way the edge leaves (open), cut by it when the edge runs
  into it (a wall); concave: cut by it either way (7.1); a second
  selected edge there: both extended when convex, both cut by the
  bisecting plane when concave (7.3, the mitre of decision 5); three
  selected edges of planes: a sphere corner (fillets) or extended
  chamfers meeting in a point (convex chamfers; three concave chamfers
  are refused). Convex and concave selected edges at one vertex, more
  than three faces with another selected edge, or a curved third face
  (except a convex open end) are `fillet-unsupported-vertex`.
- **Sphere corners** (7.4): the ball's centre `r` inside the three
  planes; each edge tool ends on the plane through it across the edge,
  and the patch is the corner region between those planes and the faces
  (offset by the margin, convex) less the ball. The edge tools of a
  corner and its patch are built as **one solid**: their shared rings
  are the same bits (the points beside a tangent come from one
  expression on the same face normals), and the caps between them are
  left out. Built as separate solids, the caps coincided exactly only on
  exact coordinates: under `rotate([10, 20, 30])` the boolean left a
  sliver between them that the export's box check rejected. Mitred
  pairs are joined the same way (`End::Mitre`) where their
  cross-sections meet point for point in the mitre (equal angles: the
  later tool takes the earlier one's ring); before that, a mitred boss
  base rotated inside the call did not reconstruct at all.
- **Checks before the boolean** (section 8), on the B-rep: every blend
  fits its cross-section (`fillet-too-large`, "no blend that size fits
  between its faces"); at five points along each edge, the face's other
  boundary edges (those at the edge's own ends excepted: the vertex
  rules handle them) are crossed by the plane across the edge, and the
  nearest crossing into the face (a distance on a plane, an arc length
  on a cylinder) must be wider than the strip (`fillet-too-large`) or,
  when it is another selected edge's, than both strips
  (`fillet-overlap`), strictly: a face is never consumed to nothing;
  and the tools' end caps must not cross. On failure the largest size
  that passes the same checks is found by bisection (60 halvings),
  then written with three significant digits, rounded to nearest if
  that still passes, else down; its hint carries it as a `replace`
  edit of the `r`/`d` argument (`session::fillets::size_edit`), and
  `crates/geom/tests/fillet_build.rs` applies every such edit and
  builds. A hole inside a strip is crossed by the ray, so it is
  `fillet-too-large` here rather than section 8's `fillet-interrupted`.
- **`fillet-unsupported-vertex`'s fix** for mixed senses rewrites the
  call's head as two nested calls, `edges = "(S) and convex"` outside
  `edges = "(S) and concave"` (`session::fillets::nested_edit`), when
  `edges` is absent or one plain string; otherwise the hint is text.
- **Normal render** (`crates/geom/src/evaluate.rs`, `Ctx::fillet`): the
  children's union as OpenSCAD tessellates it, the concave tools added
  and the convex ones subtracted in two batched booleans, the tools'
  original IDs from the node's own block in tool order (so warm equals
  cold). A fillet's cylinder faces are OpenSCAD's polygons there, so
  each is replaced by the facet its exact tangent line touches (the
  nearest child triangle facing that way, 6.3's "conform"): the arc
  then meets the facet along a mesh line, where a tangent on the exact
  cylinder left the inscribed facet standing over part of the blend.
  Chamfers keep the exact cylinder. Blend faces are not drawn as cut
  faces, and take the child's colour or `part()` when all its faces have
  one. A call that builds nothing (no edges, an error, F3's edges)
  leaves the union as it is; preview draws the node as a leaf with this
  geometry, as before.
- **STEP export** (`crates/geom/src/exact/walk.rs`, `Walk::fillet`): the
  walk now descends into fillet nodes (nested ones too, in the traced
  walk selection uses, where only the call being selected for is walked
  as its children's union), unions the children tagged and applies the
  same plan's exact tools (arcs at the
  walk's segment multiplier), placed under the node's matrix; the call
  is one `Exact` substitution ("is exported with exact blends (4
  tools)"), its arcs' sagitta and area added to the cross-check's
  bounds. With `$fn` set on the call the blend triangles are planar
  facets (a `Polygon` substitution), as for any curve.
- **After the boolean** (`fillet::blend_diags`, run by the report): per
  blend surface, the area of the normal render's triangles lying on it
  (within twice the arcs' deviation, facing along its normal, inside its
  tool's box) against the tool's blend area less what lies past an open
  end or past the bisector where two extended blends meet. Under 98% is
  `fillet-interrupted` (info); under 2% is `fillet-failed` (error). It
  is geometric rather than by original ID, because cached results are
  rebased to new IDs; the surfaces do not move.
- **Statuses and codes**: `built`, `not-built` (warning
  `fillet-not-built`, now only for circles and arcs, said by the report
  rather than the evaluator), `too-large`, `overlap`,
  `unsupported-vertex`, `failed`; new codes `fillet-too-large`,
  `fillet-overlap`, `fillet-unsupported-vertex`, `fillet-interrupted`,
  `fillet-failed`. Unsupported edges under the default `"all"` stay
  sharp with their warning and the rest is built.
- **A failed call** (decision 2): every error of a fillet call
  (`fillet-*` codes, and the argument errors, whose messages start with
  the module's name) is counted by the console
  (`eval::Console::failed_fillets`); `-o` writes its files and exits 1
  (`crates/cli/src/run.rs`), and `check` adds them to `counts.errors` as
  `counts.fillet_errors` and fails. `Extension::implemented()` is true
  for `fillet`, so `serve` advertises it.
- **Results.** The golden models (`conformance/extensions/fillet`, 15
  cases: the L-bracket, the box corner, every edge of a cube, the box
  top's lines, the lid lip's lines, a bottom outline (7.3), a mitred
  boss base, a closed end, an inside sphere corner, 60° prisms filleted
  and chamfered, a chamfered cube, plane–cylinder, a rotated cube, a
  mitred boss base rotated inside the call)
  export all-exact; their B-rep volumes are within 1.3e-8 of the closed
  forms (within 1e-9 for the cases without a sphere patch), and OCCT
  8.0.1 reads every file back as one valid closed solid with no free
  edges, its volume within 1.2e-8 (the worst, the box corner's sphere
  patch, where OCCT and `meshbrep::measure` land 1.2e-8 and 4.4e-9 on
  either side of the closed form; section 14's hand-built file measured
  the same 7804.6962015903 in OCCT). At `$fa = 2; $fs = 0.05` the mesh volumes are
  within 2e-4 of the closed forms. STEP and mesh bytes are the same at
  1, 2 and 8 threads, cold and warm; the wasm-check case
  `fillet-step-export` hashes the same STEP in node as natively.
- **Not in F2**: the rotational class (F3); a chamfer's cylinder faces
  conformed to the polygon in the mesh; blends between faces of
  different colours or parts taking one; `Limits::fillet_edges`; a
  corner of three edges with a curved face (`docs/followups.md`,
  "Fillets and chamfers").

### 15.4 Stage F3 as built

- **Revolved tools** (`crates/meshbrep/src/blend/revolve.rs`): an edge's
  `Path::Arc` (centre, axis, radius, sweep, sections) makes its
  cross-section in the meridian half-plane through its start, where a
  plane square to the axis, a coaxial cylinder or cone are lines and a
  sphere centred on the axis or a coaxial torus are circles
  (`BlendFace` gains `Cone`, `Sphere` and `Torus`), so section 6.1's
  arithmetic is the straight edges' own; the region is then revolved.
  The fillet arc sweeps a `Surface::Torus`, a chamfer's line a cone (or
  a plane or cylinder where it is square to the axis or along it), each
  other side the cone, plane or cylinder its segment sweeps; a concave
  tool's side on a plane square to the axis is tagged with that plane.
  Every ring point must stay off the axis, and a fillet's centre must
  be further from it than the radius (meshbrep writes ring tori only),
  so a boss's convex top rim takes a fillet up to half the boss's
  radius: beyond that `fillet-too-large` says so ("short of their
  axis").
- **Conforming** (6.3), not by the walk recording each leaf's count and
  phase but from the mesh the tools are applied to: the vertices of the
  child's mesh on the edge's circle (the polygon of a cylinder or cone
  beside it, wherever it came from: a primitive, an extrusion, an
  offset's arc, an earlier blend) are the tool's sections
  (`fillet::result::sectioned`), in the normal render (OpenSCAD's
  polygon) and in the export (the walk's, mapped back through the
  node's matrix). Each section is moved out along its radial by its
  vertex's distance off the circle, so the tool's tangent ring runs
  through the polygon's vertices themselves, chord for chord with its
  facets. Without that, a 2D offset's arc, a few 1e-9 inside its exact
  circle, left slivers. Arcs beside spheres and tori are not conformed
  (a sphere's rings do not pass through the rim): their mesh keeps the
  sphere's facets standing over the blend by up to their sagitta; the
  export is exact all the same. Chamfers along straight cylinders are
  now conformed to the facet as fillets were (the F2 followup).
- **Ends of arcs**: a whole circle has none. A partial arc ends where
  it runs on smoothly into another edge (cut across, on the plane
  through the axis, taken through the vertex itself), or on a plane
  through its axis (a half hole at a plate's edge: run on past it into
  the air by up to 22.5° when convex and the material ends, else cut
  by it). Anything else is `fillet-unsupported-vertex` ("an arc's blend
  ends only where it runs on smoothly or on a plane through its axis").
  One circle split at the vertices where its faces' seams reach it (a
  countersink's cone meeting its hole) is a chain of its pieces.
- **Mixed chains** (7.2): where two selected edges run on into each
  other (`End::Chain`, for lines too), their tools are one solid, as F2
  joined mitred pairs: the later takes the earlier's ring and neither
  has a cap. For the rings to meet point for point the margin is the
  least along the chain, and a concave chain's line overlaps into its
  plane face where its neighbour arc's matching face is curved. A
  rounded box's top outline (12.2) is one tool of four cylinders and
  four quarter tori; the lid's two concave outlines are two.
- **Overlap into the material** (6.2) beside a concave tool's curved
  face is at most half the gap to a coaxial cylinder behind it (the
  inside of a tube or a lip), so it cannot reach through a thin wall
  (`fillet::build::wall`).
- **Checks** (section 8) for arcs look across the edge in its meridian
  half-planes at 64 places a turn, and at every boundary point of the
  face in its own meridian with each boundary segment's point nearest
  the axis: the strip's width on a plane, along a cylinder's or cone's
  generator, or round a sphere's or torus's meridian. Straight edges
  gained the same: every boundary point across from the edge, and a
  circle's nearest point exactly, besides the five rays (a hole beside
  an edge came nearest between them).
- **Hints** offer 5% under the largest size that fits, three
  significant digits ("use r = 1.9: the largest that fits is just under
  2, which leaves almost nothing of the face beside the blend"): at the
  limit itself the blends leave a sliver of face that prints as nothing
  and that the export's reconstruction, near tangent along its whole
  length, does not survive. Every hint edit applied builds
  (`crates/geom/tests/fillet_build.rs`, ten cases, six of them
  rotational).
- **After the boolean**, a triangle of the result also counts as blend
  when it is a piece of one of its tool's facets: a coarse rim's
  facets turn too far from the surface's normal for the angle test
  alone (a 0.085 rim fillet on a 10-sided hole reported half its blend
  missing).
- **2D offsets' arcs** (`crates/geom/src/exact/profile.rs`,
  `attribute_offset`): a round join's centre is now the corner of the
  two exact lines it joins rather than the polygon's vertex, which
  Clipper's grid had rounded by about 3e-9 at 15.3; the arc then
  touches its lines exactly, which a fillet ending across the tangency
  needs.
- **`fillet-not-built` is gone**: every class but section 6.1's "other
  edges" is built. Those (two cylinders crossing, ellipses, B-splines,
  saddles) are `fillet-unsupported-edge`: an error that leaves the
  child sharp when named, a warning under the default `"all"` with the
  rest built (decision 2 and section 8, as F2 left it).
- **Results.** 29 golden models (`conformance/extensions/fillet`): F2's
  15 and 14 of F3's (12.3's boss, 12.4's chamfered hole, 12.2's box
  and lid fully built, a hole rim, both rims of a boss, a cone rim
  filleted and chamfered, a ball cut flat and a ball sunk in a plate,
  a half hole at a plate's edge, a rounded rectangle extruded from 2D,
  a blind hole chamfered, the box rotated). Their closed forms are
  Pappus's (exact boundary integrals of the revolved regions for the
  cone and sphere rims). Every STEP is all exact, OCCT 8.0.1 reads
  each back valid and closed with no free edges, and the rotational
  ones' volumes agree with the closed forms to 7e-14 (OCCT) and 7e-16
  (ours, `meshbrep::measure`; 1.3e-8 worst over all 29, F2's sphere
  corner). Meshes are
  within 5% at OpenSCAD's defaults and within 2e-4 at `$fa = 1; $fs =
  0.05` (2° was too coarse a polygon for a 10-radius frustum's own
  volume). STEP and mesh bytes are the same at 1, 2 and 8 threads,
  cold and warm, for ten goldens (five rotational); the wasm-check case
  `fillet-rotational-step` (a boss's rims, a chamfered hole, 12.2's box
  turned 90°, a ball cut flat, a half hole) hashes the same STEP in
  node as natively.
- **The stop rule's corpus** (`conformance fillet-corpus`, section 16;
  `crates/conformance/src/fillet_corpus.rs`): seeded plates, boxes,
  rounded boxes and L brackets with through and blind holes,
  countersinks, bosses, slots and pockets (rounded rectangles from 2D
  offsets), and holes through a bracket's leg along x; one call with a
  selector from the atoms and a log-uniform size from 0.25 to 6, a
  quarter of them chamfers; every model exported in its own process
  with `--limit`s, the sweep held under 2 GB, OCCT reading every file
  back. On 2,000 models (seed 2, 2.5 minutes at 4 jobs): 1,174 built
  and valid, 561 refused as too large or overlapping whose hint edit
  then built valid, 81 refused by v1's classes (76 vertices, 4 named
  unsupported edges, 1 child with no B-rep), 179 selecting nothing,
  none killed; 1,735 of the 1,740 supported-class cases (99.7%) export
  all-exact STEP that OCCT reads back valid with our volume, and no
  mesh failed. The five failures: three fixes near the fit limit
  (within 10% of the largest size, a thin band of face left between
  two blends, which smaller sizes clear), one 0.02 fillet on a sliver
  face 0.04 wide, and one F2 case that also fails on F2's commit (the
  B-rep's "more than one outer loop" on a pocketed bracket). On 300
  (seed 1, the default): 259 of 259 valid. The rule's "continue to F4"
  threshold (95%) is met.
- **Not in F3**: spindle tori (a convex rim fillet larger than half the
  rim's radius); arcs ending on other faces; a concave tool's overlap
  capped against walls other than coaxial cylinders; the near-limit
  and sliver failures above; everything F2 left (`docs/followups.md`,
  "Fillets and chamfers").

### 15.5 Stage F4 as built

- **Language server** (`crates/lsp/src/fillet.rs`). A string is a
  selector when it is the `edges` or `except` argument (named, or second
  and third; alone or in a list) of a `fillet_edges`/`chamfer_edges`
  call that resolves to the builtin, with `fillet` among the server's
  extensions (`World::fillet`, as `query` gates its names). Inside one,
  completion reads the text rather than parsing it (a string being typed
  is mostly not a selector yet): the atoms of 5.2 where an operand goes
  (start, `(`, after an operator), with snippets for `>>z[i]`,
  `child(i, j)`, `part(name)` and `box(...)`; `and`, `or`, `exc` after
  an operand; nothing inside `child(`, `part(` or `box(`; and when nothing
  matches what was typed, the "did you mean" word (the diagnostics'
  distance), its `filterText` the typed text so the editor's own filter
  keeps it. `@name` is offered only with `query`; `part(name)` always,
  its detail saying it needs `--enable part`, because the apps turn
  parts on per window, not through the server's extensions. A unit test
  parses every offered atom and operator with the evaluator's parser,
  so completion cannot write a selector the call rejects. `"` is a
  completion trigger character. Hover inside the string explains the
  atom or operator under the cursor; hover on a named argument of *any*
  builtin call shows that parameter's line of the reference (before
  the word is taken for a builtin of the same name: `scale` in
  `linear_extrude(scale = 2)` was the `scale()` module); hover on the
  call's name adds the last rendered run's line and its first eight
  edges (`session::fillets::line_text`, `edge_text`), the innermost
  report whose span holds the name, as hover on `sketch` adds its state.
  Builtin completion offers `fillet_edges` and `chamfer_edges` only with
  the extension (the F3 followup).
- **Diagnostics on the text to change** (`session::fillets::diag_span`):
  `fillet-count`, `fillet-no-edges`, `fillet-skipped` and
  `fillet-unsupported-edge` now carry the span of the `edges` argument,
  `fillet-too-large` and `fillet-overlap` that of `r`/`d`, the rest the
  call's; the console's line stays the call's. An editor's marker on a
  call around a `difference()` underlined most of the model.
- **Fixes as code actions.** Every hint with a `replace` edit was
  already a quick fix (`lsp::diagnose::fixes`); F4 adds the tests that
  a host's rendered run brings the size fix, the nested rewrite and the
  count's edit through to `textDocument/codeAction`, and "Pin count"
  is unchanged.
- **Correction: the nested rewrite under the default `"all"`.** Applied
  to the case that offers it (a block on a plate, section 7.4), the F2
  rewrite `edges = "(all) and convex"` outside `edges = "(all) and
  concave"` did not build: the inner call's mitred concave blends meet
  in four short ellipses at the block's corners, and the outer call,
  its edges now named, refused them as `fillet-unsupported-edge` errors
  where the original call, under the default, would only have warned
  (`crates/geom/tests/fillet_build.rs` and the session test had checked
  only the inner call). Under `"all"` (absent or written) with no
  `except`, the rewrite now keeps the default and leaves the other sense
  out: `except = "concave"` outside `except = "convex"`; both build, the
  ellipses sharp with the default's warning. A narrower selector is
  narrowed as before. The hint's text says which form it writes.
- **MCP.** `recipe_fillet.scad` (12.1's L-bracket, with the common
  selectors in its comment), for servers started with `--enable fillet`,
  on `SKETCH_RECIPE`'s terms: never in the instructions (unchanged, as a
  test checks); `docs` for `fillet_edges` or `chamfer_edges` ends with
  it; the index has one line; `neoscad://recipes` appends it; and `docs`
  for `fillet` or `chamfer` (the hand-written printing recipe, or the
  sketch vocabulary's entry) adds a line pointing at the builtins. The
  hand-written `fillet` recipe stays in the instructions, which are the
  same for every server. `docs/mcp.md` documents the recipe and the
  `fillet` arguments of `measure` and `snapshot`.
- **Apps and /try.** "Edge fillets and chamfers (fillet)" beside the
  other extensions: macOS Settings > Language (`LanguageSettings.fillets`,
  default key `EnableFillets`), Linux Preferences > Language
  (`language.json`'s `fillet`), Windows Design > NeoSCAD Extensions
  (`language.json`'s `fillet`). The web page has no language settings of
  its own (its only extension toggle, `exact`, is in the Export menu and
  goes with STEP exports alone), so the toggle is a View menu item under
  "NeoSCAD extensions", kept in the page's settings and sent as `enable`
  with every run, check, measure and export; the worker's language
  server takes a run's extensions (`crates/web/src/lib.rs`, `run`), so
  completion follows the toggle.
- **Editor colouring.** `fillet_edges` and `chamfer_edges` are
  coloured as transformations (`apple/Editor/web/src/lang/builtins.js`),
  whether the extension is on or not, as the query names are. Selector
  strings stay strings: no other string is coloured by meaning.
- **User reference** `docs/fillet-edges.md`, linked from the README,
  `docs/language-extensions.md`, `docs/step-export.md`, `docs/lsp.md`,
  `docs/mcp.md` and both builtins' notes. Its examples run through
  `check` in `crates/session/tests/fillet.rs` (`docs_examples_check`),
  each with exactly the diagnostic codes its fence names, and the plain
  ones built.
- **Not in F4**: geometry diagnostics, last-run hover and "Pin count"
  in `neoscad lsp --stdio`, which only evaluates; completion of part and
  anchor names inside `part(` and after `@`; share links of /try do not
  carry the toggle, so a shared filleted model opens with the extension
  off; the CHANGELOG entry, written at release (`docs/followups.md`,
  "Fillets and chamfers").

## 16. Test plan

- **Flag off.** `fillet_edges(...)` and `chamfer_edges(...)` give the
  nightly's unknown-module text; JSON hints name the flag; a program's
  own `module fillet_edges` wins with the flag on; BOSL2's `fillet()` and
  MCAD's `chamfer()` are untouched with the flag on; `conformance run`
  unchanged against `conformance/baseline.json`; the extension name
  checked against `Feature.cc` (existing test).
- **Closed forms.** Golden models under `conformance/extensions/fillet`,
  each with its closed-form volume (Pappus for revolved tools, spandrel
  areas `r²(1 − π/4)` and generalisations for other angles,
  `r³(1 − π/6)` per sphere corner): the four worked examples, the corner,
  plane–plane at 30°, 60°, 120° and 150°, concave and convex, chains,
  cylinder–plane parallel, cone and sphere rims, chamfers. STEP volume
  within 1e-8 of the closed form; mesh volume within the sagitta bound.
- **OCCT read-back.** Every golden and corpus STEP file through the
  oracle (`MESHBREP_OCCT_CHECK`): valid, closed, volume agreeing.
- **Generated corpus.** Plates, boxes and brackets with holes, bosses,
  slots and pockets at random sizes, selectors drawn from the atoms,
  radii up to and past what fits; capped (at most 2,000 models, 200
  edges each), every evaluation under `Limits::AGENT`, a 2 GB resident
  guard on the process. Each case: either a valid exact result or a
  diagnostic; a too-large case's hint edit applied must make it pass
  (the sketches' `every_hint_edit_fixes_its_problem` pattern). Built
  as `conformance fillet-corpus [--count N] [--seed S] [--occt PATH]`
  (section 15.4), 300 models by default.
- **Selection.** Unit tests of every atom and operator on known solids;
  CadQuery-equivalence cases (the same box and selector strings as
  CadQuery's selector docs, edge counts compared with the documented
  results); BOSL2 vectors on cubes against `_edges()`'s sets.
- **Determinism.** 1, 2 and 8 threads; cold and warm session exports
  byte-identical; STEP bytes native and wasm32 identical
  (`scripts/wasm-check.sh` case); the call memo and statement memo on and
  off give the same output.
- **Cancellation and limits.** A cancelled preview of a filleted large
  child stops within the poll interval; `fillet_edges` limit and the
  triangles limit trip with `resource-limit`.
- **Reports.** `check` JSON shape (documented in `docs/cli-json.md`),
  snapshot overlay pixels stable, LSP hover and code actions, MCP tool
  results.

## 17. Alternatives considered

- **`fillet()` / `chamfer()` as names.** Shadowed by BOSL2 and MCAD
  (section 2), with silently wrong geometry, and doubled with the sketch
  vocabulary.
- **Arguments on primitives** (`cube(10, rounding = 2)`, BOSL2's
  `cuboid`). Only primitives, never boolean results, and it changes what
  OpenSCAD's own builtins accept when the flag is on.
- **Exact-only fillets** (sharp in the mesh, rounded in STEP), the
  earlier audit's option. A print would not match the CAD file; rejected
  by the requirement that what renders and prints is what is exported.
- **Minkowski-based rounding** (opening and closing with a sphere).
  Rounds everything or nothing, is slow, and has no exact surfaces.
- **Edge indices** (`edges = [3, 7]` from a report). The topological
  naming problem by construction. Indices appear in reports for reading
  only.
- **A function form** (`e = edges_of(...)`, then `fillet_edges(r, e)`).
  Needs a render inside evaluation, as geometry queries do; selectors
  evaluated in geometry need none. It could be added on the query oracle
  later.
- **OCCT for fillets.** The owner's pure-Rust, WASM-clean rule, and OCCT's
  measured silent failure (`docs/audits/brep-feasibility.md:159-180`).

## 18. Decisions

Settled by the owner (2026-10-08):

1. **Names:** `fillet_edges()` and `chamfer_edges()`, not `fillet`/`chamfer`.
2. **A failed call:** the child stays sharp, the call is an error with a
   fix hint, and an export of the model exits non-zero.
3. **Where the tool generator lives:** in `meshbrep` (MIT OR Apache-2.0),
   written fresh.
4. **`.csg` export:** prints `fillet_edges(...)` as `part()` is printed.

The remaining questions take the recommendations below: one `--enable
fillet` flag for both modules; `edges = "all"` by default, unsupported
edges as warnings; mitred concave corners, with nested calls for mixed or
unequal corners for now; curved-curved blends (F5) an error until exact;
equal-distance chamfers only in v1; the normal render keeps OpenSCAD's
tessellation for the child.

## 19. The questions as first asked


1. **Names.** `fillet_edges()` and `chamfer_edges()` (recommended: no
   collision with BOSL2's `fillet`, MCAD's `chamfer` or the sketch
   vocabulary), or `fillet()`/`chamfer()` with the shadowing risk?
2. **Flag.** One flag, `--enable fillet`, for both modules
   (recommended)?
3. **Default selection.** `edges = "all"` (recommended: BOSL2's and
   FreeCAD's "all edges" default, and unsupported edges under the default
   are warnings, not errors), or require `edges` explicitly?
4. **A failed call.** Leave the child sharp and report an error
   (recommended), or produce nothing? And should exports refuse a model
   with a failed fillet, so an agent cannot ship a sharp part believing
   it rounded? (Recommended: `-o x.stl` writes it and exits non-zero.)
5. **Corners.** v1 mitres two concave edges at a sharp corner (7.3) and
   refuses mixed and unequal corners with a nested-call rewrite (7.4).
   Should one call later do the two passes itself, and should the
   concave corner wrap a rolling-ball horn torus instead of a mitre?
6. **Where the tool generator lives.** In `meshbrep` (`MIT OR
   Apache-2.0`, written fresh so the crate stays publishable: "blend
   tools from a B-rep edge"), or in `geom` (GPL), which could reuse the
   sketch's fillet arithmetic directly? Recommended: `meshbrep`.
7. **Beyond v1 (F5).** Faceted rolling-ball blends between curved
   surfaces with no common axis (meshes print fine; STEP gets facets),
   or an error until exact surfaces are possible?
8. **Chamfers.** Equal distance only in v1 (recommended), or also two
   distances / distance and angle, which need a rule for which face is
   first that does not depend on edge orientation?
9. **Normal-render tessellation.** The child keeps OpenSCAD's
   tessellation and tools conform to it (recommended, 6.4), at the cost
   of a second (aligned) render of the child for selection. The
   alternative renders the child once, aligned, so a filleted cylinder
   has 16 sides where the same unfilleted cylinder has 13.
10. **`.csg` export.** Print `fillet_edges(...)` as `part()` does
    (recommended), accepting that the `.csg` of a filleted model is not
    plain OpenSCAD?
