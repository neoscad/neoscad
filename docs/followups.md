# Follow-ups

Deferred items found along the way, with where they came from. Remove an
entry when it is done.

## Performance
- CLI cold start is about 0.7 ms lower with the GPU frameworks linked
  delay-init (`crates/cli/build.rs`), but not yet at the audit's 3 ms
  target. What is left: the first rayon use starts the whole global pool
  (about 0.27 ms of a `cube(1)` export; `RAYON_NUM_THREADS=1` removes it),
  which a small model never needs; the delay-init frameworks are still
  mapped and bound (about 0.3 ms, measured on a C program linking the
  same ones), which only a `dlopen`ed renderer or a helper binary would
  save; and mimalloc's start-up, about 0.1-0.2 ms. (O1, R2)
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

- Resolved variable lookups (O4, `crates/eval/src/resolve.rs`) still walk
  the context chain, comparing each context's region with the reference's
  candidates, rather than hopping a fixed (depth, slot). Fixed addressing
  needs a static frame layout, and four things get in the way: a scope
  assignment not yet made falls through to an outer binding, a builtin
  that binds (`intersection_for`) can be redefined by a user module, a
  C-style `for` has two iteration contexts in the chain while it
  increments, and function literals capture whatever chain they were made
  in. In the hero, 17.5M of 25M single-candidate lookups stop at the first
  context, but 2.2M walk 7 or 8. (O4)
- Each evaluation that resolves a function body or literal scans every
  expression of every unit once for named-argument names
  (`resolve::named_arguments`, about 0.25-0.4 ms over BOSL2), because a
  named argument that is not a parameter binds in any callee. Lowering
  could record the names with the program (and its fragments), so the
  session's edit loop would not rescan them. (O4)
- Keeping up to three slots inline in each context, to save the slot
  vector's allocation per call, `let` and loop iteration, measured 2-4%
  slower on the BOSL2 models: every context grows. Worth retrying with a
  smaller `Value` or a slab of contexts. (O4)
- The wasm32 frame budget's calibration in `crates/eval/src/recursion.rs`
  (budget depths at most 63% of where V8 overflows) is stale: at 9b89400
  `module-children` reaches 206 of V8's 214 and `function-lc` 199 of 326
  (`scripts/wasm-check.sh --depths --all-programs`, with and without
  `--frames=4000000000`). After O4, V8 overflows `function-lc` at 353
  and `module-children` still at 214. (O4)

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
- The render summary (stderr and `--summary-file`) has no geometry cache
  size: `geom::Rendered` reports only the entry count, so the stderr
  summary leaves out OpenSCAD's `Geometry cache size in bytes` line and
  the two `CGAL ...` lines after it, and the JSON has `bytes` and
  `max_size` null (`docs/cli-json.md`). `geom::Renderer::stats()` now
  reports the byte total and budget (7a), so both can be filled; the
  summary's text and JSON have not been changed yet. (H3)
- `-d` lists dependencies in first-seen order where OpenSCAD uses hash
  order (same set). Files read by `dxf_dim()`/`dxf_cross()` are not
  listed, and `-m` does not run for them: the evaluator reads them
  itself. (H3)
- `.ast` export does not evaluate the program, so it prints no `ECHO:` or
  evaluation warnings; the nightly evaluates first (`do_export`) and
  prints them (`echo(1);`: `ECHO: 1` on stderr). The `.ast` file is the
  same. (H3)
- `--animate` with several `-o` files evaluates each frame once for all
  of them; OpenSCAD runs all frames for the first output, then all for
  the next, so the `Exporting ...` lines come in a different order. The
  files are the same. (H3)
- `--debug` prints OpenSCAD's `Debug on.` line and nothing more; neoscad
  has no `PRINTDB` output. (H3)
- OFF export writes no colour for a face whose colour is invalid
  (`color()` with no arguments), where the nightly writes `0 0 0 0`
  (`export_off.cc:72-74`); the warnings match. Matching the bytes would
  make tier 3's `render-manifold_issue5216` fail as it does with the
  nightly (the re-import draws the face transparent), so the harness
  limit for it was dropped instead and `conformance run --binary
  <nightly>` now reports that case as a failure. (H3)
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
  (5c) Every `-O export-3mf/...` option is implemented; against the
  nightly, 55 option and model combinations give the same model XML apart
  from UUIDs, the date and triangle order, which follows the tessellation
  differences above. (H3)
- libxml prints its own parser diagnostics to stderr for a broken SVG
  (`file:1: parser error : ...`) before OpenSCAD's "Error parsing file";
  only the latter is reproduced. Likewise the lines libsvg writes to
  stdout (an invalid transform, a `<use>` href that is not `#id`). (5c)
