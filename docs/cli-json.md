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
  verbatim. The same lines go to stderr as they are printed.

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

## Changes

- 2026-09-26: first version.
- Phase 6b: `camera` in the render summary now applies the file's
  `$vp*` (as the nightly does) and, after a PNG, is the camera the image
  was drawn with. Added `neoscad snapshot --format json`.
