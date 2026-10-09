// Stage F5b: a branch of radius 3 standing on a rod of radius 5 (a tee
// of unequal cylinders), the concave junction filleted with r = 1: a
// ball rolled round it, the blend two rational B-spline patches.
// No closed form: the volume is the reference integration of
// meshbrep's `tests/sweep.rs` (`golden_reference_volumes`): the base
// solid's volume plus the blend's region swept in the normal planes of
// its exact spine, `∫ ds ∬ (1 − κξ) dA`, to about 1e-11.
// volume: 2562.946181356
fillet_edges(r = 1, edges = "concave") {
  rotate([0, 90, 0]) cylinder(r = 5, h = 30, center = true);
  cylinder(r = 3, h = 12);
}
