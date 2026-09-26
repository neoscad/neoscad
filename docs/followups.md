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
- A one-line edit re-parses the main file together with everything it
  `include`s, because OpenSCAD's includes are textual (one token stream,
  one parse): the session only skips reading and lexing unchanged
  includes (`lang::loader::LexCache`) and re-parses `use`d libraries
  only when they change. For the `edit_loop` BOSL2 case that parse is
  about 16 ms of a 33 ms re-render (load 3, parse 5, lower 7, measured
  with `lang::parse_program_cached`); the rest is whole-program
  evaluation (16 ms) and a little geometry. Parsing each included file
  on its own and splicing ASTs (or an incremental parser) is the next
  step, and needs care with reassignment across includes and with
  includes that are not whole statements. (7a)
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
- Cancellation stops the evaluator at the next call or loop iteration
  and the geometry evaluator before the next node; one long kernel
  operation (a big boolean, a hull, minkowski) runs to its end. (7a)
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
