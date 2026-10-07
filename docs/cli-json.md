# `--summary-file`: the render summary as JSON

`neoscad --summary WHAT --summary-file FILE -o out.stl in.scad` writes the
render summary of a geometry export as one JSON value to `FILE` (`-` for
stdout) instead of printing it to stderr. It is the first piece of
neoscad's machine-readable surface for agents and scripts, so this schema
is a contract: fields are only ever added, never renamed, removed or
retyped, and every change is recorded here.

The layout is OpenSCAD's own (`StreamVisitor` in `src/RenderStatistic.cc`
of the reference checkout), so a consumer written against the nightly
reads neoscad's files unchanged. The implementation is
`crates/cli/src/summary.rs`.

## When it is written

- Only by geometry exports (`stl`, `off`, `obj`, `3mf`, `wrl`, `pov`,
  `svg`, `dxf`, `pdf`), after the file is written, once per output
  frame. `.echo`, `.ast`, `.csg`, `.term` and `.param` exports write no
  summary.
- A run that fails before the export (a parse error, an empty or
  wrong-dimension result) writes nothing.
- With `--summary-file`, the summary lines are not printed to stderr.
- If the file cannot be written, neoscad prints
  `ERROR: Can't write summary file '...'` and exits 1. (The nightly fails
  silently and exits 0.)

## Layout

Compact (no whitespace), object keys in sorted order, no trailing
newline. Doubles print as nlohmann's `dump()` does: the shortest digits
that read back exactly, `.0` on whole numbers (`140.0`), exponent form
outside 1e-4 to 1e15 (`1e+16`, `1.234e-05`), `null` for NaN or infinity.

## Top level

`--summary` selects sections; it may be repeated, `all` selects every
one, and unknown names are ignored. With no section selected the file
holds `null`. Otherwise it is an object with only the selected sections:

| Key | Selected by | Present when |
|---|---|---|
| `cache` | `cache` | always |
| `time` | `time` | always |
| `geometry` | `geometry` | the result is not empty |
| `camera` | `camera` | always |

`bounding-box` adds `geometry.bounding_box` (so it does nothing without
`geometry`). `area` affects only the stderr summary.

## `cache`

```json
{"cgal_cache": CACHE, "geometry_cache": CACHE}
```

`CACHE` is `{"bytes": int, "entries": int, "max_size": int}`:
entries in the cache, their estimated size in bytes, and the cache's
budget in bytes.

- `geometry_cache.entries`: geometries in neoscad's cache after the
  render. The count follows neoscad's caching, which differs from the
  nightly's (it caches different intermediate nodes).
- `geometry_cache.bytes`: the cache's estimated size after the render
  (`geom::Renderer::stats`). It is neoscad's estimate of its own
  geometry, so it differs from the nightly's for the same model (496
  against 856 for `cube(1)`). 0 after a preview, which reports no
  entries either.
- `geometry_cache.max_size`: the cache's budget, 209715200 (200 MiB) by
  default: the sum of OpenSCAD's two default cache sizes, since neoscad
  has one cache where OpenSCAD has two.
- `cgal_cache`: always `{"bytes":0,"entries":0,"max_size":0}`. neoscad has
  no CGAL backend and so no CGAL cache; its 100 MiB are in the geometry
  cache's budget. (The nightly's `max_size` here is 104857600, and its
  geometry cache's is the same.)

## `time`

```json
{"hours": int, "milliseconds": int, "minutes": int, "seconds": int,
 "time": "h:mm:ss.mmm", "total": int}
```

Time from the start of geometry evaluation (after the program was
evaluated) to the summary. `total` is milliseconds. The other fields
split it up, and `time` is the same clock as the stderr line.

## `geometry`

Depends on what the result is:

2D (`dimensions` 2):

```json
{"bounding_box": BBOX2, "contours": int, "convex": bool, "dimensions": 2}
```

3D mesh that was not through a boolean (a single primitive, an
extrusion; OpenSCAD's `PolySet`):

```json
{"bounding_box": BBOX3, "convex": bool, "dimensions": 3, "facets": int,
 "triangular": bool}
```

3D solid (the result of a boolean or `--render=force`; OpenSCAD's
Manifold geometry):

```json
{"bounding_box": BBOX3, "dimensions": 3, "facets": int, "simple": bool,
 "vertices": int}
```

`facets` counts faces as the geometry holds them: polygons for a
`PolySet`, triangles for a solid. `simple` is true when Manifold reports
no error. `bounding_box` is there only with `--summary bounding-box` (or
`all`): `{"max": [...], "min": [...], "size": [...]}` with two or three
doubles each, `size` being `max - min`.

## `camera`

```json
{"distance": double, "fov": double, "rotation": [x, y, z],
 "translation": [x, y, z]}
```

The camera as the nightly reports it: `--camera` if given, otherwise
OpenSCAD's default (translation 0, rotation 55/0/25, distance 140, fov
22.5), with the file's top-level `$vpt`/`$vpr`/`$vpd`/`$vpf` applied
unless `--camera` locked it (`Camera::updateView`). After a PNG export it
is the camera the image was drawn with, `--viewall` fitted
(`export_png` fits the same camera object the summary prints).

## Example

`neoscad --summary all --summary-file - -o x.stl cube.scad` for
`cube(1);`:

```json
{"cache":{"cgal_cache":{"bytes":0,"entries":0,"max_size":0},"geometry_cache":{"bytes":496,"entries":1,"max_size":209715200}},"camera":{"distance":140.0,"fov":22.5,"rotation":[55.0,0.0,25.0],"translation":[0.0,0.0,0.0]},"geometry":{"bounding_box":{"max":[1.0,1.0,1.0],"min":[0.0,0.0,0.0],"size":[1.0,1.0,1.0]},"convex":true,"dimensions":3,"facets":6,"triangular":false},"time":{"hours":0,"milliseconds":0,"minutes":0,"seconds":0,"time":"0:00:00.000","total":0}}
```

The nightly writes the same apart from the cache's byte count and
budgets (`"bytes":856`, and 104857600 for both `max_size`s).

# `neoscad snapshot --format json`

`neoscad snapshot MODEL.scad --format json` writes one JSON object to
stdout after the sheet is written (`crates/cli/src/snapshot.rs`). The
same contract holds: fields are only added. Unlike the summary above
this is neoscad's own format, printed with `serde_json` (keys sorted,
compact, a trailing newline). Timings vary from run to run; everything
else is the same for the same input.

```json
{"diagnostics": DIAG, "geometry": GEOM|null, "input": "model.scad",
 "lighting": "headlight"|"openscad",
 "mode": "render"|"preview"|"diff", "output": "model-snapshot.png",
 "schema": 1, "size": [1024, 1024], "timings_ms": TIMES,
 "views": ["iso", "front", "top", "right"],
 "diff": DIFF, "preview_bbox": BBOX|null}
```

