// polyline(): a closed profile from a list of points, with constraints
// generated from the drawing.
P = [[0, 0], [40, 0], [40, 10], [25, 30], [0, 30]];

// Fully determined: each edge but the closing one dimensioned, and the
// first point fixed. Strict, so a freedom left would be an error.
sketch(name = "fixed", strict = true) {
  e = polyline(P, constrain = "fixed");
  echo(e[0], e[2].start, e[4].end, len(e));
}

// The shape alone, placed by the author's own constraint; further
// constraints attach to the handles.
translate([60, 0]) sketch(name = "placed", strict = true) {
  e = polyline(P, constrain = "edges");
  fix(e[0].start, [0, 0]);
  fillet(e[2].start, 3);
}

// Without constraints, as if written point by point: the drawing is only
// a guess, and the hints suggest what to add.
translate([0, 50]) sketch(name = "drawn") {
  e = polyline([[0, 0], [20, 0], [20, 10], [0, 10]]);
  fix(e[0].start);
}

// Strict with the edges alone: the shape can still move.
translate([60, 50]) sketch(name = "loose", strict = true) {
  e = polyline(P, constrain = "edges");
}

// One constraint the generated ones already imply, and one that
// contradicts them.
translate([0, 100]) sketch(name = "redundant", strict = true) {
  e = polyline(P, constrain = "fixed");
  length(e[4], 30);
}
translate([60, 100]) sketch(name = "conflict", strict = true) {
  e = polyline(P, constrain = "fixed");
  length(e[0], 35);
}

// Open, with the closing line written by hand.
translate([0, 150]) sketch(name = "open") {
  a = polyline([[0, 0], [10, 0], [10, 10]], closed = false, constrain = "fixed");
  b = line(a[1].end, a[0].start);
}

// A repeated point is a zero-length edge, named as such.
sketch(name = "repeat") {
  e = polyline([[0, 0], [10, 0], [10, 0], [10, 5]], constrain = "fixed");
}

// The same mistake written by hand: a length of 0.
sketch(name = "zero") {
  q = [point([0, 0]), point([10, 0]), point([10, 0])];
  e = [line(q[0], q[1]), line(q[1], q[2]), line(q[2], q[0])];
  length(e[1], 0);
}

// Argument errors.
sketch(name = "short") { e = polyline([[0, 0], [1, 0]]); }
sketch(name = "mode") { e = polyline(P, constrain = "all"); }
sketch(name = "statement") { polyline(P); }
