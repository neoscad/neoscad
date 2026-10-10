// A ball sunk in a plate to just under its equator: the concave rim 0.4
// below the sphere's centre, where the tool's tangent ring lies between
// two of the sphere's rings and is conformed to its polygon. The region
// between the plane, the arc and the sphere, revolved, as `sphere_base`.
// volume: 10174.408845339954
fillet_edges(r = 1.5, edges = "%circle") {
  translate([-15, -15, 0]) cube([30, 30, 10]);
  translate([0, 0, 10.4]) sphere(8);
}
