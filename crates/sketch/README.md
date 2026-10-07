# sketch-solver

A deterministic 2D geometric constraint solver for CAD sketches, in plain
Rust. Points, lines, arcs and circles; coincident, on, horizontal,
vertical, parallel, perpendicular, tangent, distance, length, radius,
diameter, angle, sweep, equal, midpoint, symmetric and fix constraints.
Besides the solution it reports what a sketcher needs to explain itself:
the free degrees of freedom and which coordinates can still move, the
constraints that are redundant, the ones that conflict, and where a
solution flipped away from the drawing.

It was written for [NeoSCAD](https://neoscad.org)'s constrained sketches
and has no dependency on NeoSCAD: its API is plain data keyed by entity
and constraint ids, which a host maps to its own names and source
locations.

## Example

```rust
use sketch_solver::{Along, Constraint, Pair, Sketch, Status};

let mut s = Sketch::new();
let o = s.point(Some([0.0, 0.0]))?;
let a = s.point(Some([38.0, 1.0]))?; // the drawing: roughly right
let b = s.point(Some([39.0, 28.0]))?;
let base = s.line(o, a)?;
let side = s.line(a, b)?;
s.line(b, o)?;
s.add(Constraint::Fix { entity: o, at: None })?;
s.add(Constraint::Horizontal(Pair::Line(base)))?;
s.add(Constraint::Vertical(Pair::Line(side)))?;
s.add(Constraint::Length { line: base, value: 40.0 })?;
s.add(Constraint::Distance { a, b, value: 30.0, along: Along::Y })?;

let sol = s.solve();
assert_eq!(sol.status, Status::Solved);
assert_eq!(sol.dof, 0);
assert_eq!(sol.point(b), Some([40.0, 30.0]));
# Ok::<(), sketch_solver::ModelError>(())
```

Remove the `Length` and `sol.dof` becomes 1, with `sol.free` naming the x
coordinates of `a` and `b`; add a second, different `Length` and
`sol.conflicts` names it, against the constraints it contradicts.

## How it works

Each point is two unknowns and each circle's radius one. Every
constraint is one or two equations whose residuals are lengths, with
analytic gradients. The unknowns split into connected components, each
solved on its own:

1. **Clean the drawing**: solve with every dimension at its drawn value,
   so only the geometric constraints move points. Damped (minimal-norm)
   steps leave unconstrained directions where they were drawn.
2. **Solve**: Levenberg–Marquardt with a fixed damping schedule, from the
   cleaned drawing to the real dimensions, then a few Gauss–Newton steps
   to polish to rounding level.
3. **Keep the drawn branch**: if the solve failed, backtracked, or the
   solution's orientation (which way corners turn, which side of 180° an
   arc sweeps, which side of a line a tangent arc lies) differs from the
   drawing's, *continuation* walks the dimensions from drawn to wanted in
   steps, each from the last.
4. **Exactify**: coordinates that equations make equal get the same bits;
   fixed coordinates get their exact values.
5. **Diagnose**: Householder QR of the Jacobian's rows, taken in
   constraint order, gives the rank, the free coordinates (from the null
   space), and each dependent equation with the earlier ones it depends
   on: redundant if its residual agrees with theirs, a conflict if not.

Sketches of tens to a few hundred unknowns solve in microseconds to tens of
milliseconds; the linear algebra is dense, O(n³) per iteration.
`SolveOptions` has a limit on unknowns and an interrupt callback for
hosts that need a bound.

## Determinism

The result depends only on the sketch: the same bits on aarch64, x86_64
and wasm32, and never on an earlier solve.

- Single-threaded; no hash maps; every loop runs in a fixed order.
- IEEE `+ - * /` and `sqrt` only, which are correctly rounded everywhere.
  No fused multiply-add.
- The little trigonometry there is (an angle constraint's cosine and
  sine, the drawn angle, the placement of points without a guess) goes
  through the pure-Rust [`libm`](https://crates.io/crates/libm) crate, not
  the platform's maths library.

NeoSCAD checks this by solving a fixed set of 200 generated sketches
natively on each CI platform and as wasm32 in node, and comparing a
digest of every solved coordinate's bits.

## WebAssembly

The crate uses no file system, environment, clock or threads, and builds
for `wasm32-unknown-unknown` without features or glue.

## Validation

In the NeoSCAD repository (not in the published package), the solver is
tested against cases translated from FreeCAD's Sketcher tests and
SolveSpace's constraint regression tests, and compared with SolveSpace's
solver on generated sketches by a differential script. Those test files
are derived from GPL and LGPL projects and are kept out of this
permissively licensed package.

## Licence

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT licence ([LICENSE-MIT](LICENSE-MIT))

at your option.