- OBJ: `f 1  2 3` (two separators in a row) crashes OpenSCAD with an
  uncaught `bad_lexical_cast`; the empty word is skipped here. (5c)
- `import()` of `.nef3` needs CGAL's Nef reader and still reports
  "import() is not implemented"; only preview tests (tier 4) use it. The
  experimental `import()` function (JSON) is not implemented either. (5c)

## Rendering
- Previews draw each CSG product's visible surface from real Manifold
  booleans (`geom::csg::product_meshes`), not OpenCSG's image-space CSG,
  so image-space artefacts are not reproduced: z-fighting where a
  positive and a negative face are coplanar, holes from a `convexity` set
  too low, and whatever OpenCSG makes of a mesh that is not a closed
  solid. `preview-manifold_polyhedron-tests` fails on the last: OpenCSG
  draws a subtraction from an inside-out octahedron as nothing and one
  with a single flipped face as a partial shape, while Manifold repairs
  both. (6b)
- A preview's `#` objects are drawn with a small depth offset
  (`DrawState::bias`, constant -2, slope -0.5) so that they show on the
  cut faces they make, whose triangles the boolean re-split (OpenCSG
  compares the very same triangles there). The values pass every
  highlight case; a `#` object within that offset behind a surface would
  show through it. (6b)
- `--view edges` in render mode splits quads along other diagonals than
  OpenSCAD's libtess2 (`PolySetUtils::tessellate_faces`), and a Manifold
  result's triangles follow manifold-rust's triangulation, so interior
  edge lines differ: `render-view-edges-manifold_cube10` and both
  `*-view-edges-manifold_render-preserve-colors` fail on those lines
  alone. Same root as "Faces with more than three vertices" under
  Parity. (6b)
- The preview of a model with a `.nef3` import fails like its render
  (`import()` of `.nef3` is not implemented): the two
  `preview-manifold_nef3_*` cases. (6b)
- With `--csglimit` exceeded, OpenSCAD's preview draws nothing (the
  normaliser gives up on the whole term), and so does neoscad's. For the
  GUI and snapshots the real boolean of the unnormalised term would be a
  better fallback; not done, to stay with OpenSCAD. (6b)
- The render summary after a PNG preview reports 0 geometry cache
  entries (`CsgTree::build` does not return the renderer's count). (6b)
- `.term` export still prints "No top-level CSG object"; `geom::csg`
  now builds OpenSCAD's CSG terms, so `CSGNode::dump` could be ported on
  top of it. (6b)
- Preview speed (best of 3, wall, this machine): at most `--render`'s
  time on the benchmark models, and 1.5-4.5x faster than the nightly's
  OpenCSG preview, except `csg_spheres` (380 ms against the nightly's
  273: one product of a cube minus 125 spheres is one big boolean,
  which OpenCSG never computes) and `text_30lines` (550 against 486, as
  in render mode). (6b)
- PNG export needs a GPU adapter (Metal, Vulkan, Direct3D 12). Without
  one it fails with "no GPU adapter"; a headless Linux CI runner would
  need a software Vulkan driver (lavapipe), or neoscad a CPU rasteriser.
  The PNG tests in `crates/render/tests/offscreen.rs` skip without one.
  (6a)
- Determinism: the same scene gives the same PNG bytes on one machine
  (checked with two devices on one GPU in `offscreen.rs`), but
  rasterisation rules differ between GPUs and drivers at the pixel level
  (edge pixels, depth ties between coplanar faces). Only Metal on an
  Apple M4 Pro has been measured: 318 of 320 render-mode images pass
  `image_compare` against OpenSCAD's goldens, 206 pixel-identical. Other
  GPUs are unverified. (6a)
- The first PNG export after a reboot or driver update pays for Metal's
  shader compilation (about 0.5 s on this machine; the system caches it
  after that, and a warm export costs about 18 ms over the geometry). (6a)
- The two render-mode images that fail both tier 4 rules,
  `render-manifold_issue964` and `issue1061`, are polyhedra with
  non-planar quads: `PolySet::tessellate` ear-clips them along other
  diagonals than OpenSCAD's libtess2 (see "Faces with more than three
  vertices" under Parity), so the shading of those faces differs. The
  renderer draws what `geom` hands it. Their preview and throwntogether
  cases fail the same way. (6a, 6b)
- Colour schemes are only the built-in and vendored ones; OpenSCAD also
  reads `color-schemes/render/*.json` from the user's configuration
  directory. The app can pass such files to `render::scheme::parse`. (6a)
- OpenSCAD's `PolySetRenderer` draws nothing (and logs an error) for a
  result holding both 3D and 2D parts; `geom` never returns such a
  result, so the case is not handled. (6a)

## Serve and session
- Statement reuse across edits (O3, `crates/eval/src/memo.rs`) keys each
  top-level statement on the names it mentions, followed through
  top-level definitions by name alone. A local binder that shares a
  top-level variable's name (BOSL2's `mod`, `base`, `r` parameters) makes
  that variable an input of every statement reaching the code, so editing
  it re-evaluates more than it must. The resolver (`eval::resolve`) knows
  which references a lexical binder captures; using it would narrow the
  key. Two more limits of the first version: top-level assignments always
  run again (in the hero, its `planetary_gears()` call is part of the
  32 ms a carrier edit still evaluates), and a statement is the unit of
  reuse, so an edit inside the hero's plinth statement still costs the
  whole isosurface.
