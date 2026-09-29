# Web demo examples

The example picker's files. `manifest.json` lists them in picker order
(`id`, `title`, `file`, `source`, `license`, and optionally `parts` for
NeoSCAD's `part()` extension, `libraries` fetched on first use, `heavy`
to restart the engine's worker after it, `autorun: false` to wait for
Preview, and a `note` shown instead).

| File | Source | Licence |
|---|---|---|
| `CSG.scad` | OpenSCAD `examples/Basics/CSG.scad` | CC0 1.0 (header; `COPYING-CC0.txt`) |
| `sign.scad` | OpenSCAD `examples/Parametric/sign.scad` | CC0 1.0 (header) |
| `GEB.scad` | OpenSCAD `examples/Advanced/GEB.scad` | CC0 1.0 (header) |
| `example024.scad` | OpenSCAD `examples/Old/example024.scad` | CC0 1.0 (header) |
| `helical-gear.scad` | BOSL2 `gears.scad`, `spur_gear()`'s "Helical Gear" example | BSD-2-Clause (BOSL2's `LICENSE`, shipped in `bosl2.tar.gz`) |
| `box-lid.scad` | Written for the demo | CC0 1.0 |
| `threaded-ring.scad` | `apple/Icon/concept-c.scad`, unchanged | GPL-2.0-or-later (NeoSCAD) |
| `gearbox.scad` | `apple/Icon/hero.scad`, unchanged | GPL-2.0-or-later (NeoSCAD) |

The OpenSCAD files are copied unchanged from the reference checkout
(`.reference/openscad/examples`), with `COPYING-CC0.txt` from the same
directory. The two icon models are copies so that the bundle does not
depend on `apple/`; `web/test/examples.test.js` fails when they drift
from their sources.
