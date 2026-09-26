// Reference solution for `phone_stand`: checks the grader, never shown to agents.
module posed() translate([-40, 0, 10]) rotate([-20, 0, 0]) children();
module stand() intersection() {
    // Nothing below the table.
    translate([-100, -100, 0]) cube([200, 300, 200]);
    difference() {
        union() {
            // Base plate from y = -12 to y = 70.
            translate([-40, -12, 0]) cube([80, 82, 4]);
            // The back rest and the ledge, in the phone's frame.
            hull() {
                posed() translate([0, 10, -8]) cube([80, 6, 110]);
                translate([-40, 40, 0]) cube([80, 30, 4]);
            }
            // Ledge under the phone and a lip in front.
            hull() {
                posed() translate([0, -6, -8]) cube([80, 22, 8]);
                translate([-40, -12, 0]) cube([80, 40, 4]);
            }
            posed() translate([0, -6, -8]) cube([80, 6, 16]);
        }
        // The phone, with nothing overlapping it.
        posed() cube([80, 10, 200]);
    }
}
stand();
