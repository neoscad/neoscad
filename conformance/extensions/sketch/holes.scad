// Profiles with holes: loops fill even-odd, as polygon() with several
// paths does, so a loop inside another is a hole whatever its direction.
// Run with --enable sketch.

// A plate with a round hole and a slot-shaped hole drawn clockwise.
sketch(name = "plate", $fn = 16) {
  o = point([0, 0]);
  a = point([60, 0]);
  b = point([60, 30]);
  c = point([0, 30]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, c); l4 = line(c, o);
  fix(o); horizontal(l1); vertical(l2); horizontal(l3); vertical(l4);
  length(l1, 60); length(l2, 30);

  hole = circle([15, 15], d = 10);
  fix(hole.center);

  // A hole bounded by two lines and two half-circle arcs, drawn the other
  // way round from the outline.
  c1 = point([35, 15]);
  c2 = point([50, 15]);
  t = line([50, 20], [35, 20]);
  u = line([35, 10], [50, 10]);
  e1 = arc(c1, t.end, u.start);
  e2 = arc(c2, u.end, t.start);
  fix(c1); fix(c2);
  tangent(e1, t); tangent(e1, u); tangent(e2, t); tangent(e2, u);
  radius(e1, 5); equal(e1, e2);
}

// Three nested squares: the middle one is a hole, the inner one solid.
module square_loop(x, s) sketch() {
  p = point([x, x]);
  q = point([x + s, x]);
  r = point([x + s, x + s]);
  w = point([x, x + s]);
  l1 = line(p, q); l2 = line(q, r); l3 = line(r, w); l4 = line(w, p);
  fix(p); horizontal(l1); vertical(l2); horizontal(l3); vertical(l4);
  length(l1, s); length(l2, s);
}

translate([0, 40])
sketch(name = "nested") {
  square_loop(0, 30);
  square_loop(5, 20);
  square_loop(10, 10);
}

// The same circle as circle(): the same vertices, at any $fn, $fa, $fs.
translate([70, 0]) sketch() { c = circle([0, 0], r = 7); fix(c.center); }
translate([70, 0]) circle(r = 7);
translate([90, 0]) sketch($fn = 5) { c = circle([0, 0], r = 7); fix(c.center); }
translate([90, 0]) circle(r = 7, $fn = 5);
translate([110, 0]) sketch($fa = 5, $fs = 0.5) { c = circle([0, 0], r = 7); fix(c.center); }
translate([110, 0]) circle(r = 7, $fa = 5, $fs = 0.5);
