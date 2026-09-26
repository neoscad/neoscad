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

The command line's camera: `--camera` if given, otherwise OpenSCAD's
default (translation 0, rotation 55/0/25, distance 140, fov 22.5).
Top-level `$vpt`/`$vpr`/`$vpd`/`$vpf` in the file are not reflected yet
(the nightly applies them; `docs/followups.md`).

## Example

`neoscad --summary all --summary-file - -o x.stl cube.scad` for
`cube(1);`:

```json
{"cache":{"cgal_cache":{"bytes":0,"entries":0,"max_size":0},"geometry_cache":{"bytes":null,"entries":1,"max_size":null}},"camera":{"distance":140.0,"fov":22.5,"rotation":[55.0,0.0,25.0],"translation":[0.0,0.0,0.0]},"geometry":{"bounding_box":{"max":[1.0,1.0,1.0],"min":[0.0,0.0,0.0],"size":[1.0,1.0,1.0]},"convex":true,"dimensions":3,"facets":6,"triangular":false},"time":{"hours":0,"milliseconds":0,"minutes":0,"seconds":0,"time":"0:00:00.000","total":0}}
```

The nightly writes the same apart from the three cache byte fields.

## Changes

- 2026-09-26: first version.
