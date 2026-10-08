// Every edge of an L-bracket in one call (stage F5a, two passes): the
// concave inner corner first, then the convex edges of the result,
// whose end-face outlines now run line, arc, line across the first
// pass's blend (quarter tori), with sphere corners where three meet.
// The result is the bracket opened by a ball of radius 1: its erosion
// (an L 1 thinner all round, its inner corner an arc of radius 2) swept
// by the ball. Per slice across x the eroded outline offset by ρ has
// area (163 − π) + (108 + π)ρ + πρ²; 38 slices of ρ = 1 and two ends
// integrating ρ = √(1 − t²) give 10624 + 274π/3 + π²/2.
// volume: 10915.866931228413
fillet_edges(r = 1) union() {
  cube([40, 30, 5]);
  cube([40, 5, 30]);
}
