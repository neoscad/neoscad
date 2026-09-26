// Hidden grader for the agent-eval task `pipe_adapter` (scripts/agent-eval).
// The pipes are probed at their true outside diameters (32 and 20 mm) at
// high resolution, so a bore of the asked 32.5 / 20.5 mm passes at any
// reasonable $fn while a bore drawn at the pipe's own size does not.
// `include`, not `use`: the model is judged as its author renders it,
// with its own top-level `$fn` (which `use` would drop, leaving curves at
// default resolution). Its top-level geometry is no part of any test.
include <model.scad>

module grader_marked() union() { children(); translate([500, 0, 0]) cube(1); }
module grader_ring(d1, d2, z0, z1) translate([0, 0, z0]) difference() {
    cylinder(d = d2, h = z1 - z0, $fn = 128);
    translate([0, 0, -1]) cylinder(d = d1, h = z1 - z0 + 2, $fn = 128);
}

// One printable solid.
// @expect manifold
// @expect components 1
// @expect check no-error
module test_adapter_solid() adapter();

// 70 mm tall, from z = 0: nothing below or above, material at both ends.
// @expect volume 1±0.01
module test_nothing_outside_height() grader_marked() intersection() {
    adapter();
    union() {
        translate([-100, -100, -100]) cube([200, 200, 99.99]);
        translate([-100, -100, 70.01]) cube([200, 200, 100]);
    }
}

// @expect volume 101±100
module test_reaches_top() grader_marked() intersection() {
    adapter();
    translate([-100, -100, 69.5]) cube([200, 200, 0.5]);
}

// The 32 mm pipe slides 30 mm into the bottom socket...
// @expect volume 1±0.05
module test_bottom_socket_fits() grader_marked() intersection() {
    adapter();
    translate([0, 0, -5]) cylinder(d = 32, h = 35, $fn = 128);
}

// ...and stops there (a shoulder or taper narrows the bore above).
// @expect volume 1001±999
module test_bottom_socket_stop() grader_marked() intersection() {
    adapter();
    grader_ring(24, 31.5, 30, 34);
}

// The 20 mm pipe slides 25 mm into the top socket.
// @expect volume 1±0.05
module test_top_socket_fits() grader_marked() intersection() {
    adapter();
    translate([0, 0, 45]) cylinder(d = 20, h = 30, $fn = 128);
}

// The passage is open all the way through at 16 mm.
// @expect volume 1±0.05
module test_passage_open() grader_marked() intersection() {
    adapter();
    translate([0, 0, -1]) cylinder(d = 16, h = 72, $fn = 128);
}

// Walls at least 2.5 mm around both sockets: a ring from just outside
// each bore to 2.25 mm out (the rest is left for facets) is solid, except
// 1 mm at the ends for a chamfer.
// @expect volume 6493±40
module test_bottom_wall() grader_marked() intersection() {
    adapter();
    grader_ring(32.6, 37, 1, 28);
}

// @expect volume 3624±30
module test_top_wall() grader_marked() intersection() {
    adapter();
    grader_ring(20.6, 25, 46, 69);
}
