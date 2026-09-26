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

- Many `text()` nodes side by side render slower than the nightly: 200
  lines of 125 characters take 2.25 s against 1.05 s (30 lines: 0.36 s
  against 0.26 s), with byte-identical SVGs. Shaping and outlines are not
  the cost (one `text()` of 3,750 glyphs takes 0.70 s in both); nearly
  all of it is the single-threaded top-level 2D union of the 200 results
  in clipper2-rust's `execute_internal`. (5e)

## Fonts
- Fontconfig's system configuration is not consulted, so names the
  nightly resolves to installed system fonts render in the matching
  Liberation font instead (on this Mac `Arial`, `Helvetica`, `Courier
  New` and `Times New Roman` are system fonts for the nightly; here they
  are the metric-compatible Liberation Sans, Mono and Serif). No test
  depends on it. (5e)
- `use <font.ttf>` registers the font (in the CLI, from the programs'
  `uses`), but `lang` still also treats the file as a library and parses
  the font as OpenSCAD source, and a missing font file does not print
  OpenSCAD's "Can't read font with path '...'" error
  (`SourceFile::registerUse`). Both belong in `lang`. (5e)
- The font-name matcher (`crates/text/src/pattern.rs`) ranks on charset,
  family, style, slant, weight and width. It leaves out fontconfig's
  language coverage and every value after the first for weight, slant and
  width, and matches a weight range by its midpoint. Every font name in
  the test suite resolves as in the nightly. (5e)
- The experimental `textmetrics()` and `fontmetrics()` functions can now
  be built on the `text` crate (`TextMetrics`/`FontMetrics` in
  `FreetypeRenderer.cc` use the same shaping). (5e)
- Cubic glyph segments (CFF fonts) are flattened with `powf(3.0)` like
  the C++ `std::pow`; that matches on macOS because both call the system
  libm, but a WASM libm may round a cube differently in the last bit. No
  test font is CFF. (5e)

## Parity
- `manifold-rust` 0.13.1 ports Manifold v3.5.0; OpenSCAD pins v3.5.2.
  (5a)
- `vendor/manifold-rust` patches `collapse_edge`, whose clean-up after a
  boolean could slide a vertex across a crease and fill a concave corner
  (BOSL2 `cubetruss`, 7.3 mm³ too much; `vendor/README.md`). C++ Manifold
  3.5.2 fails the same way. Report it to Manifold and manifold-rust with
  the 35- and 28-vertex operands in `crates/geom/tests/data/collapse-crease-*.txt`
  (union 328.29 instead of 314.49), then drop the copy. Separately, C++
  3.5.2 built by hand (`-O2 -ffp-contract=off`, no TBB) crashed with
  SIGSEGV intersecting the larger cubetruss operand with some boxes; not
  investigated. (H1)
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
- QuickHull (Manifold C++ and manifold-rust) sometimes returns a folded,
  non-convex hull; `geom::hull::hull_3d` checks every 3D hull, including
  minkowski's, and rebuilds a folded one (H1). About 1 in 30 rounded-box
  hulls and minkowski sums fold. Where the nightly's own hull folds (e.g.
  `hull() for (x=[0,30], y=[0,30], z=[0,5]) translate([x,y,z])
  sphere(r=3, $fn=16);`, 13453.12 against CGAL's 13453.41) neoscad now
  differs from it, correctly. Report upstream to Manifold and
  manifold-rust with `minkowski(){cube([30,20,5],center=true);
  sphere(3,$fn=48);}` (9751.29 instead of 9751.42). (H1)
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
- Fused multiply-adds follow the platform, as upstream does: OpenSCAD's
  arm64 build rounds `a * b + c` in one expression as an FMA and its
  x86_64 build does not (`[1, 0.1] * [-0.010000000000000002, 0.1]` is
  `-8.32667e-19` on the arm64 nightly, `0` on its x86_64 slice). neoscad
  routes every such site through `eval::fma` (`mul_add`, fused only on
  `aarch64`): vector and matrix products, `norm`, `cross`, `lookup`, range
  iteration, `rands`, the `rotate([x,y,z])` and `mirror` matrices, the DXF
  dimension functions, and in `geom` the extrusion and 2D transforms and
  the hull orientation test. With it, BOSL2's tests match the arm64
  nightly as `.csg` on 976/976 and its examples on 2,525/2,526 (the other
  uses unseeded `rands()`). Still plain: 3D transforms
  (`PolySet::transform`), so transformed meshes can differ in the last
  bit (e.g. `rotate([30,40,50]) cube(1)` STL); fusing `PolySet::transform`
  alone fixed a few coordinates, not all. A new port of C++ arithmetic
  should use `eval::fma` where the C++ multiplies and adds in one
  expression; which product clang fuses has to be checked against the
  nightly (`rotate` fuses the first, `mirror`'s `x*x + y*y + z*z` is
  `fma(z, z, fma(x, x, y*y))`). (5b, H1)
- `lang::number::fmt_g` prints a negative NaN as `-nan`, as glibc's
  `printf` does; macOS's `printf`, and so the nightly, always prints `nan`.
  The `.csg` export now drops the sign itself (`eval::dump`), since the
  fused `rotate` matrix made `transform-nan-inf-tests.scad` carry a
  negative NaN; the other `fmt_g` callers (the OFF, OBJ, WRL, DXF and SVG
  writers in `io`) still print `-nan`. Decide which platform to match.
  (H1)
