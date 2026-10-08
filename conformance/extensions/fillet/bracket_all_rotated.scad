// bracket_all turned inside the call: both passes select and blend on
// rotated planes, and the second pass finds the call's edges on the
// first pass's result by where they lie, not by their directions.
// volume: 10915.866931228413
fillet_edges(r = 1) rotate([10, 20, 30]) union() {
  cube([40, 30, 5]);
  cube([40, 5, 30]);
}
