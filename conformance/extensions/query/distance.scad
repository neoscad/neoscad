// child_distance(a, b): the smallest distance between two children as
// rendered, 0 where they overlap or one holds the other; 2D children in
// the plane; and its warnings. Each answer is echoed; the children are
// then instantiated, so the render shows what was measured.

module gap(label) {
  echo(label, d = child_distance(0, 1));
  children();
}

gap("apart along x") {
  cube(10);
  translate([13, 0, 0]) cube(10);
}
gap("corner to corner") {
  cube(1);
  translate([2, 2, 2]) cube(1);
}
gap("touching") {
  cube(5);
  translate([5, 0, 0]) cube(5);
}
gap("overlapping") {
  cube(5);
  translate([2, 2, 2]) cube(5);
}
// No surface of one meets the other's, but they overlap: 0.
gap("one inside the other") {
  cube(10, center = true);
  cube(2, center = true);
}
// The distance is the rendered shapes': the hole is cut first.
gap("into a cut") {
  difference() {
    cube(20, center = true);
    cube([30, 6, 6], center = true);
  }
  sphere(r = 1, $fn = 16);
}
gap("2D") {
  square(4);
  translate([7, 4]) square(4);
}
gap("2D inside a hole") {
  difference() {
    square(20, center = true);
    square(10, center = true);
  }
  circle(r = 2, $fn = 12);
}

// Indices as children() takes them: a list is those children together.
module pair() {
  echo(between_lists = child_distance([0, 1], 2));
  children();
}
pair() {
  cube(1);
  translate([0, 3, 0]) cube(1);
  translate([5, 0, 0]) cube(1);
}

// Warnings, each with the answer undef.
gap("empty") {
  cube(1);
  cube(0);
}
gap("2D and 3D") {
  cube(1);
  square(1);
}
module one() {
  echo(missing = child_distance(0));
  echo(out_of_range = child_distance(0, 3));
  children();
}
one() cube(1);
echo(top_level = child_distance(0, 1));
