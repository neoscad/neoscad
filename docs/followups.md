# Follow-ups

Deferred items found along the way, with where they came from. Remove an
entry when it is done.

## Performance
- Deep union trees and the level-4 Menger sponge render slower than the
  nightly (3.0 s vs 1.8 s and 1.3 s vs 0.7 s). Total CPU is the same, but
  the nightly spreads the work across cores better. The gap is structural:
  OpenSCAD's Manifold operators are lazy, so nested unions flatten into one
  `BatchUnion` over every leaf, whose pairwise rounds TBB runs in parallel;
  here each node is evaluated (and cached) eagerly, and manifold-rust runs
  each round's four booleans one after another. Running those rounds on
  rayon (tried in 5b, same output) gained only 5-8%, so it was dropped; a
  real fix needs lazy solids across cache boundaries. (5a, 5b)
- OFF export spends most of its time in `lang::number::fmt_g`: a
  1M-triangle `linear_extrude(twist=720, slices=2000)` takes 0.55 s, of
  which the geometry is 0.06 s (the nightly: 0.41 s and 0.12 s). (5b)
- manifold-rust's `triangulate` is slow on a shape with thousands of holes:
  `linear_extrude(5)` of a 71x71 grid of circles takes 1.0 s against the
  nightly's 0.7 s, nearly all of the difference in the cap triangulation.
  (5b)

## Parity
- `manifold-rust` 0.13.1 ports Manifold v3.5.0; OpenSCAD pins v3.5.2.
  `minkowski_sum` of two unit cubes gave volume 8.875 rather than 8; check
  it against the C++ library before relying on it in 5c. (5a)
- Faces with more than three vertices are split by ear clipping, where
  OpenSCAD uses libtess2 (`PolySetUtils.cc:152`). The surface is the same,
  but STL/OBJ bytes differ for quads. 2D shapes are not affected: with
  `USE_MANIFOLD_TRIANGULATOR` (on by default, `CMakeLists.txt:42`) OpenSCAD
  triangulates them with Manifold's `Triangulate` as neoscad does, and
  extrusion caps match the nightly byte for byte. (5a, 5b)
- Results of Manifold booleans can list the same triangles in a different
  order, or rotate a triangle's vertices, compared with the nightly (for
  example `rotate_extrude-tests.scad`, `issue1105.scad`); a few differ in
  vertex count (`example017.scad` assembled: 623 vs 626). The 2D shapes and
  extrusions going in are identical, so this is the kernel (manifold-rust
  v3.5.0 against v3.5.2, above). Images match. (5b)
- The nightly is built with Apple clang, which fuses multiply-adds (Eigen's
  matrix products become FMA chains). Extrusions and 2D transforms model
  this and match byte for byte; 3D transforms (`PolySet::transform`, and the
  matrix products in `eval`) use plain arithmetic, so transformed meshes can
  differ in the last bit (e.g. `rotate([30,40,50]) cube(1)` STL). Fusing
  `PolySet::transform` alone fixed a few coordinates, not all. (5b)
- OpenSCAD pins Clipper2 2.0.1 (submodule `c7f820f`); clipper2-rust 1.2.0
  ports 1.5.4. Every 2D case compared so far (booleans, sanitizing, all
  three offset joins, fill, projection) is byte-identical in SVG, but an
  engine change between the versions would show up here first. (5b)
- 2D `hull()` needs CGAL's `convex_hull_2` output order (start point and
  direction) to match exported SVG/DXF bytes. (5b)
- A render warning for a duplicated sibling subtree is printed once;
  OpenSCAD prints it again, because of how it caches. (5a)
- `--hardwarnings` is parsed but has no effect. Only tier 5 uses it.
  (2, 3, 5a)
- `r(3000)`-style recursion succeeds where the nightly stops with a
  recursion error. The deeper limit is deliberate, but decide whether a
  compatibility mode should match the nightly. (3, 5a)
- A `\r` inside `include<>`/`use<>` brackets doesn't count as a new line,
  as it does in OpenSCAD. (2)
- Malformed parameter-set JSON gives different error text from Boost. (2)

## Determinism
- manifold-rust's `Slice` starts each loop from a `HashSet` iteration, so
  the raw polygon order varies; `projection(cut=true)` output is canonical
  only because Clipper's union reorders it. (5b)
- manifold-rust's `compose_meshes` does not give each composed copy its own
  mesh IDs as C++ `Compose` does (`csg_tree.cpp:386-395`); `batch` in
  `manifold_geom.rs` renumbers colliding operands first. Report upstream,
  then drop the workaround. (5b)

## Structure
- The DXF reader lives in `crates/eval/src/dxf.rs` for `dxf_dim` and
  `dxf_cross`; move it to `io` when that crate exists. (3)
- The tier 3 baseline needs the pinned nightly installed as its renderer.
  CI would need it too. (5a)