- A one-line edit re-parses the main file. Since `4d877c7` an include
  between top-level statements of a file that parses on its own
  (`include <BOSL2/std.scad>`) is parsed and lowered once and spliced
  into each new program (`lang::fragment`, the session's
  `FragmentStore`); with the evaluator work since, the served BOSL2
  edit (`edit_loop`) takes about 18 ms, from 34.
  What is still redone on every edit: the main file's own parse; the
  splice, which copies each fragment's tokens and syntax tree into the
  new program (`Cst::splice`) and renumbers a copy of its AST, rather
  than sharing them; every include that is not a whole top-level unit
  (inside a module body or an expression, mid-statement, after a syntax
  error, or of a file with errors of its own), which is spliced as
  tokens and parsed again, as before; and whole-program evaluation,
  which is now most of an edit.
  One-shot command-line runs parse everything, as there is nothing to
  reuse. An incremental parser for the main file is not started. (7a,
  `4d877c7`)
- The command line's own export path (`crates/cli/src/run.rs`) does not
  go through `session::Session`; the session re-implements its steps for
  served exports and shares only the encoder (`session::export`). That
  served and direct runs agree is checked by
  `crates/cli/tests/serve.rs` (files and stderr for 3D and 2D formats,
  warnings, errors, `--format json` and snapshots), not guaranteed by
  construction (PNG exports: the server renders on the session and draws
  with the command line's `png` module). Moving `run.rs` onto the
  session would also bring echo, AST, CSG and param exports, `--animate`
  and `--hardwarnings` to the server, which run in-process today. (7a)
- A served run's render summary reports the server's `Geometries in
  cache` count and times, which differ from a fresh process's. (7a)
- Cancellation (and the time limit) stops the evaluator at the next
  call or loop iteration, the geometry evaluator before the next node,
  and primitives and extrusions at their next ring or slice; one long
  kernel operation (a big boolean, a hull, minkowski) runs to its end.
  `neoscad mcp` exits 2 s after the end of input whatever is running.
  (7a, H4)
- The memory limit is an estimate kept at the allocation-heavy points
  (`eval::limits`), not a measurement: the evaluator's large values,
  nodes and messages, and the geometry results a render holds until
  their parents use them. A kernel operation's own working memory (a
  boolean's intermediate meshes), the geometry cache (its own budget),
  the check's rays and the snapshot's drawing are not counted; the
  triangle limit bounds their inputs. Geometry results are weighted 6x
  their cache cost for the kernel's working copies, calibrated on the
  benchmark models; still, BOSL2's fractal_tree peaks at 1.96 GB real
  against under 512 MiB estimated (its evaluation alone is 451 MB). A
  host-side RSS probe (Linux `/proc/self/statm`; macOS needs
  `task_info`, i.e. `unsafe` or a crate) would make the limit real. A
  process-wide budget for the app's several documents (one
  memory-pressure hook) is still to do. (H4)
