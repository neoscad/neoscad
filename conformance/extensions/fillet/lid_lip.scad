// 12.2, the lid: where the lip stands on the lid, both of its walls'
// outlines (concave chains of four lines and four arcs each). The
// lines-only golden (lid_lip_lines) plus the arcs: the outer outline's
// spandrels revolved at 5 + 0.2234 r about each corner's axis, the inner
// outline's lines (100 long) and arcs at 3.5 − 0.2234 r.
// volume: 5176.416969137687
L = 40; W = 30; R = 5;
fillet_edges(r = 1, edges = "child(0, 1)")
{
  translate([-2, -2, 0]) fillet_edges(r = 7, edges = "|z") cube([44, 34, 3]);
  translate([0, 0, 3]) difference() {
    fillet_edges(r = R, edges = "|z") cube([L, W, 4]);
    translate([1.5, 1.5, -1]) fillet_edges(r = R - 1.5, edges = "|z")
      cube([L - 3, W - 3, 6]);
  }
}
