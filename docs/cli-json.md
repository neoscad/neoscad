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

`CACHE` is `{"bytes": int|null, "entries": int, "max_size": int|null}`:
entries in the cache, their estimated size in bytes, and the cache's
budget in bytes.

- `geometry_cache.entries`: geometries in neoscad's cache after the
  render. The count follows neoscad's caching, which differs from the
  nightly's (it caches different intermediate nodes).
- `geometry_cache.bytes` and `geometry_cache.max_size`: `null`, because
  the `geom` crate does not report its cache's size or budget yet
  (`docs/followups.md`). The nightly reports numbers here.
- `cgal_cache`: always `{"bytes":0,"entries":0,"max_size":0}`. neoscad has
  no CGAL backend and so no CGAL cache. (The nightly's `max_size` is
  104857600.)

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
{"cache":{"cgal_cache":{"bytes":0,"entries":0,"max_size":0},"geometry_cache":{"bytes":null,"entries":1,"max_size":null}},"camera":{"distance":140.0,"fov":22.5,"rotation":[55.0,0.0,25.0],"translation":[0.0,0.0,0.0]},"geometry":{"bounding_box":{"max":[1.0,1.0,1.0],"min":[0.0,0.0,0.0],"size":[1.0,1.0,1.0]},"convex":true,"dimensions":3,"facets":6,"triangular":false},"time":{"hours":0,"milliseconds":0,"minutes":0,"seconds":0,"time":"0:00:00.000","total":0}}
```

The nightly writes the same apart from the three cache byte fields.

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
  Manifold reported an error or had to keep the mesh as a triangle soup.
  `components` counts the pieces whose faces share no vertex. 2D:

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

Usage errors (a bad flag, no `-o`) happen before the run and print their
usual text with no JSON.

# Named parts: `--enable part`

`part("name") { ... }` is neoscad's language extension for naming the
pieces of a model, so that `check`, `measure` and `snapshot` can refer
to them. It is off by default: without `--enable part` (the `parts` or
`enable` option of a server request, `session::Run::parts` or
`session::Config::parts`), `part` is an unknown module exactly as in
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
                          "overhangs", "parts", "total"},
                "total"},
 "diagnostics": DIAG}
```

- `MODEL` (3D): `{"dimensions": 3, "manifold", "components",
  "floating", "volume", "area", "centroid", "bbox", "triangles",
  "min_wall": {"thickness", "point", "part"}|null, "overhang_area"}`;
  `{"dimensions": 2}` for a 2D model, `null` for an empty one.
  `min_wall` is the thinnest wall any sample measured.
- `PART`: `{"name", "instances", "context", "dimensions", "manifold",
  "components", "volume", "area", "bbox"}` for each part's own solid.
- `FINDING`: `{"id": int, "severity": "error"|"warning"|"info", "code",
  "message", "part": name|null, "location": {"point": [x, y, z],
  "bbox": BBOX}, "fix", "value": number|null, "limit": number|null}`.
  `id`s count from 1 in order: errors first. `value` is what was
  measured (mm, mm², mm³ or degrees, as the message says) and `limit`
  what it broke. Numbers are rounded to 0.1 µm.
- `counts` are before truncation; `truncated` counts, per code, the
  findings past the limit.

Codes and how each is found:

| Code | Severity | What |
|---|---|---|
| `empty` | error | Nothing to print. |
| `not-3d` | error | A 2D model. |
| `not-closed` | error | A mesh result (a lone polyhedron) with edges on one face only. |
| `not-manifold` | error | Manifold reports an error or kept the solid as a triangle soup, or edges are shared by more than two faces. |
| `floating` | error | A connected piece (triangles sharing vertices) whose lowest point is more than `bed_tolerance` above the model's lowest point. `point` is the piece's centre. The message says what is under it, straight down from its lowest points: another piece it rests on (within `bed_tolerance`), another piece N mm below, or nothing. |
| `thin-wall` | error below `nozzle`, else warning below `min_wall` | From points on every face (the centroid, or 4 or 16 points on faces larger than (4 × `min_wall`)²) a ray goes inward along the face's normal to where it leaves the solid, ignoring faces that share a corner with the start (so knife edges do not measure zero) and exits through faces more than 45° from parallel (corners and slopes are not walls). Thin faces that share an edge, or face each other across a wall, are one place; places of one part and severity within max(4 × `min_wall`, 5% of the model's diagonal) are one finding ("walls at N places"), located at its thinnest point. An exit closer than min(0.01 mm, 1e-4 of the diagonal) behind which the ray leaves through another face facing its way is a contact seam (two pieces that touch keep both surfaces), not a wall: the wall is measured to that second exit, and the seams are one `touching-surfaces` finding. |
| `touching-surfaces` | info | Surfaces of pieces that touch with no gap (coils of a spring, a lid on its box): they print fused. `value` is 0; the message gives the area. The fix: leave a gap of at least the nozzle if they should be separate, overlap them a little if they should be one. |
| `overhang` | warning | Faces pointing down more than `max_overhang` from vertical, except faces within `bed_tolerance` of the lowest point, grouped into regions by shared edges; regions under (2 × `nozzle`)² are ignored. `value` is the region's area, the message its steepest angle; `point` is on the region. |
| `bed-fit` | error, or warning when turning it 90° about z fits | With `--bed`: the bounding box against the bed. |
| `tiny-feature` | warning | A piece whose largest extent is under two nozzle widths. |
| `parts-intersect` | warning | Two parts (neither nested in the other, both reaching the model as themselves) whose solids overlap: `value` is the overlap volume, by a boolean intersection. |
| `part-not-manifold` | error | A part's own solid is not valid. |
| `off-bed` | info | The model's lowest point is not at z = 0. |

