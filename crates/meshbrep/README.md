# meshbrep

Rebuild an exact boundary representation (B-rep) from a triangle mesh
whose triangles each name the exact surface they came from, and write it
as STEP AP214. Pure Rust, no `unsafe`, deterministic, and it builds for
`wasm32-unknown-unknown`.

A mesh kernel such as [Manifold](https://github.com/elalish/manifold) does
booleans robustly, but only on triangles. Tag every input triangle with the
plane, cylinder, cone, sphere or torus it approximates, and the tags survive the
booleans: Manifold's `MeshGL::faceID` carries them through. `meshbrep`
turns the result back into exact geometry:

- **Faces** are connected regions on one surface. Equal surfaces from
  different solids are merged, so two coplanar tops give one face.
- **Edges** are the boundaries between two faces, with exact curves
  (lines, circles, ellipses) where a closed form exists, and cubic
  B-splines within a tolerance (1e-7 by default) otherwise.
- **Vertices** are where three or more faces meet, solved on the exact
  surfaces. Tangent surfaces are detected from their records, and their
  contact line or circle is used instead of an ill-conditioned solve.
  Where two surfaces touch at a point, their intersection crosses
  itself there, and that point is solved for exactly. A torus touching a
  coaxial plane, cylinder, cone, sphere or torus (profiles of one
  `rotate_extrude`), a cylinder along its tube, and a plane touching a
  cone along a generator (one profile extruded and revolved) are
  recognised too.
- **Slivers the mesh leaves** where flush faces differ in the last bits,
  or surfaces touch, are cleaned up: mesh edges shorter than the
  touching tolerance (`max(fit, 1e-9 × size)`) are collapsed and needle
  triangles narrower than it are flipped into their neighbours before
  anything else; then edges and faces of no length or area (two straight
  edges, or two curves that run along each other, between the same
  corners: a cylinder's cap on the equator of a sphere of its radius),
  and closed components of two faces with no volume, are removed.
- **Seams and parameter-space curves** go on curved faces, sharing the
  edge's parameter, so a STEP reader uses them as written. A sphere's
  axis is chosen so its bounding circles are parallels, and so that no
  other boundary passes near its poles.
- **Faceted fallback:** triangles tagged `Surface::Faceted` (from
  `polyhedron`, `hull` or imported meshes) become planar faces, and mix
  with exact faces in one valid solid. Each face names the input
  triangles it was built from (`Report::face_triangles`), the validator
  names the faces its errors are on (`Validation::error_faces`), and
  `reconstruct_located` names the triangles where reconstruction failed
  (`Failure::triangles`), so a caller can retag the faces around a
  failure as `Faceted` and try again: a partial fallback rather than
  none of the model.

It also has a structural and geometric validator, and integrates volume
and area on the exact geometry. Use them to check a result against the
mesh before trusting it: a valid solid can still be the wrong one.

## Example

```rust
use meshbrep::primitives::{self, Transform};
use meshbrep::{Options, StepOptions, measure, reconstruct, validate, write_step};

// A cylinder of radius 5 and height 10, tessellated with 32 sides. Its
// triangles are tagged with the exact cylinder and its two planes.
let mesh = primitives::frustum(10.0, 5.0, 5.0, 32, &Transform::IDENTITY);
let brep = reconstruct(&mesh, &Options::default()).unwrap();
assert!(validate(&brep, 1e-6).is_valid());
// The exact volume, not the 32-gon prism's.
let volume = measure(&brep).unwrap().volume;
assert!((volume - std::f64::consts::PI * 250.0).abs() < 1e-9);
let step: String = write_step(&brep, &StepOptions::default());
```

With booleans, give each primitive's triangles a `face_id` that indexes
one shared surface table, run Manifold, and build a `TaggedMesh` from the
output's positions, triangles and `face_id`s. The tests in
`tests/common/mod.rs` do exactly that.

## Input

`TaggedMesh { positions, triangles, triangle_surface, surfaces }`: a closed,
consistently oriented 2-manifold, counter-clockwise from outside, with a
surface index per triangle.

`Surface` has `Plane`, `Cylinder`, `Cone`, `Sphere`, `Torus` (a ring
torus: major radius larger than minor) and `Faceted`. `LinearExtrusion`
and `Revolution` are declared for extruded and revolved free-form
curves (text outlines), but `reconstruct` does not accept them yet
(`Error::Unsupported`).

A torus face can wrap around its axis, around its tube, or both (a
whole ring, perhaps with holes). Its frame puts each angle's cut where
the face's triangles leave a gap, and adds a seam where they leave none:
a meridian, a parallel, or both through one vertex.

The tessellation used for tagging never reaches the output; only its
topology does. Choose it so that the mesh's topology matches the exact
model's: segment counts a multiple of 4 with polygon vertices on the axes,
and spheres with poles and an equator (`primitives::aligned_segments`,
`primitives::sphere`). Then a cylinder tangent to an axis-aligned plane
touches it along a mesh edge, instead of crossing it in slivers that have
no exact counterpart. `find_tangencies` lists the tangent pairs of a
surface table, so a caller can place polygon vertices on contact lines in
other orientations.

When the mesh's topology differs from the exact model's anyway, so that a
face would fold over itself once its corners are exact, `reconstruct`
returns `Error::TopologyMismatch`. Retrying with a finer tagging mesh
usually cures it. It returns the same error when a corner lands on
another edge of its own face: bodies that touch along an edge, which
rounding (after a rotation, say) joined on the wrong side. A finer mesh
does not help there.

A closed component of the mesh that lies in one plane (zero volume, as
Manifold can leave where coplanar cuts meet) is dropped, and so is a face
of two straight edges along one line (a sliver triangle once its short
edge collapses), each with a note in the report.

## Output

`Brep { vertices, edges, faces, shells, report }`: plain data with every
field public.

- An `Edge` has a `Curve` and a parameter range.
- A `Face` has its `Surface`, a `Frame` (the parametrisation written to
  STEP), its sense, and `Loop`s of `Coedge`s, each with a parameter-space
  `BSpline<2>` on curved faces.
- A `Shell` is closed. Cavities are marked `void` and written as
  `BREP_WITH_VOIDS`.
- The `Report` holds the residuals, the input mesh's genus, the tangencies
  found, and notes.

`write_step` writes millimetres, with fixed header names and date unless
the caller sets them in `StepOptions`. The same B-rep gives the same bytes
on every platform: transcendental functions come from `libm`, not the
platform.

## Validation

`validate(&brep, tolerance)` checks:

- every edge is used by exactly two coedges in opposite directions, and a
  seam twice by one face;
- loops are closed and do not cross themselves in their face's parameter
  plane, and no corner lies inside another edge of its face (a boundary
  touching itself there reads back from position-based readers such as
  OCCT as an open shell); a loop of a planar face is wider than the
  tolerance (OCCT reads a narrower one as crossing itself, or as a badly
  oriented hole);
- the Euler–Poincaré genus is a whole, non-negative number, no more than
  the input mesh's (fewer is reported in `Validation::notes`: rounding
  can leave a mesh with a tunnel of no thickness that the exact faces
  close);
- curves meet their vertices and lie on their faces' surfaces;
- every shell encloses positive volume (negative for a void).

`measure(&brep)` integrates volume and area over the exact surfaces by the
divergence and Green's theorems, along the edges' exact and
parameter-space curves. It is accurate to about 1e-12 relative, so it is
the reference to compare the mesh's volume against.

The tests reconstruct 28 models: the exact-geometry audit's 15 boolean
cases, common idioms, faceted fallbacks, CSG fillets, a void, a lone
sphere and a pointed cone. Each runs at six tagging resolutions and must
be valid, match its closed-form volume to 1e-6, and write the same bytes
twice. `oracle/` builds an OCCT read-back checker for an optional test
(`MESHBREP_OCCT_CHECK`); OCCT is a test tool only, never a dependency.

## Licence

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
