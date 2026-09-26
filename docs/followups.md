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
  (5a)
- Manifold's `MinkowskiSum` (C++ and the Rust port alike) is only right
  when the second operand contains the origin: it always unions the first
  operand, unmoved, into the result (`minkowski.cpp:84`,
  `composedHulls.push_back(a)`; manifold-rust `minkowski.rs`, the same
  statement).
  Repro for an upstream report:
  `Manifold::Cube({1,1,1}).MinkowskiSum(Manifold::Cube({1,1,1}).Translate({2,0,0}))`
  has volume 9; the sum is the cube [2,4]x[0,2]x[0,2], volume 8.
  manifold-rust gives 9 (checked); the C++ result is by reading the
  source, not run. The 5a note of 8.875 for two unit cubes did not
  reproduce (8, correct). neoscad does not use `MinkowskiSum`: OpenSCAD
  builds without `USE_MANIFOLD_MINKOWSKI` (`CMakeLists.txt:43`) and sums
  convex parts with hulls, which `geom::minkowski` ports. (5d)
- 3D `minkowski()` cuts non-convex operands into convex pieces with
  Manifold booleans rather than CGAL's `convex_decomposition_3`, and covers
  a solid with more than 48 reflex edges through its boundary instead. The
  solid is the same, but the mesh has more vertices than the nightly's
  (e.g. an L of two unioned cubes plus a 32-segment sphere: 926 vs 764;
  an L plus an L: 73 vs 35), because the pieces differ and the union keeps
  vertices on flat faces. Images match; exported bytes do not. (5d)
- 3D `hull()` matches the nightly's vertices and triangle order, but some
  triangles start at a different vertex (e.g. `hull() { cylinder(r=10,
  h=1); translate([0,0,10]) cube(5, center=true); }`: 11 of 190 OFF
  lines differ, all rotations). The QuickHull port's `build_mesh` reads
  the same as C++ `buildMesh`, so the rotation presumably comes from later
  in the kernel, like the boolean rotations above; not traced. (5d)
- The nightly prints CGAL's own diagnostics for some minkowski operands
  (Nef assertion failures for cubes touching at an edge or a vertex,
  `minkowski-cubes-touch-*.scad`, `issue1137.scad`); they are not
  reproduced. Messages that come from OpenSCAD itself ("Minkowski
  hard-crashed, falling back to Nef operation.", then the fallback's
  conversion warnings) are. (5d)
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
- SVG import: libsvg's arc math is not modelled with the nightly's fused
  multiply-adds, so an arc whose sweep lands within ~1e-6 of a step
  boundary gets one step fewer (`spec-paths-arcs01.svg`: 15 vs 16 steps at
  180 degrees). The other 110 SVG import cases compared are byte for byte
  the same; the images match. (5c)
- 3MF: lib3mf's error texts for malformed files are only reproduced for a
  missing file, a non-ZIP and an empty file; other problems get our own
  description in OpenSCAD's frame. Exported object and build UUIDs are
  hashes of the content rather than random, so files are reproducible.
  Only the default export options are implemented (`-O export-3mf/...`
  is not parsed). (5c)
- libxml prints its own parser diagnostics to stderr for a broken SVG
  (`file:1: parser error : ...`) before OpenSCAD's "Error parsing file";
  only the latter is reproduced. Likewise the lines libsvg writes to
  stdout (an invalid transform, a `<use>` href that is not `#id`). (5c)
- OBJ: `f 1  2 3` (two separators in a row) crashes OpenSCAD with an
  uncaught `bad_lexical_cast`; the empty word is skipped here. (5c)
- `import()` of `.nef3` needs CGAL's Nef reader and still reports
  "import() is not implemented"; only preview tests (tier 4) use it. The
  experimental `import()` function (JSON) is not implemented either. (5c)

## Determinism
- manifold-rust's `Slice` starts each loop from a `HashSet` iteration, so
  the raw polygon order varies; `projection(cut=true)` output is canonical
  only because Clipper's union reorders it. (5b)
- manifold-rust's `compose_meshes` does not give each composed copy its own
  mesh IDs as C++ `Compose` does (`csg_tree.cpp:386-395`); `batch` in
  `manifold_geom.rs` renumbers colliding operands first. Report upstream,
  then drop the workaround. (5b)

## Structure
- File access is not all behind `lang::loader::FileSystem` yet, so it
  breaks under WASM: `dxf_dim()`/`dxf_cross()` read with `std::fs` in the
  evaluator, and the cache keys (`eval::dump`) stat imported files for
  their mtime and size with `std::fs`. The trait needs a `metadata` call
  and `Keys::new` a file system. (5c)
- `docs/architecture.md` still lists `usvg` for SVG; 5c ported OpenSCAD's
  `libsvg` instead (see `crates/io/src/svg/mod.rs` for why). (5c)
- The tier 3 baseline needs the pinned nightly installed as its renderer.
  CI would need it too. (5a)
