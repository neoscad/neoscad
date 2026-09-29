// M24x2 threaded hose-barb adapter
// Print orientation: thread end down on the bed (z = 0), barb pointing up.

$fn = 96;

// ---- main dimensions -------------------------------------------------
bore_d      = 8;      // through channel
thread_d    = 24;     // M24 nominal major diameter
pitch       = 2;      // M24x2
thread_len  = 12;     // threaded length
hex_af      = 30;     // hex flange across flats
flange_t    = 11;     // hex flange thickness
barb_len    = 25;     // barbed nipple length
n_barbs     = 3;
hose_id     = 12;

fit         = 0.2;    // clearance per side on the thread flanks/crest

// ---- derived ---------------------------------------------------------
r_bore   = bore_d / 2;
r_hex    = hex_af / sqrt(3);            // hex circumradius (across flats = hex_af)

r_crest  = thread_d / 2 - fit;          // 11.8
h3       = 0.6134 * pitch;              // ISO external thread depth 1.2268
r_root   = r_crest - h3;                // 10.5732

flank    = h3 * tan(30);                // axial run of one 60-deg flank
crest_f  = pitch / 8;                   // crest flat
root_f   = pitch - 2 * flank - crest_f; // root flat (closes exactly on one pitch)

a1 = root_f;                  // end of root flat
a2 = a1 + flank;              // end of rising flank
a3 = a2 + crest_f;            // end of crest flat (then falling flank to pitch)

z_flange = thread_len;
z_barb   = z_flange + flange_t;
z_top    = z_barb + barb_len;

// 45 deg underside chamfer on the flange. It starts at the thread root radius
// so it grows straight out of the core and leaves no down-facing ledge over
// the thread groove.
chamfer  = r_hex - r_root;

// barb sizing for hose_id
r_barb_root  = hose_id / 2 - 0.4;   // 5.6  -> slides into the hose
r_barb_crest = hose_id / 2 + 0.45;  // 6.45 -> interference grip
barb_seg     = barb_len / n_barbs;
barb_rise    = 1.0;                 // height of the 45-deg-ish retaining face
tip_cham     = 0.8;

// ---- helical thread --------------------------------------------------
// Radius of the thread as a function of axial position within one pitch.
function thr_r(z) =
    let (u = z - floor(z / pitch) * pitch)
      u < a1 ? r_root
    : u < a2 ? r_root + (u - a1) / flank * h3
    : u < a3 ? r_crest
    :          r_crest - (u - a3) / flank * h3;

// Horizontal cross-section of the thread at z = 0. One pitch maps onto one
// full turn, so the section is a simple star-shaped polygon; extruding it
// with 360 deg of twist per pitch reproduces the helix exactly.
thread_steps = 360;
thread_section = [ for (i = [0 : thread_steps - 1])
                     let (a = i * 360 / thread_steps, r = thr_r(a / 360 * pitch))
                       [r * cos(a), r * sin(a)] ];

module thread() {
    linear_extrude(height = thread_len,
                   twist = -360 * thread_len / pitch,
                   slices = 24 * thread_len / pitch,
                   convexity = 10)
        polygon(thread_section);
}

// ---- hex flange, chamfered underneath so nothing overhangs past 45 deg
module flange() {
    intersection() {
        translate([0, 0, z_flange]) cylinder(h = flange_t, r = r_hex, $fn = 6);
        union() {
            translate([0, 0, z_flange]) cylinder(h = chamfer, r1 = r_root, r2 = r_hex);
            translate([0, 0, z_flange + chamfer]) cylinder(h = flange_t - chamfer, r = r_hex + 1);
        }
    }
}

// ---- barbed nipple ---------------------------------------------------
module barb() {
    for (i = [0 : n_barbs - 1]) {
        z = z_barb + i * barb_seg;
        // retaining face: rises to the crest over barb_rise (about 40 deg from vertical)
        translate([0, 0, z]) cylinder(h = barb_rise, r1 = r_barb_root, r2 = r_barb_crest);
        // lead-in taper back down to the root diameter
        translate([0, 0, z + barb_rise])
            cylinder(h = barb_seg - barb_rise, r1 = r_barb_crest, r2 = r_barb_root);
    }
    // tip chamfer to start the hose
    translate([0, 0, z_top - tip_cham])
        cylinder(h = tip_cham, r1 = r_barb_root, r2 = r_barb_root - 0.4);
}

// ---- assembly --------------------------------------------------------
difference() {
    union() {
        thread();
        cylinder(h = z_barb, r = r_root);   // core under the thread / flange
        flange();
        translate([0, 0, z_barb - 0.01]) cylinder(h = 0.01, r = r_barb_root);
        barb();
    }
    translate([0, 0, -1]) cylinder(h = z_top + 2, r = r_bore);
}
