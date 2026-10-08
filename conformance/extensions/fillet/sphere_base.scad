// A ball half sunk in a plate: the concave rim where the plate's top
// meets the sphere, a torus tool added (its region between the plane,
// the arc and the sphere, revolved).
// volume: 5608.906958385951
fillet_edges(r = 1.5, edges = "%circle") {
  translate([-15, -15, 0]) cube([30, 30, 4]);
  translate([0, 0, 9]) sphere(8);
}
