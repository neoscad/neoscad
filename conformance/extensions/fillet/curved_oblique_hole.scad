// Stage F5b: a hole of radius 4 through a plate 8 thick at 30°; both
// rims (ellipses, convex) filleted with r = 1.
// No closed form: the volume is the reference integration of
// `crates/meshbrep/tests/sweep.rs` (`golden_reference_volumes`): the base
// solid's volume plus the blend's region swept in the normal planes of
// its exact spine, `∫ ds ∬ (1 − κξ) dA`, to about 1e-11.
// volume: 12319.415435677
fillet_edges(r = 1, edges = "%ellipse") difference() {
  cube([40, 40, 8]);
  translate([20, 20, 4]) rotate([30, 0, 0]) cylinder(r = 4, h = 40, center = true);
}
