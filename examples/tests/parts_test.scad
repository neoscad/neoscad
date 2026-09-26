// Named parts (neoscad's `part()` extension): `@expect parts` turns it on.
include <box.scad>

// The tray with its lid in place, as two named parts.
// @expect parts tray,lid
// @expect components 2
// @expect volume 10238.4+-0.1
module test_assembly() {
    part("tray") tray();
    part("lid") translate([0, 0, 25]) lid();
}
