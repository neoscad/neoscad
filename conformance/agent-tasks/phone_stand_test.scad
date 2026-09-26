// Hidden grader for the agent-eval task `phone_stand` (scripts/agent-eval).
// The phone's pose is part of the task: 80 x 10 x 160 mm, its back face
// 70° from the table, its bottom front edge at y = 0, z = 10. Probes are
// thin slabs in the phone's own frame, so "supports the back" means "has
// material within 1 mm behind the back face".
// `include`, not `use`: the model is judged as its author renders it,
// with its own top-level `$fn` (which `use` would drop, leaving curves at
// default resolution). Its top-level geometry is no part of any test.
include <model.scad>

module grader_marked() union() { children(); translate([500, 0, 0]) cube(1); }
module grader_posed() translate([-40, 0, 10]) rotate([-20, 0, 0]) children();
module grader_phone() grader_posed() cube([80, 10, 160]);

// One printable solid.
// @expect manifold
// @expect components 1
// @expect check no-error
module test_stand_solid() stand();

// It does not overlap the phone.
// @expect volume 1±0.05
module test_no_overlap() grader_marked() intersection() { stand(); grader_phone(); }

// It supports the phone's back (within 1 mm behind it, over 20 mm²).
// @expect volume 5001±4980
module test_supports_back() grader_marked() intersection() {
    stand();
    grader_posed() translate([0, 10, 0]) cube([80, 1, 160]);
}

// It supports the phone's bottom edge.
// @expect volume 5001±4980
module test_supports_bottom() grader_marked() intersection() {
    stand();
    grader_posed() translate([0, 0, -1]) cube([80, 10, 1]);
}

// A lip in front of the phone's bottom keeps it from sliding off.
// @expect volume 5001±4995
module test_front_lip() grader_marked() intersection() {
    stand();
    grader_posed() translate([0, -3, 0]) cube([80, 3, 10]);
}

// Nothing below the table, and it stands on it.
// @expect volume 1±0.01
module test_nothing_below_table() grader_marked() intersection() {
    stand();
    translate([-500, -500, -100]) cube([1000, 1000, 99.99]);
}

// Its base reaches from y <= -5 to y >= 40, so it does not tip over. The
// probes start 0.1 mm inside those lines: a base that ends exactly at
// y = -5 meets the requirement and must pass.
// @expect volume 5001±4999
module test_base_front() grader_marked() intersection() {
    stand();
    translate([-50, -500, 0]) cube([100, 495.1, 1]);
}

// @expect volume 5001±4999
module test_base_back() grader_marked() intersection() {
    stand();
    translate([-50, 39.9, 0]) cube([100, 500, 1]);
}
