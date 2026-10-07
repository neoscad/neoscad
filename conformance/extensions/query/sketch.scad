// Sketch anchors (with --enable sketch): solved values as anchors. Every
// entity the body names is one (a line's is its midpoint and direction,
// an arc's or a circle's its centre), and anchor(name, entity) adds more.

// Holes drilled at a sketch's named points, wherever the solve put them.
module holes_at(names, d = 3) {
  a = child_anchors(0);
  echo(a);
  difference() {
    linear_extrude(4) children(0);
    for (k = names) translate(a[k][0] - [0, 0, 1]) cylinder(d = d, h = 6, $fn = 16);
  }
}

w = 30;
h = 20;
holes_at(["hole", "c2", "mid"])
  translate([10, 0])
  rotate(90)
  sketch(name = "plate") {
    o = point([0, 0]); b = point([25, 0]); c = point([25, 25]); d = point([0, 25]);
    l1 = line(o, b); l2 = line(b, c); l3 = line(c, d); l4 = line(d, o);
    c2 = point([20, 10]);
    fix(o); horizontal(l1); vertical(l2); horizontal(l3); vertical(l4);
    length(l1, w); length(l2, h);
    fix(c2);
    anchor("hole", c);
    anchor("mid", l3);
    anchor("free", [5, 5]);
    // An explicit anchor named like an entity replaces the entity's own.
    anchor("d", o);
  }

// A helper module whose body is a sketch adds to the sketch it is called
// from: its named entities are not the outer sketch's to export, but its
// anchor() statements are.
module tab(at) sketch() {
  t = point(at);
  fix(t);
  anchor("tab", t);
  anchor("tab corner", [at[0] + 1, at[1] + 1]);
}
module show() { echo(child_anchors()); children(); }
show() sketch(name = "with helper") {
  p = point([0, 0]); q = point([10, 0]); r = point([0, 10]);
  a = line(p, q); b = line(q, r); c = line(r, p);
  fix(p); fix(q); fix(r);
  tab([3, 3]);
  arcs = circle([20, 0], 2);
  fix(arcs.center);
}

// A sketch that fails gives an empty polygon and exports no solved
// anchors (its anchor() with coordinates stays).
show() sketch(name = "conflict") {
  p = point([0, 0]); q = point([10, 0]);
  l = line(p, q);
  fix(p); fix(q); length(l, 5);
  anchor("kept", [1, 1]);
}