- `schema`: 1.
- `input`, `output`: the paths as given (the default output is the
  model's file stem with `-snapshot.png`, in the working directory).
- `mode`: what the sheet shows. `render` (the default) draws the rendered
  geometry; `preview` OpenSCAD's preview (`--preview`); `diff` the
  comparison (`--diff`).
- `views` and `size`: the panels in order and the sheet's pixel size.
- `lighting`: `headlight` (the default: one light at the camera, so no
  visible face is drawn near black) or `openscad` (`--lighting openscad`:
  OpenSCAD's two fixed lights, as PNG export draws). See
  `render::Lighting`.
- `geometry`: the model's rendered geometry, or `null` when it is empty
  or with `--preview` (a preview does not compute the booleans). 3D:

  ```json
  {"dimensions": 3, "bbox": BBOX, "volume": double, "area": double,
   "triangles": int, "vertices": int, "manifold": bool,
   "components": int}
  ```

  `volume` (mm³) and `area` (mm²) are Manifold's. A result that is a
  mesh rather than a solid (a lone primitive, an extrusion) is converted
  as `--render=force` would convert it first. `manifold` is false when
  Manifold reported an error or had to keep the mesh as a triangle soup,
  or when the solid is *pinched*: then `"pinched": {"edges": int,
  "point": [x, y, z]}` is added. Pinched means that once corners at the
  same position are merged, as an STL reader does, edges are shared by
  more than two faces (`point` is the midpoint of the first, to 6
  significant digits). Two pieces touching along an edge (a rib ending
  exactly on a rim, two cubes sharing an edge) do this: Manifold keeps
  a vertex for each piece and reports no error, but a file of the
  result is not manifold. `"touch_only": true` is added to `pinched`
  when the solid has no volume (under 1e-6 mm times its area): the
  result is only faces pressed together, which is what an
  `intersection()` of parts that only touch gives, and there is no
  overlap to remove (the MCP tools' and `check`'s fix then says so). When rounding the corners to 32-bit floats
  (as binary STL stores them and slicers read either kind of STL)
  leaves edges shared by other than two faces that the exact weld does
  not, `"stl_precision": {"collapsed_faces": int, "nonmanifold_edges":
  int, "point": [x, y, z], "spacing": double}` is added and `manifold`
  is unchanged: the solid is manifold, a slicer's reading of its STL is
  not. `collapsed_faces` counts triangles two of whose corners become
  one point, `nonmanifold_edges` the edges beyond the exact weld's,
  `point` the midpoint of the first, and `spacing` the gap between
  32-bit floats at the model's largest coordinate (mm). Slivers from
  surfaces lying on each other (a core cylinder at exactly a thread's
  root radius) or very fine tessellation do this. Faces that only
  collapse, with every edge still paired, are left out here (see the
  `stl-precision` finding). `components` counts the pieces whose faces
  share no vertex. 2D:

  ```json
  {"dimensions": 2, "bbox": BBOX2, "area": double, "contours": int}
  ```

- `BBOX` is `{"min": [x, y, z], "max": [...], "size": [...]}` in mm
  (two numbers each in 2D).
- `preview_bbox`: with `--preview`, the box the preview fits
  (`OpenCSGRenderer::getBoundingBox`: the products', including `%` and
  `#` objects; 2D shapes as their one-unit slabs); `null` when there is
  nothing to draw. Absent otherwise.
- `diff`, only with `--diff OTHER`:

  ```json
  {"other": "OTHER.scad", "added_volume": double,
   "removed_volume": double, "unchanged_volume": double,
   "other_geometry": GEOM|null}
  ```

  `added_volume` is MODEL − OTHER, `removed_volume` OTHER − MODEL,
  `unchanged_volume` their intersection, in mm³, from real booleans on
  the two rendered solids (a 2D model as its one-unit slab).
- `timings_ms`: `evaluate` (parsing and evaluation), `geometry` (the
  render or the preview's products, both models and the diff booleans
  with `--diff`), `gpu_init` (opening the GPU), `draw` (the panels and
  the sheet), `encode` (the PNG) and `total`, rounded to 0.1 ms.
- `diagnostics`: `errors`, `warnings` and `echoes` count the lines the
  run logged (`ERROR:`, `WARNING:`, `ECHO:`); `messages` holds the first
  20 errors then the first 20 warnings, and `echo` the first 20 echoes,
  verbatim. `items` holds the same errors and warnings as structured
  diagnostics (see "Diagnostics" below). The same lines go to stderr as
  they are printed.
- `parts` (phase 7b), with `--enable part` and a 3D model that has
  parts: their dotted names, in the order their colours are assigned.
  The sheet then draws each part in its own colour with a legend (at
  most eight entries).
- `highlight`, with `--highlight PART[,PART]`: the names given. Those
  parts (and the parts nested in them) keep their colours; the rest are
  drawn translucent grey. A name that is not a part fails the snapshot
  with the list of parts.
- `issues`, with `--issues`: the `check` result's `counts` and
  `findings` (see "`neoscad check`" below; same objects and `id`s).
  The sheet paints thin-wall faces red, overhangs amber and floating
  pieces purple, and puts a numbered marker at each error and warning's
  `location.point`, numbered by its `id`. `--highlight` and `--issues`
  draw the rendered model, so they cannot be combined with `--preview`
  or `--diff`.

When the model (or the `--diff` model) cannot be loaded or evaluated
(a syntax error, `--hardwarnings`), no sheet is written and the summary
is only

```json
{"schema": 1, "input": "model.scad", "failed": true, "exit_code": int,
 "diagnostics": DIAG}
```

with the process's exit code (1 for an error, 3 for a feature neoscad
does not have yet).

Example, `neoscad snapshot cube.scad --format json` for `cube(10);`
(timings elided):

```json
{"diagnostics":{"echo":[],"echoes":0,"errors":0,"messages":[],"warnings":0},"geometry":{"area":600.0,"bbox":{"max":[10.0,10.0,10.0],"min":[0.0,0.0,0.0],"size":[10.0,10.0,10.0]},"components":1,"dimensions":3,"manifold":true,"triangles":12,"vertices":8,"volume":1000.0},"input":"cube.scad","mode":"render","output":"cube-snapshot.png","schema":1,"size":[1024,1024],"timings_ms":{...},"views":["iso","front","top","right"]}
```

# `--format json`: a run as one JSON object

`neoscad IN -o OUT --format json` (any output format) prints one JSON
object describing the run on stdout after it ends, instead of printing
its messages on stderr (`crates/cli/src/report.rs`). When an output is
`-` (the data owns stdout) the object goes to stderr. Same contract:
fields are only added. Keys sorted, compact, a trailing newline.

```json
{"schema": 1, "command": "export", "input": "model.scad",
 "outputs": [{"file": "model.stl", "format": "stl"}],
 "exit_code": 0, "served": false,
 "counts": {"errors": 0, "warnings": 1, "echoes": 1},
 "diagnostics": [DIAG, ...], "echo": ["ECHO: ..."], "log": ["..."],
 "geometry": GEOM|null, "timings_ms": TIMES}
```

- `outputs`: each `-o`, with the format identifier it was written as.
- `exit_code`: the process's (it exits with it too).
- `served`: whether a running `neoscad serve` did the work
  (`docs/serve-protocol.md`).
- `diagnostics`: errors, warnings and deprecations in order, as
  structured diagnostics (below); each error has the `TRACE:` lines that
  followed it in `trace`.
- `echo`: `echo()` output as printed. `log`: every plain line (render
  summary, `Current top level object is empty.`, ...).
- `geometry`: the rendered geometry of a geometry export (or a
  `--render` PNG), the snapshot summary's `GEOM` object; `null` for
  other formats, or when empty.
- `timings_ms`: `total` for a local run; `parse`, `evaluate`, `geometry`
  and `total` for a served one.
- `exact`: only when a `.step` file was asked for (next section).

Usage errors (a bad flag, no `-o`) happen before the run and print their
usual text with no JSON.

# STEP with exact surfaces: `--enable exact`

`neoscad --enable exact -o model.step model.scad` (or `.stp`, or
`--export-format step`) writes STEP AP214 whose faces are the exact
planes, cylinders, cones and spheres of the model, not its triangles
(`crates/geom/src/exact`; `docs/audits/exact-geometry-rust.md`). It is a
NeoSCAD extension: without the flag `.step` is an unknown suffix, with
OpenSCAD's own `Invalid suffix step...` error, and `--enable all` does
not turn it on.

- **Which curves become exact:** a `cylinder()` or `sphere()` whose
  fragments come from `$fa`/`$fs` is written as the true surface (the
  mesh's polygon is inscribed, so the solid grows by up to the
  fragments' sagitta). An explicit `$fn` keeps OpenSCAD's polygon, which
  is planar and so written exactly as modelled (`$fn = 6` hexagons,
  polygon-sized printing holes). Rotations, mirrors and uniform scales
  keep curves exact; a non-uniform scale or shear writes them as facets.
- **Everything else** (`polyhedron`, `hull`, `minkowski`, `text`,
  `import`, extrusions, `offset`, `resize`) is written as the planar
  facets of the mesh render, in the same solid as the exact faces.
- **Every substitution is reported** at its source line, once per
  location with a count: `INFO: STEP export: sphere() is exported as an
  exact sphere, not the 30-fragment polyhedron of the mesh ($fn is not
  set) in file m.scad, line 3`; a kept polygon is `INFO`, a faceted
  region `WARNING` (so `--hardwarnings` refuses it).
- **No silent wrong file:** the B-rep is validated, its volume compared
  with the export mesh's corrected onto the exact surfaces and with the
  rendered mesh's, and its box with the render's. A failure prints
  `ERROR: STEP export failed: REASON. No file was written.` and exits 1.
  A mesh whose topology does not match the exact model at a tangency is
  retried once at twice the segments.
- **Deterministic:** fixed header names and date (`1970-01-01T00:00:00`,
  originating system `NeoSCAD`, product named after the input file), so
  the same model gives the same bytes at any thread count, warm or cold,
  natively and in wasm32.

With `--format json`, the run object has an `exact` key. For
`difference() { cube(20); translate([10, 10, -1]) cylinder(r=4, h=22); }`
(numbers shortened; the exact volume is 8000 - 320π):

```json
"exact": {"ok": true, "error": null, "attempts": 1, "retried_because": null,
 "triangles": 80, "faces": 7, "exact_faces": 7, "edges": 14, "bspline_edges": 0,
 "volume": 6994.690350851, "corrected_mesh_volume": 6994.690350812,
 "volume_error": 5.5e-12, "volume_tolerance": 1.2e-7, "normal_volume": 7033.375802149,
 "substitutions": {"exact": 1, "polygon": 0, "faceted": 0, "faceted_modules": []},
 "notes": [], "normal_render_ms": 4.2,
 "timings_ms": {"export_render": 0.4, "reconstruct": 0.2, "check": 1.5, "write": 0.1}}
```

- `faces`, `exact_faces`: B-rep faces, and those on an exact surface
  (planes included) rather than faceted; `edges` excludes seams.
- `volume`: integrated on the exact geometry; `corrected_mesh_volume`
  the export mesh's volume plus its triangles' caps up to the surfaces;
  `volume_error` their relative difference, held to `volume_tolerance`
  (relative), which follows from the tessellation.
- `normal_volume`, `normal_render_ms`: the mesh render's, for comparison.
- `faceted_modules`: the modules that fell back to facets.

`neoscad serve` does not export STEP yet, and does not list `exact` in
its features.

# Named parts: `--enable part`

`part("name") { ... }` is neoscad's language extension for naming the
pieces of a model, so that `check`, `measure` and `snapshot` can refer
to them. It is off by default: without `--enable part` (the `parts` or
`enable` option of a server request, `session::Run::extensions` or
`session::Config::extensions`), `part` is an unknown module exactly as in
OpenSCAD, with its warning (`WARNING: Ignoring unknown module 'part'
...`), and `--enable all` does not turn it on. A program's own `part`
module always wins over the extension.

- Geometrically a part is a union of its children; the rendered model
  is the same with or without it (the triangles may be grouped
  differently in exported files).
- Nested parts have dotted names: a `hinge` part inside a `lid` part is
  `lid.hinge`. A name used twice warns `Duplicate part name 'lid'`
  (code `duplicate-part`); instances of one name are measured and
  checked as one part. A name that is not a non-empty string warns and
  the children are kept as a plain group.
- The `.csg` export shows `part(name = "lid") { ... }` nodes, only when
  the extension is on.
- Each output face remembers its part through Manifold's original IDs,
  across booleans and colours; faces a `difference()` cuts into a part
  belong to that part. Parts are tracked in 3D only.
- A part's own solid (for `measure` and the checks between parts) is
  its subtree's geometry, placed by the transforms above it. When an
  operation above it changes what reaches the model, the part's
  `context` names it: `difference` (a subtracted part), `intersection`,
  `hull`, `minkowski`, `resize`, or `2d` (projected, extruded, offset).

# Sorted exports: `--enable=predictible-output`

One of OpenSCAD's experimental features, off by default as there. As in
OpenSCAD
(`src/io/export.cc`, `createSortedPolySet`), the STL, OBJ, 3MF, OFF,
WRL and POV writers first sort the mesh: `-0` becomes `0`, equal
positions merge, vertices are in (x, y, z) order, each face starts at
its lowest vertex (keeping its winding) and the faces are sorted, each
keeping its colour. STL, OBJ and 3MF are triangulated first and then
sorted. The file then no longer depends on the order the geometry
kernel emitted, which is what OpenSCAD's own export tests rely on.
`--enable all` turns it on too. Off by default; without it the files are
byte-identical to before.

It is one flag in the same feature set as `textmetrics` and the others
(`eval::Features`), so every host takes it where it takes those:
`"enable": ["predictible-output"]` on a server request (`export`,
`cli.export`), `RunOptions.enable` in the app's core, and
`neoscad mcp --enable predictible-output` for every MCP call (a
server-wide flag, so the tool schemas do not grow).

# `neoscad check`

`neoscad check MODEL.scad [--bed WxDxH] [--nozzle MM] [--min-wall MM]
[--max-overhang DEG] [--enable part] [-D var=val] [--format json]`
checks a model's rendered solid for FDM printing
(`crates/session/src/check.rs`). Without `--format json` it prints a
summary line and one line per finding with its fix; with it, one JSON
object on stdout (keys sorted, compact, a trailing newline). The
model's own messages go to stderr. Exit status: 0 when no finding is an
error, 1 when one is or when the model fails to load, evaluate or
render. A running `neoscad serve` does the work when there is one.

Defaults, for a common FDM printer: `nozzle` 0.4 mm, `min_wall` twice
the nozzle (0.8 mm: two perimeters), `max_overhang` 45° from vertical,
no bed (the bed-fit check runs only with `--bed`), `bed_tolerance` 0.05
mm (how far above the lowest point a piece may start and still count
as on the bed), at most 10 findings per code.

```json
{"schema": 1, "input": "model.scad", "ok": bool, "exit_code": 0|1,
 "settings": {"bed": [w, d, h]|null, "nozzle": 0.4, "min_wall": 0.8,
              "max_overhang": 45.0, "bed_tolerance": 0.05,
              "max_findings": 10},
 "model": MODEL, "parts": [PART, ...],
 "counts": {"errors": int, "warnings": int, "info": int},
 "findings": [FINDING, ...], "truncated": {"code": int, ...},
 "timings_ms": {"evaluate", "geometry",
                "check": {"manifold", "components", "walls",
                          "overhangs", "parts", "total", "cuts"},
                "total"},
 "diagnostics": DIAG}
```

- `MODEL` (3D): `{"dimensions": 3, "manifold", "components",
  "floating", "cavities", "volume", "area", "centroid", "bbox", "triangles",
  "min_wall": {"thickness", "point", "part", "sampled": true}|null,
  "overhang_area"}`; `{"dimensions": 2}` for a 2D model, `null` for an
  empty one. `components` counts connected surfaces, so a hollow's
  inside is one of them; `cavities` says how many of them are (see the
  `cavity` finding). `min_wall` is the thinnest wall any sample measured (after
  the layer-plane measurement and the corner samples below); `sampled`
  says so: the true thinnest wall can be a little under it (the text
  says "thinnest wall about 1.21 mm (sampled)"). `manifold` is false for a pinched
  solid too (see the snapshot's `geometry`).
- `PART`: `{"name", "instances", "context", "dimensions", "manifold",
  "components", "volume", "area", "bbox"}` for each part's own solid.
- `FINDING`: `{"id": int, "severity": "error"|"warning"|"info", "code",
  "message", "part": name|null, "location": {"point": [x, y, z],
  "bbox": BBOX}, "fix", "value": number|null, "limit": number|null}`.
  `id`s count from 1 in order: problems of the input meshes
  (`polyhedron-*`) first, since they cause others, then errors. `value` is what was
  measured (mm, mm², mm³ or degrees, as the message says) and `limit`
  what it broke. Numbers are rounded to 0.1 µm.
- `counts` are before truncation; `truncated` counts, per code, the
  findings past the limit.
- `sketches` (only when the model has constrained sketches, `--enable
  sketch`; `docs/sketch.md`): `[SKETCH, ...]`, each solved sketch in
  tree order, at most 100 (`sketches_omitted` counts the rest; a sketch
  in a module called twice is two). `SKETCH`: `{"name": string|null,
  "file", "line", "span", "status": "fully-constrained" |
  "underconstrained" | "conflict" | "not-converged" | "error", "dof",
  "unknowns", "equations", "rank", "iterations", "residual",
  "continuation": bool, "empty": bool, "codes": [code, ...]}`. `empty`
  says an error left the sketch without a shape (an under-constrained
  sketch with `strict = true` is `underconstrained` and empty); `codes`
  are those of the diagnostics printed about it, each once, which
  `diagnostics` has in full. The text report has a line per sketch
  (`sketch 'slot' (line 4): fully constrained, 12 unknowns`). The same
  JSON is in the failed form too, where a model with sketch errors that
  rendered nothing still lists them.
- `timings_ms.check.cuts` is the `cut-away` and `cuts-nothing` stage,
  which runs on the node tree right after the render, before the mesh
  checks; `check.total` does not include it.

Codes and how each is found:

| Code | Severity | What |
|---|---|---|
| `empty` | error | Nothing to print. |
| `not-3d` | error | A 2D model. |
| `not-closed` | error | A mesh result (a lone polyhedron) with edges on one face only. |
| `not-manifold` | error | Manifold reports an error or kept the solid as a triangle soup, or edges are shared by more than two faces. Also a pinched solid: Manifold calls it valid, but once corners at the same position are merged (as an STL reader does) edges have more than two faces. Then `value` is the number of such edges, `point` the midpoint of the first and `bbox` the box around all of them, and the fix says that two parts touch along an edge or at a point there and to overlap them by at least 0.01 or separate them; when the result has no volume (`touch_only` above) it says the parts only touch (no overlap) instead. |
| `floating` | error | A connected piece (triangles sharing vertices) whose lowest point is more than `bed_tolerance` above the model's lowest point. `point` is the piece's centre. The message says what is under it, straight down from its lowest points: another piece it rests on (within `bed_tolerance`), another piece N mm below, or nothing. |
| `cavity` | info | The inside surface of a sealed void (`difference() { cube(20); translate([.5,.5,.5]) cube(19); }`): a component wound inward (negative signed volume; Manifold winds every shell it outputs outward from the material) whose box lies inside a component wound outward, in a valid solid. It is not a `floating` piece and has no `tiny-feature`; it is excluded from both. `value` is the void's volume, `point` its centre. The fix says FDM needs nothing if the hollow is intended, and resin or powder printing needs a drain hole. A piece sealed inside the void is still a piece (`floating`, resting on or above the void's floor). |
| `thin-wall` | error below `nozzle`, else warning below `min_wall`, each by more than 0.001 mm (a wall modelled at the minimum measures a hair either side of it; the message gives the thickness to the ten-thousandth when the hundredth would read as the limit); info when the finding's faces add up to under 0.05 mm² (a sliver where sharp edges meet, such as a thread's crests: the message starts "a sliver, not a wall:"; the thinnest wall a 0.4 mm nozzle prints is 0.08 mm² of one layer) | From points on every face (the centroid, or 4 or 16 points on faces larger than (4 × `min_wall`)²) a ray goes inward along the face's normal to where it leaves the solid, ignoring faces that share a corner with the start (so knife edges do not measure zero) and exits through faces more than 45° from parallel (corners and slopes are not walls). A reading under `min_wall` (or under the thinnest so far) is measured again in the layer plane, along the face's normal projected onto XY, and the larger of the two is the wall: FDM lays a wall as perimeters in each layer, so the width that matters is the width in the layer, and the projected normal is exactly the in-layer normal of the outline the face cuts, however the face is tilted. (The slivers of a twisted `linear_extrude` tilt their normals up to 76°; along them a solid 20 mm square measured walls of 0.17–0.25 mm at its end caps.) A flat face has no layer direction: its reading counts only when the far side is flat too (a plate, a floor); through a sloped face it is a wedge where a slope meets a cap, not a wall. A reading from a face's middle is too thick where a wall tapers (a barb's 1.2 mm rim read 1.39), so the faces whose readings could hide one thinner than the thinnest so far (reading minus the distance from the face's middle to its farthest corner) are measured again from just inside each corner (a tenth of the way, at most 0.1 × `nozzle`), most promising first, at most max(128, one in 128 of the faces); near a corner, a layer-plane ray that leaves through a face not across from it (a plate's end) makes no reading. Thin faces that share an edge, or face each other across a wall, are one place; places of one part and severity within max(4 × `min_wall`, 5% of the model's diagonal) are one finding ("walls at N places"), located at its thinnest point. An exit closer than min(0.01 mm, 1e-4 of the diagonal) behind which the ray leaves through another face facing its way is a contact seam (two pieces that touch keep both surfaces), not a wall: the wall is measured to that second exit, and the seams are one `touching-surfaces` finding. |
| `touching-surfaces` | info | Surfaces of pieces that touch with no gap (coils of a spring, a lid on its box): they print fused. `value` is 0; the message gives the area. The fix: leave a gap of at least the nozzle if they should be separate, overlap them a little if they should be one. |
| `overhang` | warning; info for a short bridge or thread flanks | Faces pointing down more than `max_overhang` + 0.5° from vertical (a face modelled at the limit comes out a hair past it), except faces within `bed_tolerance` of the lowest point, grouped into regions by shared edges; regions under (2 × `nozzle`)² are ignored. Regions of one part within max(4 × `min_wall`, 5% of the model's diagonal) of each other are one finding ("in N places"), as thin walls are. `value` is the finding's area. The message gives the steepest angle (that of the steepest faces covering (2 × `nozzle`)², so a sliver does not set it), the heights the finding spans ("z 0 to 11.94"), and, when only part of it is steeper than `max_overhang` + 15° (at most 89°), that area and its heights ("41.1 mm² of it steeper than 60° (z 11.9)"). `point` is on the steepest faces (the centroid of the largest of them), not on the largest region: a 90° ledge's finding pointed at a 60° thread flank 5 mm below it. Two kinds of region are info, each merged only with its own kind. A **short bridge** (message "a 9.5 mm bridge between walls: ..."): every face within 1° of horizontal, and along x or y the region's box has, at both ends, boundary edges from which the face across goes down (walls holding the span up), at most 20 mm apart; a ledge held up on one side, or a slab on a post, has no such pair and stays a warning. **Thread flanks** (message "thread flanks (a band 1.23 mm deep winding round a vertical axis): ..."): about the vertical axis through the region's box centre, its vertices lie in a band at most 2.5 mm deep and less deep than its inner radius, its faces' centroids reach all twelve 30° sectors, it climbs more than it is deep (a ring or chamfer round a boss, at any angle past 45°, climbs less), and under (2 × `nozzle`)² of it leans more than `max_overhang` + 30° (so a square thread, or flanks running into a ledge, stay a warning). |
| `bed-fit` | error, or warning when turning it 90° about z fits | With `--bed`: the bounding box against the bed. |
| `tiny-feature` | warning | A piece whose largest extent is under two nozzle widths. |
| `parts-intersect` | warning | Two parts (neither nested in the other, both reaching the model as themselves) whose solids overlap: `value` is the overlap volume, by a boolean intersection. |
| `part-not-manifold` | error | A part's own solid is not valid, or is pinched. |
| `cut-away` | warning | An object in a `difference()`'s first child that the subtracted children remove entirely: posts in a `union()` with an enclosure's shell, and the cavity subtracted from both. Only differences written in the user's own files (the main file's directory and below) are looked at, not a library's. The first child is taken apart through unions, groups, module calls, `for` loops, colours, `render()` and transforms (while what is inside is the user's too); each piece is an object, and the objects one call made (four posts from one `for` loop) are one finding. An object is cut away when the subtracted children take at least a tenth of it and leave under 1% of its volume; or leave more, but only a stub (at most three quarters of the object's extent along some axis) inside the other objects, as of a post sunk into the floor. A ring left by a bore through an object buried in another (a taper inside the stem it should stick out of) is not a stub, and is not reported: the cut did not hide it. Objects thinner than 0.05 mm are ignored. The message names the difference and the call, as `file:line`; the fix names the subtracted child that took the most and says to add the objects after the subtraction. `value` is the volume left (mm³) and `limit` 1% of the objects' volume. |
| `cuts-nothing` | warning | A subtracted child of such a difference that removes nothing: its box misses the first child's, or its overlap with the objects it reaches has no volume ("it does not touch the first child"), or everything it reaches is removed by the other subtracted children already (a cutout placed in the cavity instead of through the wall). Reported per call, only when every instance of the call that was compared removed nothing, so a loop of holes of which one falls off the end is not a finding. Background (`%`) children are not operands and are left out. |
| `stl-precision` | warning, or info when no edge breaks | A valid solid's corners welded by 32-bit float position, as a slicer reads an STL (binary STL stores `f32`; slicers parse ASCII STL into `f32` too), beyond what the exact weld merges. A warning when that leaves edges shared by other than two faces: the solid checks manifold but its STL does not (the CAD pilot's twisted thread: 2998 triangles collapse and 738 edges break, the grader's count). `value` is the number of such edges, `point` the midpoint of the first, `bbox` the box around them. Info when triangles only collapse and every edge still pairs: a slicer drops the zero-area facets and the rest is closed, which Clipper-snapped slivers also give (3 of the 532 reference inputs), so it is not a warning; `value` is then the count and `point` the first one's centroid, and the message ends "so no action is needed" (the text report and the MCP tools' terse results leave its fix out; the JSON keeps it). The message gives the 32-bit spacing at the model's largest coordinate; the fix says to overlap or separate coincident surfaces by 0.01 or more, or coarsen the tessellation, keeping vertices more than 100 times that spacing apart. |
| `off-bed` | info | The model's lowest point is not at z = 0. |
| `polyhedron-inside-out`, `polyhedron-flipped-faces`, `polyhedron-open`, `polyhedron-not-manifold` | warning | A `polyhedron()` or imported mesh that does not bound a solid: the diagnostics of the same codes (see "Input meshes" under "Diagnostics"), as findings. The message ends with the call as `file:line`; `point` is in the model's coordinates. When a winding problem is among them, a `not-manifold` finding's fix says to fix that one first (`fix #1 first: an inside-out or partly flipped polyhedron is the likely cause, ...`) instead of to overlap the parts: booleans with an inside-out mesh leave pinched edges. |

Cost of `cut-away` and `cuts-nothing`: the geometry is the render's own
(its cache), and a mesh is converted to a solid or moved only when a
boolean needs it; with one object, or one subtracted child, the volumes
of the object and of the result answer without one. Booleans are capped
at 200,000 operand triangles a check, and 50,000 each; at most 64
distinct differences and 64 objects in one are looked at. A finding
that would need more is not reported.

Accuracy: on the synthetic models of `crates/session/tests/check.rs`
the thickness of a 0.3 and a 0.5 mm wall, a 200 mm² overhang, a 45°
chamfer at a 30° limit (141.42 mm²), a floating cube's 5 mm lift, the
overlap of two parts and a thin part's name come out exact. Rays
measure along the normal and in the layer: a wall whose sides are not
parallel measures thicker than its narrowest point, a leaning plate
measures its width in the layer (0.577 mm for a 0.5 mm plate leaning
30°), and a feature narrower than the sample spacing on a large face
can be missed. Twisted extrusions (squares and circles twisted 90° to
2160°, threads made by twisting an offset circle) have no thin walls,
and a 0.3 mm fin, a 0.5 mm open box and a 0.4 mm twisted fin are still
found. A flat span between two walls (a bridge) is reported as an
overhang.

# `neoscad measure`

`neoscad measure MODEL.scad [--part P] [--between A B] [--section
z=H|x=H|y=H] [--axis x|y|z] [--center A,B] [--profile FROM:TO:STEP]
[--sketch NAME] [--svg FILE] [--enable part|sketch] [-D var=val]
[--format json]` (`crates/session/src/measure.rs`). Exit status 0, or 1
when the model fails or a named part or sketch does not exist (then
`error` says which there are).

```json
{"schema": 1, "input": "model.scad", "exit_code": 0,
 "model": SOLID|GEOM2D|null, "parts": [SOLID + {"name", "instances",
 "context"}, ...],
 "between": BETWEEN, "section": SECTION|null, "profile": PROFILE|null,
 "sketch": SKETCH,
 "timings_ms": {"evaluate", "geometry", "measure", "total"},
 "diagnostics": DIAG}
```

- `SOLID`: `{"volume", "area", "bbox", "centroid", "triangles"}` (the
  model's also `"dimensions": 3, "components", "manifold"` and, when
  pinched, `"pinched"` as in the snapshot's `geometry`), in mm, mm²
  and mm³, rounded to 1e-6. `centroid` is the centre of mass of the
  enclosed volume at uniform density. A 2D model is the `GEOM` object
  of the snapshot summary.
- `parts`: every part, or with `--part P` that part and the parts
  nested in it.
- `between` (with `--between A B`): `{"a", "b", "distance",
  "touching", "overlapping", "overlap_volume", "overlap_bbox",
  "points"}`, and when they overlap `"overlap_pieces": int, "pieces":
  [{"volume", "bbox"}, ...]`: the overlap's separate pieces (connected
  parts of the intersection), largest first, at most 10 listed.
  Overlap is a boolean intersection of the two solids
  (then `distance` is 0); otherwise `distance` is the exact smallest
  distance between their surfaces (triangle to triangle, over bounding
  volume hierarchies) and `points` the closest points on A and on B.
  `touching`: within 1 µm.
- `section` (with `--section`): `{"plane": "z=5", "axes": ["x", "y"],
  "area", "perimeter", "contours", "bbox", "part"?, "svg"?}` for the
  cut through the model (or `--part`'s solid): `axes` are the section's
  2D axes (x, y for z; y, z for x; x, z for y); `bbox` is in model
  coordinates; `svg` is the file written with `--svg` (the outline in
  mm, the second axis up, holes by the even-odd rule). Added: `"axis"`,
  `"center"` and `"outlines": [{"area", "hole": bool, "bbox",
  "radius": [min, max]}, ...]`, each contour (largest first, at most
  20; `contours` counts all) with its nearest and farthest distance
  from the axis: the line parallel to `--axis` (default z) through
  `--center` (its position in the other two coordinates, in the order
  of `axes`; default the origin). Across a thread the outer contour's
  radii are its minor and major radius; a bore is a hole.
- `profile` (with `--profile FROM:TO:STEP`, at most 1,000 samples):
  `{"axis", "center", "from", "to", "step", "radius": [min, max]|null,
  "crests": [h, ...], "pitch": number|null, "pitch_span": [h0, h1]|null,
  "bands": [[h, rmin, rmax], ...]}`. At each height `h` along the axis
  the solid is cut across it; `rmin` and `rmax` are the nearest and
  farthest distance of the outer contours from the axis (`null` where
  nothing is cut). A helical thread's cut is the same at every height,
  turned, so there these are its minor and major radius; over a barb
  they are its root and crest. `crests` are the local maxima of the
  outer surface's radius on one side (the half-plane from the axis
  towards the first of `axes`, +x for z), at most 100, each at the
  middle of its top, found between the samples (the flanks crossed at
  two levels below the top and extended to it; up to 100 crests are
  refined, about 70 more cuts each), not at the sample that hit it.
  `pitch` is the mean spacing of the longest run of evenly spaced
  crests, leaving out an end crest of the run that is cut off by the
  range, lower than the others, or narrower or wider on top
  (`pitch_span`: the first and last crest fitted). The pilot's M24x2
  adapter gives pitch 2 over crests 2..10 and radii 10.64..11.64 in
  the thread.
- `sketch` (with `--sketch NAME` and `--enable sketch`): the first
  sketch named `NAME`, as `check`'s `SKETCH` plus `"instances"` (how
  many sketches have that name) and `"entities": [ENTITY, ...]`, every
  entity in creation order: `{"id": int, "name": string|null, "kind":
  "point"|"line"|"arc"|"circle", "construction"?: true, "file",
  "line", "span"}` and its solved values, absent when the sketch did
  not solve: a point's `"at": [x, y]`; a line's `"start"`, `"end"`,
  `"length"` and `"angle"` (degrees counter-clockwise from +x); an
  arc's `"center"`, `"start"`, `"end"`, `"radius"`, `"sweep"` (degrees,
  in its own direction) and `"cw"?: true`; a circle's `"center"` and
  `"radius"`. `name` is the variable holding it (`top`, `top.start`
  for a point made from coordinates, `pts[0]`). Numbers are rounded to
  1e-9. The text report has a line per entity (`top line [0, 4]..[30,
  4], length 30, angle 0°`).

