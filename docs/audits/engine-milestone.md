# Engine milestone audit: what the passing suite doesn't prove

Audited at 1e5c75b against the nightly 2026.09.23 (git 28fe66bc, the same commit as `.reference/openscad`) on an M4 Pro running macOS 27.0.

## Firm ground

- **Conformance:** I reran the suite and got 1,103/1,103 passing, 0 failing, in 2.6 s. The brief's count is right.
- **BOSL2 regression tests:** I extracted all 976 `[[test]]` entries from 53 `tests/*.scadtest` files and ran them the way `openscad-test` 1.2.4 / `openscad-runner` does (`-o x.term`; a test passes on exit 0 with no `ERROR:`/`TRACE:` line and no ECHO or WARNING unless the test allows them).
  - neoscad passes 976/976 and the nightly passes 976/976.
  - Summed wall time is 44.5 s for neoscad against 200.1 s for the nightly.
- **`conformance diff --format echo`** matches on all 976 BOSL2 tests. Over 2,526 BOSL2 documentation examples (the 2,516 non-`NORENDER` `// Example` blocks plus the 10 files in `examples/`) it matches on 2,525; the one miss is floating-point noise from finding 2.
- **Library path:** the tests use `include <../std.scad>`, so they need no library path. For the examples I rewrote `<BOSL2/…>` to `<../…>`, because `conformance diff` overrides `OPENSCADPATH` (`crates/conformance/src/diff.rs:344-348`). Both binaries resolve `OPENSCADPATH` the same way: relative to the working directory, before the user library directory (`lang/src/loader.rs:57-80` against `parsersettings.cc:149-171`). The only difference is the bundled-library directory (finding 4).
- **Geometry on 35 BOSL2 models:**
  - 31 are 3D. All 31 have identical bounding boxes. In 30 the volume agrees to within 1e-6 relative; the exception is finding 3.
  - Vertex and triangle counts are identical in 20 of the 31. The other 11 are the known kernel and minkowski mesh differences, with the same solid.
  - 4 produce 2D output or nothing. Among the 2D outputs I compared (plus two more 2D examples), 4 SVGs are byte-identical and the rest pass the tier 3 image comparison.
  - Summed time over the 31 3D models is 22.2 s for neoscad against 25.3 s for the nightly. neoscad is faster on 30 of 31.
- **Maths library:** on arm64, neoscad's sin, cos, tan, asin, acos, atan, atan2, exp, ln, pow and sqrt match the nightly's bit for bit (400 samples each). The only difference is `norm`, which is finding 2.

## Findings, by importance

### 1. The geometry cache key hashes every node's whole subtree: O(tree size × depth). This is the only benchmark where neoscad loses.
- **Our code:** `geom/src/evaluate.rs:282` runs SHA-256 over `keys.get(n)` for every node. That string is the node's entire subtree key (`eval/src/dump.rs:666-750`).
- **Measured on BOSL2 `examples/fractal_tree.scad`:**
  - Evaluation takes 4.8 s in neoscad and 10.5 s in the nightly (echo export).
  - Building the geometry takes about 9.7 s in neoscad and about 1.3 s in the nightly.
  - In `sample`, 5,026 of 5,038 samples on the geometry thread were in `sha2::compress256`, called from `run::export_mesh`.
  - The tree's `.csg` dump is 475 MB.
- **Result:** 14.55 s for neoscad against 11.82 s for the nightly with Manifold.
- **Who it affects:** deeply nested trees, which BOSL2's `attach()` recursion produces. It will matter more for `serve`, which re-keys the tree on every edit.
- **Suggested change:** build the keys as a Merkle hash, where each node's key is the hash of its own label plus its children's hashes, computed bottom-up in `Keys::new`. That makes hashing O(n).

### 2. The arm64 nightly fuses the multiply-adds in vector maths; neoscad doesn't. This explains nearly all BOSL2 tree differences.
- **Repro:** `echo([1,0.1]*[-0.010000000000000002,0.1]);`
  - The nightly on arm64 prints `-8.32667e-19`.
  - neoscad, the nightly's own x86_64 slice (`arch -x86_64`) and 2021.01 all print `0`.
