// Fillets and chamfers where two arcs meet (design section 4.5): each arc
// is shortened on its own circle, and a tangent arc (or a line) joins the
// cuts. The fillet's centre is where the two arcs' circles, offset by the
// radius, cross: square roots only. Run with --enable sketch.

// A lens: both corners are inside both circles, so the fillet touches
// each arc from inside.
sketch(name = "lens") {
  c1 = point([0, 0]);
  c2 = point([10, 0]);
  q  = point([5, -8]);
  t  = point([5, 8]);
  e1 = arc(c1, q, t);
  e2 = arc(c2, t, q);
  fix(c1); fix(c2); radius(e1, 10); radius(e2, 10);
  fillet(t, 2);
  chamfer(q, 2);
}

// A crescent: a convex arc meets a concave one at each tip, so the fillet
// is inside the outer circle and outside the inner one.
translate([30, 0])
sketch(name = "crescent") {
  o = point([0, 0]);
  m = point([5, 0]);
  t = point([6, 8]);
  q = point([6, -8]);
  outer = arc(o, t, q);
  inner = arc(m, q, t, cw = true);
  fix(o); fix(m); radius(outer, 10); radius(inner, 8);
  fillet(t, 1);
  chamfer(q, 1);
}

// A snowman: two discs' outline, whose waist corners point into the
// shape; each fillet is outside both circles and fills the notch.
translate([60, 0])
sketch(name = "snowman") {
  o  = point([0, 0]);
  n  = point([0, 15]);
  p1 = point([5, 9]);
  p2 = point([-5, 9]);
  body = arc(o, p2, p1);
  head = arc(n, p1, p2);
  fix(o); fix(n); radius(body, 10); radius(head, 8);
  fillet(p1, 2);
  fillet(p2, 2);
}

// The widest fillet that fits the lens (the hint's size below): it
// touches both arcs at the ends of a diameter, and still turns round the
// corner's side, so the lens's top becomes a half disc.
translate([90, 0])
sketch(name = "widest") {
  c1 = point([0, 0]);
  c2 = point([10, 0]);
  q  = point([5, -8]);
  t  = point([5, 8]);
  e1 = arc(c1, q, t);
  e2 = arc(c2, t, q);
  fix(c1); fix(c2); radius(e1, 10); radius(e2, 10);
  fillet(t, 5);
}

// Too large: the size that fits is the hint's edit.
translate([0, 30])
sketch(name = "too large") {
  c1 = point([0, 0]);
  c2 = point([10, 0]);
  q  = point([5, -8]);
  t  = point([5, 8]);
  e1 = arc(c1, q, t);
  e2 = arc(c2, t, q);
  fix(c1); fix(c2); radius(e1, 10); radius(e2, 10);
  fillet(t, 9);
}

// No corner where the arcs run on along each other's tangent.
translate([30, 30])
sketch(name = "tangent") {
  a = point([-4, -3]);
  j = point([5, 0]);
  z = point([14, 3]);
  s = point([14, -10]);
  w = point([-4, -10]);
  low  = arc([0, 0], a, j);
  high = arc([10, 0], j, z, cw = true);
  l1 = line(z, s); l2 = line(s, w); l3 = line(w, a);
  fix(j); fix(s); fix(w); fix(low.center); fix(high.center);
  vertical(l1); vertical(l3);
  fillet(j, 1);
}
