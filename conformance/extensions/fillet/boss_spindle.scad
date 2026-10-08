// Both rims of a boss of radius 4 with r = 3 (stage F5a): the top rim's
// blend is a spindle torus (its centre 1 from the axis, nearer than its
// radius), exported as its outer part; the base's a ring torus. Pappus
// with the spandrel's moment r³(5/6 − π/4): the top takes
// 2π(R r²(1 − π/4) − r³(5/6 − π/4)), the base adds
// 2π(R r²(1 − π/4) + r³(5/6 − π/4)), so 6400 + 160π + 108π(5/6 − π/4)
// = 6400 + 250π − 27π².
// volume: 6918.918844568036
fillet_edges(r = 3, edges = "%circle") {
  translate([-20, -20, 0]) cube([40, 40, 4]);
  cylinder(r = 4, h = 14);
}
