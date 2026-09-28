// T1 reference for the grader's self-check (grade_selftest.py): a shelf
// L-bracket to the spec. Shelf plate on the bed (z = 0..t), wall plate
// standing at x = 0..t. `t`, `cs_face`, `fillet` and `head` are overridden
// with -D to make the wrong variants the grader must fail.
t = 4;
L = 60;           // plate lengths
W = 40;           // bracket width (along y)
hole = 4.5;
head = 9;
cs_face = "inner"; // countersinks open towards the inside of the L
fillet = 4;
gusset_t = 4;
$fn = 64;

module rounded_plate(len, r) {
    // A plate in x-y from 0..len x 0..W, R r on the two far corners.
    hull() {
        square([1, W]);
        translate([len - r, r]) circle(r);
        translate([len - r, W - r]) circle(r);
    }
}

module countersunk() {
    // A hole along x through the wall plate, the 90-degree head cone on
    // the face named by cs_face.
    translate([-1, 0, 0]) rotate([0, 90, 0]) cylinder(d = hole, h = t + 2);
    depth = (head - hole) / 2;
    if (cs_face == "inner") {
        translate([t - depth, 0, 0]) rotate([0, 90, 0]) cylinder(d1 = hole, d2 = head, h = depth);
        translate([t - 0.001, 0, 0]) rotate([0, 90, 0]) cylinder(d = head, h = 1);
    } else {
        translate([depth, 0, 0]) rotate([0, -90, 0]) cylinder(d1 = hole, d2 = head, h = depth);
        translate([0.001, 0, 0]) rotate([0, -90, 0]) cylinder(d = head, h = 1);
    }
}

difference() {
    union() {
        linear_extrude(t) rounded_plate(L, 3);                       // shelf
        rotate([0, -90, 0]) translate([0, 0, -t])
            linear_extrude(t) rounded_plate(L, 3);                   // wall
        // R4 inner fillet along y.
        if (fillet > 0)
            translate([t, 0, t]) rotate([-90, 0, 0])
                linear_extrude(W) difference() {
                    translate([0, -fillet]) square(fillet);
                    translate([fillet, -fillet]) circle(fillet);
                }
        for (y = [5, W - 5 - gusset_t])
            translate([0, y + gusset_t, 0]) rotate([90, 0, 0])
                linear_extrude(gusset_t) polygon([[t, t], [t + 30, t], [t, t + 30]]);
    }
    for (y = [W / 2 - 10, W / 2 + 10]) translate([0, y, 45]) countersunk();
    for (y = [W / 2 - 10, W / 2 + 10]) translate([45, y, -1]) cylinder(d = hole, h = t + 2);
}
