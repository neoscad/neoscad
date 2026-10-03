// T5 reference for the grader's self-check (test_grade.py): a knob for a
// 6 D-shaft, printed top down so the bore opens upward (-D flip=false
// prints it bore down). Overridden for the wrong variants: `flutes`,
// `flute_d` (depth), `flat` (flat to far side), `depth` (bore) and
// `screw_z` (set screw axis from the bottom face).
d = 30;
h = 18;
flutes = 20;
flute_d = 1;
bore = 6.2;
flat = 4.7;
depth = 12;
screw = 2.5;
screw_z = 5;
flip = true;
$fn = 96;

// A flute is a groove cut by a cylinder whose edge reaches flute_d in.
fr = 2;
module knob() difference() {
    cylinder(d = d, h = h);
    for (i = [0:flutes - 1]) rotate(360 * i / flutes)
        translate([d / 2 + fr - flute_d, 0, -1]) cylinder(r = fr, h = h + 2, $fn = 32);
    // D bore from the bottom face (z = 0): the flat faces +x.
    translate([0, 0, -1]) intersection() {
        cylinder(d = bore, h = depth + 1, $fn = 64);
        translate([-bore / 2, -bore / 2, 0]) cube([flat, bore, depth + 1]);
    }
    // Set screw: radial along +x, through the middle of the flat.
    translate([0, 0, screw_z]) rotate([0, 90, 0]) cylinder(d = screw, h = d, $fn = 32);
}

if (flip) translate([0, 0, h]) rotate([180, 0, 0]) knob();
else knob();
