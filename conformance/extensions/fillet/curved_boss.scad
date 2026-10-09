// Stage F5b: a boss of radius 3 on the side of a rod of radius 8, its
// axis 2 off the rod's, the junction filleted with r = 1.5.
// No closed form: the volume is the reference integration of
// meshbrep's `tests/sweep.rs` (`golden_reference_volumes`): the base
// solid's volume plus the blend's region swept in the normal planes of
// its exact spine, `∫ ds ∬ (1 − κξ) dA`, to about 1e-11.
// volume: 8173.690799999
fillet_edges(r = 1.5, edges = "concave") {
  rotate([0, 90, 0]) cylinder(r = 8, h = 40, center = true);
  translate([0, 2, 0]) cylinder(r = 3, h = 12);
}
