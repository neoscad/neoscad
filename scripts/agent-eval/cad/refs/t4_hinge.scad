// T4 reference for the grader's self-check (test_grade.py): a pin-hinged
// box, body and lid, -D part="body" or "lid". The other variables are
// overridden to make the wrong variants: `wall`, `kn_d` (knuckle outer
// diameter), `hole` (pin hole), `gap` (between neighbouring knuckles) and
// `lid_dz` (moves the lid's knuckles off the body's axis when closed).
// `turn` turns both parts about z, so the hinge runs along y. `ribs`
// (a length) carries each body knuckle on a rib that long, centred on it,
// down the back wall to the floor (a T4 part in cad-20261003T013729Z had
// 12): hinge, not box.
part = "body";
size = [70, 45, 30];
wall = 2;
floor_t = 2;
lid_t = 3;
kn_d = 7;
kn_l = 12;
gap = 0.4;
hole = 2;
lid_dz = 0;
turn = 0;
ribs = 0;
$fn = 64;

// The hinge axis, in the closed box's coordinates: along x, just behind
// the back wall (y = 45) with 0.5 of play, and low enough that the
// knuckles' tops are flush with the closed lid's top.
ax = [size.y + kn_d / 2 + 0.5, size.z + lid_t - kn_d / 2];
span = 5 * kn_l + 4 * gap;
function kx(i) = (size.x - span) / 2 + i * (kn_l + gap);

module knuckle(i) translate([kx(i), ax[0], ax[1]]) rotate([0, 90, 0]) cylinder(d = kn_d, h = kn_l);

module pin_hole() translate([-1, ax[0], ax[1]]) rotate([0, 90, 0]) cylinder(d = hole, h = size.x + 2);

module body() difference() {
    union() {
        difference() {
            cube(size);
            translate([wall, wall, floor_t]) cube([size.x - 2 * wall, size.y - 2 * wall, size.z]);
        }
        // Each body knuckle hulls down onto the back wall at 45 degrees,
        // so it prints without support; the hull stays under the rim.
        for (i = [0, 2, 4]) hull() {
            knuckle(i);
            translate([kx(i), size.y - wall, ax[1] - (ax[0] - size.y) - kn_d / 2]) cube([kn_l, wall, 0.01]);
            translate([kx(i), size.y - wall, size.z - 0.01]) cube([kn_l, wall, 0.01]);
        }
        if (ribs > 0) for (i = [0, 2, 4])
            translate([kx(i) + kn_l / 2 - ribs / 2, size.y - 0.01, 0]) cube([ribs, 2.01, size.z - 4]);
    }
    pin_hole();
}

module lid_closed() difference() {
    union() {
        translate([0, 0, size.z]) cube([size.x, size.y, lid_t]);
        for (i = [1, 3]) translate([0, 0, lid_dz]) hull() {
            knuckle(i);
            translate([kx(i), size.y - 1, size.z]) cube([kn_l, 1, lid_t]);
        }
    }
    translate([0, 0, lid_dz]) pin_hole();
}

rotate([0, 0, turn])
    if (part == "body") body();
    else  // printed upside down: its top face on the bed, knuckles up
        translate([0, 0, size.z + lid_t]) rotate([180, 0, 0]) lid_closed();
