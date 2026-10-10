# STEP export with exact surfaces

> NeoSCAD extension (`--enable exact`); not in OpenSCAD.

NeoSCAD can write a model as a STEP file (AP214) whose faces are the
model's true planes, cylinders, cones, spheres and tori: an exact B-rep,
not the triangles of an STL. A CAD program (FreeCAD, or any that reads
STEP) can then select a hole's cylindrical face, measure its radius, put
a fillet on an edge or dimension a drawing, which a mesh does not allow.

OpenSCAD itself exports only meshes. This page is the user reference;
the design and its measurements are `docs/audits/exact-geometry-rust.md`,
and the JSON report is in `docs/cli-json.md`, "STEP with exact
surfaces".

Contents:

- [OpenSCAD superset: turning STEP export on](#openscad-superset-turning-step-export-on)
- [Quick start](#quick-start)
- [What is exact: the exact B-rep](#what-is-exact-the-exact-b-rep)
- [The $fn rule](#the-fn-rule)
- [Fallbacks and the report](#fallbacks-and-the-report)
- [Refused exports](#refused-exports)
- [Opening the file in FreeCAD and other CAD programs](#opening-the-file-in-freecad-and-other-cad-programs)
- [Where it works: command line, apps, agents, web](#where-it-works-command-line-apps-agents-web)
- [Limitations](#limitations)

## OpenSCAD superset: turning STEP export on

STEP export is one of NeoSCAD's extensions (`docs/language-extensions.md`,
section 2), and like the others it is off by default, so a run means
exactly what it means in OpenSCAD:

- Without it, `-o model.step` is OpenSCAD's own error, `Invalid suffix
  step. Either add a valid suffix or specify one using the
  --export-format option.`, and `--enable all` (OpenSCAD's "every
  experiment") does not turn it on.
- With it, `.step` and `.stp` are output suffixes, and `step` an
  `--export-format`. It adds no names to the language: a `.scad` file is
  the same program either way, and the same file exports from OpenSCAD
  as a mesh.

Its messages begin `STEP export:`, so they are never mistaken for
OpenSCAD's.

## Quick start

```
neoscad --enable exact -o bracket.step bracket.scad
```

For

```openscad
difference() {
  cube(20);
  translate([10, 10, -1]) cylinder(r = 4, h = 22);
}
```

it prints

```
INFO: STEP export: cylinder() is exported as an exact cylinder, not the 13-sided polygon of the mesh ($fn is not set) in file bracket.scad, line 3
```

and writes a solid of seven faces: six planes and one cylinder of
radius 4. Its volume is 8000 - 320π mm³ (about 6994.69), the true
value, where the 13-sided mesh has 7033.38.

`--format json` adds the report as the run's `exact` key.

## What is exact: the exact B-rep

Each face of the file lies on the surface the model's source describes:

- **Primitives:** `cube()` gives planes; `cylinder()` a cylinder, or a
  cone when `r1` and `r2` differ; `sphere()` a sphere; `circle()` and
  `square()` in a profile, a circle and lines. Each curved one only when
  its fragments come from `$fa`/`$fs` (the next section).
- **Transforms:** `translate`, `rotate`, `mirror`, `multmatrix` without
  shear and uniform `scale` keep surfaces exact. A non-uniform scale or a
  shear makes a cylinder elliptic and a sphere an ellipsoid, which are
  written as facets (reported). A `scale()` that flattens a dimension to
  zero is dropped, as the render drops it. An operation turned by an
  angle other than a multiple of 90° is built in its own frame and its
  result turned, as the render does, so faces its children share stay
  flush (`exact::walk`, `turned`; `docs/fillets.md` 15.11).
- **Booleans:** `union`, `difference` and `intersection` (and the
  implicit union of a module's children) are rendered by Manifold as
  usual; the exact faces are recovered from the result, so the
  intersection curves (a cylinder through a plane, two cylinders meeting)
  are exact curves too.
- **Extrusions:** a 2D profile's exact curves are swept. `circle()` (by
  the same `$fn` rule), `square()` and `polygon()` edges, `offset(r)`
  rounds and solved `sketch()` arcs survive 2D booleans and transforms.
  `linear_extrude` gives planes and cylinders, and cones under a uniform
  `scale` toward an arc's centre; `rotate_extrude` gives planes,
  cylinders, cones, spheres and tori, whole or partial.

Everything else has no exact surface to recover and is written as the
planar facets of the mesh render, in the same solid as the exact faces:
`polyhedron`, `hull`, `minkowski`, `text`, `import`, `projection`,
`resize`, a `linear_extrude` with `twist` or a non-uniform `scale`, and
an arc scaled off its centre (`crates/geom/src/exact/walk.rs`).

Before a file is written, the B-rep is validated, its volume is
integrated on the exact surfaces and compared with the export mesh's
(corrected onto those surfaces) and with the normal render's, and its
bounding box with the render's. A model that fails these checks is not
written at all (see "Refused exports"): an export is either right or
reported as refused, never quietly wrong.

The file is deterministic: fixed header names and date
(`1970-01-01T00:00:00`, originating system `NeoSCAD`, the product named
after the input), so the same model gives the same bytes on every run,
at any thread count, from every host, natively and in the browser.

## The $fn rule

OpenSCAD draws every curve as a polygon. Which polygon depends on `$fn`,
`$fa` and `$fs`, and NeoSCAD's export follows what the model asked for:

- **`$fn` not set** (the default `$fn = 0`): the number of fragments
  came from `$fa`/`$fs`, a display resolution. The curve is written as
  the exact circle, cylinder or sphere. The solid grows by up to the
  fragments' sagitta, because OpenSCAD's polygon is inscribed in the
  circle; the `INFO` line says how many sides the mesh had.
- **`$fn` set** (`$fn = 6`, `$fn = 64`, ...): the polygon is the model.
  A hexagon is meant to be a hexagon, and a printing hole may be sized
  for its polygon. Its faces are planar, so they are written exactly as
  modelled (the `INFO` line says the polygon was kept).

So `cylinder(r = 4, h = 10)` exports as a cylinder and
`cylinder(r = 4, h = 10, $fn = 6)` as a hexagonal prism. To get a true
cylinder from a model that sets `$fn` globally for its preview, set it
only where the polygon matters, or remove it.

## Fallbacks and the report

Every substitution is reported once per source location, with a count
when a loop makes several:

| Kind | Severity | Example |
|---|---|---|
| exact | `INFO` | `cylinder() is exported as an exact cylinder, not the 13-sided polygon of the mesh ($fn is not set)` |
| polygon | `INFO` | an explicit `$fn` kept as the modelled polygon |
| faceted | `WARNING` | `hull() is exported as planar facets: hull() has no exact surfaces in STEP export yet` |

A faceted region is a `WARNING` because it falls short of what an exact
export promises, so `--hardwarnings` refuses it.

When reconstruction fails, the export tries again before refusing:

1. **A finer mesh.** A mesh whose topology does not match the exact
   model at a near-tangency is rebuilt at twice the segments, and a
   result whose volume check is loose (coarse fragments) is held until a
   mesh at twice or four times the segments vouches for it. A finer
   mesh that comes out the same (a model with no curves the segments
   change) is not reconstructed again.
2. **Faceted extrusions.** If the exact extrusions do not reconstruct,
   they are written as facets, as before extrusions were exact.
3. **Partial faceted fallback.** If reconstruction or validation fails
   at a known place, the source regions there are written as facets and
   the rest stays exact (up to eight rounds, growing the region). Each
   such region is a `faceted` substitution at its line, and the JSON
   report's `partial` says why. The result is held to every check an
   exact export is.

The report every host shows is the same text, a line each:

```
STEP: 7 of 16 faces exact (43.8%).
1 curve made exact.
Faceted: hull() at bracket.scad, line 4 is exported as planar facets: hull() has no exact surfaces in STEP export yet
```

The share counts faces on an exact surface (planes included) against
all faces; a share that is not all of the faces never rounds to 100%.

## Refused exports

Some models do not export, and then no file is written:

```
ERROR: STEP export failed: REASON. No file was written.
```

with exit status 1. The classes known to be refused are bodies that
touch at a point or along an edge only (a Menger sponge's cubes), fins
of no thickness, and three cones meeting at one tangent point
(`docs/followups.md`, "Exact geometry"). The usual cure is to make
touching bodies overlap slightly, or to export as STL.

## Opening the file in FreeCAD and other CAD programs

FreeCAD imports STEP with OpenCASCADE (OCCT). Every file in the export's
test corpora was read back with OCCT 8.0.1 and checked with its
`BRepCheck_Analyzer`, its volume compared with NeoSCAD's
(`docs/audits/exact-geometry-rust.md`, the stage notes at the top). The
files carry the seams and parameter-space curves OCCT needs to read
curved faces without tolerance problems. FreeCAD itself, Fusion, Onshape
and SolidWorks were not run on them (the audit's section 11).

## Where it works: command line, apps, agents, web

- **Command line:** `neoscad --enable exact -o x.step x.scad`
  (`--format json` for the report).
- **`neoscad serve`:** the `export` method with `"output": "x.step"` and
  `"enable": ["exact"]` (or a server started with `--enable exact`);
  the reply's `exact` is the report (`docs/serve-protocol.md`). `serve`
  lists `exact` in its features and `step` in its export formats.
- **MCP (AI agents):** start the server as `neoscad mcp --enable exact`;
  `render` or `check` with `export: "x.step"` writes it, and the
  structured content's `exact` gives the share and each faceted region
  at its line (`docs/mcp.md`).
- **macOS app:** Settings > Language > "Exact STEP export (exact)", then
  File > Export > STEP (exact surfaces). The report is shown after the
  export; a refused model is an alert with the reason.
- **Linux app:** Preferences > Language > "Exact STEP export (exact)",
  then File > Export > STEP (exact surfaces) (`docs/linux-app.md`).
- **Windows app:** Design > NeoSCAD Extensions > "Exact STEP Export
  (exact)", then File > Export As > STEP (exact surfaces)
  (`docs/windows-app.md`).
- **Web (neoscad.org/try):** View > NeoSCAD extensions > "Exact STEP
  export (exact)", then Export > STEP (exact surfaces). The same engine runs in the browser,
  so the file is the command line's (the same bytes for the same file
  name, which the header records).

## Limitations

- **Curves only from the primitives and extrusions above.** `hull`,
  `minkowski`, `text` (its Béziers), `import`ed meshes and `polyhedron`
  are facets.
- **Non-uniform scales and shears** of curved primitives are facets
  (elliptic cylinders and ellipsoids are not written yet), as are twisted
  extrusions.
- **Export only.** The B-rep is reconstructed from what Manifold
  computed; NeoSCAD cannot import a STEP file and edit it.
- **Fillets and chamfers** on a model's edges are their own extension,
  `--enable fillet` (`docs/fillet-edges.md`): with both on, their blends
  export as exact cylinders, tori, cones and spheres, under the same `$fn`
  rule.
- **Time.** The export renders the model a second time for
  reconstruction (building a large module instantiated many times once
  and placing its copies); on large models it can take several times as
  long as the render (`docs/audits/exact-geometry-rust.md`, gate 5).
- **Some models are refused** (previous section).
