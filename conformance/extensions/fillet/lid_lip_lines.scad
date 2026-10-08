// The lid of 12.2: the lines of the concave outline where the lip's outer
// wall stands on the lid (the arcs are F3's).
// volume: 5143.495529071035
L = 40; W = 30; R = 5;
fillet_edges(r = 1, edges = "child(0, 1) and %line and not box(1, 1, 2, 39, 29, 4)")
{
  translate([-2, -2, 0]) fillet_edges(r = 7, edges = "|z") cube([44, 34, 3]);
  translate([0, 0, 3]) difference() {
    fillet_edges(r = R, edges = "|z") cube([L, W, 4]);
    translate([1.5, 1.5, -1]) fillet_edges(r = R - 1.5, edges = "|z")
      cube([L - 3, W - 3, 6]);
  }
}