- When several parallel geometry siblings pass a count limit, the
  earliest in the source that recorded one is reported; a sibling that
  stopped (on the others' trip) before its own check never records, so
  which one is named can vary between runs. One over-limit node is
  reported the same every time (`crates/session/tests/session.rs`). The
  time and memory limits depend on timing by nature. (H4)
- The one-shot command line applies `--limit` to evaluation and mesh
  and PNG geometry; `neoscad check`, `measure`, `snapshot` and `test`
  have no `--limit` (they are unlimited, as the command line is). (H4)
- The release profile unwinds so the server can answer a panicking
  request with -32603 and carry on (7b-1). That costs the one-shot
  command line 5-8% on evaluation-bound BOSL2 models (fractal_tree
  5.87 -> 6.33 s best of 5) and 2 MB of binary; cold start and
  geometry-bound models are unchanged. Cargo cannot set `panic` per
  binary; a separate `abort` profile for the benchmarked one-shot binary,
  or finding what in the evaluator unwinding tables slow down, would win
  it back. (7b-1)
- The session keeps up to four renderers (one per colour scheme and
  font set in use, since geometry keys include neither), each with its
  own geometry budget, so the worst case is four budgets. (7a)
- Unix sockets only; a Windows server would need a named pipe. The
  default socket path must fit `SUN_LEN` (104 bytes on macOS); a long
  `$TMPDIR` or `$XDG_RUNTIME_DIR` would need `NEOSCAD_SOCKET`. (7a)
- `progress` notifications report stages, not fractions of the work.
  (7a)
- Fix hints are a table by code plus "did you mean" over the program's
  and OpenSCAD's builtin names (`session::diag`, the builtin list copied
  from the reference's `Builtins::init` registrations). Scoped names
  (a module's parameters and local variables) are not candidates. (7a)
- The snapshot headlight's direction, ambient and diffuse terms were
  chosen by eye on a bracket (`render::Lighting::Headlight`); no test
  pins its images. (7a)
- Cached geometry replays its messages in a warm render only when the
  pattern of first occurrences below it is the same as when it was
  computed; otherwise the node is computed again from its children's
  cached results (`geom::RenderOptions::replay`). Correct, but a cached
  subtree whose earlier twin was edited away is recomputed once. (7a)
- A geometry cache hit is used only if the fragment, slice and triangle
  counts its subtree asked for are within the request's limits
  (`geom::evaluate`'s `Demand`), and a document's last product is reused
  only under the same limits; otherwise the node is computed again and
  refused where a cold render refuses it, so lowering the limits cannot
  let a warm cache pass a model a cold one stops. Results computed
  without limits have no recorded demand and are computed again once
  under limits. Memory and time are not re-checked on a hit (it
  allocates and takes nothing). (8f)

## Parts, check and measure
- A part's solid is its subtree's geometry: a part under a `difference()`
  that cuts it is measured uncut (its `context` is only set for the
  operations that change a part as a whole: subtracting it,
  intersecting, hull, minkowski, resize, 2D). Measuring "what of the
  model belongs to the part" would need the model's faces by part plus
  closing the cut. (7b-1)
- Face attribution survives booleans, transforms and `color()` (over
  several parts the IDs are kept instead of collapsed, which changes
  only how an export groups triangles); `hull()`, `minkowski()` and
  2D operations make new solids and drop the parts inside them. (7b-1)
- Wall thickness is sampled along face normals from fixed points per
  face (up to 16 on large faces, at most 400,000 rays); a wall whose
  sides are not parallel measures thicker than its narrowest point, and
  a narrow feature in the middle of a big face between samples can be
  missed. A medial-axis or sphere-probe estimate would be exact. Knife
  edges formed by two faces sharing a corner are skipped; the feather
  edges a `difference()` leaves where a curved cut meets a face are
  reported (correctly thin, but many). (7b-1)
- Overhangs do not recognise bridges (a flat span supported at both
  ends); they are reported as overhangs. (7b-1)
- Checks run serially (about 150 ms for a 220k-triangle model). Rays are
  independent, so they could run on rayon with a deterministic merge.
  (7b-1)
- `snapshot --issues` uses the default check settings from the command
  line (the server's `issues` takes any). Markers are drawn whether or
  not the model hides the point from that view. (7b-1)
- `measure --section` cuts the model or one part; a per-part breakdown
  of a model section is not reported. (7b-1)

## Tooling: fmt, test, docs
- `session::diag`'s "did you mean" pools are hand-copied lists of
  OpenSCAD's builtin modules and functions; `eval::builtins()` (7b-2)
  now lists the evaluator's own tables and could replace them. (7b-2)
- `neoscad fmt` keeps what OpenSCAD's customizer reads at the top of a
  file (before the first `{`): there an assignment with a trailing `//`
  comment is never wrapped (it can run past the width), assignments
  sharing a line keep sharing it, and indented `//` comments keep their
  indent. A narrower rule (only lines whose annotations would change)
  would format more of those headers. (7b-2)
- Formatter layout limits: binary chains break all or nothing (no
  filling); only a lone vector argument hugs its parentheses (no
  "last argument" hugging of a trailing vector or function literal); a
  `//` comment inside an expression ends the line there, and block
  comments are kept verbatim, not re-indented; blank lines are kept
  between list items as between statements. (7b-2)
- `neoscad fmt` refuses files that need `--enable` to parse (the
  unicode-identifier tests); it has no `--enable`. It rewrites files in
  place (no temporary file and rename) and never goes through a running
  server. (7b-2)
- `neoscad test` runs in-process; unlike `check` and `measure` it does
  not hand its work to a running `neoscad serve` (a `cli.test`), so a
  command-line run starts cold. Each test re-parses its file (a test's
  program is changed, so it skips the parse cache; included files still
  come from the lex cache). (7b-2)
- `@expect parts` checks that the named parts exist, not that they are
  the only ones; there are no expectations on echo output (tests use
  `assert()`), on 2D contour counts, or on `measure --between`
  distances. (7b-2)
- `neoscad docs --in` prints a user definition's parameters as the
  `.ast` dump does (`r = 1`), builtins as written in `builtins.toml`
  (`r=1`); it follows `use`d libraries one level, not the libraries they
  use. Experimental builtins (`roof`, `textmetrics`, ...) have no
  entries, only a note that they are not enabled. (7b-2)

## MCP and the agent eval
- `neoscad mcp` implements MCP 2026-07-28 statelessly plus the legacy
  `initialize` handshake, and only the core: no `subscriptions/listen`,
  no progress notifications (a long render sends nothing until it
  ends), no logging, no MRTR (`input_required`), no completions. The
  client's `roots` capability is not read either: the roots are the
  working directory and `--root`s given at start. (7c)
- Claude Code (2.1.283) shows the model the JSON of `structuredContent`
  instead of the text summary when a result has both, so the text is
  what other clients see. If a client shows both, a result costs about
  twice its tokens; a flag to send only one would fix that. (7c)
- Inline `source` is one document per `base_dir` (`inline.scad`), so
  inline calls take turns rather than running in parallel, and while
  one runs it shadows a real `inline.scad` in that directory. (7c)
- `notifications/cancelled` does not reach an MCP `snapshot` call: the
  tool calls the session directly, not through the server's request
  table that `$/cancelRequest` looks up. The end of input still stops
  it (`Session::cancel_all`). (H4)
- `neoscad serve --socket PATH` in a shared directory binds and then
  makes the socket 0600, a short window harmless under the default
  umask 022 (connecting needs write permission); binding under a
  tightened umask needs nix's `fs` feature. (H4)
- The snapshot sheet's header line runs under the legend at the MCP
  default size (768 px) when `issues` adds check counts. (7c)
- Tool-description token counts are estimates from byte counts (6,069
  bytes of compact JSON as a client receives the list, 5,498 as the
  test measures it); no tokenizer was run. The test's 5,500-byte guard
  has 2 bytes to spare. (7c, H4)
- The pilot is n = 1 per cell (`docs/agent-eval.md`); a real comparison
  needs several runs per task and condition, more tasks, and a second
  model. (7c)
- The agent eval's graders can only express geometry through `@expect`
  on derived solids (intersections with probes plus a 1 mm³ marker, so
  "no overlap" measures 1 instead of failing as an empty model). An
  `@expect empty` or `@expect volume-between` would make them plainer.
  (7c)

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

## macOS app
- A viewport frame holds the main thread for about 2.6 ms (p50; p95
  3.3 ms) at 60 Hz, nearly all of it `-[CAMetalLayer nextDrawable]`
  waiting for a free drawable; encoding is 0.14 ms. `Immediate` present
  mode halved it against `Fifo` (whose wait was up to a whole refresh).
  A render thread would take the wait off the main thread, but wgpu-hal's
  acquire reads the window's `occlusionState`
  (`wgpu-hal-30.0.1/src/metal/surface.rs:353-369`), an AppKit property of
  the main thread; `CAMetalDisplayLink` hands out drawables itself, which
  wgpu cannot take. (8c)
- 120 Hz pacing is unverified: the only display online during 8c was a
  60 Hz external one. The display link asks for up to 120 Hz
  (`preferredFrameRateRange`); run `NEOSCAD_VIEWPORT_BENCH=8` on a
  ProMotion panel. (8c)
- View-option lines are one pixel and the scale markers' numbers are sized
  in pixels, as in OpenSCAD's offscreen export (DPI 1). OpenSCAD's GUI
  scales them by the screen's DPI (`glLineWidth(dpi)`), so on a 2x display
  NeoSCAD's are half as heavy. (8c)
- Switching between the light and dark scheme renders the last request
  again, because `session.render` bakes the scheme's face colours into the
  geometry (`geom::color::Scheme`); a slow render-mode model is recomputed.
  (8c)
- Memory (8f; `footprint`, Debug app, one window 1320x760 points on a 2x
  display). The 900-950 MB measured in 8d was six windows restored from
  the saved state, not one file. One window with `cube(10)` went from
  114 to 52 MB: its two 4x MSAA buffers (colour and depth, 61 MB at
  1280x1520 pixels) are memoryless now (`TRANSIENT_ATTACHMENT`). 125
  spheres: 834 to 121 MB, from the allocator's cache of freed large
  blocks (338 MB "Malloc Large (empty)", now off through the app's
  `LSEnvironment`) and the upload's staging copy of the vertex data
  (57 MB, now freed after the upload). The six windows: 731 to 257 MB.
  What remains per window is mostly the model's vertex buffer, about
  20 MB of malloc (parse caches, the language server's index), the
  layer's drawables and WebKit's layers (10 MB IOSurface). Left open:
  - mimalloc (O1) replaced `MallocLargeCache=0`, but keeps more after a
    big preview: 125 spheres settle at about 210 MB 30 s after the preview
    (about 90 MB of it mimalloc's, tagged "IOAccelerator" by `footprint`,
    since mimalloc marks its memory with VM tag 100), where the system
    allocator without its large cache settled at about 125 MB.
    `MIMALLOC_PURGE_DELAY=0` gets there within a second instead of about
    30 but no lower, and costs 1-7% of render time. mimalloc v2 (the
    crate's `v2` feature) settled lower in `neoscad serve` with that
    variable (83 against 185 MB), but was 2-3% slower and peaked 65%
    higher on fractal_tree (2.52 against 1.53 GB); v3 is also what
    OpenSCAD ships (its `submodules/mimalloc` is the v3.3.2 tag). Worth
    trying: `mi_collect` on the pool's threads after a run (needs an
    `unsafe` call into `libmimalloc-sys`). (O1)
  - The web content processes were not measured again (56 MB and a 32 MB
    prewarmed one in 8d).
  - A preview's peak is far above its result: 125 spheres peaked at
    972 MB for 13 MB of live data afterwards; not broken down.
  - The snapshot renderer now shares the viewports' device
    (`Offscreen::on_gpu`); the app made no snapshot before, so the second
    device had not cost memory at idle yet.
- Mouse mapping covers orbit, pan and zoom; OpenSCAD's shift-drag
  (pitch/roll), middle-drag (forward/back), shift-wheel (field of view) and
  zoom-to-cursor are not mapped. A Magic Mouse's precise scroll pans, as a
  trackpad's does. (8c)
- The file's `$vpt`, `$vpr`, `$vpd` and `$vpf` move the view when they
  first appear and whenever their values change; OpenSCAD's GUI applies
  them after every evaluation (`Camera::updateView` from
  `MainWindow::instantiateRoot`). Live preview runs after each pause in
  typing, and snapping the view back after each keystroke would undo
  every orbit, hence the difference. The program also sees the view it
  is shown in as `$vp*` (`setRenderVariables`), not the command line's
  default camera. (8f)
- An untitled document's relative `include`s and `use`s resolve against
  the document controller's current directory (the last folder a file
  was opened from or saved to, else Documents), as if it were a file
  named "Untitled.scad" there. OpenSCAD resolves them against its
  working directory (`parser.y`, `fs::current_path()`), which is `/` for
  a Finder-launched app. (8f)
- Document loop gaps (8f):
  - Echo lines carry no location, so they do not jump: the evaluator
    emits them without one (OpenSCAD prints none), and giving the record
    one needs the call's span passed into `echo`.
  - A change to another open document's unsaved text does not re-run the
    documents that include it; only changes on disk to files a run read
    do (`FileWatcher`), and open documents are not watched (their
    buffers, not the disk, are what runs read).
  - Customizer values run as `-D` assignments after the text; OpenSCAD's
    GUI writes them into the parsed program (`applyParameters`). Top-level
    reassignment makes the two the same for a top-level assignment. Not
    done: parameter-name NFC normalisation when reading sets
    (`ParameterSets::readFile`), deleting a set, the description-only and
    other view styles, and the Animate panel.
  - Browse All Versions and Revert To are AppKit's for an
    `autosavesInPlace` document with the File menu's Revert item; the
    app test covers saving in place and reverting to other contents, not
    the versions browser: NSFileVersion keeps no versions in the
    temporary directory, and the browser needs a person. Checklist: open
    a saved file, edit, wait for the autosave, edit again; File > Revert
    To > Browse All Versions shows the earlier text; choosing one
    restores it in the editor and the view; File > Revert To > Last
    Saved (Opened) does the same for the opened version.
- Each editor keystroke costs the app about 1 ms on a 1.1 MB file (p50;
  `EditorBenchmark`), nearly all of it the offset conversion and the edit
  of the document's `String` copy, which are linear in the text; the
  core's `edit` is 0.07 ms. A rope or an incremental line index would make
  it logarithmic. The round trip from the page's change to the app's copy
  is 2.2 ms p50 (3.1 ms p95), CodeMirror's own work 1-2 ms. (8d)
- Editor checks that need a person (verified by test so far: IME's
  NSTextInputClient calls commit once, in `EditorTests`; keys through
  NSApp, in the opt-in `EditorKeyTests`):
  - Japanese input with the system input method: composing (underlined
    marked text, the candidate window placed at the caret), committing,
    cancelling with Escape, and reconversion; the same for Chinese
    (Pinyin) and Korean. The document must see only committed text.
  - Dead keys and the accent menu (hold `e`).
  - VoiceOver: the editor is announced as a text area labelled "OpenSCAD
    source"; reading by line, word and character; hearing typed and
    deleted text; lint markers and the search panel being reachable.
    An in-process query of the web view's accessibility tree found no text
    area without an assistive client attached, so this was not testable
    from `xcodebuild`.
  - Dictation, and Services (Edit > Services) on selected text.
  (8d)
- The key tests that go through NSApp (`EditorKeyTests`) run only with
  `NEOSCAD_EDITOR_KEYS=1` and the test host in front: macOS does not let a
  test host started in the background take focus. They passed once with the
  host brought forward (`open -a` on its bundle), but a later run could not
  get it forward. Whether the menu alone would take F5 and F6 from a
  focused web view is unverified; the editor forwards them itself
  (`appKeys` in `src/editor.js`). (8d)
- With the editor focused, CodeMirror's keymap takes ⌘[ and ⌘] (indent
  less and more), so the View menu's Zoom Out and Zoom In keys do not reach
  the 3D view; they still work with the view focused. Undo and Redo work
  only while the editor has focus: elsewhere the window's own undo manager
  answers the menu, and it has nothing to undo. (8d)
- The builtin names are coloured by name (OpenSCAD's editor's lists), so a
  user module called `cube` is coloured as the primitive; OpenSCAD's editor
  does the same. The grammar accepts non-ASCII identifiers, which OpenSCAD
  accepts only with the `unicode-identifiers` feature; the core's
  diagnostic marks them. (8d)
- The editor runs in the page's content world, not a dedicated
  `WKContentWorld` as `docs/audits/macos-prep.md` §4 suggested: the page
  holds only the bundle (the Content-Security-Policy admits no other
  script and no network), so there is nothing to isolate the bridge from.
  Revisit if the page ever shows content from elsewhere. (8d)
- Building the app needs node 18 or newer (`scripts/apple/build-editor.sh`
  finds nvm's and Homebrew's), and the network once, for `npm ci`. (8d)
- Release (8j, `docs/release.md`):
  - The Developer ID path (`-exportArchive`, notarization, stapling, an
    accepting Gatekeeper) has never run: no Developer ID identity or
    notary profile exists yet. The first signed release is its test, and
    the clean-machine checklist in `docs/release.md` is still open.
  - Ad-hoc release builds carry
    `com.apple.security.cs.disable-library-validation`, since the
    hardened runtime will not load an ad-hoc framework into an ad-hoc
    process; a Developer ID build must not, and the script checks.
  - `CLAUDE.md`'s build list does not mention `scripts/apple/release.sh`.

- Check and measure panels, export and App Intents (8i):
  - The check panel marks findings in the view with numbered rings and
    the selected finding's box (`render::viewport::Annotations`); it does
    not paint thin-wall, overhang and floating faces as `snapshot
    --issues` does, which needs a second scene drawn over the model
    (`session::snapshot::marked_scene` is private and builds the whole
    model).
  - "Auto" checks after each render (F6), not each preview: `check`
    renders the model in full (`Session::check` calls `render_parts`),
    which after every pause in typing would cost a render. Check and
    measure evaluate the text again even when the last render was of the
    same text; the geometry cache makes the build cheap, but the
    evaluation is repeated.
  - The printer presets' bed sizes (`PrinterPreset.all`) were written
    from memory, not checked against the makers' spec sheets.
  - Measurements are of the text when Measure was pressed; an edit does
    not re-measure or mark them stale. Picking casts against the model's
    solid only, not a part's.
  - Export progress is by stage (parse, evaluate, geometry); encoding and
    writing come after the last check for cancellation, so Cancel stops
    evaluation and geometry but not a large file's encoding.
  - AMF: neither NeoSCAD nor OpenSCAD's current source exports it
    (`.reference/openscad/src/io/export.h:27-46` has no AMF format), so
    File > Export does not offer it. WRL and POV, which both have, are
    not in the popup either (`ExportFormat`); the core writes them.
  - Only the 3MF options (colour mode, colour, material type) are
    offered; SVG fill and stroke, PDF paper and 3MF unit and metadata
    are the core's defaults.
  - App Intents: the file parameters accept `public.plain-text`, not
    `org.openscad.scad`: the metadata processor refuses a type it cannot
    resolve at build time ("Could not determine the identifier of
    '.scad', please use a UTType defined by
    UniformTypeIdentifiers.framework"). Outputs go to
    `$TMPDIR/NeoSCAD-Shortcuts/<uuid>/` and are left to the system's
    temporary-file cleanup. A file handed over as data (no URL) runs
    from a temporary copy, so its relative includes do not resolve.
  - Registration was verified from the built app's
    `Contents/Resources/Metadata.appintents/extract.actionsdata` (three
    actions and three App Shortcuts). Not verified: that Shortcuts.app
    lists them and runs them (the `shortcuts` command only lists and
    runs the user's own shortcuts), nor Siri or Spotlight phrases. The
    test host logs `connection to service named
    com.apple.linkd.autoShortcut` errors at launch, as a test host that
    is not a registered app would.
  - The intents run on the app's shared core, whose limits are
    `Limits::AGENT` because nothing in the app changes them; a future
    limits preference would reach the intents too.

## Language server
- `neoscad lsp --stdio`'s diagnostics are the session's parse and
  evaluation, not the geometry stage: warnings only a render prints (the
  kernels', `render()`'s) reach the console, not the markers. The app's
  markers come from its runs instead (`Options::host_diagnostics`), the
  geometry stage's warnings included; a preview's geometry stage says
  less than a render's, so an F6 render's warnings show until the next
  edit's preview replaces them. (8e, 8f)
- Name resolution is lexical from the syntax tree (`crates/lsp/src/index.rs`,
  `world.rs`), not the evaluator's: an `include` inside a module body is
  treated as a top-level one; an empty `include <>` does not reuse the
  previous name as OpenSCAD's scanner does; among included files a module
  defined twice resolves to the file asked from, then the document, then
  the includes in the order they were found (OpenSCAD's last definition
  wins); `use`d libraries are searched last `use` first. (8e)
- The evaluator now resolves names statically (`crates/eval/src/resolve.rs`,
  O4), and some of its rules differ from the index's "assignments are
  visible throughout their scope": an assignment that reads a name
  assigned later in the same scope gets the outer binding
  (`a = 5; module m() { b = a; a = 1; }` sets `b` to 5); a function called
  while its scope is being initialised sees only the assignments made so
  far; a named argument that is not a parameter binds in the callee's body
  (`function f() = zz; f(zz = 5)` is 5); and parameter defaults are
  evaluated in the defining scope, so they never see other parameters.
  The index could share `resolve`'s region model, but the resolver works
  on a whole program's spliced AST and the index per file on the syntax
  tree, accepting broken code, so sharing means moving the model into
  `lang`. (O4)
- References and rename see the document and what it includes, not the
  files that include it (there is no workspace index): renaming a
  top-level name of a file other files include can break them. Rename
  refuses whenever an included file defines or uses the name, and renames
  a parameter's named arguments only in calls within the document. (8e)
- Completion: no path completion inside `include <...>` and `use <...>`;
  no `completionItem/resolve` (each item carries its one-line summary);
  more than 400 matches are cut and marked incomplete. (8e)
- Hover shows a top-level constant's value when it folds from the syntax
  (literals, vectors, arithmetic, conditionals, other constants); function
  calls and `$` variables show only the expression. (8e)
- Formatting follows `.neoscad-fmt.toml`, not the client's `tabSize` and
  `insertSpaces`. Range formatting formats the top-level statements the
  range touches, as a file of their own (the formatter lays out whole
  programs). (8e)
- Positions count lines at `\n` only, as the core and the app's editor do;
  a client that also breaks lines at a lone `\r` (VS Code) disagrees on a
  file with classic Mac line endings. UTF-16 is the only position
  encoding offered. (8e)
- `$/cancelRequest` is ignored: requests are answered synchronously in
  milliseconds; only diagnostics' evaluations stop (on a newer change or
  the host's cancel). (8e)
- The app has keys but no menu items for the language features: Format
  Document (⌥⇧F), Go to Definition (F12, ⌘-click), Rename (F2) and Find
  References (⇧F12). ⌘-click now goes to the definition and ⌥-click adds
  a cursor (in 8d ⌘-click added one), as in Xcode and VS Code. (8e)
- Library viewers (read-only tabs for BOSL2, MCAD and other library files)
  are not documents: they are not restored after a relaunch, and one
  showing the bundled MCAD, which exists only in memory, has no proxy
  icon. A library file changed on disk while shown is not reloaded. (8e)
- Keystroke to markers and to the view (`PipelineLatencyTests`, p50):
  CSG.scad 189 and 184 ms, a BOSL2 cuboid 195 and 192 ms; before 8f the
  markers took 167 and 180 ms and the view 431 and 433 ms (two
  evaluations per pause, the view's after a 400 ms pause). Nearly all of
  it is the 150 ms pause (`SCADDocument.previewDelay`). (8e, 8f)
- `neoscad lsp --stdio` has no page on setting it up in VS Code, Zed,
  Neovim or Helix. (8e)
- The release `wasm_check.wasm` is 44.6 MB with the language server in it
  (the WASM section's 38 MB is from H2); the language server's share was
  not measured. (8e)
