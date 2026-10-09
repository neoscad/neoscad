# meshbrep

Rebuild an exact boundary representation (B-rep) from a triangle mesh
whose triangles each name the exact surface they came from, and write it
as STEP AP214. Pure Rust, no `unsafe`, deterministic, and it builds for
`wasm32-unknown-unknown`.

A mesh kernel such as [Manifold](https://github.com/elalish/manifold) does
booleans robustly, but only on triangles. Tag every input triangle with the
plane, cylinder, cone, sphere, torus or B-spline surface it approximates, and
the tags survive the booleans: Manifold's `MeshGL::faceID` carries them through. `meshbrep`
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
  recognised too, and so is a B-spline patch touching another surface
  along a side of its domain (a blend along the face it rolls on): that
  side is the edge, and corners on it are solved along it.
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

`Surface` has `Plane`, `Cylinder`, `Cone`, `Sphere`, `Torus` and
`Faceted`. A torus's major radius must be above zero; when it is no
larger than the minor one (a spindle or horn torus, which crosses its
axis) the surface is its outer part only, the apple, and faces on it
must keep off the axis. `write_step` writes such a torus as a
`DEGENERATE_TOROIDAL_SURFACE` with `select_outer` true (OCCT 8.0.1 reads
it back as that part: with `select_outer` false the same file reads as
the inner part, with another volume). `LinearExtrusion`
and `Revolution` are declared for extruded and revolved free-form
curves (text outlines), but `reconstruct` does not accept them yet
(`Error::Unsupported`).

`Surface::BSpline` is a clamped B-spline patch (`BSplineSurface`:
degrees, control net, knot vectors, and weights when it is rational):
the exact form of a surface with no closed form, such as a blend
between two curved faces. Faces on it lie inside its domain, which does
not wrap (no seams; a closed B-spline surface is not supported), and it
must be regular (no collapsed side such as a sphere's pole). Its points
are projected onto it by Newton's method from the nearest of a grid of
seeds; its implicit form, for solving corners and edges, is the signed
distance along the normal at that projection, continued a little past
the patch's sides by its end spans. `write_step` writes it as
`B_SPLINE_SURFACE_WITH_KNOTS`, or for a rational one as the complex
entity with `RATIONAL_B_SPLINE_SURFACE`. A patch that was fitted (a
blend whose spine and contact curves were) meets its neighbours only to
within the fit: `Tolerances::surface_fit` (default 1e-7) is how far its
side may be from the surface it touches and still be their contact, and
the edge's deviation, the corners' residuals and the validator's
tolerance must allow for it on top of the tagging's sagitta.

The `spline` module evaluates and projects onto them (`Evaluator`), and
builds what blends need: cubic curves fitted on one knot vector
(`fit_curves`: a spine and its two contact curves), a cubic surface
through a grid or fitted to a function (`interpolate_surface`,
`fit_surface`), the rational canal surface a ball sweeps between two
contact curves with exact circular arcs across (`canal_surface`), and
the ruled surface between two curves (`ruled_surface`, a chamfer).

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
  `BSpline<2>` on curved faces. A B-spline face's parameters are its
  surface's own; its frame is only a placement (a point of the patch,
  its normal and `∂u`), not written.
- A `Shell` is closed. Cavities are marked `void` and written as
  `BREP_WITH_VOIDS`.
- The `Report` holds the residuals, the input mesh's genus, the tangencies
  found (`Contact::Boundary` for a B-spline patch's side, one per side),
  and notes.

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
twice. `tests/bspline.rs` does the same for solids with a B-spline face
(a quarter cylinder as the exact rational patch, a box with a bicubic
top, that box drilled through the top by Manifold, a boss's fillet as
the exact rational torus patch and as a fitted canal surface, its
chamfer as a ruled surface) at five resolutions, to the closed-form
volume within 1e-9 (1e-8 where the surface was fitted). `oracle/` builds an OCCT read-back checker for an optional test
(`MESHBREP_OCCT_CHECK`); OCCT is a test tool only, never a dependency.

## Blend tools

`blend::tools(&spec, segments)` makes the solids that round or chamfer
edges when a mesh kernel subtracts them (convex edges) or adds
them (concave ones): for each edge between two planes at any angle, or a
plane and a parallel cylinder, or two parallel cylinders, a constant-radius
fillet (a cylinder blend) or an equal-distance chamfer (a plane), swept
between end planes (a face the edge runs into, a mitre, the plane across a
tangent continuation) or run on into the air; and sphere patches where
three filleted edges meet between three planes. Every triangle is tagged
with its exact surface, and the arcs have vertices exactly on the tangent
lines, so the boolean's result reconstructs with true blend faces.
`blend::section` gives an edge's cross-section (centre, tangent points,
how far into each face) for checks before anything is built. The edges of
a sphere corner and its patch come out as one solid, so no two tools share
a face.

Circular edges (`Path::Arc`) between surfaces of revolution about one
axis (a plane square to it, a coaxial cylinder, cone or torus, a sphere
centred on it) get the same cross-section in the meridian half-plane,
revolved: a torus blend for a fillet, a cone (or plane, or cylinder) for
a chamfer. The caller gives the angles to put its sections at, those of
the polygon the face beside the edge has in the mesh the tool is applied
to, and how far each of that polygon's vertices lies off the exact
circle, so the tool's tangent ring runs through the polygon's own
vertices. Edges that run on into each other (`End::Chain`: a rounded
rectangle's lines and arcs) come out as one solid.

A convex rim's fillet may be wider than half the rim's radius: its blend
is then a spindle torus (a boss's top rim filleted with anything that
leaves some of the top). The tools of a sphere corner share one margin
with the rest of any tangent chain one of its edges belongs to, so a
corner whose edge runs on into an arc (an L-bracket's end face rounded
with its outline) closes.

## Changes since 0.2.0

- `Surface::Torus` accepts spindle and horn tori (major radius above
  zero, no longer above the minor radius), written to STEP as
  `DEGENERATE_TOROIDAL_SURFACE(..., .T.)` when the major radius is the
  smaller. Blend tools build convex rim fillets up to the rim's radius.
  A caller that relied on `reconstruct` refusing such a torus as
  malformed sees it accepted now.
- `blend::tools`: the edges of a sphere corner take the least margin of
  every tangent chain one of them belongs to (before, a corner edge
  chained to an arc could make an open tool).
- B-spline surfaces: `Surface::BSpline` and `BSplineSurface` (clamped,
  optionally rational), reconstructed, validated, measured and written
  to STEP, and the `spline` module (evaluation and projection, curve and
  surface fitting, canal and ruled surfaces). Breaking for a caller
  that matches `Surface` exhaustively.
- `Contact::Boundary`: a B-spline patch touching another surface along
  a side of its domain, in `Report::tangencies` and `find_tangencies`
  (one entry per side). Breaking for a caller that matches `Contact`
  exhaustively.
- `Tolerances::surface_fit` (default 1e-7): how far a B-spline patch's
  side may be from the surface it touches. Breaking for a caller that
  builds `Tolerances` with a struct literal (use `..Default::default()`).
- Blends between curved surfaces with no common axis: `blend::Path::Curve`
  (an edge's points, and per face the triangles near it that the tool is
  conformed to). The ball's centre is marched along the intersection of
  the two faces' offsets, and the blend is a canal (fillet) or ruled
  (chamfer) B-spline surface fitted to it, two patches for a closed curve;
  its ends are `End::Plane` or `End::Open`. Breaking for a caller that
  matches `Path` exhaustively.
- `blend::Tool::fit`: how far a tool's B-spline blends may be from the
  faces they meet (0 when every blend is a quadric or torus); a caller
  reconstructing the result sets `Tolerances::surface_fit` to at least
  that. Breaking for a caller that builds `Tool` with a struct literal.
- `blend::curve_sections` (a curve's blend across it at fractions of its
  length) and `blend::check` (what `tools` would refuse, without fitting
  or meshing the curves' blends), for size checks run many times.
- Reconstruction: a parameter-space curve along a tangent contact is
  refined until its image stays on the edge's own curve too, not only
  on the other face (across a tangent contact the distance to that face
  changes only to second order, so a coarse curve passed 1e-4 off it).
  Files with tangent blends grow (a plate's straight and rim fillets:
  137 KB to 325 KB). A mesh chain along a B-spline patch's contact side
  is accepted within a fifth of the patch's width (before: 1e-4 of the
  chain's length), measured after moving it onto the other face; and a
  corner where contact sides of different patches meet is taken where
  they meet.
- `spline::Evaluator::project_extended`: projection that may run a
  little past the patch's sides onto the surface its end spans continue
  (what reconstruction's implicit form uses), for measuring how far
  points beside a side stand off the patch.
- Reconstruction of B-spline faces is several times faster: the corner
  and edge solver projects once per step for both the value and the
  gradient, stops (on a patch only) once the residual stops halving and
  keeps the best point, and an edge fit on a patch that does not
  converge is given up after three doublings without progress. Points
  that solves left wandering at the projection's rounding are now the
  best ones found, so fitted edges change in their last digits.
- A sphere face none of whose bounding circles can be parallels (a box
  corner's patch) is framed with its poles square to the face and as
  many bounding circles as possible meridians, whose parameter-space
  curves are segments: a rounded box's STEP file shrinks (a plate's
  straight and rim fillets: 325 KB to 198 KB). A periodic face's seam
  avoids passing within 1e-6 of the model's size from a vertex of
  another loop, which split off a degenerate edge.

## Licence

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