- **OpenSCAD source:** the multiply-add loops at `Value.cc:1123` (`multmatvec`), `Value.cc:1150` (`multvecmat`) and `Value.cc:1170` (`multvecvec`), plus `builtin_functions.cc:899` (`norm`, `sum += x*x`) and `:917` (`cross`). That clang contracts these into FMA on arm64 is my inference from the arm64/x86_64 difference; I have not seen the build flags.
- **Our code:** `eval/src/ops.rs:190-300` uses plain `sum += a * b`.
- **Size of the gap:**
  - BOSL2 test `.csg` output matches on 942/976 against the arm64 nightly. Against the x86_64 slice, all 34 misses match.
  - BOSL2 examples as `.csg`: 1,997/2,526 match against arm64. Of the 529 misses, 364 are last-bit numbers only and 165 are structural (different polyhedron face diagonals from tie-breaks, and arc point counts).
  - Against x86_64, 429 of those 529 match. Of the 101 left, 75 are noise of at most 5e-13. The other 26 also differ between the nightly's own arm64 and x86_64 builds, and one of them uses unseeded `rands()`. They are numerically fragile inputs, not our bugs.
- **Who it affects:** byte-exact comparison with the pinned oracle, and BOSL2 meshes, which get different triangulations with the same volume.
- **Decision for the owner (upstream itself differs by platform):**
  - Option A: use `f64::mul_add` in these five places to match the pinned arm64 oracle. That departs from x86 and Windows OpenSCAD, and `mul_add` is a software fallback on wasm32.
  - Option B: keep plain arithmetic and document that the oracle comparison is noise-tolerant here.
  - Either way, record it in `docs/followups.md` next to the existing FMA entry, which only covers transforms and extrusions.

### 3. Our bug: an implicit union of three BOSL2 cubetruss parts comes out 7.3 mm³ too large.
- **Repro** (with `OPENSCADPATH` set to the directory holding BOSL2):
  ```
  include <BOSL2/std.scad>
  include <BOSL2/cubetruss.scad>
  union(){union(){cubetruss(extents=[2,3]); cubetruss(extents=[1,4,2]);} cubetruss(extents=[1,4,2], bracing=false);}
  ```
- **Volumes:** neoscad 85024.0; nightly with Manifold 85016.68; nightly with CGAL (exact) 85016.63.
- **What I narrowed down:**
  - `(A∪C)∪B` and `A∪(B∪C)` are correct in neoscad. So are all single parts and the pairs I checked with correct pairings. The same wrong volume comes from the implicit top-level form and from `render()` or `intersection()` wrappers.
  - The nightly is correct in every one of these forms.
  - The error disappears after an OFF round trip (OFF rounds to 6 significant digits). So it depends on exact coordinates, and the mesh stays manifold with one component.
- **Where it shows up:** BOSL2 example `cubetruss.scad` no. 1, volume off by 2e-4.
- **Suggested change:** dump the two operands at full precision, then run manifold-rust and C++ Manifold 3.5.2 `Boolean` on them. That will show whether this is a port bug or a v3.5.0→3.5.2 fix.

### 4. Out of the box, neoscad finds neither the default font nor MCAD.
- **Repro:** `neoscad -o t.svg t.scad` with `text("Hello");` and no `NEOSCAD_FONT_DIR` prints `WARNING: Can't get font` and "not a 2D object".
- **Repro:** `include <MCAD/units.scad>` without `OPENSCADPATH` fails in neoscad; the nightly prints `ECHO: 1`.
- **Cause:** OpenSCAD adds its resource `libraries/` directory (`parsersettings.cc:166-171`, and `--info` lists it). `LibraryPath::from_env` (`lang/src/loader.rs:57-80`) does not. The font half is a known followup.
- **Who it affects:** every user outside the test harness, including the phase 6 snapshots and the WASM build, which has no file system.
- **Suggested change:**
  - Embed Liberation 2.00.1 and MCAD (both licences are an owner decision: SIL OFL for the fonts, LGPL for MCAD).
  - Add a bundled-libraries directory to `LibraryPath`.

### 5. Some CLI flags are accepted and silently do nothing; some stable ones are missing.
Parsed but unused (`crates/cli/src/main.rs:101-181`, and nothing else in `crates/cli` reads them):
- `-d` writes no dependency file. The nightly writes one (`s3.stl: \ c.scad`).
- `--summary-file` writes no file. The nightly writes JSON, which is exactly the structured output the agent surface wants.
- `--summary all` omits the cache-size and bounding-box lines.
- `-m` is ignored.
- `--enable` has no effect: with `--enable all`, `textmetrics`, `object` and `roof` still say they are not enabled.

