# Geometry queries: bounding box, measurements and anchors

> NeoSCAD extension (`--enable query`); not in OpenSCAD.

A geometry query lets a module ask about its own children while the
program runs: their bounding box (`child_bounds()`), their volume, area
and size (`child_measure()`), the distance between two of them
(`child_distance()`), or the named points they declare (`anchor()` and
`child_anchors()`). The answers are ordinary numbers, so
a module can size a base plate to its child, put a hole beside it, or
centre it, which plain OpenSCAD cannot do without the numbers being
typed in by hand.

The design, with its reasons and sources, is
`docs/language-extensions.md` (sections 5 and 11.4 to 11.6).

Contents:

- [OpenSCAD superset: turning queries on](#openscad-superset-turning-queries-on)
- [Quick start](#quick-start)
- [child_bounds(): the bounding box of a module's children](#child_bounds-the-bounding-box-of-a-modules-children)
- [child_measure(): volume, area, size and centre](#child_measure-volume-area-size-and-centre)
- [child_distance(): the gap between two children](#child_distance-the-gap-between-two-children)
- [anchor() and child_anchors(): named points](#anchor-and-child_anchors-named-points)
- [Evaluation order and cost](#evaluation-order-and-cost)
- [Preview and render](#preview-and-render)
- [Limits and diagnostics](#limits-and-diagnostics)
- [Exporting plain OpenSCAD (.csg)](#exporting-plain-openscad-csg)
- [Comparison with BOSL2 attachments](#comparison-with-bosl2-attachments)
- [Upstream OpenSCAD requests and the probe() pull request](#upstream-openscad-requests-and-the-probe-pull-request)

Every example below is evaluated by a test
(`crates/session/tests/query.rs`, `docs_examples_evaluate`), which
checks that it gives exactly the output shown with it.

## OpenSCAD superset: turning queries on

Queries are off by default, and off a file means exactly what it means
in OpenSCAD: `child_bounds()`, `child_measure()`, `child_distance()` and
`child_anchors()` are unknown functions and `anchor()` an unknown module, with OpenSCAD's
own warnings. A program's own definitions of those names win, on or off.
Turn queries on with:

- the command line: `neoscad --enable query` (the same flag works for
  `check`, `measure`, `snapshot`, `test`, `serve`, `lsp` and `mcp`).
  OpenSCAD's `--enable all` turns on OpenSCAD's experiments only, not
  NeoSCAD's extensions;
- `neoscad serve` and MCP requests: `"enable": ["query"]`.

- the apps: Settings > Language > "Geometry queries (query)" on macOS,
  Preferences > Language on Linux, and Design > NeoSCAD Extensions on
  Windows. Changing it runs the open documents again.

Nothing is new syntax: the queries are functions and `anchor()` is a
module, so a file parses, formats and prints its `.ast` the same with
the extension on or off.

## Quick start

A base plate under any child, a margin wider than the child, with a
fixing hole 6 mm beyond its +x edge:

```openscad
module plate_for(margin = 4, thick = 3, hole_d = 5) {
  b  = child_bounds(0);
  lo = b[0];
  hi = b[1];
  cy = (lo[1] + hi[1]) / 2;
  echo(lo = lo, hi = hi);
  difference() {
    translate([lo[0] - margin, lo[1] - margin, -thick])
      cube([hi[0] - lo[0] + 2 * margin + 12, hi[1] - lo[1] + 2 * margin, thick]);
    translate([hi[0] + 6, cy, -thick - 1])
      cylinder(d = hole_d, h = thick + 2, $fn = 32);
  }
  children(0);
}

plate_for() translate([10, 5, 0]) cylinder(d = 20, h = 5, $fn = 6);
```

```text
ECHO: lo = [0, -3.66025, 0], hi = [20, 13.6603, 5]
```

Change the child and the plate follows. The same model, with a gear as
the child, is `conformance/extensions/query/plate-bounds.scad`.

## child_bounds(): the bounding box of a module's children

`child_bounds(index = undef)` is valid inside a module body. It is the
axis-aligned bounding box of what `children(index)` would make at that
point, rendered:

- `[[x0, y0, z0], [x1, y1, z1]]` for 3D children, `[[x0, y0], [x1, y1]]`
  for 2D ones: the minimums and the maximums, the format of BOSL2's
  `pointlist_bounds()`;
- the box of the result, not of the operands: a `difference()` is
  measured after the cut, a `hull()` as the hull;
- in the module's own frame: transforms written in the module body
  around a later `children()` do not apply;
- `index` as `children()` takes it: a number, a list, a range, or
  nothing for all the children;
- `undef` with a `query-empty` warning when the children make no
  geometry.

```openscad
module show() {
  echo(child_bounds(0));
  children(0);
}
show() difference() {
  cube(10);
  translate([5, 5, -1]) cube(20);
}
show() rotate(45) square(2);
```

```text
ECHO: [[0, 0, 0], [10, 10, 10]]
ECHO: [[-1.41421, 0], [1.41421, 2.82843]]
```

## child_measure(): volume, area, size and centre

`child_measure(index = undef)` measures the same render, as an object:

| Key | Value |
|---|---|
| `dim` | 2 or 3 |
| `empty` | `true` when the children make no geometry |
| `bounds` | as `child_bounds()` |
| `size` | `bounds[1] - bounds[0]` |
| `center` | the middle of the box |
| `area` | 2D: the area, holes subtracted |
| `volume` | 3D: the enclosed volume |
| `surface_area` | 3D: the area of the surface |

For empty children, `empty` is `true` and `dim`, `bounds`, `size` and
`center` are `undef`, with no warning: the object says so itself.

```openscad
module report() {
  m = child_measure(0);
  echo(dim = m.dim, size = m.size, volume = m.volume, surface = m.surface_area);
  children(0);
}
report() difference() {
  cube(10);
  translate([5, 5, -1]) cube(20);
}
```

```text
ECHO: dim = 3, size = [10, 10, 10], volume = 750, surface = 550
```

The sums are taken over the rendered mesh in a fixed order, so the
numbers are the same at any thread count and on every platform (the
browser build included).

## child_distance(): the gap between two children

`child_distance(a, b)` is the smallest distance between what
`children(a)` and `children(b)` would make, rendered, with `a` and `b` as
`children()` takes an index (a list is those children together). It is
measured as `neoscad measure --between` measures two parts: 0 when they
overlap, one inside the other included, and otherwise the exact distance
between their surfaces, which is 0 where they touch. Two 2D children are
compared in the plane. A child that makes no geometry gives `undef` with
a `query-empty` warning, and a 2D child with a 3D one gives `undef` with
an `invalid-argument` warning.

```openscad
module clearance() {
  echo(gap = child_distance(0, 1));
  children();
}
clearance() {
  cube(10);
  translate([13, 0, 0]) cube(10);
}
clearance() {
  cube(10, center = true);
  sphere(2);
}
```

```text
ECHO: gap = 3
ECHO: gap = 0
```

## anchor() and child_anchors(): named points

`anchor(name, point, dir = undef)` names a point, and optionally a
direction, where it is written. `child_anchors(index = undef)` gives a
module the anchors of its children, carried through their transforms
into the module's frame, as an object from each name to `[point, dir]`.
No rendering is involved, so anchors are the cheap way to expose points
that the geometry alone does not show (a hole's axis, a mounting face).

```openscad
module bolt(l = 10) {
  cylinder(d = 3, h = l);
  anchor("tip", [0, 0, l], [0, 0, 1]);
}
module cap_on_tip() {
  a = child_anchors(0);
  echo(tip = a.tip);
  translate(a.tip[0]) sphere(2);
  children(0);
}
cap_on_tip() translate([5, 0, 0]) bolt(20);
```

```text
ECHO: tip = [[5, 0, 20], [0, 0, 1]]
```

An anchor makes no node, so it changes nothing else the model builds or
prints. It moves with `translate`, `rotate`, `scale`, `mirror`,
`multmatrix`, `linear_extrude` and `projection`; under `resize` and
`rotate_extrude` it is not visible. A constrained sketch (`--enable
sketch`) exports every entity its body names as an anchor
(`docs/sketch.md`). `neoscad docs anchor` and `neoscad docs
child_anchors` have the details.

## Evaluation order and cost

OpenSCAD evaluates the whole program into a tree first and renders it
afterwards, so a value cannot normally depend on geometry. A query
reverses that for one child:

1. The child is instantiated early, as `children(index)` would
   instantiate it at the query, in a sandbox: what it echoes and warns
   is held back, and the `rands()` state and node numbering are put back
   afterwards, so adding a query changes nothing else the model prints
   or builds.
2. `child_bounds()`, `child_measure()` and `child_distance()` render
   it, through the same geometry cache as the final render. For
   `child_bounds()` of primitives, extrusions and 2D hulls under
   transforms, and of 3D unions of them, the box is found without the
   kernels: the leaves are built and moved as a render builds and moves
   them, and a union's box is its operands' (a test renders thousands of
   generated models to check that this gives the rendered box to the
   last bit wherever it is used). Differences, intersections, 2D unions,
   3D hulls, `minkowski`, `offset` and the like are rendered.
3. The module goes on with the numbers.
4. A later `children(index)` in the same module call reuses the
   instance when the `$` variables it read are the same there, and
   prints its held-back messages then. The final render finds the
   child already built in the cache.

So a query costs one render of its child, which the final render does
not repeat, and one evaluation of the child, which `children()` usually
does not repeat. Nested queries (a queried child that queries its own
children) cost one evaluation per level. A child that calls `rands()`,
reads a file or imports one is evaluated a second time by `children()`.

A top-level statement or module call that asked a geometry query is
always evaluated anew: the incremental evaluation of the apps and the
language server, and the replay of repeated module calls, skip it, so
that every query is counted against the limits. Its renders still come
from the cache.

## Preview and render

A query always measures the child as a full render (F6) makes it, in
preview too: `%` (background) children are left out and `#`
(highlighted) children are kept. Otherwise a model would change shape
between preview and render. The child's `$preview` is that of the run,
since it is the instance `children()` uses.

```openscad
module show() {
  echo(child_bounds());
  children();
}
show() {
  cube(1);
  %translate([10, 0, 0]) cube(1);
  #translate([0, 5, 0]) cube(1);
}
```

```text
ECHO: [[0, 0, 0], [1, 6, 1]]
```

## Limits and diagnostics

Query renders are renders: the time limit and a cancellation stop them,
and they count against the memory and triangle limits like the final
render. The `queries` limit (`--limit queries=N`; 10,000 for `serve`,
`mcp` and the apps, unlimited on the command line) counts the
`child_bounds()`, `child_measure()` and `child_distance()` calls of one
evaluation (whether they render or not) and stops
a loop or recursion that queries at every step. `child_anchors()`
renders nothing and does not count.

| Code | Severity | When |
|---|---|---|
| `query-empty` | warning | `child_bounds()` or `child_distance()` of children that make no geometry; the answer is `undef` |
| `query-unavailable` | warning | the host cannot render (no geometry is available), or the child uses something the renderer does not build; the answer is `undef` |
| `query-outside-module` | warning | a query outside any module body; the answer is `undef` |
| `query-index` | warning | an index out of range or not a number, with `children()`'s text; the answer is `undef` |
| `query-duplicate-anchor` | warning | two anchors of one name among the children; the first wins |
| `resource-limit` | error | a limit passed, by the query count or by the child's render |

An error in the child stops evaluation at the query, as it would at
`children()`, with the child's messages before it. A child that asks
about itself is OpenSCAD's recursion error. The child's own geometry
warnings ("Mixing 2D and 3D objects is not supported" and the like) are
printed once, by the final render, where they would be without the
query.

## Exporting plain OpenSCAD (.csg)

A query's answer is a number by the time the tree is built, so the
`.csg` export of a model that uses queries is plain OpenSCAD with the
numbers written in, and the stock OpenSCAD renders it to the same shape
(to the six significant digits a `.csg` prints). This is how a model
built with queries is shared with OpenSCAD users:

```text
neoscad --enable query model.scad -o model.csg
```

## Comparison with BOSL2 attachments

[BOSL2](https://github.com/BelfrySCAD/BOSL2)'s attachments place parts
relative to each other from geometry each shape declares:
`attachable()` takes the shape's analytic description (`size`, `r`,
`l`, `vnf`, `path`, `region` and so on) and its own `anchors`, and
`attach()` and `position()` use them (`attachments.scad` in BOSL2). It
never measures rendered geometry, so it works in plain OpenSCAD, and an
arbitrary child (an imported mesh, a `difference()`) has the anchors of
whatever its author declared. BOSL2's `bounding_box()`
(`miscellaneous.scad`) is a module: it builds the box as geometry, from
`hull()`, `projection()` and `minkowski()` of the children, for use in
booleans; it gives no numbers to compute with.

| | BOSL2 | NeoSCAD queries |
|---|---|---|
| Needs | the library, in any OpenSCAD | `--enable query` |
| Source of truth | what each shape declares | the rendered child (`child_bounds()`, `child_measure()`); what it declares (`child_anchors()`) |
| Arbitrary children (imports, booleans) | as declared, or not at all | measured exactly |
| Named points | `named_anchor()`, `anchors=` | `anchor()` |
| Orientation and spin | `attach()`, `orient=`, `spin=` | not built in; write it with the anchor's direction |
| Cost | none beyond evaluation | one render of the child (cached) |

The two combine: a BOSL2 model can use `child_bounds()` inside its own
modules.

## Upstream OpenSCAD requests and the probe() pull request

OpenSCAD users have asked for the size of children for a long time.
Among the requests (each retrieved from github.com/openscad/openscad):

- issue #1088, "module bounds(index)/bounds()/$bounds for module
  children(index)/children()/$children" (2014), closed as not planned
  in 2026;
- issue #4520, "Feature request: Get the dimensions of child objects"
  (2023), closed;
- pull request #1713, "Probe (bounding box, volume, centroid)", opened
  in June 2016 and closed unmerged in December 2016. Its `probe()`
  module analysed its first child and set variables (`empty`, `bbsize`,
  `bbcenter`, `bbmin`, `bbmax`, `volume`, `centroid`) for its later
  children. Comments on it after it was closed give the objections: it
  needs a render, which a preview does not compute (2019), and "the
  question is how to fit this into the language in a way that is not
  based on magic variables" (2020).

NeoSCAD's queries answer both: they are ordinary functions whose results
go in ordinary variables, scoped to the module whose children they ask
about; and a render is accepted, with the same answer in preview and
render, cached so the final render does not repeat it.
