// Stage F5b: a hole of radius 3 across a rod of radius 8, 4 along it
// from the middle; both rims (convex, between the rod and the hole's
// wall) filleted with r = 1.
// No closed form: the volume is the reference integration of
// meshbrep's `tests/sweep.rs` (`golden_reference_volumes`): the base
// solid's volume plus the blend's region swept in the normal planes of
// its exact spine, `∫ ds ∬ (1 − κξ) dA`, to about 1e-11.
// volume: 7580.873559849
fillet_edges(r = 1, edges = "convex and not %circle") difference() {
  rotate([0, 90, 0]) cylinder(r = 8, h = 40, center = true);
  translate([4, 0, 0]) cylinder(r = 3, h = 20, center = true);
}
