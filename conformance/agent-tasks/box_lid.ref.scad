// Reference solution for `box_lid`: checks the grader, never shown to agents.
module box() difference() {
    cube([60, 40, 30]);
    translate([2, 2, 2]) cube([56, 36, 30]);
}
// A 2 mm plate on the rim with a 1.2 mm lip 4 mm deep, 0.2 mm clearance.
module lid() {
    translate([0, 0, 30]) cube([60, 40, 2]);
    translate([2.2, 2.2, 26]) difference() {
        cube([55.6, 35.6, 4]);
        translate([1.2, 1.2, -1]) cube([53.2, 33.2, 6]);
    }
}
box();
lid();