- OpenSCAD pins Clipper2 2.0.1 (submodule `c7f820f`); clipper2-rust 1.2.0
  ports 1.5.4. Every 2D case compared so far (booleans, sanitizing, all
  three offset joins, fill, projection) is byte-identical in SVG, but an
  engine change between the versions would show up here first. (5b)
- A render warning for a duplicated sibling subtree is printed once;
  OpenSCAD prints it again, because of how it caches. (5a)
- `--hardwarnings` stops at the first warning, but a geometry warning is
  only acted on after the whole geometry is built (the output is the
  same; the time is not), and an evaluator warning after its check point
  rather than at its throw (messages in between are dropped). Against
  the nightly with `--hardwarnings`, the output and exit code match on
  259 of 263 test inputs as `.echo` (the 4 others are recursion-limit
  depths; see `eval::recursion` for the policy) and 235 of 237 as `.stl`
  (the 2 others differed without the flag too, in geometry error paths,
  which H2 fixed; not rechecked with the flag).
  Warnings OpenSCAD prints inside a `catch` never raise it; the CLI knows
  them by text (`printed_in_handler` in `crates/cli/src/run.rs`), and the
  DXF ones printed by `dxf_dim()`/`dxf_cross()` in the evaluator are not
  exempted. (5f)
- Upstream's `export-param-hardwarnings` and
  `export-paramset-hardwarnings` tests pass without `--hardwarnings`
  doing anything: `shouldfail.py` appends `--export-format=json` to
  arguments that already hold `--export-format param`, and Boost rejects
  the repeated option with exit 1. neoscad now exits 1 on usage errors as
  OpenSCAD does (clap's default was 2), so they pass for the same reason;
  `--hardwarnings` with a param export is checked against the nightly by
  hand (identical). Worth reporting upstream. (5f)
- PDF export draws what Cairo draws (checked by image on all six test
  PDFs, rasterised with poppler), but the file is hand-built: labels use
  the standard Helvetica font unembedded where Cairo embeds Liberation
  Sans (same metrics, slightly different glyphs), and Cairo's object
  layout and compression are not reproduced. Cairo's path simplification
  is modelled only as far as its single-rectangle `re` output. (5f)
- `-O` options are parsed for SVG and PDF only; `export-3mf/...` is still
  ignored (see 3MF below). (5f)
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

## WASM
- Recursion on wasm32 stops at a frame budget calibrated for V8's default
  stack in node 18 (`eval::recursion`): function depth 498 and module
  depth 249 for the simplest recursions, against 52,417 and 13,046
  natively and the nightly's 9,190 and 7,043. Without the budget V8
  overflows at 1,076 and 527. Raising it needs smaller wasm frames: per
  level, rendering's walk over the node tree costs about four times an
  expression's stack, and list comprehensions twice. Only node 18 was
  measured; browsers (and workers, which may have less stack) are
  unverified. (H2)
- Operations over a whole value other than printing and freeing it
  (comparing with `==` or `<`, `ops::equals` and the ordering) recurse
  once per level of vector nesting. Nesting deeper than the budget can only be built by tail
  recursion (`f(n, acc) = ... f(n - 1, [acc])`), and comparing such a
  value can overflow a WASM engine's stack; natively it needs a far deeper
  value, and the nightly crashes even on `len()` of one. (H2)
- A wasm32 build must be linked with `-C link-arg=-zstack-size=8388608`
  (`eval::recursion::WASM_STACK_SIZE`; `crates/wasm-check/build.rs` does
  this). With rustc's default 1 MiB, recursion stops earlier, still
  cleanly. The release `wasm_check.wasm` is 38 MB, of which all but
  9.0 MB are DWARF line tables (the release profile keeps them); 4.3 MB
  of the rest is the bundled fonts and MCAD. (H2)
- Rust's wasm32 maths functions differ from macOS libm in the last bit
  (engine milestone audit, finding 6.5), so WASM output is not
  byte-identical to native. Decide whether to accept that or use one libm
  everywhere. (H2)

## Determinism
- manifold-rust's `Slice` starts each loop from a `HashSet` iteration, so
  the raw polygon order varies; `projection(cut=true)` output is canonical
  only because Clipper's union reorders it. (5b)
- manifold-rust's `compose_meshes` does not give each composed copy its own
  mesh IDs as C++ `Compose` does (`csg_tree.cpp:386-395`); `batch` in
  `manifold_geom.rs` renumbers colliding operands first. Report upstream,
  then drop the workaround. (5b)

## Structure
- `docs/architecture.md` lists text under `geom` and A5 proposed `fontdb`
  for font discovery; 5e added a `text` crate (the evaluator's
  `textmetrics()` needs shaping and sits below `geom`) with its own small
  font index instead of `fontdb`. (5e)
- `docs/architecture.md` still lists `usvg` for SVG; 5c ported OpenSCAD's
  `libsvg` instead (see `crates/io/src/svg/mod.rs` for why). (5c)
- The tier 3 baseline needs the pinned nightly installed as its renderer.
  CI would need it too. (5a)
- The six PDF cases need a PDF rasteriser (Ghostscript or poppler), which
  CI would need too. (5f)
