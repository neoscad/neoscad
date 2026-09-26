// Reference solution for `pipe_adapter`: checks the grader, never shown to agents.
$fn = 96;
module adapter() difference() {
    union() {
        cylinder(d = 38, h = 30);
        translate([0, 0, 30]) cylinder(d1 = 38, d2 = 26, h = 15);
        translate([0, 0, 45]) cylinder(d = 26, h = 25);
    }
    translate([0, 0, -1]) cylinder(d = 32.5, h = 31);
    translate([0, 0, 30]) cylinder(d1 = 32.5, d2 = 20.5, h = 15);
    translate([0, 0, 44.99]) cylinder(d = 20.5, h = 26);
}
adapter();
