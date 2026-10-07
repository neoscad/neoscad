// child_bounds() and child_measure(): 2D and 3D children, through
// transforms and booleans, `%` and `#` children, empty children, nested
// queries and a recursion, and every warning. Each answer is echoed; the
// queried children are then instantiated, so the render shows what was
// measured.

module show(label) {
  echo(label, bounds = child_bounds(), measure = child_measure());
  children();
}

show("cube") cube([1, 2, 3]);
show("2D") translate([1, 1]) square(2);
show("2D with a hole") difference() {
  square(10, center = true);
  square(4, center = true);
}
show("rotated") rotate([0, 0, 45]) cube(1);
// The box of the result, not of the operands: a render is needed.
show("difference") difference() {
  cube(10);
  translate([5, 5, -1]) cube(20);
}
show("intersection") intersection() {
  sphere(r = 5, $fn = 24);
  cube(4);
}
// Measured as a render measures it, in preview too: `%` left out, `#` in.
show("background") {
  cube(1);
  %translate([10, 0, 0]) cube(1);
}
show("highlight") {
  cube(1);
  #translate([10, 0, 0]) cube(1);
}
// The first child with geometry decides the dimension, as in a render.
show("mixed") {
  translate([0, 0, 1]) cube(1);
  square(5);
}
// Empty: child_bounds() warns and is undef; child_measure() says so.
show("empty") cube(0);
show("no children");

// Indices, as children() takes them.
module pick() {
  echo(first = child_bounds(0), second = child_bounds(1), both = child_bounds([0, 1]));
  children();
}
pick() {
  cube(1);
  translate([5, 0, 0]) cube(1);
}

// Nested: a frame around its child, around a framed child.
module frame(m = 1) {
  b = child_bounds(0);
  translate(b[0] - [m, m, 0]) cube([b[1][0] - b[0][0] + 2 * m, b[1][1] - b[0][1] + 2 * m, 0.5]);
  children(0);
}
show("nested") frame() frame() translate([0, 0, 1]) cube(2);

// A recursion: each level stacks a cube on the one below's top.
module on_top() {
  b = child_bounds(0);
  children(0);
  translate([0, 0, b[1][2]]) cube(1);
}
module tower(n) {
  if (n == 0) cube(1);
  else on_top() tower(n - 1);
}
show("tower") tower(4);

// Warnings.
echo(outside = child_bounds(0));
module bad() {
  echo(index = child_bounds(2), not_a_number = child_measure("a"));
  children();
}
bad() cube(1);
