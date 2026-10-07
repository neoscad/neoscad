# Audit: an exact-geometry backend in pure Rust (STEP export, later 3D fillets)

Status: judgement plus measurements. Written 2026-10-07 against
`56cb815` and the reference checkout at `28fe66b`.

**Update, stage 1a built (2026-10-07):** path 1 is approved. Its
reconstruction is the standalone crate `crates/meshbrep` (`MIT OR
Apache-2.0`, no NeoSCAD dependency, to move to `neoscad/meshbrep`). It is
driven by tests and not yet wired into the evaluator or CLI (stage 1b).
It now has:

- seams, and parameter-space curves on every curved face;
- analytic tangency, arc merging and short-edge collapse;
- faceted fallback, voids (`BREP_WITH_VOIDS`), and a structural
  validator;
- volume and area integrated on the exact geometry.

All 28 test models (the 15 cases below, x01–x11, f01–f02) pass at the six
resolutions of section 3.4. Validity is checked by its own validator and
by OCCT 8.0.1 read-back (168 files). The volume error against closed form
is at most 4.5e-9 relative. STEP bytes are identical across runs and
between native and wasm32 (`scripts/wasm-check.sh`). F3 is confirmed and
fixed, with one addition: OCCT drops a parameter-space curve that is not
parametrised like its edge's 3D curve, and projects its own instead
(`XSAlgo_AlgoContainer::CheckPCurve`, OCCT 7.8.1
`src/XSAlgo/XSAlgo_AlgoContainer.cxx:314-359`). Doing so put c01 5.9e-5
off in OCCT's own volume (adaptive integration), although the file read
back valid. F2 has a
faceted counterpart: x07 at 8–16 segments gives folded sliver faces,
which `reconstruct` now reports as `TopologyMismatch` and a finer tagging
mesh cures. Leftovers are in `docs/followups.md`, "Exact geometry".
**Update, stage 1b built (2026-10-07):** `neoscad --enable exact -o
x.step` exports STEP through `crates/geom/src/exact` (an export render
tagged per surface, `meshbrep`, then a volume cross-check against the
mesh and the normal render). Its design follows F7: a second render
whose tessellation never reaches the file; curves are exact unless `$fn`
is set; every substitution is reported at its source line. Stage 1b also
fixed two `meshbrep` bugs it found: plane loops were unwrapped as if `u`
were an angle, so a hole in a face larger than about π mm read as a
second outer loop; and equal surfaces were merged by a pairwise scan
(7.5 s on 125 `$fn = 48` spheres, now 0.4 s, same bytes). The gate-1
measurement (`conformance exact`, with OCCT 8.0.1 read-back):

| Corpus | 3D models | Eligible | Valid, ours | Valid, ours + OCCT | All-faceted | Failed |
|---|---|---|---|---|---|---|
| This audit's 28 cases × 4 `$fa`/`$fs` settings | 112 | 104 | 104 | 104 (100%) | 0 | 0 |
| OpenSCAD's `render-manifold` inputs | 214 | 81 | 79 | 78 (96.3%) | 80 | 21 |
| BOSL2 examples, every 5th | 311 | 44 | 38 | 38 (86.4%) | 145 | 53 |
| Benchmark models | 14 | 4 | 4 | 3 (75%; 1 not read in 2 GB) | 5 | 2 |
| **Real corpora, deduplicated** | 537 | 127 | 119 (93.7%) | **118 (92.9%)** | 230 | 76 |

"Eligible" means nothing fell back to facets (a non-uniform scale
aside). Gate 3: all 108 closed-form cases within 4.5e-9. Gate 4: every
exported file passed the cross-checks; OCCT disagrees with 10 of 564
(one eligible, the Menger sponge; `docs/followups.md`). Gate 5 is
missed: (reconstruct + check + write) / render has a median of 0.65 on
the render tests but 5.4 on the benchmarks. By the stop rule, 92.9% is
between 80% and 95%: the owner decides, with the failure classes in
`docs/followups.md`, "Exact geometry".

