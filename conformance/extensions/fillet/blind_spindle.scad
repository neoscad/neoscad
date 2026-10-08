// The bottom rim of a blind hole of radius 4 with r = 3 (stage F5a): a
// concave blend whose torus is a spindle (centre 1 from the axis).
// Pappus: 8000 − 240π + 2π(R r²(1 − π/4) − r³(5/6 − π/4))
// = 8000 − 213π − 4.5π².
// volume: 7286.427544980472
fillet_edges(r = 3, edges = "%circle and concave") difference() {
  cube(20);
  translate([10, 10, 5]) cylinder(r = 4, h = 20);
}
