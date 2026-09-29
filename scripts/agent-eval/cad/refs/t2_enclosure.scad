// T2 reference for the grader's self-check (grade_selftest.py): an
// enclosure for a 50 x 26 PCB. -D part="base" or "lid"; `lip_clear`,
// `wall` and `vents` are overridden to make the wrong variants.
// `lip_relief` cuts the lip's outer face back by that much per side except
// a 1 mm catch band at its free end (the OpenSCAD lid of
// cad-20260928T231444Z, whose relief clears the base's snap barbs).
part = "base";
pcb = [50, 26];
clear = 0.4;
wall = 2;
floor_t = 2;
height = 16;          // base, floor included
lip_clear = 0.2;
lip_t = 1.5;
lip_h = 4;
vents = 5;
lip_relief = 0;
$fn = 48;

cav = pcb + [2 * clear, 2 * clear];
outer = cav + [2 * wall, 2 * wall];

module base() {
    difference() {
        cube([outer.x, outer.y, height]);
        translate([wall, wall, floor_t]) cube([cav.x, cav.y, height]);
        // USB-C cutout, 9.5 x 3.5, in the -x wall.
        translate([-1, outer.y / 2 - 9.5 / 2, floor_t + 4]) cube([wall + 2, 9.5, 3.5]);
        // Snap grooves inside the long walls (not through).
        for (y = [wall - 0.6, wall + cav.y]) translate([outer.x / 2 - 5, y, height - 3]) cube([10, 0.6, 1]);
    }
    // M2 posts, 3.5 in from the PCB corners.
    for (x = [3.5, pcb.x - 3.5], y = [3.5, pcb.y - 3.5])
        translate([wall + clear + x, wall + clear + y, floor_t - 0.01]) difference() {
            cylinder(d = 5, h = 5);
            cylinder(d = 1.8, h = 6);
        }
}

module lid() {
    lip = cav - [2 * lip_clear, 2 * lip_clear];
    difference() {
        union() {
            cube([outer.x, outer.y, 2]);
            translate([(outer.x - lip.x) / 2, (outer.y - lip.y) / 2, 1.99]) difference() {
                cube([lip.x, lip.y, lip_h]);
                translate([lip_t, lip_t, -1]) cube([lip.x - 2 * lip_t, lip.y - 2 * lip_t, lip_h + 2]);
                if (lip_relief > 0)
                    translate([0, 0, -1]) difference() {
                        cube([lip.x, lip.y, lip_h]);
                        translate([lip_relief, lip_relief, -1])
                            cube([lip.x - 2 * lip_relief, lip.y - 2 * lip_relief, lip_h + 2]);
                    }
            }
            // Snap bumps on the long sides of the lip.
            for (y = [(outer.y - lip.y) / 2 - 0.4, (outer.y + lip.y) / 2])
                translate([outer.x / 2 - 4, y, 2 + lip_h - (lip_relief > 0 ? 0.9 : 2.5)]) cube([8, 0.4, 0.8]);
        }
        for (i = [0:vents - 1])
            translate([outer.x / 2 - 10, outer.y / 2 - 9 + i * 4.5 - 1, -1]) cube([20, 2, 4]);
    }
}

if (part == "base") base(); else lid();
