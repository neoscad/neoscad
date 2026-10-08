// Both rims of a boss on a plate in one call: the top rim convex (a
// torus subtracted, at 6 − 0.2234 r), the base concave (one added, at
// 6 + 0.2234 r).
// volume: 7535.792324179338
fillet_edges(r = 2, edges = "%circle") {
  translate([-20, -20, 0]) cube([40, 40, 4]);
  cylinder(r = 6, h = 14);
}
