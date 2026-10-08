# Design: 3D fillets and chamfers (`--enable fillet`)

Status: design, not built. Written 2026-10-08 against `127be03` and the
reference checkouts in `.reference/openscad` and `.reference/BOSL2`.
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

Operators, loosest first: `or`, `exc` (set difference), `and`, `not`, and
parentheses, as CadQuery's (`selectors.rst:33-50`). So `"|z and >x"` is
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

These are written in the proposed syntax and have not run (nothing is
built). Volumes are closed forms; sections 12.1, 12.3 and 12.4 and the
top outline of 12.2 were each built by hand as the tools the design
generates, and exported and read back as section 14 reports.

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
  (the sketches' `every_hint_edit_fixes_its_problem` pattern).
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
