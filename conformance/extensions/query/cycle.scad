// A child that asks about itself (through a `$` function the module
// passes down) is an error, not an endless recursion.

module q() {
  $ask = function() child_anchors(0);
  a = $ask();
  children(0);
}
module asks() {
  x = $ask();
  cube(1);
}
q() asks();
