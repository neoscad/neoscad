# Audit: an exact B-rep mode (STEP export, 3D fillets) alongside the mesh pipeline

Status: judgement, nothing built in the tree. Written 2026-10-07 against
`4aa80a4` and the reference checkout at `28fe66bc`. Claims about this
codebase cite `path:line` or a command; claims about OpenSCAD cite
`.reference/openscad`; claims about third-party kernels cite the repository,
release or file retrieved, or say "unverified".

Measurements come from three scratch spikes (not committed): one crate per
kernel, each building the same OpenSCAD-shaped trees, run as release builds
on an Apple M4 Pro (14 cores, macOS 27), one process per case under a
watchdog. Timings are single runs unless marked "best of 3"; treat them as
±10%. NeoSCAD timings are `neoscad 0.4.3` (`target/release/neoscad` at
`4aa80a4`) reporting `--summary time`, best of 3, which counts render time
only (millisecond resolution), with rayon on all cores.

## Contents

1. Firm ground
2. Findings, ranked
3. Kernel candidates
4. Spike results
5. STEP output
6. Semantics: what OpenSCAD constructs map to exactly
7. 3D fillets and chamfers, and choosing edges
8. FreeCAD compared
9. Cost, risks and the faceted-STEP alternative
10. Checked and found fine
11. Recommendation
12. Not verified

## 1. Firm ground

- **Neither OpenSCAD nor NeoSCAD writes STEP today.** OpenSCAD's export
  formats are `.reference/openscad/src/io/export.h:28-47` (STL, OBJ, OFF,
  WRL, 3MF, DXF, SVG, NEF, CSG, AST, PNG, PDF, POV, PARAM and the rest), and
  `-o x.step` fails in both with the same message, "Invalid suffix step"
  (run against the nightly and `neoscad`). So STEP export would be a
  NeoSCAD-only addition, and with its flag off nothing changes.
