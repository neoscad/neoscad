// Every diagnostic stage 2 prints, each at the span of the constraint or
// entity it is about. A failed sketch gives an empty polygon and the rest
// of the model still evaluates. Run with --enable sketch.

module triangle() sketch() {
  o = point([0, 0]);
  a = point([10, 0]);
  b = point([0, 10]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, o);
}

// Free degrees of freedom: information (as in FreeCAD).
sketch(name = "under") { triangle(); }

// The same with strict = true: an error, and no shape.
sketch(name = "strict", strict = true) { c = circle([0, 0], r = 5); }

// Two lengths that cannot both hold.
sketch(name = "conflict") {
  o = point([0, 0]); a = point([10, 0]); b = point([0, 10]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, o);
  fix(o); horizontal(l1); vertical(l3);
  length(l1, 10);
  length(l3, 10);
  distance(o, a, 12);
}

// The same distance stated twice: a warning, and the shape.
sketch(name = "redundant") {
  o = point([0, 0]); a = point([10, 0]); b = point([0, 10]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, o);
  fix(o); horizontal(l1); vertical(l3);
  length(l1, 10);
  length(l3, 10);
  distance(o, a, 10);
}

// A profile that does not close.
sketch(name = "open") {
  o = point([0, 0]); a = point([10, 0]); b = point([0, 10]);
  l1 = line(o, a); l2 = line(a, b);
  fix(o); fix(a); fix(b);
}

// A fillet longer than the lines it trims.
sketch(name = "big") {
  o = point([0, 0]); a = point([10, 0]); b = point([0, 10]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, o);
  // Constraints in a loop make no geometry.
  for (p = [o, a, b]) fix(p);
  fillet(o, 30);
}

// Geometry and values that are not entities in a body.
sketch(name = "misuse") {
  o = point([0, 0]);
  circle(5);
  fix(3);
  translate([1, 0]) square(2);
}

// Unknown outside a sketch body: the vocabulary is not global.
fix(1);
echo("still evaluating");
