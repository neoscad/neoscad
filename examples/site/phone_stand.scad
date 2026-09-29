// A phone stand with three printing problems, for `neoscad check`:
// a lip wall thinner than the nozzle can print, a shelf that overhangs
// with nothing under it, and a cable clip that floats above the bed.

$fn = 64;

// Base plate.
cube([70, 60, 4]);

// Back rest, leaning back 20 degrees.
translate([0, 42, 4]) rotate([-20, 0, 0]) cube([70, 5, 70]);

// Front lip that holds the phone: 0.3 mm thick, below a 0.4 mm nozzle.
translate([0, 6, 4]) cube([70, 0.3, 12]);

// A shelf sticking straight out of the back rest, unsupported.
translate([10, 34, 45]) cube([50, 26, 3]);

// A cable clip modelled 2 mm above the plate: it would print in mid-air.
translate([35, 30, 6]) difference() {
    cylinder(h = 6, d = 14);
    translate([0, 0, -1]) cylinder(h = 8, d = 8);
    translate([0, -10, -1]) cube([3, 10, 8]);
}