Missing; each is a usage error with exit 1:
- `--help-export`
- `--info`
- `--debug`
- `--animate_sharding`

Export formats:
- `pov` exits 3 as not implemented; it is a stable format in `src/io/export.cc:97`.
- `nef3` and `nefdbg` exit 3 too; they are CGAL formats.
- `png` is tier 4.

Suggested change: implement `-d` and `--summary-file` first. For `--enable`, either refuse it or honour the features we have. Decision for the owner: which experimental features, if any, to support. `textmetrics` is now cheap (see the followups).

### 6. WASM: everything compiles; the runtime blockers are recursion depth, threads, direct `std::fs` calls, and maths bits.
All crates build for `wasm32-unknown-unknown`, including `neoscad-cli` and `neoscad-geom` with default features. So compilation isn't the blocker; runtime is. I ran a harness over lang, eval, geom and text in node 18, with an in-memory `FileSystem`.

What works:
- `cube`, boolean `difference`, `minkowski`
- `text()` with fonts from `FontDb::add_data`
- `import()` and `include` through `FileSystem`

What blocks:
1. **Recursion depth.**
   - A recursive function at depth 1,600 (1,200 works) fails with V8's `RangeError: Maximum call stack size exceeded`.
   - A recursive module fails below depth 800 with `RuntimeError: memory access out of bounds`: the 1 MiB default wasm stack overflows.
   - Natively the nightly runs depth 2,000.
   - The recursion guard (`eval/src/eval.rs:280-313`, 48 MiB `DEFAULT_STACK_LIMIT`) never fires. It measures the wasm stack, and V8's native stack runs out first.
   - This needs a depth or budget scheme that doesn't depend on the stack, or a less stack-hungry evaluator. It ties to the followup on recursion depth deeper than the nightly's.
2. **Threads.**
   - geom's default `parallel` feature compiles but panics at run time: `evaluate.rs:366` `geometry thread pool: ... operation not supported on this platform`. Make it target-gated rather than relying on callers to pass `default-features = false`.
   - `eval::with_stack` (the 64 MiB thread, `eval/src/lib.rs:212-220`) must not be called on wasm.
3. **`std::fs` calls that bypass `FileSystem`.**
   - `dxf_dim`/`dxf_cross` (`eval/src/builtins/functions.rs:468`): I confirmed they print `Can't open DXF file` under the in-memory file system.
   - Import cache keys stat mtime and size (`eval/src/dump.rs:568-582`).
   - `FontDb::add_file` and `add_dir`, which `use <font.ttf>` relies on (`text/src/fontdb.rs:95,138,157`).
   - Customizer parameter files (`lang/src/customizer/params.rs:92`).
