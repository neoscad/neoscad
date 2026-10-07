// Anchors through every kind of node: transforms compose, operations
// that keep coordinates pass them on, and the ones that need geometry to
// place a point (resize, rotate_extrude) hide them.

module show(label) {
  echo(label, child_anchors());
  children();
}

module peg(h = 10) {
  cylinder(d = 4, h = h, $fn = 12);
  anchor("top", [0, 0, h], [0, 0, 1]);
  anchor("base", [0, 0, 0], [0, 0, -1]);
}

show("plain") peg();
show("translate") translate([10, 0, 0]) peg(5);
show("rotate") rotate([90, 0, 0]) peg();
show("rotate z") rotate(30) translate([10, 0, 0]) peg();
show("scale") scale([2, 3, 4]) peg();
show("mirror") mirror([1, 0, 0]) translate([5, 0, 0]) peg();
show("multmatrix") multmatrix([[1, 0, 0, 1], [0, 1, 0, 2], [0, 0, 1, 3], [0, 0, 0, 1]]) peg();
// The direction is a unit vector, and a point keeps its frame through
// booleans, colours, render, hull, minkowski.
show("dir") anchor("d", [1, 2, 3], [0, 0, 7]);
show("csg") difference() {
  color("red") render() translate([0, 0, 1]) peg();
  hull() minkowski() { cube(1); anchor("in minkowski", [1, 1, 1]); }
}
// Children of a group, a for loop, an if, a let.
show("for") for (i = [0:2]) translate([i * 10, 0, 0]) anchor(str("p", i), [0, 0, 0]);
show("if") if (true) anchor("yes", [1, 0, 0]); else anchor("no", [2, 0, 0]);
show("let") let(x = 4) anchor("let", [x, 0, 0]);
// An echo whose only child is an anchor makes no node; the anchor stays.
show("echo") echo("only an anchor") anchor("echoed", [0, 1, 0]);
// 2D anchors are at z = 0, and a linear extrusion keeps them at its base.
show("2D") translate([1, 1]) { square(2); anchor("corner", [2, 2]); }
show("extrude") linear_extrude(4) translate([3, 0]) { circle(1); anchor("c", [0, 0], [1, 0]); }
show("extrude centered") linear_extrude(4, center = true) { square(1); anchor("c", [0, 0]); }
show("projection") projection() translate([0, 0, 5]) { cube(1); anchor("up", [1, 1, 1], [0, 0, 1]); anchor("side", [1, 1, 1], [1, 0, 1]); }
// Hidden: their placement depends on the geometry.
show("resize") resize([10, 10, 10]) peg();
show("rotate_extrude") rotate_extrude($fn = 8) translate([5, 0]) { circle(1); anchor("hidden", [0, 0]); }
// A transform with NaN removes its children, and their anchors.
show("nan") multmatrix([[1, 0, 0, 0 / 0], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]]) peg();
// Anchors among the children themselves belong to `children()`'s node.
show("among children") { cube(1); anchor("loose", [9, 9, 9]); }
// Indices pick children, as children() does.
module pick() {
  echo(first = child_anchors(0), second = child_anchors(1), both = child_anchors([0, 1]), range = child_anchors([1:1]));
}
pick() { translate([1, 0, 0]) anchor("a", [0, 0, 0]); translate([2, 0, 0]) anchor("b", [0, 0, 0]); }
