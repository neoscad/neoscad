// Stage F5b: curved_tee's junction chamfered with d = 0.8: on each face,
// in the plane across the edge, the points 0.8 from it, joined by a
// ruled B-spline surface.
// No closed form: the volume is the reference integration of
// meshbrep's `tests/sweep.rs` (`golden_reference_volumes`): the base
// solid's volume plus the chamfer's region swept in the normal planes
// of the edge, `∫ ds ∬ (1 − κξ) dA`, to about 1e-11.
// volume: 2566.814173266
chamfer_edges(d = 0.8, edges = "concave") {
  rotate([0, 90, 0]) cylinder(r = 5, h = 30, center = true);
  cylinder(r = 3, h = 12);
}
