// Hidden grader for the agent-eval task `box_lid` (scripts/agent-eval).
// The agent never sees this file: it is copied next to the agent's
// model.scad only after the run ends. `grader_marked(...)` adds a 1 mm³ cube far
// away so an empty intersection (no overlap) measures 1 instead of
// failing as "an empty model".
// `include`, not `use`: the model is judged as its author renders it,
// with its own top-level `$fn` (which `use` would drop, leaving curves at
// default resolution). Its top-level geometry is no part of any test.
include <model.scad>

module grader_marked() union() { children(); translate([500, 0, 0]) cube(1); }

// The box: 60 x 40 x 30 at the origin, 2 mm walls and floor, open top.
// @expect bbox [[0, 0, 0], [60, 40, 30]]±0.05
// @expect volume 15552±3%
// @expect manifold
// @expect components 1
module test_box() box();

// The lid is one solid piece with a 60 x 40 footprint.
// @expect manifold
// @expect components 1
module test_lid_solid() lid();

// @expect bbox [60, 40]±0.3
module test_lid_footprint() projection() lid();

// In its assembled position the lid does not overlap the box.
// @expect volume 1±0.05
module test_lid_does_not_overlap() grader_marked() intersection() { box(); lid(); }

// It covers the top: some of it is above the rim.
// @expect volume 20001±19000
module test_lid_on_top() grader_marked() intersection() {
    lid();
    translate([-100, -100, 30]) cube([260, 240, 100]);
}

// A lip or plug reaches down into the opening, so it cannot slide off.
// @expect volume 15001±14990
module test_lid_locates() grader_marked() intersection() {
    lid();
    translate([2, 2, 2]) cube([56, 36, 28]);
}
