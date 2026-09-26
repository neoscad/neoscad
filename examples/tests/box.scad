// A parametric storage box: an open tray and a lid that sits on it.

wall = 2;
floor_thickness = 2;

// The tray: outer size, open at the top.
module tray(size=[40, 30, 20]) {
    difference() {
        cube(size);
        translate([wall, wall, floor_thickness])
            cube([size.x - 2 * wall, size.y - 2 * wall, size.z]);
    }
}

// The lid: a plate with a lip that fits inside the tray's walls.
module lid(size=[40, 30], thickness=2, lip=3, clearance=0.2) {
    cube([size.x, size.y, thickness]);
    inset = wall + clearance;
    translate([inset, inset, -lip])
        difference() {
            cube([size.x - 2 * inset, size.y - 2 * inset, lip]);
            translate([wall, wall, -1])
                cube([size.x - 2 * inset - 2 * wall, size.y - 2 * inset - 2 * wall, lip + 2]);
        }
}

// Inside volume in mm³ (ml / 1000) of a tray of this outer size.
function capacity(size) = (size.x - 2 * wall) * (size.y - 2 * wall) * (size.z - floor_thickness);