- **Only OCCT survives OpenSCAD-shaped trees.** On 15 boolean cases (section
  4), OCCT 8.0.1 gave a solid that passed `BRepCheck_Analyzer` every time.
  `truck` (git `88ed005`) and its fork `monstertruck` (git `1fbc7a5`) each
  produced a solid for 4 of the 15 at their default tolerance. Both failed
  on a cube unioned with a cylinder standing on the same plane, on two
  cubes sharing a face and on a cube minus a sphere at every tolerance
  tried. Fornjot has been shut down (README: "This project has been shut
  down. Its goals were never reached."; repository archived).
- **OCCT is about 2× slower than the mesh path on CSG, and its tessellation
  is the larger cost** (best of 3; the trees differ slightly, see section
  4). A plate with 400 countersunk holes takes 740 ms in OCCT's boolean
  (545 ms with its parallel mode), plus 4.2 s to tessellate it serially at
  0.05 mm deflection. NeoSCAD renders the same plate in 108 ms at
  OpenSCAD's default fragment count and 434 ms at `$fn=64`.
- **Faithful `$fn` polygons are expensive in B-rep.** 24 holes cut as
  64-sided prisms took 681 ms and wrote a 6.8 MB STEP file. The same holes
  as true cylinders took 11 ms and 106 KB (best of 3). OCCT's STEP costs
  about 4 KB per planar face (an 8.1 MB file for a 2,000-sided prism).
- **OCCT is large.** A stripped native binary using it is 21.6 MB; the same
  spike on `truck` is 1.8 MB. For scale, `neoscad` itself is 18.1 MB
  stripped. In the browser, OCCT builds weigh 13.6 MB raw (3.3 MB brotli,
  4.7 MB gzip) for a STEP-to-glTF demo, and 21.2 MB (4.5 MB brotli, 6.7 MB
  gzip) for the full `occt-wasm` 5.6.0 package. NeoSCAD's web core is
  2.34 MB gzipped (`docs/web-demo-plan.md:39-40`), and GitHub Pages serves
  gzip only (`docs/web-demo-plan.md:37-39`).
- **About half of real models contain something with no exact B-rep
  equivalent.** NeoSCAD's CSG dump was classified for 49 OpenSCAD examples
  and every fifth BOSL2 documentation example (452 that rendered). 24 of
  the 49, and 212 of the 452, use no `polyhedron`, `hull`, `minkowski`,
  `surface`, `import`, `projection`, `resize`, `roof` or `text`. BOSL2
  produces `polyhedron` in 215 of the 452 (section 6).
- **OCCT was deterministic in every check made here.** Five models,
  including the 400-hole plate, wrote byte-identical STEP (header line
  aside) across repeated runs, with `BOPAlgo_CellsBuilder::SetRunParallel`
  off or on. `truck`'s face order changed between two identical runs.

## 2. Findings, ranked

### F1. A pure-Rust exact backend is not available; OCCT is the only credible kernel

**What our code does:** nothing yet. NeoSCAD's 3D kernel is `manifold-rust`
(`docs/architecture.md:23`), which works on meshes.

**What the candidates do:** see sections 3 and 4. `truck` and
`monstertruck` handle primitives, sweeps and a few booleans of
non-coplanar solids. They fail on most OpenSCAD idioms, and whether a case
succeeds depends on the tolerance, in both directions. For example,
`truck` unions a cube with a cylinder that pokes through its bottom face
at tolerance 0.05 and 0.1, but not at 0.01. `monstertruck` does it at 0.01
but not at 0.05 or 0.1. Their boolean results carry approximated
intersection curves: volumes are off by 0.03–0.1% where OCCT is exact, and
OCCT reading `truck`'s STEP reports 804.42 for a punched cube whose exact
volume is 803.65.

**Gap:** total for this use. Coplanar and touching faces are the norm in
OpenSCAD models (an object standing on the build plate, a pocket cut flush
with a top face).

**Suggested change:** if an exact mode goes ahead, plan it on OCCT. Do not
spend effort hardening `truck`'s booleans. Revisit `monstertruck` yearly; it
is active and fixed the coplanar pocket case (`c10`) that `truck` fails.

### F2. OpenSCAD's `$fn` semantics and exact geometry disagree, and the choice changes printed dimensions

**What OpenSCAD does:** circles, cylinders and spheres are inscribed
polygons. The fragment count is `$fn`, or else
`ceil(max(min(360/$fa, 2πr/$fs), 5))`
(`.reference/openscad/src/core/CurveDiscretizer.cc:100-152`). Spheres are
rings at `phi = 180(i+0.5)/rings`, so they have no vertex at the poles
(`.reference/openscad/src/core/primitives.cc:184-198`). So `sphere(10)` at
the defaults has a bounding box of ±9.95 in y and z (run with
`--summary bounding-box`). A `cylinder(d=4.5)` hole at the defaults is an
octagon (8 fragments by that formula) whose width across the flats is
4.16 mm, not 4.5. Users rely on this: `$fn=6` for hexagons, low `$fn` for
printable holes, and compensation tricks sized to the polygon.

**What an exact mode must do:** pick a rule, and every rule changes some
models. FreeCAD's importer makes a polygonal prism when `$fn` is at most 16
and a true cylinder above that (`importCSG.py:1085-1131`, preference
`useMaxFN`, default 16). It makes every sphere exact, ignoring `$fn`
(`importCSG.py:1059-1067`). As F6 shows, faithful polygons are expensive.

**Gap:** an exact export of a model tuned for printing is not the model the
user printed. For a 4.5 mm hole at the defaults, the difference is 0.34 mm.

**Suggested change:** an owner decision (section 6 proposes rules). The
safest default makes a curve exact only when its fragment count came from
`$fa`/`$fs` (or the experimental `$fe`, which already means "a curve within
this error", `CurveDiscretizer.cc:115-151`). An explicit `$fn` keeps the
polygon. Report every substitution.

### F3. About half of models would not export exactly, so the mode needs a defined fallback from day one

**What the corpora show (the run behind the numbers in section 1):**

| Corpus | Models rendered | No mesh-only construct | Most common blockers (models) |
|---|---|---|---|
| `.reference/openscad/examples` | 49 (1 failed) | 24 | `text` 10, `import` 8 (mostly DXF, which is 2D and polygonal), `projection` 4, `hull` 3, `surface` 2, `polyhedron` 1 |
| BOSL2 `examples_x`, every 5th | 452 (53 failed or timed out) | 212 | `polyhedron` 215, `text` 25, `hull` 13, `projection` 4, `minkowski` 3 |

BOSL2 builds rounded and textured shapes as `vnf_polyhedron`. `spheroid()`
does this in its default style, and so do `cyl()` with rounding or a
texture, gears, threads and sweeps
(`.reference/BOSL2/shapes3d.scad:3964-3990`; `vnf_polyhedron(` appears 18
times in `shapes3d.scad`, 8 in `gears.scad`, 12 in `skin.scad`). Its
`cube`, `cylinder` and `sphere` wrap the builtins, so they stay exact
(`shapes3d.scad:62-74`, `2012-2024`, `3840-3846`).

**Gap:** a mode that refuses the whole model when it meets one
`polyhedron` is unusable for BOSL2 users. One that silently facets is
misleading.

**Suggested change:** per-subtree fallback, reported. OCCT can hold
faceted planar faces and exact faces in one solid and run booleans
between them (case `g02`: valid, but 60× slower than exact holes). An
exact export would then name each faceted subtree, with its source
location, as a warning, and `--enable exact=strict` would make that an
error. FreeCAD does the same for `hull`/`minkowski`, through meshes (F8).

### F4. OCCT fillets work on simple solids, fail loudly on some booleans, and can succeed with an invalid solid

**Measured (section 4):**

- Filleting all 100 edges of the countersunk plate at r 0.5 took 19 ms and
  gave a valid solid.
- Filleting the vertical edges of a box (CadQuery's `|Z`) worked.
- Chamfering the top edges of a hex nut worked.
- Filleting every edge of a cube unioned with a cylinder failed with an
  error: "radius=1 does not fit the local geometry on 16 edge(s):
  BRepFilletAPI_MakeFillet did not complete".
- Filleting a 10 mm cube with r 6, larger than half an edge, *returned a
  solid*. It failed `BRepCheck_Analyzer`, had volume 1042 (more than the
  cube's 1000), and read back from STEP with volume −954.7.

**Gap:** an exact mode must validate after every operation, not trust
`IsDone()`.

**Suggested change:** a "validate every result" rule. Any operation whose
output fails `BRepCheck_Analyzer` (or has a volume sign or bound out of
range) becomes an error with the source location. Never export such a
result.

### F5. Adding OCCT conflicts with three standing rules: one C dependency, WASM-clean libraries, no `unsafe`

**What our code does:** mimalloc is "the one C dependency outside the system
frameworks" (`docs/architecture.md:28`). The workspace forbids
`unsafe_code` (`Cargo.toml:40-41`). Library crates must build for
`wasm32-unknown-unknown` (`CLAUDE.md`, "Rules"). NeoSCAD already turned
down a C++ dependency on these grounds: planegcs "cannot build for
`wasm32-unknown-unknown` without a C++ runtime" (`docs/language-extensions.md:963-965`).

**What OCCT needs:** a C++17 compiler and CMake to build from source
(cadrum README, "Build"). To ship it, either use a third-party prebuilt
static library (cadrum downloads 33.4 MB for `aarch64-apple-darwin` from
its GitHub releases, `build.rs`; release `occt-8_0_1_rev2`) or build OCCT
in our own CI. For `wasm32-unknown-unknown` it needs wasi-sdk, which cadrum
supplies as a Docker image, plus a call to the C++ static constructors
before first use (cadrum README, "Building for wasm32-unknown-unknown").
The binding layer is FFI (cadrum uses `cxx`).

**Gap:** the backend can't be a normal library crate under the current
rules.

**Suggested change:** if it goes ahead, keep it in a host-only crate (like
`cli` or `ffi`), behind a cargo feature that is off for `wasm-check` and
the web core. Exempt only that crate from `unsafe_code = "forbid"`, or put
the FFI in a dependency. Record the exception in `docs/architecture.md`.

### F6. Determinism holds in what was tested; OCCT's interruption and memory are not under NeoSCAD's limits

**Determinism:** OCCT wrote byte-identical STEP (FILE_NAME line excluded)
for `c01`, `c07b`, `c14`, `c16` and `f01` across 3–4 runs, with
`SetRunParallel` off or on. One state leak: the PRODUCT name carries a
per-process counter ("Open CASCADE STEP translator 8.0 1", then "… 4" after
earlier writes in the same process). A session that exports twice would
differ from a fresh one, breaking "warm equals cold"
(`docs/architecture.md:365-370`), unless the writer sets the name. cadrum
uses `TShape` pointer addresses as face IDs (`src/ffi.cpp`,
`relay_from_builder`). That is fine within one run, but such IDs must
never be used for ordering.

**Time and memory:** NeoSCAD checks time "before each geometry node; one
long kernel operation runs to its end" (`docs/architecture.md:309-310`),
and kernel working memory is outside the estimate (`:292-294`). OCCT's
`BOPAlgo_Builder::Perform` takes a `Message_ProgressRange`
(`BOPAlgo_Builder.hxx:138-139` in the OCCT 8.0.1 headers), and
`Message_ProgressScope` exposes `UserBreak()`. Interruption is therefore
possible but has to be wired up. Sequential differences on the 400-hole
plate took 17.2 s, against 740 ms for one N-ary operation.

**Suggested change:** gate any backend on a determinism test (1, 2 and 8
threads; warm after cold) that compares whole STEP files. Set the STEP
header and product fields explicitly, and evaluate OpenSCAD's
`difference(){a; b; c; …}` as one N-ary operation.

### F7. WASM: possible, about 2–3× the current download, single-threaded

The cadrum demo's OCCT build (STEP read plus meshing only) is 13.6 MB raw,
4.7 MB gzip. `occt-wasm` 5.6.0, which has booleans, fillets, sweeps and
STEP, is 21.2 MB raw, 6.7 MB gzip, 4.5 MB brotli; its README says "Single
WASM thread" and that it needs WASM SIMD, tail calls and wasm exceptions.
Against a 2.34 MB gzip core, an exact module adds 2–3× the download.
opencascade.js, the older Emscripten build, was last pushed 2023-08-15,
and its npm 1.1.1 package unpacks to 66.7 MB.

**Suggested change:** keep the web demo mesh-only. If the web ever gets
exact export, load it as a separate module only when the user asks for
STEP.

### F8. A product decision, not a defect: what "exact mode" promises

Decisions for the owner, not assumed here:

- the `$fn` rule (F2);
- fallback versus strict (F3);
- whether exact mode is export-only, or also drives the preview (preview
  from OCCT tessellation costs seconds on large models; section 4);
- whether fillets are in scope (section 7);
- native-only versus web;
- whether to depend on cadrum's prebuilt OCCT or build OCCT ourselves.

Licence is *not* a blocker. OCCT is LGPL-2.1 with the "Open CASCADE
exception (version 1.0)" (`OCCT_LGPL_EXCEPTION.txt`), and LGPL-2.1 code can
be combined into a GPL work. The owner has already accepted that
distributed binaries are effectively GPLv3 and that "GPLv3-compatible
dependencies are acceptable" (`docs/language-extensions.md:976-979`).
`truck`/`monstertruck` are Apache-2.0, which is already the case for
`manifold-rust` (`packaging/licenses/README.md`). This is not legal advice.
Shipping static OCCT means shipping its source with each release, which
the release licence check (`scripts/release/licenses.sh`) would need to
cover.

## 3. Kernel candidates

| Kernel | Version / state (retrieved 2026-10-07) | Licence | Rust route | Booleans | Fillets | STEP | WASM |
|---|---|---|---|---|---|---|---|
| **OCCT** (Open-Cascade-SAS/OCCT) | V8.0.1, released 2026-07-30; pushed daily | LGPL-2.1 + OCCT exception | cadrum 0.8.20 (MIT, prebuilt static OCCT 8.0.1, `cxx`); opencascade-rs 0.3 / occt-sys 7.8.1 (LGPL, builds OCCT 7.8.1 from a 14.4 MB source crate with CMake); occt-wasm 4.0.0 crate (MIT/Apache tooling, LGPL wasm, runs OCCT as WASM on native) | BOPAlgo (general fuse, cells builder), fuzzy value, parallel mode (`BOPAlgo_Options.hxx:114,123`) | `BRepFilletAPI_MakeFillet`/`MakeChamfer` (via cadrum `fillet_edges`, `chamfer_edges`) | read/write (`STEPControl`, XCAF colours) | cadrum ships a `wasm32-unknown-unknown` static OCCT (35.5 MB tarball; Docker/wasi-sdk); occt-wasm (Emscripten) |
| **truck** (ricosjp/truck) | crates.io truck-shapeops 0.4.0 / truck-modeling 0.6.0 published 2024-09-20; master `88ed005` 2026-09-28 | Apache-2.0 | native Rust | `truck_shapeops::and`/`or`, difference via `Solid::not` | master only: `fillet::simple_fillet`, one edge between two faces at a time, needs its own surface enum (`truck-shapeops/src/fillet/mod.rs:179`); `truck_modeling::Surface` has no fillet variant (`truck-modeling/src/geometry.rs:126-135`) | `truck-stepio` write (and read) | builds for wasm32 (spike: 1.8 MB raw, 0.30 MB brotli) |
| **monstertruck** (virtualritz/monstertruck) | 0.4.1 on crates.io 2026-09-19; master `1fbc7a5` 2026-09-30; 33 stars | Apache-2.0 | native Rust; repo pins nightly for rustfmt only | `monstertruck_solid::{and, or, difference}` returning `Result` | README claims a "Fillet engine rewrite: per-edge radii … multi-chain + chamfer" (not exercised here) | write and read; assembly STEP | has a `monstertruck-wasm` crate (not built here) |
| **Fornjot** | archived; README: "This project has been shut down"; last release v0.49.0 2024-03-21 | 0BSD | — | — | — | — | — |
| **opencascade.js** | last push 2023-08-15 | LGPL-2.1 | JS only | OCCT | OCCT | OCCT | Emscripten; npm 1.1.1 unpacks to 66.7 MB |

The binding choice matters more than it looks. cadrum is a single
maintainer's project (64 stars) that downloads prebuilt static libraries at
build time. It is the cheapest way to try OCCT, but it is a supply-chain
dependency. opencascade-rs builds from source and is
"a major work in progress" (its README). A NeoSCAD-owned `cxx` binding to
the few OCCT classes needed is the likely long-term choice.

## 4. Spike results

Each spike builds the trees below directly in the kernel's API. These are
the constructions NeoSCAD's evaluator would emit for the OpenSCAD source
shown (the plate, for example):

```openscad
nx=6; ny=4; t=5;   // c06: 2x2, c07: 6x4, c07b: 20x20
difference(){
  cube([15*nx,15*ny,t]);
  for(i=[0:nx-1], j=[0:ny-1]) translate([7.5+15*i,7.5+15*j,0]) {
    translate([0,0,-1]) cylinder(r=2.25,h=t+2);
    translate([0,0,t-4.5]) cylinder(r1=0,r2=4.51,h=4.51);
  }
}
```

"ok" means a solid came back. For OCCT it also means `BRepCheck_Analyzer`
passed and the STEP round-trip gave the same volume. For truck and
monstertruck it means a solid came back whose shells are `Closed`. "fail"
is the kernel returning `None`/`Err`; "panic" is an unwinding panic.
Tolerance is the kernel's boolean tolerance (default 0.05, the value in
truck's own example).

| Case | truck (tol 0.05) | monstertruck (tol 0.05) | OCCT 8.0.1 (best of 3) |
|---|---|---|---|
| primitives: cube, cylinder, sphere, cone | ok; cone's topology check overflows the stack (checks skipped: volume correct) | ok | ok, exact volumes |
| b01 cubes overlapping on a diagonal, union | ok | ok | ok 3.6 ms |
| b03 cube − through cylinder | boolean ok, tessellation panics; ok only at tol 0.5 | fail; ok at 0.1 (vol 803.75 vs 803.65) | ok 1.3 ms |
| c01 cube − sphere (`difference(){cube(15,center=true);sphere(10);}`) | fail at 0.001–0.5 | fail at 0.001–0.5 | ok 8.0 ms |
| c02 cube ∩ sphere | fail | fail | ok 6.2 ms |
| c03 cube ∪ cylinder standing on the same plane | fail at all tolerances | fail at all tolerances | ok 1.7 ms |
| c04 `linear_extrude` of a square with a square and a circular hole | ok | ok | ok 0.4 ms |
| c05 `rotate_extrude` of a 5-point profile | ok; topology check overflows the stack | ok | ok 0.2 ms |
| c06 plate, 4 countersunk holes | panic (`intersection_curve.rs:220` unwrap) | fail at first hole | ok 9.3 ms |
| c07 plate, 24 holes | fail at 2nd boolean | fail at 1st | ok 38 ms (sequential: 106 ms) |
| c07b plate, 400 holes | not run | not run | ok 740 ms; 545 ms parallel; sequential 17.2 s |
| c08 10 overlapping cylinders, union | fail at 1st | fail at 1st | ok 16 ms |
| c09 two cubes sharing a face, union | fail at all tolerances | fail at all tolerances | ok 0.9 ms |
| c10 pocket flush with the top face | fail | ok | ok 1.4 ms |
| c11 hex prism (`$fn=6`) − cylinder | ok, 1.0–1.6 s | ok 46 ms | ok 1.7 ms |
| c12 pin with a cross-drilled hole | ok 1.1 s, tessellation panics | ok 80 ms | ok 9.4 ms |
| c13 sphere − offset sphere | fail | fail | ok 2.3 ms |
| c14 equal-radius cylinder tee | panic ("This shell is not oriented and closed") | fail | ok 3.2 ms |
| c15 cylinder tangent to a cube face, cut | fail | fail | ok 1.4 ms |
| c16 M8 bolt: helix sweep of the thread profile, ∩ crest, ∪ hex head | not attempted (no helical sweep in the builder) | not attempted | ok 617 ms, 43 faces, 924 KB STEP |
| g02 plate, 24 holes as 64-sided prisms | — | — | ok 681 ms, 1,542 faces, 6.8 MB STEP |
| g03 same, true cylinders | — | — | ok 11 ms, 30 faces, 106 KB STEP |

Fillet cases (OCCT only): f01 all edges of c06 at r 0.5, ok 19 ms; f03
vertical box edges at r 3, ok; f04 chamfer of the nut's top edges, ok; f02
all edges of c03 at r 1, error; f05 cube r 6, "ok" but invalid (F4).

**Speed against NeoSCAD's mesh path** (best of 3; NeoSCAD is the whole
render, OCCT is the boolean alone, then tessellation at 0.05 mm / 0.2 rad):

| Model | NeoSCAD defaults | NeoSCAD `$fn=64` | NeoSCAD `$fn=256` | OCCT boolean | OCCT tessellation |
|---|---|---|---|---|---|
| c01 cube − sphere | 2 ms | 4 ms | 37 ms | 8 ms | 5 ms |
| c06 plate 2×2 | 2 ms | 6 ms | 30 ms | 9 ms | 10 ms |
| c07 plate 6×4 | 8 ms | 29 ms | 162 ms | 38 ms | 43 ms |
| c07b plate 20×20 | 108 ms | 434 ms (155k facets) | 2,455 ms | 740 ms | 4,184 ms (204k triangles, serial) |
| c16 M8 bolt | 123 ms (the recipe thread: twisted `linear_extrude`, 133k facets) | — | — | 617 ms (exact helix sweep) | 55 ms |

The bolt rows build different geometry (a twisted extrusion against a true
helical sweep), so they compare cost per model, not per operation. cadrum
calls `BRepMesh_IncrementalMesh` with parallel meshing off
(`src/ffi.cpp`, `mesh_shape`). OCCT can mesh in parallel (not measured
here).

**Binary size:** stripped, the OCCT spike is 21.6 MB, the truck spike 1.8
MB and `neoscad` 18.1 MB. OCCT's static libraries for `aarch64-apple-darwin`
are 106 MB on disk (24 toolkits, from `TKernel` to `TKDESTEP`).

## 5. STEP output

FreeCAD is not installed (`brew list`, `mdfind` and `/Applications` show
no FreeCAD, Fusion or other CAD application). Validation was done with OCCT
instead:

- Every OCCT-written file (AP214, `FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'))`)
  read back as one solid, passed `BRepCheck_Analyzer`, and had the same
  volume to 1e-12, except f05 (F4).
- Every `truck` STEP file that was written (primitives and the booleans
  that succeeded) also read into OCCT as one valid solid. Volumes are
  exact for primitives and off by up to 0.1% for boolean results.
- OCCT writes the date as `1979-01-01T00:00:00` in this build, which
  suits reproducible output. truck writes the wall-clock time into
  `FILE_NAME`.

What a person still has to check in FreeCAD, Fusion or another importer:

1. The file opens as one solid body, not a shell or a compound of faces.
2. Units are millimetres, and a 10 mm cube measures 10.
3. Cylindrical and conical faces come in as analytic surfaces, so a hole
   can be selected as one face and its diameter measured.
4. Colours, if `color()` is mapped to XCAF, land on the right faces.
5. The faceted fallback subtrees (F3) import as planar faces, and the
   importer does not reject a file with thousands of them.

## 6. Semantics: what OpenSCAD constructs map to exactly

OpenSCAD's geometry builtins, from the reference's registrations: `cube`,
`sphere`, `cylinder`, `polyhedron`, `square`, `circle`, `polygon`, `text`,
`surface`, `import`, the transforms (`translate`, `rotate`, `scale`,
`mirror`, `multmatrix`, `resize`, `color`), the booleans (`union`,
`difference`, `intersection`, `intersection_for`), `hull`, `minkowski`,
`linear_extrude`, `rotate_extrude`, `projection`, `offset`, `fill`, `render`
and the experimental `roof`.

| Construct | Class | Notes and rule |
|---|---|---|
| `cube`, `square` | exact | |
| `cylinder`, `circle` | exact *or* polygon, by rule (F2) | Proposed: exact when fragments come from `$fa`/`$fs`/`$fe`; a polygonal prism or polygon when `$fn` is given. FreeCAD's threshold is `$fn` ≤ 16 |
| `cylinder(r1, r2)`, cones | as above | Apex cones (`r1=0`) are fine in OCCT (case p04) |
| `sphere` | exact or polyhedron | OpenSCAD's sphere has flattened poles (±9.95 for r 10 at the defaults); an exact sphere is bigger. Same rule |
| `polygon`, `polyhedron` | exact as given (planar facets) | A `polyhedron` is exact *as a polyhedron*: each face is a planar face. BOSL2's smooth shapes stay faceted. Faces that are not planar need triangulating, as FreeCAD does with `makeFilledFace` (`importCSG.py:1342-1350`) |
| `translate`, `rotate`, `mirror`, uniform `scale`, rigid `multmatrix` | exact | |
| non-uniform `scale`, general `multmatrix`, `resize` | approximable | OCCT turns analytic surfaces into B-splines (`transformGeometry`). FreeCAD's documentation warns that "those BSplines are known to cause trouble in later boolean operations". Report as approximated |
| `union`, `difference`, `intersection`, `intersection_for` | exact | N-ary, one cells-builder or general-fuse call per node (F6) |
| `linear_extrude` (no twist, scale 1) | exact | Of an exact 2D region |
| `linear_extrude` with `scale` | exact (loft between scaled sections) | OpenSCAD's slices are ruled; a loft of two sections is the same surface |
| `linear_extrude` with `twist` | approximable | Exact twisted sweep along a helical auxiliary spine, as FreeCAD's `Twist` does (`OpenSCADFeatures.py:372-416`). OpenSCAD's result is `slices` ruled bands, so the two shapes differ by design |
| `rotate_extrude` | exact | Partial `angle` too; `$fn` polygonal revolutions follow the same rule as cylinders |
| `offset(r)` | exact in 2D | Arcs at convex corners; OCCT's 2D offset (`BRepOffsetAPI_MakeOffset`), or arcs written from Clipper's result. Not measured |
| `offset(delta, chamfer)` | exact | Lines only |
| `text` | approximable, potentially exact | `crates/text` produces the glyph outlines (quadratic or cubic Béziers), which convert exactly to B-spline edges. Today they are flattened by `$fn`. FreeCAD does not do this; it asks OpenSCAD for a DXF (`importCSG.py:912-914`) |
| `import` STL/OFF/OBJ/3MF | mesh-only | Faceted fallback. FreeCAD converts with `makeShapeFromMesh` (`importCSG.py:881-900`) |
| `import` DXF/SVG | exact as polygons, approximable as curves | OpenSCAD flattens arcs and Béziers by `$fn` at import; keeping curves is a departure |
| `hull` | mesh-only in general | The hull of exact solids has ruled and blended faces. Only special cases are closed-form (the hull of two spheres or circles is a "stadium"). Faceted fallback |
| `minkowski` | mesh-only in general | Minkowski with a sphere is an offset (`BRepOffsetAPI_MakeThickSolid`, with known failures). Faceted fallback |
| `surface` | mesh-only | Height field; faceted fallback |
| `projection`, `projection(cut=true)` | approximable | Section (cut) is exact (plane ∩ solid). Silhouette projection of curved solids needs hidden-line style outline extraction (FreeCAD tessellates first, `importCSG.py:1358-1370`) |
| `fill` | exact | Removes holes from a 2D region |
| `roof` | mesh-only | Straight skeleton; NeoSCAD registers it disabled (`docs/research/experimental-features.md`) |
| `color` | attribute | Maps to XCAF colours on faces |
| `render` | no-op | |

**Rules an exact mode needs:**

1. **Flags off, identical behaviour.** No new names, no new suffix: `-o
   x.step` keeps today's "Invalid suffix step" error unless the flag is on.
   This follows the convention for `part()` and `--enable sketch`
   (`docs/language-extensions.md:40-49`).
2. **The mesh path stays the source of truth for preview, `check` and
   printing.** Exact mode builds a second geometry from the same node tree
   at export time.
3. **Every substitution is reported.** Exact, approximated (B-spline),
   faceted fallback or refused, with the node's source location, in the
   same message channel as warnings. `strict` turns the last three into
   errors.
4. **Cross-check against the mesh path.** For each exported solid, compare
   the exact volume and bounding box with the mesh render. Warn above a
   tolerance derived from the fragment rule. This catches kernel failures
   that pass validity (F4) and rule surprises (F2).
5. **Validate every operation** with `BRepCheck_Analyzer`; an invalid
   intermediate is an error, never exported.

## 7. 3D fillets and chamfers, and choosing edges

**Kernels:** OCCT's `BRepFilletAPI_MakeFillet` and `MakeChamfer` handle
constant and variable radius, through cadrum's `fillet_edges` and
`chamfer_edges`. They worked on the cases in section 4, with the failure
modes in F4; occt-wasm's README adds that "Not every edge is filletable.
Seam and degenerate edges are not … on the box-plus-cylinder fusion, only
13 of 20 edges do". truck master has single-edge `simple_fillet` at
face-pair level, not a solid operation. monstertruck claims a fillet
engine (unverified here).

**The language problem.** OpenSCAD's CSG has no names for edges, and
booleans create new ones. Existing approaches:

| Approach | How edges are named | Fit for OpenSCAD syntax |
|---|---|---|
| CadQuery string selectors | geometry of the result: `|Z` (parallel to Z), `#Z` (perpendicular), `>Z` (farthest in +Z), `%Line`/`%Circle` (type), `>>Y[-2]` (nth by centre), combined with `and`/`or`/`not`/`exc` (`doc/selectors.rst:32-131`) | Good: a string argument, e.g. `fillet(r=1, edges="|Z and >X") difference(){…}`; selectors work on any result |
| build123d | `ShapeList` methods `filter_by(Axis/Plane/GeomType)`, `sort_by`, `group_by`, operators `>`, `<`, `>>`, `<<`, `|`; and history: `Select.LAST`/`Select.NEW` "narrow the result to what the last operation did" (`docs/topology_selection.rst:25-58,87-118`) | History selection ("the edges this `difference` created") fits CSG well: `fillet(r=1, edges="new")` around a boolean. OCCT exposes the history (cadrum `iter_history`) |
| BOSL2 edge specs | 12 edges of the bounding cuboid of an attachable: vectors like `TOP+LEFT`, `"X"`/`"Y"`/`"Z"`, `"ALL"`, `except=` (`.reference/BOSL2/attachments.scad:209-239`) | Already OpenSCAD syntax, but only for primitives before booleans, and BOSL2 implements them with mesh masks. Not a general edge selector |
| Position or region | edges inside a box, or near a point | Simple and robust to topology changes; verbose |

**Options** (not a design):
(a) CadQuery-style selector strings on any 3D child, evaluated on the
exact result;
(b) history selection on booleans: `new`, `from child i`, `between children
i and j`;
(c) BOSL2-compatible edge specs for primitives only;
(d) combinations of these.
Every option needs a mesh-mode behaviour too. Either fillets are exact-only
and the mesh preview shows sharp edges plus a warning, or the mesh path
tessellates the exact fillet. The second ties the preview to OCCT, which
rule 2 above avoids. The geometry-query design (`--enable query`,
`docs/language-extensions.md`) is the natural neighbour for selectors on
results.

## 8. FreeCAD compared

FreeCAD (LGPL-2.1, release 1.1.4 on 2026-09-28; main at `e326ee2f07`) is
the closest open-source program to what exact mode would replicate. It is
built on OCCT, and its Part and PartDesign workbenches offer the booleans,
fillets, chamfers and STEP import and export discussed here.

**What exact mode would replicate:** the OCCT layer of FreeCAD's Part
workbench, driven from code: primitives, booleans, sweeps and extrusions,
fillets and chamfers, STEP export (and perhaps import).

**What it would not:** FreeCAD's interactive feature tree and parametric
history (PartDesign bodies, the topological naming work), the constraint
sketcher (NeoSCAD's own sketch design is separate,
`docs/language-extensions.md`), assemblies, TechDraw drawings, FEM and CAM.

**Where NeoSCAD differs:**

- The model is OpenSCAD source, evaluated identically with flags off.
- Mesh stays the default, and printing never depends on OCCT.
- A WASM build and an agent surface (`serve`, `mcp`, `check`, `measure`)
  that FreeCAD does not have.
- Determinism and byte-identical output as a rule.

**What FreeCAD's OpenSCAD workbench shows about mapping OpenSCAD to
B-rep** (`src/Mod/OpenSCAD/importCSG.py` at FreeCAD main, last changed
2026-09-23 in `58878a606b`, and the workbench page in
FreeCAD-documentation `wiki/OpenSCAD_Workbench.md`):

- It needs OpenSCAD installed. A `.scad` file is first turned into `.csg`
  by running the OpenSCAD binary (`importCSG.py:118-120`, `callopenscad`),
  then the CSG tree is mapped to Part objects once. It is an import, not
  a live link.
- `$fn` handling: cylinders and cones become polygonal prisms or frusta
  when `$fn` ≤ `useMaxFN` (default 16), and true cylinders and cones
  otherwise (`importCSG.py:1093-1131`); circles alike (`:1202-1208`);
  spheres are always exact (`:1059-1067`). This is the F2 trade-off,
  resolved with a fixed threshold.
- `hull` and `minkowski` are "CGAL features". The children are tessellated,
  sent to OpenSCAD as meshes, and the result comes back as a faceted solid
  (`OpenSCADUtils.py:608-627`). Above `tempmeshmaxpoints` (default 5000)
  nothing is returned (`:618-619`), so the feature fails. The page says
  so: "Currently we run the OpenSCAD binary in order to perform hull and
  minkwoski operations and import the result. This means that the
  involved geometry will be triangulated."
- Non-uniform scale and `resize` use `transformGeometry`
  (`importCSG.py:471-505`, `:1010-1026`). The page: "geometric primitives …
  are converted to BSpline prior to performing such deformations. Those
  BSplines are known to cause trouble in later boolean operations. An
  automatic solution is not available at the moment."
- `text` goes through OpenSCAD to DXF and back (`importCSG.py:912-914`), so
  glyphs are polygons. `polyhedron` faces become planar faces
  (`:1324-1356`), and imported STL becomes a faceted solid via
  `makeShapeFromMesh` (`:881-900`).
- Twisted `linear_extrude` is a sweep with a helical auxiliary spine
  (`OpenSCADFeatures.py:372-416`), which is exact but not OpenSCAD's
  ruled-slice shape.
- The page's hint: "If FreeCAD crashes when importing CSG, it is strongly
  recommended that you enable 'automatically check model after boolean
  operation'". This is the F4 lesson from production use.

The mapping has been in FreeCAD for years and still falls back to meshes
for `hull`, `minkowski`, `text` and imported meshes, with a point limit.
Any NeoSCAD exact mode should expect the same mesh-only list (section 6)
and plan the fallback, not hope to close it.

## 9. Cost, risks and the faceted-STEP alternative

**Effort by stage** (person-weeks, wide bands; nothing here has been built
in the tree):

| Stage | Scope | Estimate |
|---|---|---|
| 0 | OCCT in the build: from-source CMake build (or a pinned prebuilt) for macOS arm64/x86_64, Linux, Windows MSVC; cargo-dist, PGO and notarisation interplay; licences and source offer; CI time | 2–3 |
| 1 | Exact export, native only: a host-side crate mapping the node tree (rules in section 6), N-ary booleans, `linear_extrude`/`rotate_extrude` of exact 2D (circles, squares, polygons and 2D booleans as OCCT faces), validation, mesh cross-check, substitution reports, STEP writer with fixed header, determinism tests | 6–9 |
| 2 | App and agent surface: export menu and `check` reporting in the macOS, Linux and Windows apps and `mcp`; preview of the exact result on demand | 3–5 |
| 3 | 2D exactness: `offset(r)` arcs, text outlines as B-splines, twisted and scaled extrude, `projection(cut=true)` | 4–8 |
| 4 | Fillets and chamfers: language design, selectors, failure diagnostics, mesh-mode behaviour | 6–10 |
| 5 | Web: wasi-sdk build, a lazily loaded module, memory limits | 4–6 |

Stages 0–2 come to 11–17 person-weeks; everything comes to 25–41.

**Ongoing load:** OCCT ships patch and minor releases through the year
(V8_0_0_p1 on 2026-06-17, V8.0.1 on 2026-07-30), and upgrades change
behaviour. Each would
need the spike suite and determinism tests re-run. A C++ toolchain joins
every release platform. Crash triage reaches into C++ that unwinding cannot
catch (`docs/architecture.md:336-343` relies on Rust unwinding). Kernel
memory sits outside the estimate.

**Risks, ranked:** semantic surprise for users (F2, F3) > OCCT fillet and
boolean silent failures (F4) > build and supply-chain load (F5) > a crash
in OCCT taking down the app's process > WASM size (F7) > upgrade drift.

**The alternative: mesh-only plus faceted STEP.** Writing the current mesh
as STEP (AP214 planar faces, or AP242 tessellated geometry) is about 1–2
person-weeks in `crates/io`, with no new dependency, WASM-clean and
deterministic by construction. What it buys:

- A file that STEP-only pipelines accept.
- Coplanar triangles can be merged into polygonal faces, so prismatic
  parts stay small.

What it does not buy:

- Analytic cylinders and holes. A receiving CAD sees facets, so it can't
  pick a hole's diameter or fillet an edge. This is the same as importing
  STL or 3MF, which major CAD tools already do and often convert to
  solids (that tool behaviour was not verified for this audit).
- Size control on curved parts. At OCCT's measured ~4 KB per planar face,
  the 133k-facet thread would be roughly 500 MB. A leaner writer might
  halve that (estimate).

Faceted STEP is therefore useful as the *fallback inside* an exact export
(F3), and weak as a feature on its own.

## 10. Checked and found fine

- **Licence compatibility:** OCCT (LGPL-2.1 + exception), truck and
  monstertruck (Apache-2.0) and cadrum (MIT) are all acceptable under the
  owner's 2026-10-07 licence decision (F8).
- **OCCT determinism** in the cases tested, serial and parallel (F6).
- **OCCT robustness** on every OpenSCAD idiom tried: coplanar and touching
  faces, tangent cylinders, equal-radius tees, sphere–sphere, 800-tool
  N-ary difference, helical sweep.
- **STEP round-trip** of every valid OCCT result, and OCCT reading truck's
  STEP.
- **The 2D-with-holes extrude and `rotate_extrude`** work in all three
  kernels (truck needs care with profile orientation: a reversed profile
  makes an inside-out solid whose topology check overflows the stack).
- **Today's `-o x.step` behaviour** matches the nightly exactly.

## 11. Recommendation

**Go with conditions:** a native-only, export-time exact mode on OCCT,
built in stages, each with a stop point. Not a second preview pipeline,
and not on the web.

Conditions:

1. **Strict superset.** Behind `--enable exact` (name checked against
   OpenSCAD's `Feature.cc`, like `sketch`/`query`). Flags off, the
   language, outputs and errors are byte-identical to today, and the mesh
   pipeline stays the default for preview, `check`, printing and every
   existing export.
2. **The owner decides F2 and F3 first.** The `$fn` rule, and fallback
   versus strict. The recommendation is: exact only for fragments from
   `$fa`/`$fs`/`$fe`; per-subtree faceted fallback with reported
   locations; `strict` to refuse.
3. **The architecture exception is explicit.** OCCT lives in a host-side
   crate behind a cargo feature, outside `wasm-check` and the web core.
   The FFI is either in a dependency or in the one crate allowed `unsafe`.
   `docs/architecture.md` records it.
4. **Gates at every stage:** `BRepCheck_Analyzer` on every intermediate;
   a volume and bounding-box cross-check against the mesh render; a
   determinism test (1/2/8 threads, warm after cold, whole STEP bytes)
   with the STEP header and product names fixed.
5. **Stop rule.** After stage 1, export the conformance and BOSL2 example
   corpora and the benchmark models in exact mode. If fewer than about
   half of the 3D models export with no fallback, or validity failures
   persist, stop at stage 1 or 2. Do not start fillets (stage 4) until
   the stage 1 data are in.

**First stage (0 + 1, about 8–12 person-weeks):** build OCCT from source
in CI for the release targets (or pin cadrum's prebuilt behind a
checksum, if the owner prefers speed to supply-chain control). Then map
cube, cylinder, cone, sphere, polygon, circle, square, rigid transforms,
the three booleans (N-ary), plain `linear_extrude` and `rotate_extrude` to
OCCT. Add the faceted fallback for everything else, the substitution
report, the validity and mesh cross-checks, and a deterministic AP214
writer behind `-o x.step` with `--enable exact`.

Do not build faceted STEP as a separate feature. If the owner declines the
conditions, especially the C++ dependency (F5), the answer is **no-go**.
Revisit when a pure-Rust kernel passes section 4's cases (monstertruck is
the one to watch).

## 12. Not verified

- **The cost of building OCCT from source** (time, disk, CI minutes). Not
  attempted for lack of disk; cadrum's prebuilt was used.
- **A wasm32 OCCT build of our own.** Sizes come from cadrum's published
  demo and the `occt-wasm` npm package, not from linking NeoSCAD's code.
- **monstertruck's fillet engine** and its `monstertruck-wasm` crate.
- **OCCT's parallel tessellation speed**, and determinism of
  `BRepMesh_IncrementalMesh` in parallel.
- **Cross-platform bit-identity of OCCT output** (aarch64 against x86_64
  against wasm32). NeoSCAD already accepts last-bit differences on wasm32
  (`docs/architecture.md:383-386`); OCCT's tolerances may amplify them.
- **How FreeCAD, Fusion or other CAD tools import these STEP files**
  (section 5 lists what to check by hand). Also whether STEP-only
  pipelines (machine shops, quoting services) reject STL or 3MF.
- **The FreeCAD wiki page** (wiki.freecad.org) returned a bot challenge;
  its text was read from the FreeCAD-documentation repository, which was
  last pushed 2025-01-15 and may lag the live wiki.
