// Stage 3's hints: each diagnostic below comes with hints that are exact
// edits where one is known (see hints.json, the diagnostics as JSON).
// Run with --enable sketch.

// Under-constrained: the suggested constraints, measured on the solution,
// that would fix the rest (here the widths, and where it sits).
sketch(name = "loose") {
  o = point([0, 0]); a = point([20, 0]); b = point([20, 10]); c = point([0, 10]);
  bottom = line(o, a); right = line(a, b); top = line(b, c); left = line(c, o);
  horizontal(bottom); horizontal(top); vertical(right); vertical(left);
}

// The same, strict: an error with the same hints.
sketch(name = "strict", strict = true) {
  o = point([0, 0]); a = point([20, 0]); b = point([20, 10]); c = point([0, 10]);
  bottom = line(o, a); right = line(a, b); top = line(b, c); left = line(c, o);
  fix(o); horizontal(bottom); horizontal(top); vertical(right); vertical(left);
  length(bottom, 20);
}

// Redundant: delete the statement (its line, as nothing else is on it).
sketch(name = "redundant") {
  o = point([0, 0]); a = point([20, 0]); b = point([20, 10]); c = point([0, 10]);
  bottom = line(o, a); right = line(a, b); top = line(b, c); left = line(c, o);
  fix(o); horizontal(bottom); horizontal(top); vertical(right); vertical(left);
  length(bottom, 20); length(right, 10);
  length(top, 20);
}

// Two horizontal edges a distance apart: the parallel that distance()
// implies is not reported as redundant.
sketch(name = "apart") {
  o = point([0, 0]); a = point([20, 0]); b = point([20, 10]); c = point([0, 10]);
  bottom = line(o, a); right = line(a, b); top = line(b, c); left = line(c, o);
  fix(o); horizontal(bottom); horizontal(top); vertical(right); vertical(left);
  length(bottom, 20);
  distance(bottom, top, 12);
}

// Conflict: remove either statement.
sketch(name = "conflict") {
  o = point([0, 0]); a = point([20, 0]); b = point([20, 10]); c = point([0, 10]);
  bottom = line(o, a); right = line(a, b); top = line(b, c); left = line(c, o);
  fix(o); horizontal(bottom); horizontal(top); vertical(right); vertical(left);
  length(bottom, 20); length(right, 10);
  length(top, 25);
}

// Flipped: a corner drawn turning one way, dimensioned the other; pin the
// drawing to the solution (one edit of the whole call).
sketch(name = "flipped") {
  o = point([0, 0]); a = point([10, 0]); b = point([10, 1]);
  base = line(o, a); side = line(a, b); back = line(b, o);
  fix(o); horizontal(base); vertical(side);
  length(base, 10);
  distance(a, b, -6, along = "y");
}

// A point without a guess: the solver placed it.
sketch(name = "placed") {
  o = point([0, 0]); a = point([10, 0]); b = point();
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, o);
  fix(o); fix(a);
  distance(o, b, 4, along = "x"); distance(o, b, 5, along = "y");
}

// Loops that cross: a warning, and the shape as even-odd fills it.
sketch(name = "crossing") {
  o = point([0, 0]); a = point([10, 0]); b = point([10, 10]); c = point([0, 10]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, c); l4 = line(c, o);
  fix(o); fix(a); fix(b); fix(c);
  ring = circle([10, 5], r = 3);
  fix(ring.center);
}

// A fillet too large: the size replaced with the largest that fits.
sketch(name = "big") {
  o = point([0, 0]); a = point([10, 0]); b = point([0, 10]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, o);
  fix(o); fix(a); fix(b);
  fillet(o, 30);
}

// Labels from values: an entity a function returns, and list elements.
sketch(name = "labels") {
  at = function(xy) point(xy);
  p = at([0, 0]);
  q = [point([5, 0]), point([0, 5])];
  l1 = line(p, q[0]); l2 = line(q[0], q[1]); l3 = line(q[1], p);
  fix(p); fix(q[0]);
  echo(p, q);
}
