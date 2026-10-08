# Constrained sketches (2D constraint solver)

> NeoSCAD extension (`--enable sketch`); not in OpenSCAD.

A constrained sketch is a 2D profile written as points, lines, arcs and
circles tied together by geometric and dimensional constraints
(horizontal, tangent, a length, a radius, ...). NeoSCAD's solver finds
the positions that meet every constraint, and the result is an ordinary
2D shape: `linear_extrude`, `rotate_extrude`, `offset` and the 2D
booleans take it as they take a `polygon()`. This is the kind of sketch
FreeCAD's Sketcher, CadQuery's `Sketch` or a parametric CAD program's
sketch mode gives, written as OpenSCAD source.

The design, with its reasons and sources, is
`docs/language-extensions.md` (section 4).

Contents:

- [OpenSCAD superset: turning sketches on](#openscad-superset-turning-sketches-on)
- [Quick start](#quick-start)
- [Entities](#entities)
- [Constraints](#constraints)
- [Fillets and chamfers](#fillets-and-chamfers)
- [Profiles, holes and construction geometry](#profiles-holes-and-construction-geometry)
- [Parameters and the customizer](#parameters-and-the-customizer)
- [Degrees of freedom and diagnostics](#degrees-of-freedom-and-diagnostics)
- [Guesses, flips and "Pin drawing"](#guesses-flips-and-pin-drawing)
- [Tools: check, measure, the editor and agents](#tools-check-measure-the-editor-and-agents)
- [Comparison with the FreeCAD Sketcher](#comparison-with-the-freecad-sketcher)
- [Comparison with CadQuery sketches](#comparison-with-cadquery-sketches)

Every example below is evaluated by a test
(`crates/session/tests/sketch.rs`, `docs_examples_evaluate`), which
checks that it gives exactly the diagnostics shown with it.

## OpenSCAD superset: turning sketches on

NeoSCAD's language is a superset of OpenSCAD's, and sketches are off by
default. Off, a file means exactly what it means in OpenSCAD:
`sketch(...)` is an unknown module with OpenSCAD's own warning, and none
of the sketch vocabulary exists. Turn them on with:

- the command line: `neoscad --enable sketch` (and the same flag for
  `check`, `measure`, `snapshot`, `test`, `serve`, `lsp` and `mcp`). OpenSCAD's
  `--enable all` turns on OpenSCAD's experiments only, not NeoSCAD's
  extensions;
- the apps: Settings > Language > "Constrained sketches (sketch)" on
  macOS, Preferences > Language on Linux, and Design > NeoSCAD
  Extensions on Windows. Changing it runs the open documents again.

Every surface marks the vocabulary with the label "NeoSCAD extension
(`--enable sketch`); not in OpenSCAD": `neoscad docs sketch`, the
editor's hover and completion, and the MCP `docs` tool.

Nothing in a sketch is new syntax: `sketch()` is a module, entities are
function calls assigned to variables, and constraints are module
statements. A file with sketches parses, formats and prints its `.ast`
the same with the extension on or off. Inside a sketch body the
vocabulary comes first, so a library's own `arc()` (BOSL2) or
`distance()` (MCAD) is not reachable there by that name; everywhere else
the vocabulary does not exist and those names keep their meaning. A
program's own `module sketch` wins over the builtin, and its children
get no vocabulary.

The output is plain OpenSCAD: a solved sketch exports to `.csg` as the
`polygon(points = ..., paths = ...)` it is, a file stock OpenSCAD reads
without the extension.

## Quick start

```openscad
// A 40 x 20 plate with rounded corners and a centred hole.
w = 40; h = 20;
linear_extrude(4)
sketch(name = "plate") {
  o = point([0, 0]); a = point([w, 0]); b = point([w, h]); c = point([0, h]);
  bottom = line(o, a); right = line(a, b); top = line(b, c); left = line(c, o);
  fix(o); horizontal(bottom); vertical(right); horizontal(top); vertical(left);
  length(bottom, w); length(left, h);
  hole = circle([w / 2, h / 2], d = 6);
  distance(o, hole.center, w / 2, along = "x");
  distance(o, hole.center, h / 2, along = "y");
  fillet(o, 3); fillet(a, 3); fillet(b, 3); fillet(c, 3);
}
```

- `sketch(name, strict = false, convexity = 1)` is the module. `name`
  names it in messages and in `measure --sketch NAME`; `strict = true`
  makes free degrees of freedom an error; `convexity` is `polygon()`'s.
  `$fn`, `$fa` and `$fs` at the call tessellate its arcs and circles as
  they tessellate `circle()`.
- **Entities are assignments.** Each `point`, `line`, `arc` or `circle`
  call makes a new entity and returns a handle to it; the variable's
  name labels it in messages.
- **Constraints are statements.** They add equations and make no
  geometry.
- **The body is declarative.** Before the solve an entity has no
  coordinates, so the body cannot read them; dimensions are ordinary
  expressions over parameters. The solved values come out through
  `measure --sketch` and the editor's hover.
- The coordinates written in the source (`point([w, 0])`) are
  **guesses**: the drawing the solver starts from. See [Guesses, flips
  and "Pin drawing"](#guesses-flips-and-pin-drawing).

A module whose body is a `sketch()` adds to the sketch it is called
from, so constraint patterns can be reused:

```openscad
module rectangle(o, a, b, c) sketch() {
  bottom = line(o, a); right = line(a, b); top = line(b, c); left = line(c, o);
  horizontal(bottom); vertical(right); horizontal(top); vertical(left);
}
sketch(name = "two") {
  o = point([0, 0]); a = point([10, 0]); b = point([10, 5]); c = point([0, 5]);
  rectangle(o, a, b, c);
  fix(o); fix(b);
}
```

## Entities

| Entity | Parameters | Members |
|---|---|---|
| `point([x, y])` | `at`: where it is drawn | — |
| `line(p, q, construction = false)` | `p`, `q`: point handles, or `[x, y]` for a new point there | `.start`, `.end` |
| `arc(center, start, end, cw = false, construction = false)` | points or `[x, y]`; counter-clockwise from `start` to `end` unless `cw` | `.center`, `.start`, `.end` |
| `circle(center, r, d, construction = false)` | `center`: a point or `[x, y]`; `r` or `d`, when given, is a radius constraint | `.center` |

Two lines that share a point handle are joined there. A handle prints as
`<sketch line "top">`, and two handles are equal when they name the same
entity.

### point

```openscad
sketch() { p = point([3, 4]); fix(p); echo(p); }
```

A point written `point()`, with no guess, is placed by the solver,
which says where (`sketch-no-guess`, below).

### line

```openscad
sketch() { l = line([0, 0], [10, 0], construction = true); fix(l); echo(l.start); }
```

### arc

An arc adds one equation of its own: its start and end are the same
distance from its centre.

```openscad
sketch() {
  c = point([0, 0]);
  a = arc(c, [5, 0], [0, 5], construction = true);
  fix(c); fix(a.start); angle(a, 90);
}
```

### circle

```openscad
sketch() { c = circle([0, 0], r = 5); fix(c.center); }
```

## Constraints

Every constraint names entities by their handles; dimensions are
numbers in the model's units, angles in degrees.

### coincident

`coincident(a, b)`: two points are the same point. The profile joins
curves there, as if they shared one handle.

```openscad
sketch() {
  l1 = line([0, 0], [10, 0], construction = true);
  l2 = line([10, 1], [10, 8], construction = true);
  fix(l1); fix(l2.end);
  coincident(l1.end, l2.start);
}
```

### horizontal

`horizontal(l)` or `horizontal(p, q)`: a line, or the line between two
points, is horizontal.

```openscad
sketch() {
  l = line([0, 0], [10, 1], construction = true);
  fix(l.start); horizontal(l); length(l, 10);
}
```

### vertical

`vertical(l)` or `vertical(p, q)`.

```openscad
sketch() {
  p = point([0, 0]); q = point([1, 10]);
  fix(p); vertical(p, q); distance(p, q, 10);
}
```

### parallel

`parallel(l1, l2)`.

```openscad
sketch() {
  a = line([0, 0], [10, 0], construction = true);
  b = line([0, 5], [10, 6], construction = true);
  fix(a); fix(b.start); parallel(a, b); length(b, 10);
}
```

### perpendicular

`perpendicular(l1, l2)`.

```openscad
sketch() {
  a = line([0, 0], [10, 0], construction = true);
  b = line([0, 0], [1, 10], construction = true);
  fix(a); coincident(a.start, b.start); perpendicular(a, b); length(b, 10);
}
```

### tangent

`tangent(a, b)`: a line and an arc or circle, or two arcs or circles,
touch. Which side, and inside or outside, comes from the drawing. Where
the two share an end point the tangency is written at that point (the
radius there is perpendicular to the line, as in FreeCAD's endpoint
tangency), which is how a slot's caps meet its sides:

```openscad
slot_len = 30; slot_w = 8;
sketch(name = "slot") {
  c1 = point([0, 0]); c2 = point([slot_len, 0]);
  axis = line(c1, c2, construction = true);
  top = line([0, slot_w / 2], [slot_len, slot_w / 2]);
  bot = line([slot_len, -slot_w / 2], [0, -slot_w / 2]);
  e1 = arc(c1, top.start, bot.end);
  e2 = arc(c2, bot.start, top.end);
  fix(c1); horizontal(axis); length(axis, slot_len);
  tangent(e1, top); tangent(e1, bot); tangent(e2, top); tangent(e2, bot);
  diameter(e1, slot_w); equal(e1, e2);
}
```

### distance

`distance(a, b, d, along)`: between two points, a point and a line, or
two lines (which it also makes parallel). With `along = "x"` or `"y"`
(two points) it is the signed offset `b - a` along that axis, FreeCAD's
DistanceX and DistanceY.

```openscad
sketch() {
  o = point([0, 0]); p = point([9, 1]);
  fix(o); distance(o, p, 10, along = "x"); distance(o, p, 2, along = "y");
}
```

### length

`length(l, d)`.

```openscad
sketch() {
  l = line([0, 0], [9, 0], construction = true);
  fix(l.start); horizontal(l); length(l, 10);
}
```

### radius

`radius(c, r)`, for an arc or a circle.

```openscad
sketch() { c = circle([0, 0]); fix(c.center); radius(c, 4); }
```

### diameter

`diameter(c, d)`.

```openscad
sketch() { c = circle([0, 0]); fix(c.center); diameter(c, 8); }
```

### angle

`angle(l1, l2, deg)`: the signed angle, counter-clockwise, from `l1` to
`l2`. `angle(a, deg)` is the angle an arc sweeps.

```openscad
sketch() {
  a = line([0, 0], [10, 0], construction = true);
  b = line([0, 0], [7, 7], construction = true);
  fix(a); coincident(a.start, b.start); angle(a, b, 45); length(b, 10);
}
```

### equal

`equal(a, b)`: two lines have equal lengths, or two arcs or circles
equal radii.

```openscad
sketch() {
  a = line([0, 0], [10, 0], construction = true);
  b = line([0, 5], [8, 5], construction = true);
  fix(a); fix(b.start); horizontal(b); equal(a, b);
}
```

### point on curve (on)

`on(p, c)`: a point lies on a line (extended without end), or on an
arc's or circle's circle.

```openscad
sketch() {
  c = circle([0, 0], r = 5, construction = true);
  p = point([4, 3]);
  fix(c.center); on(p, c); distance(c.center, p, 4, along = "x");
}
```

### midpoint

`midpoint(p, l)`.

```openscad
sketch() {
  l = line([0, 0], [10, 0], construction = true);
  m = point([4, 1]);
  fix(l); midpoint(m, l);
}
```

### symmetric

`symmetric(p, q, about)`: mirror images about a line or a point.

```openscad
sketch() {
  axis = line([0, -5], [0, 5], construction = true);
  p = point([-3, 1]); q = point([4, 1]);
  fix(axis); fix(p); symmetric(p, q, axis);
}
```

### fix

`fix(p, at)`: a point stays where it is drawn, or at `at`. On a line it
fixes both of its points.

```openscad
sketch() { p = point([2, 3]); fix(p); q = point([0, 0]); fix(q, at = [1, 1]); }
```

## Fillets and chamfers

`fillet(corner, r)` rounds the corner where exactly two profile curves
meet, lines or arcs, with a tangent arc of radius `r`;
`chamfer(corner, d)` cuts it with a straight line. Both are applied
after the solve, so they add no unknowns, and the corner point stays the
sharp corner the dimensions refer to (FreeCAD's fillet with "preserve
corner" keeps such a point too).

- Between two lines, a chamfer cuts `d` along each line.
- Between a line and an arc, the line is trimmed and the arc shortened
  on its own circle. A fillet touches the arc from inside its circle
  when the line runs into it (a quarter disc's corners) and from outside
  when it runs away (a concave scoop), so its radius must be under the
  arc's in the first case. A chamfer cuts the line `d` from the corner
  and the arc where it is `d` from the corner in a straight line. Where
  the line runs on along the arc's tangent there is no corner, which is
  an `invalid-argument` error.
- Between two arcs, each arc is shortened on its own circle. A fillet
  touches each arc from inside its circle where the other arc runs into
  that circle (a lens's corners) and from outside where it runs away (the
  waist where two discs' outlines meet), so it can be inside one and
  outside the other (a crescent's tip). A chamfer cuts each arc where it
  is `d` from the corner in a straight line. Arcs that run on along each
  other's tangent have no corner (`invalid-argument`).

A fillet or chamfer that needs more of a curve than there is is an
error with the largest size that fits as its fix
(`sketch-fillet-too-large`).

```openscad
sketch() {
  o = point([0, 0]); a = point([20, 0]); b = point([20, 10]); c = point([0, 10]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, c); l4 = line(c, o);
  fix(o); fix(a); fix(b); fix(c);
  fillet(b, 3); chamfer(c, 2);
}
```

A quarter disc with its arc's corners rounded and cut:

```openscad
sketch() {
  o = point([0, 0]); a = point([20, 0]); b = point([0, 20]);
  l1 = line(o, a); e = arc(o, a, b); l2 = line(b, o);
  fix(o); horizontal(l1); vertical(l2); length(l1, 20);
  fillet(a, 3); chamfer(b, 2);
}
```

## Profiles, holes and construction geometry

- **The profile is every curve that is not construction geometry.**
  Curves join at shared points; each point of the profile must join
  exactly two curves, so each loop closes (`sketch-open-profile`
  otherwise). A circle is a loop on its own.
- **Loops fill even-odd**, as `polygon()` with several paths does: a
  circle inside an outline is a hole, whichever way either runs. Loops
  that cross are a warning (`sketch-self-intersection`), because
  even-odd then gives a shape the author rarely meant.
- **Construction geometry** (`construction = true`) takes part in the
  solve and in no profile: axes, reference circles, helper lines.
- **Arcs and circles are tessellated like `circle()`**, with the same
  `$fn`, `$fa` and `$fs` rules, so a sketch circle has exactly the
  vertices of `circle(r)`. An arc's end vertices are the solved points
  themselves, so every loop closes exactly.

```openscad
$fn = 32;
sketch(name = "washer") {
  outer = circle([0, 0], r = 10);
  inner = circle([0, 0], r = 4);
  coincident(outer.center, inner.center);
  fix(outer.center);
}
```

## Parameters and the customizer

Dimensions are ordinary expressions, so a sketch follows the file's
parameters, the customizer's values and `-D`:

```openscad
// [Bracket]
leg = 40;   // [20:80]
t = 4;      // [2:8]
linear_extrude(10)
sketch(name = "L") {
  o = point([0, 0]); a = point([leg, 0]); b = point([leg, t]);
  c = point([t, t]); d = point([t, leg]); e = point([0, leg]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, c);
  l4 = line(c, d); l5 = line(d, e); l6 = line(e, o);
  fix(o); horizontal(l1); vertical(l2); horizontal(l3);
  vertical(l4); horizontal(l5); vertical(l6);
  length(l1, leg); length(l6, leg); length(l2, t); length(l5, t);
}
```

The guesses here are written from the parameters too, so the drawing
moves with them; see the next sections for what happens when it does
not.

## Degrees of freedom and diagnostics

Each point has two unknowns (x and y) and each circle one more (its
radius). Each constraint adds equations. The solver reports:

- the **free degrees of freedom**: unknowns the constraints leave free
  (a sketch with nothing fixed can still move and turn as a whole: three
  of them). Free DOF are information, not an error, as in FreeCAD, and
  the sketch stays where it is drawn in those directions;
- **redundant** constraints, implied by the others, and **conflicting**
  ones, which cannot all hold.

Every diagnostic has a stable code, NeoSCAD's text in OpenSCAD's
`WARNING: ... in file X, line N` form, the span of the statement it is
about, and hints. Where an exact edit is known the hint carries it
(`hints[].replace` in `--format json`), and the editor offers it as a
quick fix. A sketch with an error gives an empty shape; the rest of the
model still evaluates. The info severity is NeoSCAD's own: printed
`INFO:`, listed in the JSON diagnostics with `"severity": "info"`, shown
as an information marker in the editor.

### sketch-underconstrained

Information; an error with `strict = true`. Names the coordinates that
can still move, and suggests constraints, measured on the solution, that
would fix them; with several, a first hint adds them all.

```openscad expect=sketch-underconstrained
sketch(name = "loose") {
  o = point([0, 0]); a = point([20, 0]); b = point([20, 10]); c = point([0, 10]);
  bottom = line(o, a); right = line(a, b); top = line(b, c); left = line(c, o);
  fix(o); horizontal(bottom); horizontal(top); vertical(right); vertical(left);
  length(bottom, 20);
}
```

### sketch-redundant

Warning: a constraint the others already imply. The hint deletes it.

```openscad expect=sketch-redundant
sketch(name = "twice") {
  o = point([0, 0]); a = point([20, 0]); b = point([20, 10]); c = point([0, 10]);
  bottom = line(o, a); right = line(a, b); top = line(b, c); left = line(c, o);
  fix(o); horizontal(bottom); horizontal(top); vertical(right); vertical(left);
  length(bottom, 20); length(right, 10);
  length(top, 20);
}
```

### sketch-conflict

Error: constraints that cannot all hold. Each hint deletes one of them.

```openscad expect=sketch-conflict
sketch(name = "clash") {
  o = point([0, 0]); a = point([20, 0]); b = point([20, 10]); c = point([0, 10]);
  bottom = line(o, a); right = line(a, b); top = line(b, c); left = line(c, o);
  fix(o); horizontal(bottom); horizontal(top); vertical(right); vertical(left);
  length(bottom, 20); length(right, 10);
  length(top, 25);
}
```

### sketch-no-convergence

Error: the solver found no solution and no conflict among the
equations; it names the equations still unmet and the points whose
guesses to move. Here a point is asked to be on a circle of radius 5
and on a line 10 away from its centre:

```openscad expect=sketch-no-convergence
sketch(name = "apart") {
  c = circle([0, 0], r = 5, construction = true);
  l = line([10, -5], [10, 5], construction = true);
  p = point([7, 1]);
  fix(c.center); fix(l); on(p, c); on(p, l);
}
```

### sketch-flipped

Warning: the solution turns a corner, an arc or a tangency the other way
than the drawing does, which usually means the solver jumped to another
branch. The hint pins the drawing to the solution (next section).

```openscad expect=sketch-flipped
sketch(name = "flipped") {
  o = point([0, 0]); a = point([10, 0]); b = point([10, 1]);
  base = line(o, a); side = line(a, b); back = line(b, o);
  fix(o); horizontal(base); vertical(side);
  length(base, 10);
  distance(a, b, -6, along = "y");
}
```

### sketch-open-profile

Error: a profile curve's end joins no other curve, or a point joins more
than two. Share the point, or mark the curve `construction = true`.

```openscad expect=sketch-open-profile
sketch(name = "open") {
  o = point([0, 0]); a = point([10, 0]); b = point([0, 10]);
  l1 = line(o, a); l2 = line(a, b);
  fix(o); fix(a); fix(b);
}
```

### sketch-self-intersection

Warning: profile loops that cross each other or themselves, named with
where; the shape is what even-odd filling makes of them.

```openscad expect=sketch-self-intersection
sketch(name = "crossing") {
  o = point([0, 0]); a = point([10, 0]); b = point([10, 10]); c = point([0, 10]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, c); l4 = line(c, o);
  fix(o); fix(a); fix(b); fix(c);
  ring = circle([10, 5], r = 3);
  fix(ring.center);
}
```

### sketch-fillet-too-large

Error: a fillet or chamfer needs more of a line than the line has. The
hint replaces the size with the largest that fits.

```openscad expect=sketch-fillet-too-large
sketch(name = "big") {
  o = point([0, 0]); a = point([10, 0]); b = point([0, 10]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, o);
  fix(o); fix(a); fix(b);
  fillet(o, 30);
}
```

### sketch-no-guess

Information: a point written without a guess, `point()`, was placed by
the solver. The hint writes where it solved in as its guess.

```openscad expect=sketch-no-guess
sketch(name = "placed") {
  o = point([0, 0]); a = point([10, 0]); b = point();
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, o);
  fix(o); fix(a);
  distance(o, b, 4, along = "x"); distance(o, b, 5, along = "y");
}
```

### sketch-unknown-entity

Error: a constraint given something that is not an entity handle.

```openscad expect=sketch-unknown-entity
sketch(name = "typo") {
  o = point([0, 0]);
  fix(3);
}
```

### sketch-geometry-in-body

Error: a geometry statement in a sketch body (`circle(5);`), which in a
sketch makes nothing; entities are assigned (`c = circle(center, r =
5);`).

```openscad expect=sketch-geometry-in-body
sketch(name = "misuse") {
  o = point([0, 0]);
  fix(o);
  circle(5);
}
```

### sketch-foreign-entity

Error: a handle used outside the sketch that made it. This guards an
invariant: every way of passing a handle out of its sketch that has been
tried (helper modules, children, `$` variables, function literals) runs
inside the sketch and merges into it, so no example program reaches it.

### Other errors

Wrong entity kinds (`length` of a circle), bad values and a fillet at a
point that is not the corner of two lines are `invalid-argument`. Past
the `sketch_unknowns` resource limit (`--limit sketch_unknowns=N`; 5,000
under the agent limits of `serve`, `lsp` and `mcp`) a sketch stops
evaluation with a `resource-limit` error before it is solved.

## Guesses, flips and "Pin drawing"

The solver starts from the drawing, the coordinates written in the
source, and keeps the solution on the same branch as the drawing: which
way each corner turns, which side of 180° each arc sweeps, which side of
a line a tangent arc lies on. When a dimension changes a lot, it moves
the dimensions towards their targets step by step (continuation) so the
shape keeps its branch. Solving never starts from an earlier run's
solution, so the same source always gives the same shape, in a warm
editor session or a cold command line, on every platform.

When the drawing is far from the solved shape, a later parameter change
can still land on another branch (`sketch-flipped`). **Pin drawing**
rewrites every literal `[x, y]` guess in the `sketch()` call to its
solved coordinates (6 significant digits), so later edits start from the
shape as it is now. Guesses computed from parameters keep their
expressions. It is offered:

- in the editor, as a code action anywhere in the sketch
  ("Pin drawing: rewrite the guesses to the solved coordinates"), and
  as a fix on the sketch's own diagnostics;
- as the hint of a `sketch-flipped` warning, in `--format json` and the
  MCP tools.

## Tools: check, measure, the editor and agents

- **`neoscad check --enable sketch`** lists each sketch after the
  findings (`sketch 'slot' (line 4): fully constrained, 12 unknowns`),
  and its JSON has a `sketches` array: per sketch its name, place,
  `status` (`fully-constrained`, `underconstrained`, `conflict`,
  `not-converged` or `error`), `dof`, unknowns, equations, rank,
  iterations, residual, whether continuation ran, whether its shape is
  empty, and the codes of its diagnostics (`docs/cli-json.md`).
- **`neoscad measure --enable sketch --sketch NAME`** gives every
  entity's solved values without rendering it in a picture: points,
  each line's ends, length and angle, each arc's centre, radius, ends and
  sweep, each circle's centre and radius, whether it is free to move,
  and every constraint statement with its state (`satisfied`,
  `redundant`, `conflicting` or `unmet`). These are the reference
  dimensions FreeCAD shows as non-driving constraints.
- **`neoscad snapshot --enable sketch --sketch NAME`** draws the sketch
  flat in its own plane: the solved profile filled, every entity as a
  line (construction geometry dashed), its points, the names it has in
  the source, and a glyph per constraint (`H`, `V`, `||`, `L 30`,
  `R 5`...) in the colour of its state. Entities the constraints leave
  free to move are orange and those in a conflict red, so the picture
  shows where the sketch needs another constraint. The summary's
  `sketch` object counts the constraints by state and names the free
  entities.
- **The editor** (the apps, and `neoscad lsp --enable sketch` in any
  LSP editor): the vocabulary is completed, with snippets, and coloured
  only inside sketch bodies; hover shows each name's reference with the
  extension label, an entity variable's solved values from the last run,
  a constraint statement's state (satisfied, redundant, conflicting or
  not met, with its residual) and a `sketch` call's state; go to definition on a handle's member
  (`top.start`) goes to the point it names; every hint with an edit is a
  quick fix; and "Pin drawing" is a code action. An under-constrained
  sketch shows as an information marker.
- **MCP** (`neoscad mcp --enable sketch`): `check` lists the sketches,
  `measure` and `snapshot` take `sketch`, and `docs` for `sketch` ends with a complete
  sketch to adapt.

## Comparison with the FreeCAD Sketcher

NeoSCAD's sketches follow the same model as FreeCAD's Sketcher: points,
lines, arcs and circles; the core geometric and dimensional constraints;
a numerical least-squares solver; and reports of degrees of freedom,
redundant and conflicting constraints. The FreeCAD side of this table is
from its source, as surveyed in `docs/language-extensions.md`, section 3.

| FreeCAD Sketcher | NeoSCAD |
|---|---|
| Coincident | `coincident(a, b)`, or one shared point handle |
| PointOnObject | `on(p, c)` |
| Horizontal, Vertical | `horizontal(...)`, `vertical(...)` |
| Parallel, Perpendicular | `parallel(l1, l2)`, `perpendicular(l1, l2)` |
| Tangent (edge and endpoint) | `tangent(a, b)` |
| Distance, DistanceX, DistanceY | `distance(a, b, d)`, with `along = "x"` or `"y"`; `length(l, d)` for a line |
| Radius, Diameter | `radius(c, r)`, `diameter(c, d)` |
| Angle | `angle(l1, l2, deg)`, `angle(arc, deg)` |
| Equal | `equal(a, b)` |
| Symmetric | `symmetric(p, q, about)` |
| Block; the GUI's Lock (believed to be DistanceX plus DistanceY; unverified) | `fix(p, at)` |
| (no constraint type of its own) | `midpoint(p, l)` |
| Fillet (an edit that adds an arc and constraints) | `fillet(corner, r)`, applied after the solve |
| Construction mode | `construction = true` on the entity |
| Reference (non-driving) dimensions | `measure --sketch`; every constraint drives |
| Ellipses, B-splines, Snell's law, external geometry | not supported |

Behaviour that differs:

- **The sketch is code.** Entities are named by variables, and the
  drawing is the guesses written in the source, not positions a GUI
  stores.
- **Solving is part of evaluation and has no history.** The same source
  always gives the same profile. FreeCAD keeps the last solution as the
  next starting point; NeoSCAD offers "Pin drawing" to write it into the
  source instead.
- **Fillets and chamfers are a step after the solve**, not edits of the
  sketch, so they never add unknowns.
- **Under-constrained is information**, as in FreeCAD; `strict = true`
  makes it an error.

## Comparison with CadQuery sketches

CadQuery's `Sketch` also writes constraints in code
(`.segment((0, 0), (0, 3.0), "s1")` ... `.constrain("s1", "a1",
"Coincident", None)` then `.solve()`, marked experimental in its
documentation, as surveyed in `docs/language-extensions.md`, section 3).
NeoSCAD names entities by variables rather than string tags, so a typo
is an unknown-variable warning and the editor can navigate them, and
writes constraints as statements rather than a method chain.
