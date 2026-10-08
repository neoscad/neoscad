// Every edge of an L-bracket chamfered in one call (stage F5a, two
// passes): the inner corner first (a 1 × 1 triangle along it, + 20),
// then the convex edges of the result, three chamfers meeting in a point
// at each corner. All faces are planes; the volume is OCCT 8.0.1's of the
// exported file (exactly 10810 + 1/6 to its digits), not written out.
// volume: 10810.166666666667
chamfer_edges(d = 1) union() {
  cube([40, 30, 5]);
  cube([40, 5, 30]);
}