4. **Unseeded `rands()` always uses seed 0 on wasm** (`eval/src/eval.rs:1217-1229`). Every run gives `[0.592845, 0.844266]`.
5. **Maths results differ.** Rust's wasm32 math functions differ from macOS libm in the last bit on this share of 400 samples:

   | Function | Differ |
   |---|---|
   | sin | 20 |
   | cos | 14 |
   | tan | 163 |
   | asin | 40 |
   | acos | 64 |
   | exp | 48 |
   | powf(x, 1.37) | 30 |
   | powf(x, 3) | 42 |
   | atan2 | 12 |
   | ln | 7 |
   | atan | 4 |
   | sqrt | 0 |

   So WASM output will not be byte-identical to native or to the nightly. This confirms the CFF `powf(3.0)` followup. Decision for the owner: accept it, or ship one pure-Rust libm on every platform (that breaks today's bit-exact match with the nightly on macOS).

### 7. Shared upstream weakness: QuickHull sometimes returns a folded, non-convex hull. neoscad and the nightly fail on different inputs.
- **neoscad repro:** `minkowski(){cube([30,20,5],center=true); sphere(3,$fn=48);}`.
  - Volume is 9751.31, against 9751.42 in the nightly with Manifold and 9751.42 with CGAL.
  - One vertex sits 2.0 above a face plane. The fold is coplanar triangles with opposite normals at z = -3.914.
- **Another neoscad repro:** `hull() for (x=[-20,20], y=[-5,5], z=[-2,2]) translate([x,y,z]) sphere(r=4.5, $fn=40);`
- **The nightly fails on** `hull() for (x=[0,30], y=[0,30], z=[0,5]) translate([x,y,z]) sphere(r=3, $fn=16);` (fold height 10.4), which neoscad gets right.
- **Rate:** 40 random rounded-box hulls fail once in each binary. It is the same algorithm with different last bits in the inputs (the FMA in transforms), so different inputs trip it.
- **Who it affects:** rounded boxes made with `hull()` or `minkowski()`, which are very common. The volume error is small, but the mesh self-overlaps.
- **Suggested change:** after a 3D hull, check convexity. On failure, re-run or repair, for example by re-hulling the output vertices. Report both repros upstream, to Manifold and to manifold-rust.

### 8. Smaller items
- **Clipper2 version:** `docs/followups.md` says OpenSCAD pins Clipper2 2.0.1. The submodule at c7f820f is 2.0.1 plus 5 commits (checked on GitHub). But the oracle binary's `--info` reports `Clipper2 version: 1.5.3`, while `Manifold version: 3.5.2` matches. I could not confirm which Clipper2 is actually linked. Record both facts.
- **Stale harness limit:** `conformance/tier3-limits.json` lists `render-manifold_issue5216`, but it passes. neoscad leaves faces uncoloured where the nightly writes `0 0 0 0` (`export_off.cc:72-74`); the warnings match. It is a harmless divergence; drop the limit entry or match the bytes.
- **`conformance diff` can't be given a library path** (`diff.rs:344-348`), so library corpora need their includes rewritten. Add a `--library-path` option.
- **Doc drift:**
  - The two "Structure" followups about `architecture.md` (usvg, the text crate) are already resolved (`architecture.md:25-33`). Remove them.
  - `architecture.md:156` still says "tier 3, 660/660"; it is now 737.
  - `CLAUDE.md` says not to use 2021.01, while the architecture's benchmark series requires it. Scope that line to differential testing.

## Benchmark baseline

Data: `docs/audits/engine-milestone-bench.json`.

Method: wall time of export to ASCII STL, best of 3, run one after another. A run over 60 s is measured once; the timeout is 300 s. Machine: M4 Pro (10 performance + 4 efficiency cores), 48 GB, on AC power.

Conditions to keep in mind:
- 2021.01 is x86_64 only and runs under Rosetta 2.
- The nightly's floor is about 65 ms of process start (Qt and friends); neoscad's is about 12 ms. So the small models mostly measure start-up.

| Model | neoscad | nightly manifold | nightly cgal | 2021.01 | nightly ÷ neo |
|---|---|---|---|---|---|
| BOSL2 fractal_tree | 14.55 | 11.82 | >300 | >300 | 0.81 |
| BOSL2 helical gear | 0.071 | 0.222 | 0.222 | 0.366 | 3.1 |
| BOSL2 isosurface no. 6 | 1.29 | 2.50 | 2.51 | 5.26 | 1.9 |
| BOSL2 screws no. 1 | 0.553 | 0.668 | 31.96 | 72.39 | 1.2 |
| BOSL2 spring_handle | 0.643 | 0.756 | 13.57 | 41.29 | 1.2 |
| 800-cube union | 0.052 | 0.139 | 7.69 | 20.24 | 2.6 |
| cube − 125 spheres | 0.739 | 1.200 | >300 | >300 | 1.6 |
| examples/Basics/CSG | 0.013 | 0.066 | 0.819 | 2.281 | 5.3 |
| Menger n=3 (example024) | 0.160 | 0.161 | 15.31 | 46.51 | 1.0 |
| twisted extrude (400 slices) | 0.093 | 0.229 | 0.233 | 0.334 | 2.5 |
| import 21 MB STL − cubes | 0.171 | 0.561 | 10.37 | 37.51 | 3.3 |
| minkowski convex | 0.011 | 0.074 | 0.156 | 0.109 | 6.8 |
| minkowski non-convex | 0.015 | 0.067 | 0.368 | 0.923 | 4.4 |
| text, 30 lines | 0.646 | 0.685 | 0.653 | 1.411 | 1.06 |

Where neoscad is slower:
- **fractal_tree:** finding 1 (SHA-256 cache keys).
- **Menger n=4** (extra, neoscad and Manifold only): 3.09 s against 2.15 s. neoscad uses less CPU (8.1 s user against 12.6 s) but parallelises less. `sample` shows manifold-rust's boolean kernels on few threads. This is the known eager-against-lazy `BatchUnion` item in the followups.

## Feature coverage outside the suite

- **Builtins:** every builtin OpenSCAD registers (`Builtins::init` across `src/core`) is registered in neoscad (`eval/src/builtins/functions.rs:85-135`, `modules.rs:73-112`).
  - The experimental ones are known but disabled, which matches the architecture's scope: `textmetrics`, `fontmetrics`, `is_object`, `object`, `has_key`, `import()` as a function, and `roof`. They are gated in `Feature.cc:28-57`.
  - Also skipped by design: `lazy-union`, `vector-swizzle`, `unicode-identifiers`, `discretization-by-error` and `predictible-output`.
- **Import formats:** all present (stl, off, obj, 3mf, dxf, svg), except `nef3`, which is known.
- **Export formats:** everything except `pov`, `nef3`/`nefdbg` and `png`.
- **CLI flags:** see finding 5.
- **Known stable gaps:** `-O export-3mf/...` is not parsed (known).

## Followups triage (ranked by user impact)

Fix before phases 6–9:
1. **Fonts (and MCAD, finding 4):** before phase 6, where snapshots need text, and phase 9, which has no file system.
2. **File access outside `FileSystem`:** before phase 9. Also add fontdb and customizer params to the entry (finding 6).
3. **Recursion depth policy:** before phase 9. It is now a crash in WASM, not a compatibility preference.
4. **Geometry error paths printed relative to the main file's directory instead of the working directory:** before phase 7. JSON diagnostics and agents consume file paths.
5. **Determinism** (the order of `Slice`'s `HashSet`, the IDs from `compose_meshes`): before phase 7. `serve` cache keys and diffs depend on stable output.
6. **New, not yet in followups:** SHA-256 cache keys (finding 1) before phase 7; the arm64 FMA decision (finding 2); the cubetruss union bug (finding 3); hull validation (finding 7).

Can wait:
- **Performance:** eager union and Menger; OFF `fmt_g`; triangulating with many holes; the union of many `text()` nodes. Each costs about 1.5–2× in narrow cases, and neoscad wins 13 of 14 benchmarks.
- **System fontconfig names** (Arial, Helvetica): before phase 8 (the macOS app).
- **`-O export-3mf`** (3MF colour modes matter for multi-material printing): before phase 8.
- **Parity-only items:** kernel v3.5.0 against v3.5.2; hull start-vertex rotation; minkowski vertex counts; libtess2; FMA in transforms; Clipper2 version.
- **Low-impact items:** `--hardwarnings` timing; `\r` in include brackets; parameter JSON error text; the SVG arc step; 3MF and libxml diagnostics; the OBJ `bad_lexical_cast` case; `.nef3` import; PDF fidelity; the duplicate render warning.
- **Remove:** the two resolved architecture-doc entries.

## Checked and found fine

- The suite rerun: 1,103 passing.
- The BOSL2 tests: 976/976 pass in both binaries; echo output matches on 976/976.
- BOSL2 examples as echo: 2,525/2,526 identical.
- Maths functions match arm64 libm bit for bit.
- 2D BOSL2 output: SVGs byte-identical or passing the image comparison.
- Bounding boxes identical on all 31 3D models.
- Import speed: 3.3× faster than the nightly.
- `OPENSCADPATH` semantics.
- Builtin registration is complete.
- Every crate compiles for wasm32.
- Lib-crate WASM rendering of primitives, booleans, minkowski, fonts from memory, and import/include through `FileSystem`.

## Unverified

- That Apple clang's `-ffp-contract` is the mechanism behind finding 2. That arm64 and x86_64 differ is verified; the mechanism is inferred.
- Which Clipper2 the nightly actually links. `--info` reports 1.5.3; the submodule is 2.0.1+5.
- Whether the cubetruss union bug is in manifold-rust or upstream Manifold 3.5.0.
- WASM behaviour in browsers. I tested only node 18; stack limits vary by engine.
- Whether wasm32 neoscad matches OpenSCAD's own emscripten web build. Both use musl-derived maths, but I have not compared them.
