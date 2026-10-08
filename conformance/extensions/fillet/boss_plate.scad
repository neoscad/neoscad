// 12.3: the concave rim where a boss stands on a plate, a torus tool
// added (Pappus: the spandrel r²(1 − π/4) revolved at 6 + 0.2234 r).
// volume: 7565.744034295069
fillet_edges(r = 2, edges = "child(0, 1)") {
  translate([-20, -20, 0]) cube([40, 40, 4]);
  cylinder(r = 6, h = 14);
}
