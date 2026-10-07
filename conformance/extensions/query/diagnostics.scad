// Every query diagnostic but the errors (errors.scad), at its span.

module m(n) { cube(1); anchor(str("m", n), [n, 0, 0]); }

// query-outside-module: there are no children to ask about.
echo(top = child_anchors(0));
function f() = child_anchors(0);
module calls_f() { echo(in_function = f()); children(); }
calls_f() m(0);

// query-index: out of range, not a number, in a list, in a range.
module bad() {
  echo(child_anchors(5));
  echo(child_anchors(-1));
  echo(child_anchors("x"));
  echo(child_anchors([0, 7]));
  echo(child_anchors([0:3]));
  echo(child_anchors(index = 0));
}
bad() m(1);
module none() { echo(no_children = child_anchors(), first = child_anchors(0)); }
none();

// query-duplicate-anchor: the first is kept.
module dup() { echo(child_anchors()); }
dup() { m(2); m(2); translate([5, 0, 0]) m(3); m(3); }

// anchor()'s arguments.
module show() { echo(child_anchors()); children(); }
show() {
  anchor(1, [0, 0, 0]);
  anchor("p", "x");
  anchor("q", [1, 2, 3, 4]);
  anchor("r", [1, 0 / 0]);
  anchor("s", [0, 0, 0], [0, 0, 0]);
  anchor("t", [0, 0, 0], "up");
  anchor("u", [0, 0], extra = 1);
  anchor("v", [0, 0]) cube(1);
  anchor("ok", [1, 2]);
}
