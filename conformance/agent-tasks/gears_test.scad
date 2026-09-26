// Hidden grader for the agent-eval task `gears` (scripts/agent-eval):
// module 2, 20° pressure angle, 24 and 12 teeth, 6 mm thick, 5 mm bores,
// centres 36 mm apart. Tooth counts are checked by symmetry: a gear with
// N teeth is unchanged by a turn of 360/N degrees, one with N±1 is not.
// `include`, not `use`: the model is judged as its author renders it,
// with its own top-level `$fn` (which `use` would drop, leaving curves at
// default resolution). Its top-level geometry is no part of any test.
include <model.scad>

module grader_marked() union() { children(); translate([500, 0, 0]) cube(1); }

// @expect manifold
// @expect components 1
// @expect bbox [52, 52, 6]±0.8
module test_gear_a_solid() gear_a();

// @expect manifold
// @expect components 1
// @expect bbox [28, 28, 6]±0.8
module test_gear_b_solid() gear_b();

// 24 teeth: a 15° turn changes almost nothing.
// @expect volume 16±15
module test_gear_a_teeth() grader_marked() difference() {
    gear_a();
    rotate([0, 0, 15]) gear_a();
}

// 12 teeth, turning about its own centre.
// @expect volume 16±15
module test_gear_b_teeth() grader_marked() difference() {
    gear_b();
    translate([36, 0, 0]) rotate([0, 0, 30]) translate([-36, 0, 0]) gear_b();
}

// About as much material as a gear of these sizes has.
// @expect volume 10700±1600
module test_gear_a_volume() gear_a();

// @expect volume 2550±450
module test_gear_b_volume() gear_b();

// They mesh without overlapping (real interference is tens of mm³; up to
// 1 mm³ allows for flanks drawn with no backlash)...
// @expect volume 1±1
module test_no_overlap() grader_marked() intersection() { gear_a(); gear_b(); }

// ...with gear_a's teeth reaching inside gear_b's pitch circle, and gear_b's
// inside gear_a's.
// @expect volume 51±50
module test_a_meshes() grader_marked() intersection() {
    gear_a();
    translate([36, 0, -1]) cylinder(r = 12, h = 8, $fn = 96);
}

// @expect volume 51±50
module test_b_meshes() grader_marked() intersection() {
    gear_b();
    translate([0, 0, -1]) cylinder(r = 24, h = 8, $fn = 192);
}

// 5 mm bores on the axes (a 4.8 mm pin fits).
// @expect volume 1±0.05
module test_bores() grader_marked() intersection() {
    union() { gear_a(); gear_b(); }
    union() {
        translate([0, 0, -1]) cylinder(d = 4.8, h = 8, $fn = 64);
        translate([36, 0, -1]) cylinder(d = 4.8, h = 8, $fn = 64);
    }
}
