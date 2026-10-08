// A block on a plate, every edge (stage F5a, two passes; the case v1
// refused with the nested rewrite): the block's base mitred concave
// first, then every convex edge of the result. The block's vertical
// blends end square where the base blends' tangent lines meet them, and
// the ellipses where the mitred base blends meet stay sharp, as in the
// nested calls. No closed form is written out: the volume is OCCT 8.0.1's
// of the exported file, which ours (2956.03833638) agrees with to 1e-10.
// volume: 2956.0383360803
fillet_edges(r = 1) union() {
  cube([20, 20, 5]);
  translate([5, 5, 0]) cube([10, 10, 10 + 5]);
}