It follows `docs/audits/brep-feasibility.md` (below, "the previous
audit"), which found that only OCCT survives OpenSCAD-shaped trees. The
owner prefers an exact backend written in-house: pure Rust, publishable as
its own crate (`MIT OR Apache-2.0`), WASM-capable and deterministic. This
audit compares three ways to get one.

Claims about this codebase cite `path:line`. Claims about third-party code
cite the repository, crate version or paper retrieved, or say "unverified".
Measurements come from scratch spikes (not committed): release builds on an
Apple M4 Pro, one process per run under a watchdog. Timings are single runs
unless marked; treat them as ±10%.

## Contents

1. Firm ground
2. Findings, ranked
3. Path 1: mesh-guided B-rep reconstruction (spiked)
4. Path 2: improving monstertruck or truck upstream
5. Path 3: a focused exact kernel of our own
6. Comparison
7. Fillets and chamfers on each path
8. Packaging as a standalone crate
9. Checked and found fine
10. Recommendation, first stage and stop rule
11. Not verified

## 1. Firm ground

- **Mesh-guided reconstruction works on all 15 boolean cases.** The spike
  lets Manifold do every boolean. It tags each input triangle with the
  exact surface it came from and rebuilds faces, edges and vertices from
  the output mesh on those exact surfaces. It then writes AP214 STEP. OCCT
  8.0.1 read all 15 files back as one closed, valid solid
  (`BRepCheck_Analyzer`), with no free edges. On 12 of the 15 the volume
  matches OCCT's own boolean, or the closed-form value, to better than
  1e-6 relative. The other 3 (all spheres) are within 3.1e-4. Section 3
  shows that error comes from the reader rebuilding the trimming curves
  in parameter space, not from our geometry.
- **The weak point is tangency, and it is fixable in the spike.** With
  OpenSCAD's own tessellation, a capsule, a rounded plate built from
  cylinders and cubes, and a CSG fillet come out invalid at some fragment
  counts. The tessellation used for attribution is ours to choose, because
  only the exact surfaces reach the file. With it aligned (fragment counts
  a multiple of 4, spheres with poles and an equator ring), all 21
  boolean cases passed at all 6 resolutions tried, and so did the two
  fillet cases at both resolutions tried (sections 3.4–3.5).
- **The cost over the mesh path is small.** On the 400-hole plate (33,612
  triangles), reconstruction took 39–49 ms and writing 6.5 ms. NeoSCAD
  renders the same model in 116–123 ms (`neoscad 0.4.3`, 3 runs). OCCT's
  boolean alone took 0.88 s here (the previous audit measured 740 ms, best
  of 3).
- **The output was deterministic in the checks made.** All 21 STEP files
  were byte-identical across two runs that processed the cases in reverse
  order. Manifold's process-wide mesh IDs differed between those runs.
- **Upstream truck and monstertruck have not fixed the failing classes.**
  monstertruck 0.4.1 (crates.io), run on the same reconstructed trees,
  passed 5 of 15 at tolerance 0.01, 0.05 and 0.1. It failed every
  sphere case, every coplanar or touching union and both countersunk
  plates. Its boolean is itself mesh-guided, but with its own
  non-robust mesh intersection, and any failed step aborts the operation
  (section 4). truck's coplanar-union issue has been open since
  2024-02-06 (#57), and the PR that fixes it (#110) has been open since
  2026-01-30 with no review.
- **Exact quadric booleans are a research-grade problem.** ESOLID, an
  exact boundary evaluator for exactly these primitives, states that it
  "cannot be considered a robust system", because its input is
  "restricted to non-degenerate configurations". It was also up to two
  orders of magnitude slower than an inexact system (section 5).

## 2. Findings, ranked

### F1. Path 1 is viable, and is the only pure-Rust path with measured evidence

**Our code today:** NeoSCAD tracks attribution per *original* (part or
colour), not per surface. `set_original_id` sets every triangle's
`face_id` to -1 (`crates/geom/src/manifold_geom.rs:797`). `make_original`,
used by `render()`, `color()` and `to_original`, clears `face_id` when it
rebuilds the mesh (`:672-677`). `from_polyset` gives each colour group one
original ID and passes no `face_id` (`:214-228`). `docs/followups.md:1229-1232`
says attribution survives booleans, transforms and `color()` at part
level.

**The brief was partly wrong here.** NeoSCAD relies on *original* IDs
surviving booleans, not face IDs. Per-surface attribution is new plumbing.
The kernel already supports it. `MeshGLP::face_id` is a per-triangle
source ID (`vendor/manifold-rust/src/types_meshgl.rs:103-104`). It is
kept on import (`manifold_meshgl.rs:213-221`) and exported after booleans
(`:441-445`). Simplification only merges triangles that are `same_face`
(`types.rs:456-460`, used in `edge_op.rs`), so triangles from two source
surfaces are never merged into one.

**Measured:** section 3. 15/15 valid, and 21/21 including six common
idioms.

**Who it affects:** everyone who would use STEP export. This is the path
that matches the owner's constraints.

**Suggested change:** adopt path 1. Start with stage 1 and its stop rule
(section 10).

### F2. Mesh topology can disagree with exact topology at tangencies; an aligned attribution tessellation fixes the measured cases

**Measured:** with OpenSCAD's fragment rule, these were invalid:

- the capsule `x02` (cylinder plus two end spheres), at 3 of 4
  resolutions;
- the rounded plate `x03` (4 corner cylinders tangent to 2 cubes) and the
  CSG fillets `f01`/`f02`, whenever the fragment count put no polygon
  vertex on the tangent line;
- the equal-radius tee `c14`, at `$fn=7`.

OCCT reported these files invalid, or reported wrong volumes. With the
aligned tessellation, every one passed (section 3.4).

**Why:** the inscribed polygon of a cylinder that is tangent to a plane
either touches the plane, crosses it, or misses it, depending on the
fragment phase. Manifold resolves each of those correctly *for the
polygons*. But "crosses" produces sliver faces that have no exact
counterpart.

**Suggested change:** treat the attribution mesh as an export-time render
with its own tessellation rule. Fragments are a multiple of 4 with
axis-aligned phase, and spheres get poles and an equator. Tangency is
also detected analytically from the surface records (pairs of records
that touch), so that vertices can be placed at tangent lines in
orientations that are not axis-aligned (not spiked). The owner's `$fn` rule
already allows this. Primitives with an explicit `$fn` keep OpenSCAD's
polygons, which are planar and exact as they are. Only `$fa`/`$fs`/`$fe`
primitives become exact, and their tessellation never reaches the file.

### F3. The STEP writer must emit seams and pcurves; without them, curved faces read back with tolerances up to 6.7e-3

**Measured:** the spike's writer emits 3D curves only. OCCT's reader then
rebuilds the 2D trimming curves itself. On faces of revolution it adds
seams (reported as B-spline edges) with tolerance 1e-5. On spheres
trimmed by circles that are not parallels, it ends at tolerance 1.8e-3
(`c01`, `c02`) and 5.5e-3 to 6.7e-3 (`c13`).

Those are the three cases whose volume is off: 3.1e-4, 2.8e-5 and 1.6e-6
relative. Our edges there are exact circles, with deviation from both
surfaces ≤ 2e-15.

Two checks confirm the cause:

- Writing `c13`'s sphere with its axis along the circle's normal (so the
  circle is a parallel) made it read back to OCCT's volume exactly
  (3619.114737), at tolerance 1e-5.
- OCCT's own files for `c01`/`c02` carry 63 and 27 `PCURVE`s plus 2
  `SEAM_CURVE`s each, and read back at 1.2e-7.

**Gap:** a 0.03% volume error and loose tolerances on spheres, in every
OCCT-based importer, FreeCAD included.

**Suggested change:** in stage 1, write `SEAM_CURVE`s on periodic faces,
and write `PCURVE`s (2D B-splines fitted in the surface's parameter space,
to 1e-7) for edges on curved faces. Choose the parametrisation axis of
spheres and tori from the face's loops. About 1–2 person-weeks (estimate).

### F4. monstertruck/truck fail by design, not by a few bugs

Section 4. The boolean finds intersection curves by triangulating both
shells at `tol` and intersecting the meshes
(`monstertruck-solid-0.4.1/src/transversal/classic/mod.rs:90-91`,
`intersection_curve.rs:88`). It joins segments by hashing points into
`2·TOLERANCE` cells (`polyline_construction/mod.rs:40-44`). There is no
coplanar or tangent handling, and a `None` at any step aborts the whole
operation. Making that robust means redoing what Manifold already does
(symbolic perturbation), but on curved surfaces.

**Suggested change:** do not invest in path 2. Re-run the 15 cases
against monstertruck yearly, as the previous audit suggested.

### F5. A focused exact kernel (path 3) is a multi-year research effort with known failure modes

Section 5. **Suggested change:** reject it as a primary path. Some of its
parts (exact intersection classification of quadric pairs, exact
predicates) are useful *inside* path 1's curve construction. manifold-rust
already carries `dashu` for its robust engine
(`vendor/manifold-rust/Cargo.toml:115-129`,
`src/robust/exact/mod.rs:1-25`).

### F6. 2D profile attribution needs Clipper2's Z channel, which NeoSCAD's pinned behaviour has not been checked with

`linear_extrude`/`rotate_extrude` of circles, `offset(r)` arcs and text
need each 2D vertex to remember its source curve through Clipper2
booleans and offsets. clipper2-rust has a `using_z` feature
(`vendor/clipper2-rust/Cargo.toml:54-56`, with tests in
`src/using_z_tests.rs`). NeoSCAD pins Clipper's output vertex for vertex
(`crates/geom/Cargo.toml:26-28`).

**Suggested change:** in stage 2, enable `using_z` only in the export
render, and run the conformance suite to prove the default output is
unchanged. Not spiked.

### F7. A product decision: export needs a second render

The aligned attribution mesh differs from the display mesh, so export
renders the tree again (about the cost of today's render) and then
reconstructs. On the 400-hole plate that is roughly 120 ms + 45 ms. The
alternatives are to try the display mesh first and re-render only on
failure, or to always render twice. This is an owner decision. It also
interacts with the cache (the export render must not share entries with
the display render).

## 3. Path 1: mesh-guided B-rep reconstruction (spiked)

### 3.1 What the spike does

About 1,800 lines of Rust. Its only dependency is the vendored
`manifold-rust` (with `default-features = false`), and it builds for
`wasm32-unknown-unknown`.

1. **Primitives.** Tessellated as OpenSCAD does: `cube`, `cylinder` (and
   cones, including apex cones), `sphere` with OpenSCAD's ring layout, and
   polygon prisms for `$fn` polygons. Each triangle's `face_id` is the
   index of an exact surface record: a plane, a cylinder, a cone (apex,
   axis, slope) or a sphere. The record is transformed with the triangle.
   Faceted children (a stand-in for `polyhedron`/`hull`) get one plane per
   triangle.
2. **Booleans** go through `Manifold::boolean`. Output triangles keep
   their record through `face_id`.
3. **Surface classes.** Records that are geometrically the same (to
   1e-9 × model size) are merged. Coplanar tops of 10 cylinders, two cubes
   on one plane and a coaxial stack each become one face.
4. **Faces** are connected regions of triangles in one class. A face's
   orientation is the area-weighted vote of its triangles' normals
   against the surface normal.
5. **Vertices** are mesh vertices where three or more faces meet, solved
   on the exact surfaces (Gauss–Newton, minimum-norm steps). Also vertices
   where two curved surfaces are *tangent*: solved with
   (fA, fB, nA×nB) = 0 near the mesh vertex, keeping one vertex per
   tangent point.
6. **Edges** are boundary chains between two faces. The curve comes in
   closed form where one exists:

   | Surface pair | Curve |
   |---|---|
   | plane–plane | line |
   | plane–cylinder | circle (perpendicular), line (parallel) or ellipse |
   | plane–cone (perpendicular), plane–sphere, sphere–sphere | circle |
   | coaxial surfaces of revolution | circle |
   | parallel cylinders | line |
   | two quadrics whose intersection is planar (equal-radius cylinders with crossing axes) | ellipse, found by testing the projected chain for planarity |

   Otherwise the chain is projected onto both surfaces and densified until
   a cubic B-spline interpolant lies within 1e-7 of both. Each curve is
   oriented to follow its chain.
7. **STEP AP214:** a hand-written writer (180 lines) with a fixed header
   and date. It emits `PLANE`, `CYLINDRICAL_SURFACE`, `CONICAL_SURFACE`,
   `SPHERICAL_SURFACE`, `LINE`, `CIRCLE`, `ELLIPSE` and
   `B_SPLINE_CURVE_WITH_KNOTS`, wrapped in `MANIFOLD_SOLID_BREP`. It writes
   no pcurves or seams (F3).

Validation used OCCT 8.0.1 out of process: cadrum's prebuilt static
library (release `occt-8_0_1_rev2`) and a 200-line C++ checker. The
checker reads each file and reports `BRepCheck_Analyzer`, solids, closed
shells, free edges, volume, area, face and edge types, and the largest
tolerance. As an oracle it also built the same tree with OCCT's
`BRepAlgoAPI` from a text dump the spike writes, so both sides use
identical case definitions. OCCT is a test tool here only, never a
dependency.

### 3.2 The 15 cases, reconstructed

The previous audit counts 15 boolean cases but defines only some of them.
Its truck and monstertruck tallies (4/15 each) fit b01, b03 and c01–c15
minus the two extrusions (c04, c05), so those are the 15 used here.
Dimensions were reconstructed. c01, c02 and the plates (c06, c07) are
given in that audit. b03's dimensions follow from its stated exact volume
803.65 (a 10 mm cube minus a radius-2.5 cylinder). The rest are new and
written as OpenSCAD in the spike's case table:

| Case | Tree |
|---|---|
| b01 | `cube(10) ∪ translate([5,5,5]) cube(10)` |
| b03 | `cube(10) − translate([5,5,-1]) cylinder(r=2.5,h=12)` |
| c01 / c02 | `cube(15,center=true)` − / ∩ `sphere(10)` |
| c03 | `cube(10) ∪ translate([10,5,0]) cylinder(r=3,h=15)` (both on z=0) |
| c06 / c07 | the countersunk plate, 2×2 / 6×4 (previous audit, section 4) |
| c08 | `for(i=[0:9]) rotate(36*i) translate([3,0,0]) cylinder(r=2,h=10)`, union |
| c09 | `cube(10) ∪ translate([10,0,0]) cube(10)` |
| c10 | `cube([20,20,10]) − translate([5,5,5]) cube([10,10,5])` (pocket flush with top) |
| c11 | `cylinder(r=10,h=5,$fn=6) − translate([0,0,-1]) cylinder(r=4,h=7)` |
| c12 | `cylinder(r=5,h=30) − translate([0,0,20]) rotate([90,0,0]) cylinder(r=1.5,h=12,center=true)` |
| c13 | `sphere(10) − translate([8,0,0]) sphere(6)` |
| c14 | `rotate([0,90,0]) cylinder(r=5,h=40,center=true) ∪ cylinder(r=5,h=20)` |
| c15 | `cube([20,20,10]) − translate([5,10,-1]) cylinder(r=5,h=12)` (hole tangent to x=0) |

Results come from OpenSCAD's own tessellation (`$fa=12`, `$fs=2`). The
reference volume is OCCT's boolean, except for c01, c12 and c14, which
use closed forms computed for this audit. "Faces" is ours / OCCT's.

| Case | OCCT reads ours: valid | Volume ours | Volume ref | Rel. error | Edges (ours) | Faces | Max tol after read | Recon ms |
|---|---|---|---|---|---|---|---|---|
| b01 | yes | 1875.0000 | 1875.0000 | 0 | 30 line | 12/12 | 1e-7 | 0.15 |
| b03 | yes | 803.6505 | 803.6505 | 0 | 12 line, 2 circle | 7/7 | 1e-5 | 0.12 |
| c01 | yes | 266.0489 | 266.1323 | 3.1e-4 | 12 line, 6 circle | 7/7 | 1.8e-3 | 1.1 |
| c02 | yes | 3108.9560 | 3108.8677 | 2.8e-5 | 6 circle | 7/7 | 1.8e-3 | 2.2 |
| c03 | yes | 1282.7433 | 1282.7433 | 0 | 16 line, 3 circle | 9/11 | 1e-5 | 0.08 |
| c06 | yes | 3991.0620 | 3991.0620 | 0 | 12 line, 12 circle | 14/14 | 1e-5 | 0.31 |
| c07 | yes | 23946.3719 | 23946.3719 | 0 | 12 line, 72 circle | 54/54 | 1e-5 | 1.8 |
| c08 | yes | 714.1537 | 714.1537 | 0 | 20 line, 40 circle | 22/110 | 1e-7 | 0.26 |
| c09 | yes | 2000.0000 | 2000.0000 | 0 | 12 line | 6/10 | 1e-7 | 0.03 |
| c10 | yes | 3500.0000 | 3500.0000 | 0 | 24 line | 11/11 | 1e-7 | 0.04 |
| c11 | yes | 1047.7107 | 1047.7107 | 0 | 18 line, 2 circle | 9/9 | 1e-5 | 0.07 |
| c12 | yes | 2286.3130 | 2286.3131 (closed form) | 4.4e-8 | 2 circle, 2 B-spline | 4/4 | 1e-5 | 1.8 |
| c13 | yes | 3619.1090 | 3619.1147 | 1.6e-6 | 1 circle | 2/2 | 5.5e-3 | 0.36 |
| c14 | yes | 4379.0558 | 4379.0556 (closed form) | 2.7e-8 | 3 circle, 2 ellipse | 5/5 | 1e-5 | 0.22 |
| c15 | yes | 3214.6018 | 3214.6018 | 0 | 16 line, 2 circle | 8/9 | 1e-7 | 0.15 |

Notes:

- **Exact-face fraction is 100%** in every case. Every face carries its
  primitive's analytic surface. **Exact-edge fraction** is 100% except
  c12, where the two cylinder–cylinder curves are B-splines within
  2.4e-8 of both surfaces. OCCT also uses B-splines there. OCCT's own
  c12 volume is 4.1e-6 off the closed form; ours is 4.4e-8 off.
- **Fewer faces than OCCT.** Coplanar and coaxial faces merge (c08: 22
  faces against OCCT's 110; c09: 6 against 10), which makes a smaller and
  more useful file for a receiving CAD.
- **Tangent case c15.** A hole tangent to a wall reads back valid with the
  correct volume. OCCT's own result splits the cylinder at the tangent
  line (9 faces); ours keeps one face that touches the wall.
- **Tangent case c14.** The tee needed two additions to the spike: the
  tangent-point vertices (the two ellipses cross where the cylinders are
  tangent) and the planar-section test. Before them, a single closed
  B-spline through the kinks read back as a *valid* solid whose volume
  wandered from 4360 to 4385 as the fit tolerance changed. This is the
  previous audit's F4 lesson again: validity checks alone are not enough,
  so the volume cross-check is mandatory.

### 3.3 Coincident and split faces (the attribution question)

The brief asked whether Manifold's tessellation and coplanar merging keep
enough attribution. They did in every case run:

- Faces a boolean splits keep their record.
- Faces that touch a coincident face are removed or kept by Manifold's
  symbolic perturbation, and the survivors keep their record (c09, c10,
  c03's shared floor, x05's flush counterbore).
- Opposite-facing coincident faces vanish.

No output triangle lost its `face_id` (the spike checks for that). Two
things need care in production:

1. **Equal surfaces from different primitives** must be merged by
   geometry, not by ID (step 3 above). Otherwise a union of two cubes on a
   plate gives two coplanar faces sharing an edge. The tolerance (1e-9 ×
   size here) has to absorb transform rounding. `rotate(120)` applied
   three times is the kind of input that will test it (not tried).
2. **NeoSCAD's `to_original`/`set_color` rebuild** drops `face_id`
   (`manifold_geom.rs:672-677`). The export render needs its own path that
   keeps it, or a parallel table keyed by triangle.

### 3.4 Robustness across tessellations

Each set was run at 6 attribution resolutions: the defaults, `$fs=0.5`,
and `$fn` of 5, 7, 13 and 64. The `$fn` values here set only the
attribution mesh; every curved primitive is still exported exact. "x"
cases are common idioms added for this audit:

- x01: two stacked coaxial cylinders
- x02: a capsule
- x03: a rounded plate from 2 cubes and 4 corner cylinders
- x04: a groove whose axis lies on the top face
- x05: a counterbore flush with the top
- x06: a sphere on a cylinder

The table counts results that are valid with volume within 1e-3; in
brackets, also within 1e-6.

| Attribution tessellation | 15 cases | + x01–x06 |
|---|---|---|
| OpenSCAD's rule, defaults | 15/15 (12) | 5/6 (x02 invalid) |
| OpenSCAD's rule, `$fs=0.5` | 15/15 (12) | 5/6 (x03 invalid) |
| OpenSCAD's rule, `$fn=7` | 14/15 (c14 invalid) | 4/6 (x02, x03 invalid) |
| OpenSCAD's rule, `$fn=5`, `13` | 15/15 (12) | not run |
| OpenSCAD's rule, `$fn=64` | 15/15 (12) | 5/6 (x02 invalid) |
| **Aligned**, every resolution above | 21/21 together (18–19) | |

The 1e-6 misses that remain are c01, c02 and c13 (F3). x02 under the
aligned rule is valid, but its two tangent circles were split into 16
arcs each. The tangent-vertex test fires along a tangent *curve*, not
just at isolated tangent points. A pass that merges consecutive edges
lying on the same curve between the same faces fixes that (not done).

### 3.5 Fallback regions, fillets as CSG, timing, size

- **Faceted plus exact in one solid.** A `$fn=12` sphere treated as a
  polyhedron (one plane per triangle) minus an exact skewed cylinder
  (x07): valid, 66 planar faces, 1 cylindrical face, 16 exact ellipse
  edges. A faceted slab unioned with an exact cylinder (x08): valid. So
  the per-subtree fallback the previous audit asks for (its F3) falls out
  of this design: mesh-only subtrees are simply regions without a curved
  record.
- **Fillets built as CSG.** A cube minus a "corner block minus cylinder"
  tool (f01), and a plate with four filleted vertical edges and a hole
  (f02): invalid with OpenSCAD's rule at the defaults and at
  `$fs=0.5`, valid and exact with the aligned rule at both.
- **Speed and size against OCCT.** Spike timings are the median of 3; the
  OCCT boolean is one run.

  | Model | Triangles | Recon | Write | Our STEP | OCCT boolean | OCCT STEP |
  |---|---|---|---|---|---|---|
  | c07b plate, 400 countersunk holes | 33,612 | 40 ms | 6.3 ms | 1.09 MB | 0.88 s | 2.68 MB |
  | g02, 24 holes as `$fn=64` prisms (planar, kept as polygons) | 6,252 | 8.0 ms | 13 ms | 2.6 MB | — | 6.8 MB |
  | g03, the same holes as exact cylinders | 876 | 0.6 ms | 0.26 ms | 45 KB | — | 106 KB |

  All three read back valid with OCCT's volume. The spike's own Manifold
  stage is not representative: it folds 800 tools one at a time and takes
  4.4 s, where NeoSCAD renders c07b in 116–123 ms. A production export
  costs one render plus reconstruction (F7).

### 3.6 What path 1 does not yet cover

These were not spiked.

| Construct | How it would map | Risk |
|---|---|---|
| `linear_extrude` of exact 2D | Side faces are extrusion surfaces of the profile's curves (planes, cylinders, `SURFACE_OF_LINEAR_EXTRUSION` of B-splines for text). Needs 2D attribution (F6) | Medium |
| `rotate_extrude` | Planes, cylinders, cones, spheres, and tori (from arcs off-axis). Needs a torus record and torus pair rules | Medium |
| `offset(r)` arcs | 2D arcs → cylinders after extrusion; Clipper's Z channel | Medium |
| `text` | Glyph Béziers → exact B-spline profile (`crates/text` has them) → extrusion surfaces | Medium |
| `hull`, `minkowski`, `polyhedron`, `surface`, `import`, twisted extrude | Faceted fallback regions (x07 shows mixing works), reported per the previous audit's rules | Low |
| Non-uniform `scale`, `resize` | Quadrics stay quadrics under affine maps (cylinder → elliptic cylinder). STEP has no elliptic cylinder, so a B-spline surface or faceted fallback. Report it | Medium |
| General curved–curved intersections | B-spline fit to 1e-7 (c12). Branch selection near singular points is the fragile part | Medium |

## 4. Path 2: improving monstertruck or truck upstream

**State (retrieved 2026-10-07 with `gh`):**

- **truck** (`ricosjp/truck`, Apache-2.0, 1,581 stars, 39 open issues,
  pushed 2026-09-28). Commits since 2026-01-01 are almost all by one
  maintainer (149 under two spellings of his name, plus 38 by a `truck`
  identity). Four external PRs that touch booleans or STEP are open:
  - #110, the coplanar fix for #57: open 2026-01-30, 9 comments, 0 reviews;
  - #111, booleans on STEP geometry types: open 2026-02-05;
  - #129: open 2026-08-25;
  - #134, shapeops classification: open 2026-09-30.

  The #57 thread (open since 2024-02-06) records a contributor's
  diagnosis that "`intersection_curve::intersection_curves` only detects
  surface intersections, we may need a specific algorithm to handle
  coplanar faces … changing the algorithm might be tough".
- **monstertruck** (`virtualritz/monstertruck`, a fork of truck,
  Apache-2.0, 33 stars, pushed 2026-09-30). One maintainer (143 + 22
  commits since 2026-01-01), plus merged contributions from one other
  person (#13, #19). Its README says it "Reverted upstream's … boolean
  rewrite after bisect confirmed it regressed `punched_cube` and
  `adjacent_cubes_or`". Issue #24, "fails on first cube–sphere boolean
  (`EmptyOutputShell`)", was closed on 2026-08-27; this run still fails
  every cube–sphere case.

**Measured, monstertruck 0.4.1 from crates.io, same 15 trees** (the
spike's case table, built with `monstertruck_modeling::builder`):

| Tolerance | Passing | Failing, with the error |
|---|---|---|
| 0.05 | b01, b03, c10, c11, c14 | c01, c02, c13: "invalid output shell for `and`: no boundary shells"; c03, c06, c07, c08, c09, c12, c15: "This shell is not oriented and closed" |
| 0.01 | b01, b03, c10, c11, c12 | as above, plus c14; c06 fails at the 2nd hole with "no boundary shells" |
| 0.1 | b01, b03, c10, c11, c14 | as at 0.05 |

The idiom cases x02 (capsule) and x03 (rounded plate) failed at all
three tolerances. These tallies differ from the previous audit's (4/15:
it ran git master and its own case dimensions), so compare classes, not
counts. Volumes on success are from monstertruck's own tessellation and
were not compared.

**Diagnosis by failure class** (from the code above and the error
strings; this is inference, not a per-case trace):

1. **Coplanar or touching faces** (c03, c08, c09, x03). The mesh–mesh
   interference of coplanar triangles is an area, not a segment, so no
   consistent intersection loop forms. The face division then leaves an
   open shell. This is truck #57, still open.
2. **Spheres** (c01, c02, c13, x02). The sphere is a revolution surface
   with degenerate pole edges. Parameter search near poles and seams
   fails, and classification then removes every face ("no boundary
   shells").
3. **Intersections through existing edges and vertices** (the
   countersink rim lies near the top face; c06, c07). Endpoints are
   inserted with `add_polygon_vertex`, a parameter search, and a near
   miss leaves the loop unclosed.
4. **Tangency** (c15; c12 and c14 succeed or fail by tolerance). The
   interference is ill-conditioned and Newton refinement can converge to
   the wrong branch.

**Work estimate:** classes 1 and 3 need an imprinting stage for
overlapping faces and topological event handling (vertex on edge, edge on
face). That is the core of OCCT's General Fuse. Class 2 needs real
periodic-surface and pole handling throughout. Tangency needs certified
classification. Together that is a rewrite of the boolean core: roughly
25–50 person-weeks of specialist work (estimate), with no evidence it
converges. Upstream review capacity is the bottleneck: truck has left
boolean PRs unreviewed for 8 months. In monstertruck the work would be
accepted, but it would live in a one-maintainer fork.

**Fit:** WASM yes (truck built for wasm32 in the previous audit).
Determinism no: the previous audit saw truck's face order change between
identical runs. Fillets: monstertruck claims a fillet engine (README), not
exercised. As a NeoSCAD-org crate it would be a fork of a fork.

## 5. Path 3: a focused exact kernel of our own

**Scope:** planes, quadrics (cylinder, cone, sphere), tori, and extrusion
and revolution surfaces of exact 2D curves, with booleans computed on the
B-rep directly.

**What the literature says** (retrieved):

- ESOLID (Keyser, Culver, Foskey, Manocha, Krishnan, "ESOLID – A System
  for Exact Boundary Evaluation", ACM SMA 2002, doi:10.1145/566282.566289)
  is the closest prior system: exact Booleans on "low-degree curved
  solids". From the paper:
  - "Boolean operations are not supported for degenerate configurations …
    surfaces of different objects should not overlap" (p. 2);
  - "ESOLID cannot be considered a robust system, in terms of handling all
    possible input configurations" (p. 4);
  - "less than one order of magnitude slower in most cases and no more
    than two orders of magnitude slower in the worst case" than the
    inexact BOOLE (p. 1). Its Table 2 has one model at 633 s against
    6.7 s.

  OpenSCAD models are mostly degenerate configurations (coplanar floors,
  flush pockets, tangent fillets), which is exactly what ESOLID excludes.
- Exact intersection of two quadrics alone has a decade of work behind
  it. Lazard, Peñaranda and Petitjean ("Intersecting Quadrics: An
  Efficient and Exact Implementation", SoCG 2004) describe "the first
  complete, exact and efficient C++ implementation". It is built on GMP
  and LiDIA, with "less than 50 milliseconds" per pair at 10-digit
  coefficients. It is a single pair's curve, before any topology.
- Manifold avoids the problem for meshes with symbolic perturbation. The
  vendored port has a "Symbolic perturbation shadow predicate"
  (`vendor/manifold-rust/src/boolean3_kernels.rs:88`). Manifold's README
  says it is "fast with guaranteed manifold output" and "has IDs that make
  it easy to keep track of … what surfaces belong to what input objects
  or faces". Symbolic perturbation does not extend cleanly to curved
  surfaces, because perturbed quadrics no longer meet in the curves the
  user modelled.

**Robustness approaches, and what each costs here:**

- **Exact predicates with filtered floats.** Fine for planes; manifold-rust
  already has the arithmetic (`dashu`). Quadric–quadric–quadric vertices
  are algebraic numbers of degree up to 8, so exact comparison needs
  algebraic arithmetic, not rationals.
- **Interval arithmetic.** Certifies the easy cases and leaves exactly
  the degenerate ones (coplanar, tangent) undecided, which in OpenSCAD
  are the common ones.
- **Symbolic coincidence handling** (detect equal and tangent surfaces
  from the records, as path 1 does) is the useful part. It is cheap
  because the records are exact and few.

**Effort:** a production kernel of this kind is 60–120+ person-weeks
before fillets (estimate, by analogy with ESOLID's scope and OCCT's
General Fuse). Its risk is high, and its failure mode is the one
NeoSCAD's users hit most.

**Fit:** WASM and determinism are achievable by construction.
Performance would be slower than path 1, since every boolean is exact on
curved geometry. Maintenance would be the heaviest of the three.

## 6. Comparison

| | Path 1: mesh-guided reconstruction | Path 2: fix monstertruck/truck | Path 3: own exact kernel | OCCT (previous audit) |
|---|---|---|---|---|
| Validity on the 15 | **15/15 measured** (21/21 with idioms, aligned tessellation) | 5/15 measured today; 15/15 only after a core rewrite (estimate) | unknown; ESOLID-class systems exclude the degenerate cases | 15/15 measured |
| Exact-face fraction | 100% on the cases; faceted regions only for mesh-only constructs | 100% when it succeeds | 100% | 100% |
| Accuracy | ≤ 4.4e-8 rel. where the writer is complete; 3.1e-4 until pcurves land (F3) | tessellation-tolerance curves (previous audit: 0.03–0.1%) | exact | 1e-7 tolerances; c12 4.1e-6 off |
| Speed vs mesh path | render + 30–40% (c07b: +45 ms on 120 ms); a second render if aligned (F7) | slower than OCCT (previous audit) | 10–100× slower than inexact (ESOLID) | about 7× (c07b boolean alone); seconds to tessellate |
| WASM | yes (only manifold-rust) | yes | yes | separate 3–7 MB module, single-threaded |
| Determinism | byte-identical in the spike; inherits NeoSCAD's mesh determinism | no (face order varies) | achievable | yes in tests, with care |
| Fillets later | CSG tools + reconstruction for plane–plane/plane–cylinder; general fillets hard (section 7) | claimed by monstertruck | must be built | mature, with silent-failure caveats |
| Effort to STEP export, native + web | 12–20 pw | 25–50 pw + upstream dependence | 60–120+ pw | 11–17 pw native; +4–6 web |
| Main risk | mesh topology ≠ exact topology at near-degeneracies | never converging | never converging | C++ build, size, licence posture |
| Maintenance | ours, bounded (~5–10k lines expected) | fork of a fork | ours, large | upgrades of a large C++ dependency |

## 7. Fillets and chamfers on each path

**Path 1.** Rolling-ball fillets on the edges the owner names are
themselves CSG in this design:

- **plane–plane (straight edge):** a "block minus cylinder" tool, i.e.
  the `fillet` recipe NeoSCAD's MCP instructions already teach;
- **plane–cylinder, perpendicular (hole rims, boss bases):** a
  `rotate_extrude` of "square minus circle", whose surface is a torus;
- **plane–cylinder, parallel:** a cylinder;
- **chamfers:** wedges of planes and cones;
- **three equal-radius fillets meeting at a box corner:** a sphere
  octant.

Manifold does the booleans, and reconstruction gives the exact faces.
The fillet face is tangent to both neighbours, which is the F2 case, and
the measured fix (aligned tessellation) worked for f01 and f02. The
generator controls the tool's tessellation, so tangent lines can always be
placed on mesh vertices.

Needed beyond stage 1:

- torus records and their pair rules;
- the edge-selection language (previous audit, section 7);
- computing each tool from the exact B-rep edge (the B-rep gives the edge
  curve and its two face normals);
- a rule for vertices where fillets of different radii meet. Those
  patches are not quadrics and need B-spline surfaces or a fallback.

Variable-radius fillets and setbacks are out of reach of this approach.

**Path 2:** monstertruck's fillet engine sits on a boolean core that fails
on the inputs fillets create. **Path 3:** fillets are a second major
project on top of the kernel. **OCCT:** `BRepFilletAPI` works on simple
cases, fails loudly on some booleans, and once returned an invalid solid
reported as done (previous audit, F4).

## 8. Packaging as a standalone crate

Path 1 splits cleanly into a crate with no NeoSCAD dependency:

- **In:** a triangle mesh, a per-triangle surface ID and a surface table.
- **Out:** a B-rep (faces, loops, edges, vertices, curves) and STEP
  AP214/AP242 text.

Its dependencies would be `manifold-rust` (Apache-2.0, published), or none
if the caller supplies the mesh. A published crate needs:

- its own exact-surface, curve and pcurve types;
- the STEP writer;
- tolerance policy;
- a test suite that can run OCCT as an *optional, CI-only* oracle;
- a structural validator in Rust, so tests pass without OCCT. `step-p21`
  (Apache-2.0, `virtualritz/step-p21`) could parse the files back.

`truck-stepio` 0.3.0 (Apache-2.0) is not usable as the writer. It writes
truck's types only, and it has no `CYLINDRICAL_SURFACE`,
`CONICAL_SURFACE`, `PCURVE` or `SEAM_CURVE`: cylinders become
`SURFACE_OF_REVOLUTION` (its `src/out/geometry.rs` writes `PLANE`,
`SPHERICAL_SURFACE`, `TOROIDAL_SURFACE`, revolution and extrusion surfaces
and B-splines).

**Decision for the owner: licensing provenance.** NeoSCAD is
GPL-2.0-or-later. A `MIT OR Apache-2.0` crate must be written fresh, or
from code whose authors agree to dual-license it. The spike code (agent
written for this audit) qualifies. Code ported from NeoSCAD's GPL modules
would not, unless the copyright holder relicenses it.

## 9. Checked and found fine

- **Attribution survives booleans** at triangle level in manifold-rust,
  including coplanar and coincident cases (section 3.3). `same_face`
  includes `face_id` (`types.rs:456-460`).
- **Deterministic output** of the reconstruction and writer: byte-identical
  across runs and case orders. It uses no hash-ordered output, and the
  loop starts are chosen lexicographically.
- **The pure-Rust spike builds for `wasm32-unknown-unknown`.** It depends
  only on manifold-rust, which NeoSCAD's web core already ships.
- **Mixed faceted and exact regions** in one valid solid (x07, x08).
- **Merged coplanar and coaxial faces** read back valid in OCCT (c03,
  c08, c09, x01, x03).
- **OCCT reading files without pcurves** works: OCCT repairs them, at a
  tolerance cost (F3).
- **OCCT's own boolean is not the gold standard for curved–curved
  curves:** c12 is off the closed form by 4.1e-6.

## 10. Recommendation, first stage and stop rule

**Ranked:**

1. **Path 1 (recommended).** It reuses the robust part we already have
   (Manifold), and it is the only pure-Rust path with measured success on
   the 15 cases and common idioms. It is WASM-clean and deterministic, and
   its main risk (F2) has a measured mitigation.
2. **OCCT** as the fallback, per the owner's earlier decision: built in
   our CI, host-only.
3. **Path 2:** watch only.
4. **Path 3:** reject as a primary path. Borrow its ideas (exact
   classification of surface pairs) inside path 1.

**Effort, path 1** (person-weeks, wide bands):

| Stage | Scope | Estimate |
|---|---|---|
| 1 | Crate skeleton and export render: surface records for cube/cylinder/cone/sphere/`$fn` polygons, `face_id` plumbing that survives `render()`/`color()`, aligned attribution tessellation, reconstruction as spiked plus arc merging and analytic tangency detection, STEP writer with seams and pcurves, structural validator, mesh cross-check (volume of the B-rep tessellated on its exact surfaces against the render), faceted fallback with substitution reports, `-o x.step` behind `--enable exact`, determinism tests, OCCT oracle in CI only | 6–9 |
| 2 | Extrusions: 2D attribution via Clipper's Z channel (F6), `linear_extrude`/`rotate_extrude`, torus, `offset(r)`, text Béziers, corpus hardening | 5–8 |
| 3 | Apps, agent surface (`check`/`mcp` reporting), web export (no extra module: same core) | 2–4 |
| 4 | Fillets/chamfers as generated CSG tools on selected B-rep edges, edge selection language, corner spheres | 8–14 |

Stages 1–3 come to 13–21 person-weeks, and all four to 21–35. That is
comparable to OCCT's 11–17 for native export, but it includes the web
and has no C++ toolchain.

**First stage:** stage 1, with these gates:

1. **Corpora.** Export the conformance 3D models, the BOSL2 examples with
   no mesh-only construct (212 of 452 in the previous audit's sample), the
   benchmark models, and this audit's 15 + x + f cases at the 6
   resolutions of section 3.4.
2. **Validity oracle.** OCCT `BRepCheck_Analyzer` in CI, plus our own
   validator.
3. **Accuracy.** Volume within 1e-6 of a reference: OCCT's boolean, a
   closed form, or the B-rep tessellated on its exact surfaces.
4. **No silent errors.** Zero "valid but wrong" results. Any volume or
   bounding-box mismatch must become a reported error.
5. **Time.** Reconstruction and write within 50% of the render time on
   the benchmark models.

**Stop rule:** continue to stage 2 if at least 95% of eligible models
export valid, with gates 3 and 4 met. If fewer than 80% do after two
further weeks of fixes, stop path 1 and fall back to OCCT. Between 80% and
95%, the owner decides, with the failure classes listed.

**What would make me switch to OCCT after all:**

- Topology mismatches that aligned tessellation, analytic tangency
  placement and retry at a second resolution cannot fix, found at more
  than 5% of eligible corpus models.
- Fillets needing B-rep surgery the CSG-tool approach cannot express (for
  example variable radius, setbacks, or fillets across faceted regions),
  if the owner puts those in scope.
- A need to *import* STEP and edit it, which path 1 cannot do: it only
  writes what Manifold computed.

## 11. Not verified

- **Path 1 beyond the spiked primitives:** extrusions, `rotate_extrude`,
  tori, text, non-uniform scale, `offset(r)` (section 3.6).
- **Writing pcurves and seams.** The effect is inferred from c13 with an
  aligned sphere axis and from OCCT's own files. Not implemented.
- **Importers other than OCCT** (FreeCAD is OCCT; Fusion, Onshape and
  SolidWorks were not available). What a person should check is in the
  previous audit's section 5.
- **Analytic tangency detection** for tangencies that are not
  axis-aligned. Only the aligned tessellation was measured.
- **Per-case root causes for monstertruck.** They are inferred from the
  error strings and code, not traced.
- **ESOLID's speed claim** is from its paper (a 2002-era comparison).
- **Effort figures** are estimates from scope, not from a plan.
