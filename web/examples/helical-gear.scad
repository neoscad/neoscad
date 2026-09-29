// A helical spur gear from BOSL2 (https://github.com/BelfrySCAD/BOSL2),
// the "Helical Gear" example of spur_gear() in its gears.scad. BOSL2 is
// BSD-2-Clause; the demo fetches it the first time a file includes it.

include <BOSL2/std.scad>
include <BOSL2/gears.scad>

spur_gear(
    circ_pitch=5, teeth=20, thickness=10,
    shaft_diam=5, helical=-30, slices=12,
    $fa=1, $fs=1
);