Accuracy: on the synthetic models of `crates/session/tests/check.rs`
the thickness of a 0.3 and a 0.5 mm wall, a 200 mm² overhang, a 45°
chamfer at a 30° limit (141.42 mm²), a floating cube's 5 mm lift, the
overlap of two parts and a thin part's name come out exact. Rays
measure along the normal: a wall whose sides are not parallel measures
thicker than its narrowest point, and a feature narrower than the
sample spacing on a large face can be missed. A flat span between two
walls (a bridge) is reported as an overhang.

# `neoscad measure`

`neoscad measure MODEL.scad [--part P] [--between A B] [--section
z=H|x=H|y=H] [--svg FILE] [--enable part] [-D var=val] [--format json]`
(`crates/session/src/measure.rs`). Exit status 0, or 1 when the model
fails or a named part does not exist (then `error` says which parts
there are).

```json
{"schema": 1, "input": "model.scad", "exit_code": 0,
 "model": SOLID|GEOM2D|null, "parts": [SOLID + {"name", "instances",
 "context"}, ...],
 "between": BETWEEN, "section": SECTION|null,
 "timings_ms": {"evaluate", "geometry", "measure", "total"},
 "diagnostics": DIAG}
```

- `SOLID`: `{"volume", "area", "bbox", "centroid", "triangles"}` (the
  model's also `"dimensions": 3, "components", "manifold"`), in mm, mm²
  and mm³, rounded to 1e-6. `centroid` is the centre of mass of the
  enclosed volume at uniform density. A 2D model is the `GEOM` object
  of the snapshot summary.
- `parts`: every part, or with `--part P` that part and the parts
  nested in it.
- `between` (with `--between A B`): `{"a", "b", "distance",
  "touching", "overlapping", "overlap_volume", "overlap_bbox",
  "points"}`. Overlap is a boolean intersection of the two solids
  (then `distance` is 0); otherwise `distance` is the exact smallest
  distance between their surfaces (triangle to triangle, over bounding
  volume hierarchies) and `points` the closest points on A and on B.
  `touching`: within 1 µm.
- `section` (with `--section`): `{"plane": "z=5", "axes": ["x", "y"],
  "area", "perimeter", "contours", "bbox", "part"?, "svg"?}` for the
  cut through the model (or `--part`'s solid): `axes` are the section's
  2D axes (x, y for z; y, z for x; x, z for y); `bbox` is in model
  coordinates; `svg` is the file written with `--svg` (the outline in
  mm, the second axis up, holes by the even-odd rule).

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
An unknown name exits 1 with "did you mean" (the diagnostics' matcher),
or says the name is an experimental OpenSCAD builtin neoscad does not
enable.

With `--format json`:

```json
{"schema": 1, "exit_code": 0, "name": "cube", "entries": [ENTRY, ...]}
```

A builtin `ENTRY` is `{"source": "builtin", "kind": "module"|"function"|
"variable", "name", "signature", "summary", "params": [{"name", "type",
"default", "doc"}], "returns", "example", "notes"}`; a user one is
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
  missing ';' ...``), and says so when the line has HTML-escaped
  brackets (`&lt;`).

Codes a run's failure itself can have, besides the evaluator's:

- `input-not-found`: the input cannot be read. Its `text` is OpenSCAD's
  `Can't open input file 'x.scad'!` (the bytes on stderr are unchanged);
  it was only a plain `log` line before.
- `output-not-writable`: a served export's output cannot be written
  (`ERROR: Can't write to ...`).
- `resource-limit`: the run passed one of the resource limits (below).

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
| `memory` | 4096 (MiB; `4G` also works) | An estimate, not a measurement: every list, string, range and function value the evaluator holds (however small; the limit trips from the allocation that passes it), every node and message it makes, and the geometry results the render holds, weighted for the kernel's working copies (each counts until its parent has used it; the geometry cache has its own budget). On the benchmark models it runs from about the process's peak RSS to 8 times below it (BOSL2's fractal_tree: 1.96 GB real, under 512 MiB estimated), so the count limits, not this, are what stop a runaway primitive. |
| `fragments` | 10,000 | Segments of one circle, sphere, cylinder, `rotate_extrude` or round `offset`. |
| `slices` | 10,000 | Slices of one `linear_extrude`. |
| `list` | 10,000,000 | Elements of one list (checked as a comprehension grows, and before `concat`). |
| `string` | 67,108,864 | Bytes of one string (`str`, `chr`). |
| `rands` | 10,000,000 | Numbers from one `rands()` call. |
| `triangles` | 10,000,000 | Triangles of one geometry result (2D: vertices), checked before a primitive or extrusion is built and after every node. |

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
