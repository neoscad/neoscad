// Fillets and chamfers where a line meets an arc (design section 4.5,
// stage 7): the line is trimmed, the arc shortened on its own circle, and
// a tangent arc (or a line) joins the cuts. The sharp corner stays the
// dimensioned point. Run with --enable sketch.

// A quarter disc: the lines run into the arc's circle at its two corners,
// so the fillet touches the arc from inside.
sketch(name = "pie") {
  o  = point([0, 0]);
  a  = point([20, 0]);
  b  = point([0, 20]);
  l1 = line(o, a);
  e  = arc(o, a, b);
  l2 = line(b, o);
  fix(o); horizontal(l1); vertical(l2); length(l1, 20);
  fillet(a, 3);
  chamfer(b, 2);
  fillet(o, 1);
}

// A scoop: a concave arc across the top, so the lines meet it from
// outside its circle and the fillet touches it from outside.
translate([30, 0])
sketch(name = "scoop") {
  p0 = point([0, 0]);
  p1 = point([30, 0]);
  p2 = point([30, 20]);
  p3 = point([0, 20]);
  c  = point([15, 30]);
  bottom = line(p0, p1);
  right  = line(p1, p2);
  top    = arc(c, p2, p3, cw = true);
  left   = line(p3, p0);
  fix(p0); fix(p1); fix(p3); fix(c); vertical(right);
  fillet(p2, 2);
  chamfer(p3, 2);
}

// Both corners of an arc filleted.
translate([0, 30])
sketch(name = "both corners") {
  o  = point([0, 0]);
  a  = point([12, 0]);
  b  = point([0, 12]);
  e  = arc(o, a, b);
  fix(o); horizontal(line(o, a)); vertical(line(b, o)); radius(e, 12);
  fillet(a, 1.5);
  fillet(b, 1.5);
}

// Too large: the size that fits is the hint's edit.
translate([70, 0])
sketch(name = "too large") {
  o  = point([0, 0]);
  a  = point([20, 0]);
  b  = point([0, 20]);
  l1 = line(o, a);
  e  = arc(o, a, b);
  l2 = line(b, o);
  fix(o); horizontal(l1); vertical(l2); length(l1, 20);
  fillet(a, 25);
}

// No corner where the line runs on along the arc's tangent.
translate([0, -30])
sketch(name = "tangent") {
  c1  = point([0, 0]);
  c2  = point([20, 0]);
  top = line([0, 4], [20, 4]);
  bot = line([20, -4], [0, -4]);
  e1  = arc(c1, top.start, bot.end);
  e2  = arc(c2, bot.start, top.end);
  fix(c1); fix(c2); tangent(e1, top); tangent(e1, bot);
  tangent(e2, top); tangent(e2, bot); radius(e1, 4); equal(e1, e2);
  fillet(top.start, 1);
}

// Two arcs meeting: not supported yet.
translate([40, -30])
sketch(name = "two arcs") {
  o  = point([0, 0]);
  a  = point([10, 0]);
  b  = point([0, 10]);
  m  = point([10, 10]);
  e1 = arc(o, a, b);
  e2 = arc(m, b, a);
  fix(o); fix(m); fix(a);
  fillet(a, 1);
}
