// Stage F5b: a rod of radius 3 standing in a ball of radius 8, its axis
// 2 off the ball's centre; the junction (between the sphere and the
// cylinder) filleted with r = 1.
// No closed form: the volume is the reference integration of
// meshbrep's `tests/sweep.rs` (`golden_reference_volumes`): the base
// solid's volume plus the blend's region swept in the normal planes of
// its exact spine, `∫ ds ∬ (1 − κξ) dA`, to about 1e-11.
// volume: 2331.750528339
fillet_edges(r = 1, edges = "concave") {
  sphere(8);
  translate([2, 0, 0]) cylinder(r = 3, h = 14);
}
