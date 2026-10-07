// The sketch vocabulary is bound only inside sketch bodies, so names that
// BOSL2 and MCAD define keep their meaning everywhere else. These are
// those libraries' shapes of definition (BOSL2: `function arc`,
// `module arc`, `module circle`, `module fillet`; MCAD: `function
// distance`, `function angle`, `module chamfer`), cut down to echoes.
// Run with --enable sketch.

function arc(r) = "BOSL2 arc()";
module arc(r) echo("BOSL2 arc module");
module circle(r) echo("BOSL2 circle module");
module fillet(r) echo("BOSL2 fillet module");
function distance(a, b) = "MCAD distance()";
function angle(v) = "MCAD angle()";
module chamfer(len, r) echo("MCAD chamfer module");

echo(arc(1), distance([0, 0], [1, 1]), angle([1, 0, 0]));
arc(1);
circle(3);
fillet(1);
chamfer(1, 2);

// A helper whose body is a sketch adds to the sketch it is called from:
// a reusable constraint pattern. Its vocabulary is the sketch's too.
module rectangle(o, w, h) sketch() {
  a = point([w, 0]);
  b = point([w, h]);
  c = point([0, h]);
  l1 = line(o, a); l2 = line(a, b); l3 = line(b, c); l4 = line(c, o);
  horizontal(l1); vertical(l2); horizontal(l3); vertical(l4);
  length(l1, w); length(l2, h);
}

module rounded(corner, r) sketch() fillet(corner, r);

linear_extrude(2) sketch(name = "plate", $fn = 24) {
  o = point([0, 0]);
  fix(o);
  rectangle(o, 40, 20);
  rounded(o, 3);
  hole = circle([20, 10], r = 5);
  fix(hole.center);
  // In the body, `circle`, `arc`, `fillet` and `chamfer` are the sketch's;
  // a function the vocabulary does not have (`distance()` is only a
  // statement here) is still the program's own.
  echo(hole, hole.center, distance(1, 2));
}

// Outside the sketch again, the program's own definitions.
echo(arc(2));
fillet(2);
