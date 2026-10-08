// A loose plate (height free), with a redundant and a construction line.
sketch(name = "loose") {
  o = point([0, 0]);
  a = point([20, 0]);
  b = point([20, 10]);
  c = point([0, 10]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, c); l4 = line(c, o);
  diag = line(o, b, construction = true);
  fix(o); horizontal(l1); vertical(l2); horizontal(l3); vertical(l4);
  length(l1, 20); horizontal(o, a);
  h = circle(point([10, 5]), r = 2);
}
// Conflicting dimensions.
translate([30, 0])
sketch(name = "conflict") {
  o = point([0, 0]);
  a = point([20, 0]);
  l1 = line(o, a);
  fix(o); horizontal(l1); length(l1, 20); distance(o, a, 25);
}