# `neoscad fmt`

`neoscad fmt [PATHS...] [--check] [--diff] [--stdin] [--indent N]
[--width N] [--format json]` formats OpenSCAD files
(`crates/session/src/format.rs` over the `scadfmt` crate,
`crates/fmt`). Directories are searched for `.scad` files, skipping
hidden ones; no path is the current directory. Files are rewritten in
place; `--check` lists the files that would change (`would reformat
PATH` on stdout), `--diff` prints unified diffs, and both write nothing
and exit 1 when something would change. `--stdin` formats standard
input to standard output (a path, if given, names it and is where the
configuration lookup starts). A file that does not parse is left as it
is and reported on stderr (`neoscad fmt: PATH: line N: Parser error:
...`); the exit status is then 1.

**Guarantees**, checked on every file before anything is written: only
whitespace changes (the same tokens and comments in the same order; a
`//` comment may lose trailing blanks), and the `.ast` dump, customizer
annotations included, is byte-identical; a file that would fail either
is an internal error and is left alone. The output is idempotent. Over
OpenSCAD's test inputs and examples, MCAD and BOSL2 (4,202 files that
parse) every file formats with its program unchanged and formatting it
again changes nothing.

**Style** (close to BOSL2's and OpenSCAD's own examples): 4-space
indent, 100 columns, one statement per line, `{` on its statement's line,
`name = value` assignments and `for`/`let` bindings but `name=value`
named arguments and parameter defaults (the corpus' majority), spaces
around binary operators, `[0:n]` ranges of plain bounds and `[0 : n - 1]`
otherwise, at most one blank line kept, none after `{`. A child
statement stays on the line of its parent unless it was on its own line.
Lists (arguments, parameters, vectors) that do not fit break one item
per line (numbers fill lines instead), a lone vector argument hugs its
parentheses (`polygon([` ... `])`), a long right-hand side moves to the
next line, and ternary chains break before each `:`. At the top of the
file, before the first `{`, where OpenSCAD reads customizer annotations
from the raw lines, the layout keeps what they depend on: an assignment
with a `//` annotation stays on one line, assignments sharing a line
keep sharing it, and an indented `//` comment keeps its indent (in
column 1 it would become the next assignment's description).

**Configuration**: `.neoscad-fmt.toml` in the file's directory or the
nearest one above it (read through the session's files), with
`indent = N` (1-16) and `width = N` (20-1000) lines and `#` comments; an
unknown key is an error. `--indent` and `--width` override it.

With `--format json`, one object on stdout:

```json
{"schema": 1, "exit_code": 0, "mode": "write"|"check"|"diff",
 "counts": {"files": 12, "changed": 3, "errors": 0},
 "files": [FILE, ...]}
```

`FILE` is `{"path", "changed", "error", "config"}` (and `"diff"` with
`--diff`): `config` is the configuration file used or `null`; `error` is
`null` or `{"kind": "syntax"|"unsupported"|"internal"|"read"|"config",
"message", "errors": [{"line", "message"}]}`. With `--stdin` the object
is the one `FILE` plus `schema`, `exit_code`, `mode` (`stdin`) and
`text`, the formatted text.

# `neoscad docs`

`neoscad docs [NAME] [--in FILE] [--full] [--format json]` prints the
reference of a builtin module, function or special variable (entries
written for neoscad in `crates/docs/builtins.toml` and compiled in: a
test keeps them in step with the evaluator's builtins and runs every
example), or with `--in FILE` of the modules and functions the file
defines, includes or `use`s, with the comment block above each. BOSL2's
structured blocks (`// Module:`, `// Synopsis:`, `// Usage:`,
`// Arguments:` ...) are shown compactly: synopsis, usage, the first
lines of the description and the arguments; `--full` shows the whole
block. No name: a compact index (with `--in`, the file's definitions).
An unknown name exits 1 with "did you mean" (the diagnostics' matcher)
or, with nothing close and no `--in`, a hint to add `--in FILE` (the
server's `docs` says `file`, the MCP tool `path`),
or says the name is an experimental OpenSCAD builtin, naming the
`--enable` flag that turns it on (or that neoscad does not have it).

With `--format json`:

```json
{"schema": 1, "exit_code": 0, "name": "cube", "entries": [ENTRY, ...]}
```

A builtin `ENTRY` is `{"source": "builtin", "kind": "module"|"function"|
"variable", "name", "signature", "summary", "params": [{"name", "type",
"default", "doc"}], "returns", "example", "notes", "extension",
"label"}`; `extension` is the `--enable` name of the NeoSCAD extension
the builtin belongs to (`"part"`; null for OpenSCAD's builtins) and
`label` the line every surface shows for it ("NeoSCAD extension
(`--enable part`); not in OpenSCAD"), which the text form prints under
the summary and the language server puts in hover and completion. A user
one is
`{"source": "user", "kind", "name", "signature", "file", "line",
"comment"?, "sections"?}` (`sections`: `[{"title", "text", "lines"}]`
for structured blocks, synopsis, usage and arguments unless `--full`;
`comment`: the block's lines otherwise). An unknown name gives
`{"schema": 1, "exit_code": 1, "name", "error", "did_you_mean",
"entries": []}`; the index is `{"schema": 1, "exit_code": 0,
"builtins": [{"kind", "name", "summary"}], "definitions": [{"kind",
"name", "file", "line"}]}`.

# `neoscad test`

Model tests; the command, the `@expect` grammar and the JSON are in
`docs/model-tests.md`.

# `neoscad bench`

The community benchmark; `--json FILE` writes a result in its own
versioned schema (`bench/result.schema.json`). The command, the schema
and how results are submitted are in `docs/community-bench.md`.

# Diagnostics

Every diagnostic in JSON output (the run object above, the snapshot
summary's `items`, and the server's results and notifications) has the
same shape:

```json
{"code": "unknown-module", "severity": "warning",
 "message": "Ignoring unknown module 'cub'",
 "text": "WARNING: Ignoring unknown module 'cub' in file model.scad, line 2",
 "file": "/abs/model.scad", "line": 2,
 "span": {"start": {"line": 2, "column": 1}, "end": {"line": 2, "column": 8}},
 "hints": [{"message": "did you mean 'cube'?"}]}
```

- `code`: the stable code (`lang::diag::DiagCode::as_str`: `syntax-error`,
  `unknown-module`, `assertion-failed`, `geometry`, ...); `log` for a
  line with no code.
- `severity`: `error`, `warning`, `deprecated` (and `echo`, `trace`,
  `info` where those appear).
- `message`: the message alone; `text`: the whole line exactly as
  OpenSCAD prints it, word for word, which is what to compare with
  OpenSCAD.
- `file`, `line`, `span`: present when the diagnostic has a location.
  `line` is the line OpenSCAD reports (for a syntax error, where the
  offending token ends); `span` is the precise range, 1-based lines and
  1-based byte columns, `end` exclusive.
- `hints`, when there are any: how to fix it. A hint the front end
  attached may carry `replace` (`{"span", "text"}`), an exact edit.
  Otherwise hints come from the code: "did you mean" against the names
  the program and its libraries define and OpenSCAD's builtins for
  unknown modules, functions and variables, and a short suggestion for
  syntax errors, missing includes, reassignments, argument mismatches,
  assertions, recursion and iteration limits and undefined operations.
  A syntax error's hint names the offending token and its column
  (``unexpected `cube` at line 1, column 11: look just before it for a
  missing ';' ...``), or for a program that stops short ``unexpected
  end of input at line 1, column 7`` just after its last character, and
  says so when the line has HTML-escaped brackets (`&lt;`).

Codes a run's failure itself can have, besides the evaluator's:

- `input-not-found`: the input cannot be read. Its `text` is OpenSCAD's
  `Can't open input file 'x.scad'!` (the bytes on stderr are unchanged);
  it was only a plain `log` line before.
- `output-not-writable`: a served export's output cannot be written
  (`ERROR: Can't write to ...`).
- `resource-limit`: the run passed one of the resource limits (below).

## Input meshes

NeoSCAD's own warnings, which OpenSCAD does not print: each
`polyhedron()` and each imported mesh (in the form the render read it)
is looked at on its own (`crates/session/src/orient.rs`), and each
problem is a warning at the call that made the mesh. They are in the
JSON (the run object with `--format json`, the server's and MCP
results, the editor's diagnostics) and never on stderr, so the console
text stays OpenSCAD's word for word; their `text` is the line as it
would print. OpenSCAD itself says at most `PolySet -> Manifold
conversion failed: NotManifold` without where or why, and nothing at
all for an inside-out mesh, which converts as a solid of negative
volume and makes every boolean with it go wrong.

| Code | When | Message says |
|---|---|---|
| `polyhedron-inside-out` | closed, consistently wound, every face pointing inward (negative signed volume) | how many faces and the signed volume |
| `polyhedron-flipped-faces` | some faces wound against the others | how many of how many and where the first one's centroid is; on a closed mesh which faces point *inward* (the side that makes the volume positive, so when most faces are wrong the few right ones are not blamed), on an open one the minority |
| `polyhedron-open` | edges used by one face only | how many and the first one's midpoint |
| `polyhedron-not-manifold` | edges used by more than two faces, or a surface that cannot be wound consistently | how many and where |

Corners are compared by exact position, as a file reader would. Two
pieces that touch only through points of their own at one position
(two cubes sharing an edge, each with its own vertices) are neither
open nor non-manifold here, since Manifold takes them by index; the
pinched-edge check covers what a file of the result shows. A closed
shell inside another (by bounding box) is a cavity and should face
inward. Points are in the model's coordinates (the call's transforms
applied; `resize()` is not). A call is reported once per code however
many times it is instantiated, and at most 10 calls are reported.
`hull()` children (which use only the points) and `%` subtrees are
skipped; imported meshes are looked at in a render only. The hint says
how to fix it: OpenSCAD wants each face's points clockwise seen from
outside (`PolyhedronNode::createGeometry` reverses them,
`src/core/primitives.cc:399-414`), so an inside-out polyhedron's faces
are counter-clockwise and each list must be reversed. When the call's
`faces` argument is written out as lists of numbers, the hint also
carries `replace`: the same text with the offending faces' indices
reversed in place. (Mirroring twice does not help: each mirror
reverses the faces along with the points.)

## Specials of a used file

NeoSCAD's own warning `use-special-variables`, like the input-mesh
warnings in the JSON only and never on stderr: at the first call into a
`use`d file whose top level assigns `$fn`, `$fa` or `$fs`
(`crates/session/src/usehint.rs`). Special variables come from the
caller, so those assignments never apply to the file's modules; the
file's plain variables do (OpenSCAD 2026.09.23: a used file with `$fn =
64; w = 7;` at its top echoes `$fn = 0, w = 7` from its module, and
`$fn = 64` when included). The message names the assignments as
written and the file, and the hint says to pass them in the call or set
them in the calling file. A variable is left out when the call, or a
call it is made from, passes it, or when a calling file assigns it
anywhere; `@expect no-warnings` does not count this warning.

## Resource limits

`neoscad serve`, `neoscad mcp` and (later) the app run models that
agents and editors write, so they limit what one request may use
(`eval::limits`); the OpenSCAD-compatible command line is unlimited, as
OpenSCAD is, unless `--limit` is given. A request that would pass a
limit fails with exit code 1 and one `resource-limit` error, located at
the call or node that asked when there is one:

```text
ERROR: Resource limit exceeded: sphere() would make 100,000 fragments, over the fragments limit of 10,000 in file m.scad, line 1
```

with the hint "lower $fn (or raise $fa/$fs); or raise the limit: start
`neoscad serve` or `neoscad mcp` with `--limit fragments=N` (`=off`
removes it)". The limits, and the defaults of `serve` and `mcp`:

| Name | Default | What |
|---|---|---|
| `time` | 60 (s) | Wall time, checked at evaluator calls and loop iterations, before every geometry node, and in primitive and extrusion loops. One kernel operation (a boolean) is not interrupted. |
| `memory` | 4096 (MiB; `4G` also works) | Measured where the platform allows (the process's footprint on macOS, resident set on Linux, private bytes on Windows; a trip says "(measured)", and the server first evicts cached geometry to stay under it), and always estimated: every list, string, range and function value the evaluator holds (however small; the limit trips from the allocation that passes it), every node and message it makes, and the geometry results the render holds, weighted for the kernel's working copies (each counts until its parent has used it; the geometry cache has its own budget). On the benchmark models it runs from about the process's peak RSS to 8 times below it (BOSL2's fractal_tree: 1.96 GB real, under 512 MiB estimated), so the count limits, not this, are what stop a runaway primitive. |
| `fragments` | 10,000 | Segments of one circle, sphere, cylinder, `rotate_extrude` or round `offset`. |
| `slices` | 10,000 | Slices of one `linear_extrude`. |
| `list` | 10,000,000 | Elements of one list (checked as a comprehension grows, and before `concat`). |
| `string` | 67,108,864 | Bytes of one string (`str`, `chr`). |
| `rands` | 10,000,000 | Numbers from one `rands()` call. |
| `triangles` | 10,000,000 | Triangles of one geometry result (2D: vertices), checked before a primitive or extrusion is built and after every node. |
| `sketch_unknowns` | 5,000 | Unknowns of one constrained sketch (`--enable sketch`): two per point, one per circle, checked before the solve, whose factorisations take O(n³) time in this count. The solve itself stops at the time limit or a cancellation between iterations. |
| `queries` | 10,000 | Geometry queries (`child_bounds()`, `child_measure()`; `--enable query`) in one evaluation, each a render of its child. A query's render is stopped by the time limit and a cancellation like any render, and counts against the triangle and memory limits. `child_anchors()` renders nothing and does not count. |

`--limit NAME=VALUE` (repeatable) changes one: seconds for `time`, MiB
for `memory`, a count otherwise, `off` for none. The one-shot command
line accepts the same flag; such a run stays in-process.

## Human-readable diagnostics

When stderr is a terminal, each located error and warning is followed by
its source line and a caret under the span:

```
WARNING: Ignoring unknown module 'cub' in file model.scad, line 2
  2 | cub(2);
    | ^^^^^^^
```

Only then: OpenSCAD prints no such lines, and whatever compares
neoscad's output with OpenSCAD's (the conformance harness, scripts,
agents) reads stderr from a file or a pipe, so it keeps the exact text
without a flag to remember. `NEOSCAD_DIAGNOSTICS=openscad` turns the
excerpts off on a terminal, `=rich` on in a pipe. `.echo` exports never
have them.

## Changes

- 2026-09-26: first version.
- Phase 6b: `camera` in the render summary now applies the file's
  `$vp*` (as the nightly does) and, after a PNG, is the camera the image
  was drawn with. Added `neoscad snapshot --format json`.
- Phase 7a: added `--format json` for every export, the structured
  diagnostics, `lighting` and `diagnostics.items` in the snapshot
  summary, and the server's results (`docs/serve-protocol.md`), which
  use the same `GEOM` and diagnostic objects.
- Phase 7b: added named parts (`--enable part`), `neoscad check`,
  `neoscad measure`, and the snapshot's `parts`, `highlight` and
  `issues`.
- Phase 7b-2: added `neoscad fmt`, `neoscad docs` and `neoscad test`
  (`docs/model-tests.md`).
- Hardening (H4): added the diagnostic codes `input-not-found`,
  `output-not-writable` and `resource-limit`, resource limits and
  `--limit`, the syntax error hint's token and column, and the
  `check` finding `touching-surfaces`. A missing input is now a
  diagnostic rather than a `log` line. `neoscad docs --in` names a
  library's files relative to their library directory
  (`BOSL2/affine.scad`).
- `--enable=predictible-output` sorts exported meshes ("Sorted
  exports"); `--info`'s `Features` line lists the experimental features
  neoscad implements.
- After the first CAD comparison runs (`docs/agent-eval.md`):
  `manifold` is false for a pinched solid, with `pinched` added (the
  `geometry` object, `measure`'s model, `check`'s model and parts, and
  a `not-manifold` finding at the first pinched edge); thin walls are
  measured again in the layer plane, which removes the false thin walls
  of twisted extrusions (flat faces now count only against a flat far
  side); nearby overhang regions are one finding, so there are fewer
  `overhang` findings and their `value` is the sum; `measure` adds
  `--axis`, `--center` and `--profile`, the section's `axis`, `center`
  and `outlines`, and `between`'s `overlap_pieces` and `pieces`.
- After a CAD validation run whose T3 part was an inside-out thread
  sweep: the diagnostic codes and `check` findings
  `polyhedron-inside-out`, `polyhedron-flipped-faces`,
  `polyhedron-open` and `polyhedron-not-manifold` ("Input meshes");
  they come first among `check`'s findings, and a `not-manifold`
  finding's fix points to a winding problem when there is one.
- After the CAD run cad-20260929T024448Z (a twisted thread that checked
  manifold, and whose STL had 738 non-manifold edges at 32-bit
  precision): the `geometry` object's `stl_precision` (render,
  snapshot, and the `--format json` report of an export) and the
  `check` finding `stl-precision`. Both are additive; OpenSCAD's
  console text is unchanged.
- After the CAD run cad-20260929T031249Z: an `overhang` finding's
  `point` is on its steepest faces, and its message adds the heights it
  spans and the area steeper than `max_overhang` + 15°; its "up to"
  angle is that of the steepest (2 × `nozzle`)² of faces, not of a
  sliver. `min_wall` adds `sampled` (additive) and is sampled near the
  corners of the faces that could be thinnest, so a tapered rim reads
  close to its edge (1.21 for 1.2, was 1.39); findings' thin-wall
  values can be lower for the same reason. A profile's `crests` are
  refined between samples, and `pitch` leaves out odd end crests of
  its run (the adapter's 1.98 is now 2.00). `docs`' not-found hint
  names the caller's file argument.
- After the T2 run cad-20260929T042719Z: thin walls within 0.001 mm
  of a limit are not under it; an info-level `stl-precision` finding
  says no action is needed; `pinched` adds `touch_only` for a
  zero-volume result, with its own fix; the diagnostic code
  `use-special-variables` ("Specials of a used file"). All additive;
  OpenSCAD's console text is unchanged.
- Quieter `check`: faces within 0.5° of `max_overhang` (and of the
  steep note's angle) are not past it; an `overhang` finding that is a
  short bridge or thread flanks, and a `thin-wall` finding under
  0.05 mm² of faces, are info rather than warnings (or errors), with
  their own message prefixes and fixes. Codes are unchanged. The syntax
  error hint of a program that stops short says "unexpected end of input"
  just after its last character (it named the end marker OpenSCAD
  appends: ``unexpected `\u0003` at line 2, column 1``).
- Constrained sketches, stage 4 (`docs/sketch.md`): `check`'s
  `sketches` and `sketches_omitted` (only for a model with sketches),
  and `measure --sketch NAME` with its `sketch` object. Additive.
