// world_transform() and to_world(): a library module reports where its
// cube is in the model, under the transforms its callers wrote.
module stick(tag, s) {
  echo(tag, from = to_world([0, 0, 0]), to = to_world(s));
  cube(s);
}
module wall(x) translate([x, 0, 0]) children();
module flip() mirror([0, 1, 0]) children();

// Nested translate, mirror and rotate.
translate([10, 20, 30]) mirror([1, 0, 0]) rotate([0, 0, 90]) stick("post", [2, 4, 6]);
// Through modules that place their children, in a loop: each call is
// reported where it runs (no memo replays it).
wall(100) flip() for (i = [0:2]) translate([0, i * 40, 0]) rotate([0, -90, 0]) stick(str("joist ", i), [2, 4, 90]);
// The matrix itself, and a list of points.
multmatrix([[1, 0, 0, 5], [0, 2, 0, 0], [0, 0, 1, 0]]) scale([1, 1, 3]) {
  echo(m = world_transform());
  echo(corners = to_world([[0, 0], [1, 1], [1, 1, 1]]));
}
// A centred extrusion's profile plane, and a projection's.
linear_extrude(10, center = true) { echo(base = to_world([1, 1])); square(1); }
projection() translate([0, 0, 7]) { echo(flat = to_world([1, 1, 1])); cube(1); }
// A query's child, instantiated early, then put in place again.
module lift() { b = child_bounds(0); echo(bounds = b); translate([0, 0, 50]) children(0); }
lift() stick("lifted", [1, 1, 1]);
// No matrix under resize() and rotate_extrude(), and a bad point.
resize([5, 5, 5]) { echo(r = to_world([0, 0, 0])); cube(1); }
rotate_extrude() translate([5, 0]) { echo(re = world_transform()); square(1); }
echo(bad = to_world("x"), top = world_transform());
