// A ball cut flat: the convex rim between the plane and the sphere. The
// ball's centre is 9 from the sphere's centre and 4 below the plane;
// the region between the corner, the arc and the sphere, revolved.
// volume: 3530.996392678154
fillet_edges(r = 1, edges = "%circle")
  intersection() {
    sphere(10);
    translate([-20, -20, -20]) cube([40, 40, 25]);
  }
