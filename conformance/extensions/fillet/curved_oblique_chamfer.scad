// Stage F5b: curved_oblique_hole's rims chamfered with d = 1.
// No closed form: the volume is the reference integration of
// `crates/meshbrep/tests/sweep.rs` (`golden_reference_volumes`): the base
// solid's volume plus the chamfer's region swept in the normal planes
// of the edge, `∫ ds ∬ (1 − κξ) dA`, to about 1e-11.
// volume: 12308.518475017
chamfer_edges(d = 1, edges = "%ellipse") difference() {
  cube([40, 40, 8]);
  translate([20, 20, 4]) rotate([30, 0, 0]) cylinder(r = 4, h = 40, center = true);
}
