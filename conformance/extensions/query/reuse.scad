// The sandbox and reuse: a query instantiates the child early, holds its
// messages back, and a later children() reuses it (printing them then)
// whenever a fresh instantiation would be the same. Every echo below
// prints where it would with no query at all.

module mark(n) {
  cube(1);
  anchor(str("m", n), [n, 0, 0]);
  echo("mark", n, fn = $fn);
}

// Nested queries: each level is instantiated once, by the outermost query.
module q(tag) {
  a = child_anchors(0);
  echo(tag, [for (k = a) k]);
  children(0);
}
q("outer") q("middle") q("inner") translate([1, 2, 3]) mark(1);

// The child reads $fn, which differs where children() runs: it is
// instantiated again there, and prints from there.
module dollar() {
  a = child_anchors(0);
  echo("dollar", a);
  let($fn = 7) children(0);
}
dollar() mark(2);

// The same $fn at both places: reused.
module same() {
  a = child_anchors(0);
  echo("same", a);
  translate([0, 0, 1]) children(0);
}
same($fn = 5) mark(3);

// rands() in the child: not reused, and the numbers drawn after it are
// those of the model without the query.
module r() {
  a = child_anchors();
  children();
}
r() { echo(seeded = rands(0, 1, 1, 42)); echo(unseeded = rands(0, 1, 1)); mark(4); }
echo(after = rands(0, 1, 1));

// A child never instantiated prints nothing.
module unused() { a = child_anchors(0); echo("unused", a); }
unused() mark(5);

// Two children() of the queried child: the first reuses, the second runs.
module twice() {
  a = child_anchors(0);
  children(0);
  translate([5, 0, 0]) children(0);
}
twice() mark(6);

// The same call repeated, so the call memo replays it.
module pegs() { for (i = [0:3]) translate([i * 3, 0, 0]) q(str("peg ", i)) mark(7); }
pegs();
pegs();

// A warning in the child, held back with its echo.
module warns() { a = child_anchors(); echo("warns"); children(); }
warns() { echo("child"); cube(undefined_size); }
