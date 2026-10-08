# Edge fillets and chamfers

> NeoSCAD extension (`--enable fillet`); not in OpenSCAD.

`fillet_edges()` rounds, and `chamfer_edges()` bevels, chosen edges of
any solid: a primitive, a `difference()`, an extrusion, the result of
another fillet. The edges are chosen by a short selector string such as
`"|z"` (the vertical edges) or `"%circle and >z"` (the top rims of
holes), as in CadQuery's `.edges("|Z").fillet(2)`, and the result is an
ordinary solid that previews, renders, prints and exports like any
other. With `--enable exact` it exports to STEP with true cylinder,
torus, cone and sphere faces, the fillets a CAD program such as FreeCAD
gives with PartDesign Fillet and Chamfer.

OpenSCAD has no operation on a solid's edges: a model rounds a 2D
profile with `offset()` before extruding, rounds everything with
`minkowski()`, or subtracts hand-made tools edge by edge. This page is
the user reference; the design, with its sources, measurements and
stage notes, is `docs/fillets.md`.

Contents:

- [OpenSCAD superset: turning fillets on](#openscad-superset-turning-fillets-on)
- [Quick start](#quick-start)
- [The two modules](#the-two-modules)
- [Selecting edges](#selecting-edges)
- [Worked examples](#worked-examples)
- [What is rounded](#what-is-rounded)
- [Diagnostics and fixes](#diagnostics-and-fixes)
- [STEP export, `--enable exact` and `$fn`](#step-export---enable-exact-and-fn)
- [Tools: check, measure, snapshot, the editor and agents](#tools-check-measure-snapshot-the-editor-and-agents)
- [Comparison with FreeCAD, CadQuery and BOSL2](#comparison-with-freecad-cadquery-and-bosl2)
- [Limitations](#limitations)

Every `openscad` example below is checked by a test
(`crates/session/tests/fillet.rs`, `docs_examples_check`), which runs
`check` on it with the extension on and requires exactly the diagnostic
codes its fence names (```` ```openscad expect=code ````), none for a
plain one.

## OpenSCAD superset: turning fillets on

NeoSCAD's language is a superset of OpenSCAD's, and fillets are off by
default. Off, a file means exactly what it means in OpenSCAD:
`fillet_edges(...)` is an unknown module with OpenSCAD's own warning,

```text
WARNING: Ignoring unknown module 'fillet_edges' in file model.scad, line 1
```

and the JSON hint names the flag. Turn them on with:

- the command line: `neoscad --enable fillet` (and the same flag for
  `check`, `measure`, `snapshot`, `test`, `serve`, `lsp` and `mcp`).
  OpenSCAD's `--enable all` turns on OpenSCAD's experiments only, not
  NeoSCAD's extensions;
- the apps: Settings > Language > "Edge fillets and chamfers (fillet)"
  on macOS, Preferences > Language on Linux, and Design > NeoSCAD
  Extensions on Windows. Changing it runs the open documents again;
- the web page (neoscad.org/try): View > "Edge fillets and chamfers
  (fillet)".

Every surface marks the two modules with the label "NeoSCAD extension
(`--enable fillet`); not in OpenSCAD": `neoscad docs fillet_edges`, the
editor's hover and completion, and the MCP `docs` tool.

Nothing is new syntax: both are module instantiations with named
arguments and children. A program's own `module fillet_edges` wins over
the builtin. The names are not `fillet()` and `chamfer()` because
libraries already use those: BOSL2's `fillet()` is an edge mask and
MCAD's `chamfer()` a bolt chamfer, and the sketch vocabulary has its own
`fillet(corner, r)` (`docs/sketch.md`).

A filleted model's `.csg` export prints the call as
`fillet_edges(r = 2, edges = "|z", ...) { ... }`, which stock OpenSCAD
does not know; STL, 3MF and STEP are the formats to hand on.

## Quick start

```openscad
fillet_edges(r = 2, edges = "|z") cube([40, 30, 20]);
```

```
neoscad --enable fillet -o box.stl box.scad
```

rounds the four vertical edges with a radius of 2. `check` says what
the call selected:

```text
$ neoscad check --enable fillet box.scad
check box.scad: 0 errors, 0 warnings, 0 info (manifold, 1 component, thinnest wall about 20 mm (sampled))
fillet_edges at line 1: 4 edges (4 line, convex, 90°), r 2, edges = "|z" [built]
```

## The two modules

```text
fillet_edges(r, edges = "all", except = undef, expect = undef) children;
chamfer_edges(d, edges = "all", except = undef, expect = undef) children;
```

| Argument | Meaning |
|---|---|
| `r` / `d` | The fillet radius, or the chamfer's distance, measured along each face from the edge. `chamfer_edges` also takes `r`, so the module name can be swapped (not both) |
| `edges` | Which edges: a selector string, a BOSL2-style direction vector, or a list of them (their union). Default `"all"` |
| `except` | Edges to leave out, in the same forms |
| `expect` | The number of edges the selection must match; any other number is an error that lists what matched |
| `$fn`, `$fa`, `$fs` | The blends' arcs, as for `circle()`; and in STEP export, whether they are exact (below) |

- **Children.** They are unioned into one solid, as OpenSCAD's
  operations do. `%` children are left out and `#` ones kept.
  Empty children give nothing, silently; 2D ones are an error.
- **Both senses in one call.** A *convex* edge (the material angle under
  180°, the outside of a box) loses material; a *concave* one (an inside
  corner) gains it.
- **Frame.** Directions in selectors are the call's own axes, so
  `rotate(...) fillet_edges(...)` rotates the rounded part with its
  selection.
- **One operation per call.** A second rounding is a nested call:
  `fillet_edges(r = 2, edges = ">z") fillet_edges(r = 5, edges = "|z")
  cube(...)` rounds the vertical edges first, then the top outline of
  that result. This is how different radii, and convex and concave edges
  that meet, are written.
- **A failed call** (a radius that does not fit, an edge it cannot
  round) is an error at the call; its children are left sharp, the rest
  of the model renders, and an export (`-o`) writes its file and exits
  with status 1, so a script or an agent never ships a sharp part
  believing it rounded. `check` fails with it too.

## Selecting edges

A selector string is case-insensitive (CadQuery's `>Z` is `>z`), with
spaces between words.

| Atom | Selects |
|---|---|
| `all` | every selectable edge (the default) |
| `none` | no edge |
| `convex`, `concave` | by the material angle at the edge |
| `%line`, `%circle`, `%ellipse`, `%bspline` | by curve; an arc of a circle is `%circle` |
| `\|z`, `\|x`, `\|y`, `\|(a, b, c)` | lines parallel to the direction; a bare `z` is `\|z` |
| `#z`, `#(a, b, c)` | lines perpendicular to it, and circles whose axis is along it |
| `>z`, `<z`, `>(a, b, c)` | the edges whose centre is farthest along, or against, the direction |
| `>>z[i]`, `<<z[i]` | the i-th group of edges by centre: `>>z[0]` is the lowest, `>>z[-1]` the highest (`>z`), `<<z[0]` the highest |
| `new` | edges whose two faces come from different leaves of the model: the edges booleans made |
| `child(i)`, `child(i, j)` | edges with a face of child `i` of this call; edges where child `i` meets child `j` |
| `part(name)` | edges with a face of `part(name)` (needs `--enable part`) |
| `@name` | edges through anchor `name` of the children, and along its direction if it has one (needs `--enable query`, `docs/geometry-queries.md`) |
| `box(x0, y0, z0, x1, y1, z1)` | edges lying wholly in the box |

Operators, loosest first: `not`, `exc` (set difference; `except` is the
same), `or`, `and`, and parentheses. This is CadQuery's grammar, so `not
convex and |z` is `not (convex and |z)`, and a CadQuery selector string
of these atoms means the same here. CadQuery's `+z`/`-z` (edges by their
orientation, which an OpenSCAD model does not let you choose) are an
error with the hint "use `|z`"; its `>y[1]` (the n-th parallel edge) is
an error suggesting `>>y[1]`.

```openscad
// Vertical edges on the +x side.
fillet_edges(r = 2, edges = "|z and >x") cube([40, 30, 20]);
```

```openscad
// Everything but the bottom outline, so the part sits flat on the bed.
fillet_edges(r = 1, edges = "all exc <z") cube([40, 30, 20]);
```

**BOSL2's edge descriptors** work too, on the bounding box of the
children: a vector of -1, 0 and 1 with one non-zero entry selects the
edges in that face of the box, two the edges along that edge of the
box, three those at that corner. On a cuboid this is BOSL2's own set
(`cuboid(..., rounding = 2, edges = [TOP+FRONT, TOP+RIGHT])`). The
strings `"X"`, `"Y"`, `"Z"`, `"ALL"` and `"NONE"` are aliases, and a list
is the union of its items.

```openscad
// The top front and top right edges, as BOSL2 would name them.
fillet_edges(r = 2, edges = [[0, -1, 1], [1, 0, 1]]) cube([30, 20, 10]);
```

**Never selected:** edges between tangent faces, the seams between the
facets of one `$fn` polygon (a `$fn = 24` cylinder's 24 sides), and
edges of faceted regions (`hull()`, `minkowski()`, `polyhedron()`,
imports). Without this rule `"all"` would put a tiny fillet on every
side of a polygonal cylinder. A selector that names such edges gets an
information line saying which were skipped and why. A `$fn` cylinder's
rims stay real edges: the model asked for that prism.

**Pinning a count.** A selector is re-run on every change, so a new hole
or a moved part can change what `">z"` matches. `expect = 4` makes a
different count an error that lists the edges matched; the editor's
"Pin count" action writes the current count in.

## Worked examples

These are the design's worked examples (`docs/fillets.md`, section 12),
now golden models with closed-form volumes
(`conformance/extensions/fillet`). Each exports to STEP all-exact, and
OCCT reads each back as a valid closed solid with that volume.

**L-bracket.** The inner call rounds the concave corner where the legs
meet (`child(0, 1)` stays that edge at any thickness or length); the
outer call rounds the outer heel of the result. Volume 6469.9557.

```openscad
t = 5; w = 20;
fillet_edges(r = 4, edges = "convex and |y and <x and <z")   // outer heel
fillet_edges(r = 3, edges = "child(0, 1)")                   // inner corner
{
  cube([40, w, t]);
  cube([t, w, 30]);
}
```

**Rounded box.** R5 vertical edges, then the top outline of that
result, which is four lines and four arcs tangent at every vertex,
rounded as one chain: four cylinders and four quarter tori. Volume
23460.397.

```openscad
L = 40; W = 30; H = 20; R = 5;
fillet_edges(r = 2, edges = ">z")
  fillet_edges(r = R, edges = "|z") cube([L, W, H]);
```

**Boss on a plate.** A concave circle: an added torus of major radius 8
and minor radius 2. Volume 7565.744.

```openscad
fillet_edges(r = 2, edges = "child(0, 1)") {
  translate([-20, -20, 0]) cube([40, 40, 4]);
  cylinder(r = 6, h = 14);
}
```

**Chamfered hole.** `%circle and >z` is the hole's top rim (not its
bottom rim, not the cube's lines); the chamfer is a cone. Volume
3706.785.

```openscad
chamfer_edges(d = 1, edges = "%circle and >z")
  difference() {
    cube([20, 20, 10]);
    translate([10, 10, -1]) cylinder(d = 6, h = 12);
  }
```

## What is rounded

- **Straight edges** between two planes at any angle, or a plane and a
  cylinder parallel to the edge, convex or concave, with open ends, ends
  against a wall, mitred concave corners, and a sphere patch where three
  rounded edges meet at a box corner (`fillet_edges(r = 2) cube(10)`
  rounds all twelve edges and eight corners).
- **Circles and arcs about one axis**: the rims of holes, bosses,
  countersinks, cones and spheres, and the arcs of a rounded outline,
  between a plane square to the axis, a coaxial cylinder, cone or
  torus, or a sphere centred on it.
- **Chains**: lines and arcs that run on into each other (a rounded
  rectangle's outline) are rounded as one piece.
- **Chamfers** are equal-distance, on the same edges; three convex
  chamfers meet in a point at a corner.

Other edges (two cylinders crossing, ellipses, B-splines, edges whose
sense changes along them) are not rounded yet: named, they are an error;
under the default `"all"` they stay sharp with a warning and the rest is
built.

## Diagnostics and fixes

Every message names the module (`fillet_edges():`), and every code
starts `fillet-`. A fix, where one is known, is a hint in the JSON
(`check --format json`, the diagnostics' `hints`) with an edit, which
the editor offers as a quick fix.

| Code | Severity | What, and the fix |
|---|---|---|
| `fillet-selector` | error | The selector does not parse: an unknown word (with "did you mean"), unbalanced parentheses, `part()` without `--enable part`, an `@name` no child declares, a `child(i)` past the children. Points at the column in the string |
| `fillet-count` | error | The selection is not `expect` edges; lists them. Fix: the matched count written into `expect` |
| `fillet-no-edges` | warning | The selector matched nothing; the child is unchanged |
| `fillet-skipped` | info | Edges the selector named that are never rounded (polygon seams, tangent edges), by reason |
| `fillet-too-large` | error | A blend does not fit its cross-section or the face beside it. Fix: a size 5% under the largest that fits |
| `fillet-overlap` | error | Two blends overlap on the face between them. Same fix |
| `fillet-unsupported-vertex` | error | Selected edges meet at a vertex one call cannot round (convex and concave together, more than three faces, a curved third face). Fix for convex and concave: the call rewritten as two nested calls, the concave edges first |
| `fillet-unsupported-edge` | error, or a warning under the default `"all"` | A selected edge of a kind not rounded yet (above) |
| `fillet-no-brep` | error | The children could not be read as faces and edges (bodies touching along an edge only, say); overlap them |
| `fillet-2d` | error | 2D children: use `offset(r = ...)` or a sketch's `fillet()` |
| `fillet-interrupted` | info | Something else in the model cuts into part of a blend |
| `fillet-failed` | error | The result does not hold the blends the plan made |

The selector is checked as the model is evaluated, so its errors show
while typing; the others need the children's geometry and come with a
render (the editor's preview, `check`, an export). An editor puts the
marker on the text to change: the selector for `fillet-count`,
`fillet-no-edges`, `fillet-skipped` and `fillet-unsupported-edge`, the
`r` or `d` for `fillet-too-large` and `fillet-overlap`, the call
otherwise.

```openscad expect=fillet-selector
fillet_edges(r = 2, edges = "|z and convx") cube(10);
```

```text
ERROR: fillet_edges(): edges = "|z and convx", column 8: unknown selector 'convx': did you mean 'convex'? in file model.scad, line 1
```

```openscad expect=fillet-overlap
fillet_edges(r = 3, edges = "|y") cube([20, 10, 4]);
```

```text
ERROR: fillet_edges(): the blends of edges 1 and 2 overlap on the plane between them: they need 3 + 3 of its 4 in file model.scad, line 1
```

with the hint "use r = 1.9: the largest that fits is just under 2,
which leaves almost nothing of the face beside the blend", whose edit
replaces the `3`.

```openscad expect=fillet-count
fillet_edges(r = 1, edges = ">z", expect = 3) cube(10);
```

```text
ERROR: fillet_edges(): edges = ">z" matched 4 edges, expect = 3: 1. line (convex, 90°) at [0, 5, 10], 10 long; 2. line (convex, 90°) at [5, 0, 10], 10 long; 3. line (convex, 90°) at [5, 10, 10], 10 long; 4. line (convex, 90°) at [10, 5, 10], 10 long in file model.scad, line 1
```

```openscad expect=fillet-skipped,fillet-no-edges
fillet_edges(r = 1, edges = "|z") cylinder(r = 5, h = 3, $fn = 12);
```

```text
INFO: fillet_edges(): 12 edges named by the selector were skipped: 12 polygon seams of cylinder() at line 1 in file model.scad, line 1
WARNING: fillet_edges(): edges = "|z" matched no edge; the child is unchanged in file model.scad, line 1
```

Convex and concave edges at one vertex (the default `"all"` on a block
standing on a plate):

```openscad expect=fillet-unsupported-vertex
fillet_edges(r = 1) union() {
  cube([20, 20, 5]);
  translate([5, 5, 0]) cube([10, 10, 15]);
}
```

```text
ERROR: fillet_edges(): edges 5, 6, 11 meet at [5, 5, 5], where convex and concave edges meet, which one call cannot round in file model.scad, line 1
```

Its fix rewrites the call's head as two calls, the inner one rounding
the concave edges and the outer one the convex edges of that result.
Under the default `"all"` each keeps it and leaves the other sense out
with `except`, so the four short curves where the inner call's blends
meet at the block's corners, which this version cannot round, stay sharp
with the default's warning rather than failing the call (a narrower
selector `S` becomes `"(S) and convex"` and `"(S) and concave"`):

```openscad expect=fillet-unsupported-edge
fillet_edges(r = 1, except = "concave") fillet_edges(r = 1, except = "convex") union() {
  cube([20, 20, 5]);
  translate([5, 5, 0]) cube([10, 10, 15]);
}
```

## STEP export, `--enable exact` and `$fn`

Fillets render to meshes without `--enable exact`. With it, `-o x.step`
writes every blend face as its true surface: a straight edge's fillet as
a cylinder, a rim's as a torus, a chamfer as a plane or cone, a box
corner as a sphere (`docs/step-export.md`). The call is reported as one
substitution:

```text
$ neoscad --enable fillet --enable exact -o box.step box.scad
INFO: STEP export: fillet_edges() is exported with exact blends (4 tools), not the polygonal arcs of the mesh ($fn is not set) in file box.scad, line 1
```

The rounded box above exports as 18 faces, all exact (6 planes, 8
cylinders, 4 tori), volume 23460.3974, the closed form.

The `$fn` rule of `docs/step-export.md` applies to the call's own arcs:
with `$fn` unset they are exact in STEP (the mesh has polygons, as for
any curve); with `$fn` set on the call, or above it, the polygon is the
model and the blends are written as its flat facets:

```text
INFO: STEP export: fillet_edges() keeps its blends' arcs as polygons because $fn is set (4 tools) in file fn.scad, line 1
```

## Tools: check, measure, snapshot, the editor and agents

- **`neoscad check --enable fillet`** prints a line per call after the
  findings (`fillet_edges at line 1: 4 edges (4 line, convex, 90°), r 2,
  edges = "|z" [built]`), and its JSON has a `fillets` array: per call
  its place, size, selector, status (`built`, `selected`, `no-edges`,
  `count`, `too-large`, `overlap`, `unsupported-vertex`, `unsupported`,
  `failed`, ...), the matched count, each selected edge (curve, sense,
  angle, class, length, centre, ends, the two faces' kinds), the skipped
  edges by reason, and the codes of its diagnostics
  (`docs/cli-json.md`). A failed call fails `check`
  (`counts.fillet_errors`).
- **`neoscad measure --enable fillet --fillet N`** (or `--fillet
  '"|z"'`, by selector) lists the call's selected edges:

  ```text
  fillet_edges at line 2: 8 edges (4 circle, convex, 90°; 4 line, convex, 90°), r 2, edges = ">z" [built]
    1. line (convex, 90°, translational) at [0, 15, 20], 20 long, plane | plane
    ...
    5. circle (convex, 90°, rotational) at [1.82, 1.82, 20], 7.85 long, plane | cylinder
  ```

- **`neoscad snapshot --enable fillet --fillet N`** draws the call's
  children unrounded with every selectable edge thin, the selected ones
  bold and numbered as `check` numbers them, edges not blended yet red
  and skipped ones dashed: the "show me the edges" before choosing a
  radius.
- **The editor** (the apps, the web page, and `neoscad lsp --enable
  fillet` in any LSP editor): inside an `edges` or `except` string,
  completion offers the atoms where an operand goes and the operators
  after one, with "did you mean" for a slip, and hover explains the word
  under the cursor; hover on an argument shows its line of the
  reference, and on the call's name what the last render selected; each
  fix above is a quick fix, and "Pin count" writes `expect`. The two
  modules are coloured as operations. Diagnostics that need the
  geometry come from the apps' and the page's renders; `neoscad lsp`
  alone evaluates without rendering, so it shows the selector's errors
  only (`docs/lsp.md`).
- **MCP** (`neoscad mcp --enable fillet`): `check` lists the calls,
  `measure` and `snapshot` take `fillet` (an index or a selector), and
  `docs` for `fillet_edges` ends with a complete model to adapt
  (`docs/mcp.md`).

## Comparison with FreeCAD, CadQuery and BOSL2

The other tools' side is from their sources, as surveyed in
`docs/fillets.md`, sections 3 and 13.

| | FreeCAD PartDesign Fillet/Chamfer | CadQuery `fillet()`/`chamfer()` | BOSL2 rounding | NeoSCAD |
|---|---|---|---|---|
| Edges chosen by | picks in the 3D view, stored as edge names | selector strings, re-run each time | `edges=`/`except=` on the 12 edges of a primitive's box | selector strings (CadQuery's, plus `convex`, `new`, `child()`, `part()`, `@anchor`, `box()`), BOSL2 vectors |
| On boolean results | yes | yes | no (primitives and attachables) | yes |
| Stable under edits | names can break on a change | re-selected | re-selected | re-selected; `expect` pins a count |
| Convex and concave | both | both | convex; external fillets top and bottom only | both, in one call |
| Corners of three edges | vertex blends | vertex blends | `trimcorners` | sphere patch, equal radii; otherwise nested calls |
| Too-large radius | error, or an invalid solid | error | assertion or wrong result (unverified) | error with the size that fits as a fix |
| Chamfers | equal, two distances, distance and angle | one or two lengths | `chamfer=` | equal distance |
| Variable radius | yes | no | no | no |
| STEP export | yes | yes | mesh only | `--enable exact` |

In CadQuery the quick start is `.edges("|Z").fillet(2)` on a box;
build123d's `Select.NEW` is
`new`, and its `Convexity` filter `convex`/`concave`.

## Limitations

What the design leaves for later, and what is known not to work yet,
is in `docs/followups.md` ("Fillets and chamfers"). In short:

- **Edges between curved faces with no common axis** (two cylinders
  crossing, ellipses, B-splines) are not rounded, and **variable radii,
  asymmetric chamfers and mixed corners in one call** are not
  supported; mixed and unequal corners are nested calls.
- **A convex rim's fillet** must be under half the rim's radius (a
  boss's top edge), and **an arc's blend** ends only where it runs on
  smoothly into another edge or on a plane through its axis; other ends
  are `fillet-unsupported-vertex`.
- **Near the largest size that fits**, a sliver of face can be left that
  the STEP export writes as facets; the size hints keep 5% under it for
  this reason.
- **The mesh beside spheres and tori** keeps the sphere's facets standing
  over the blend by up to their sagitta (the STEP export is exact).
- **Colours and parts**: a blend takes the children's colour or part
  only when all their faces share one.
- **Seams of `offset(r)` with `$fn`** are selectable edges, so `"all"`
  on such an extrusion rounds its polygon's sides.
- **`@name` in a `.csg` round trip** does not resolve (the `.csg` has no
  anchors).
- **`neoscad lsp` by itself** shows no geometry diagnostics and no "Pin
  count", which need a render.
- **Rotations other than multiples of 90°** can give different STEP
  bytes in the browser than natively (the platform's sine).
- **No limit of its own** on the number of edges selected; the tools
  count against the triangle limit.
