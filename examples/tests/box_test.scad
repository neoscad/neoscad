// Tests for box.scad: run with `neoscad test examples/tests`.
include <box.scad>

// The default tray: 40 x 30 x 20 outside, 36 x 26 x 18 inside:
// 24000 - 16848 mm³ of plastic.
// @expect volume 7152
// @expect bbox [40, 30, 20]
// @expect manifold
// @expect components 1
module test_tray() tray();

// A lid lies flat on its plate, lip down: shift it up to print it.
// @expect bbox [[0, 0, -3], [40, 30, 2]]±0.001
// @expect components 1
// @expect check no-error
module test_lid() lid();

// Printable as it comes: no thin walls, overhangs or floating parts.
// @expect check clean
module test_tray_prints() tray([60, 40, 25]);

// Functions are tested with assert(): a failed one fails the test.
module test_capacity() {
    assert(capacity([40, 30, 20]) == 36 * 26 * 18);
    assert(capacity([10, 10, 2]) == 0, "a tray as tall as its floor holds nothing");
}
