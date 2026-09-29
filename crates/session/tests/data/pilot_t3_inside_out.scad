// The CAD validation round's T3 part (progress/agent-eval/cad-20260928T231444Z/
// T3-neoscad-1, the adapter.scad the agent rendered at its 61st tool call):
// thread_groove()'s polyhedron is consistently wound but inside out (signed
// volume -1716 mm^3), and the difference with it leaves pinched edges. The
// agent was told only to overlap touching parts. Kept verbatim below.
//
// M24x2 threaded hose-barb adapter with a 12 ID hose barb.
//
// Print orientation: hex flange flat on the bed (z = 0), barb pointing up.
// No supports needed; the only overhangs are the thread flanks, which are
// inherent to the ISO 60 degree form and self-support on the turn below.
//
// All dimensions in millimetres.
//
//   z  0 .. 8    hex flange, 30 across flats
//   z  8 .. 20   M24x2 right-hand external thread, 12 long
//   z 20 .. 45   barb, 25 long, three barbs
//   bore 8 through

$fn = 128;

// ---------------------------------------------------------------- parameters
bore_r      = 8 / 2;    // 8 mm through-channel

// hex flange
hex_af      = 30;       // across flats
hex_t       = 8;        // thickness
hex_r       = hex_af / 2 / cos(30);         // circumscribed radius 17.3205

// M24 x 2 external thread
P           = 2;                            // pitch
thread_len  = 12;
thread_z0   = hex_t;                        // 8
thread_z1   = thread_z0 + thread_len;       // 20

fit         = 0.2;      // radial clearance on the major diameter
flank_fit   = 0.1;      // axial clearance per flank

r_maj       = 24 / 2 - fit;                 // 11.80   crest radius
r_pitch     = (24 - 0.6495 * P) / 2;        // 11.3505 nominal pitch radius
r_root      = (24 - 1.2269 * P) / 2;        // 10.7731 root radius
// The groove's flanks are followed to r_brk, just outside the crest, so that
// no vertex of the sweep lies on the stud's cylindrical surface.  Beyond that
// the groove runs straight out; gw() must stay under P/2 there or neighbouring
// turns of the sweep would intersect each other.
r_brk       = r_maj + 0.05;                 // 11.85
r_out       = r_maj + 0.6;                  // 12.40

runout      = 1.2;      // top chamfer, also lets the last turn fade out

// Axial half width of the groove between two turns, at radius r.
// (ISO 60 degree flanks, opened up by flank_fit on each side.)
function gw(r) = P / 2 - (0.5 + tan(30) * (r_pitch - r) - flank_fit);

// barb
barb_z0     = thread_z1;                    // 20
barb_len    = 25;
barb_z1     = barb_z0 + barb_len;           // 45, overall height
barb_min_r  = 5.70;                         // valley, 11.4 dia
barb_max_r  = 6.50;                         // crest, 13.0 dia for 12 ID hose
barb_pitch  = 7;
barb_rise   = 6;                            // cone length of one barb
tip_ch      = 0.6;                          // lead-in chamfer at the tip
tip_r       = 5.25;                         // leaves 1.25 wall at the tip

// ------------------------------------------------------------------ helpers

// The helical groove between the thread turns, swept as a single polyhedron.
// This is a true helix: every cross-section is advanced by P * angle / 360.
module thread_groove(z_start, z_end, step = 5) {
    prof = [                        // (r, dz), one groove, centred on dz = 0
        [r_root, -gw(r_root)],
        [r_brk,  -gw(r_brk)],
        [r_out,  -gw(r_brk)],
        [r_out,   gw(r_brk)],
        [r_brk,   gw(r_brk)],
        [r_root,  gw(r_root)]
    ];
    np    = len(prof);
    turns = (z_end - z_start) / P;
    n     = ceil(turns * 360 / step);
    da    = turns * 360 / n;

    verts = [
        for (i = [0:n], j = [0:np - 1])
            let (a = i * da, zc = z_start + P * a / 360)
            [prof[j][0] * cos(a), prof[j][0] * sin(a), zc + prof[j][1]]
    ];

    // Side walls are triangulated: a helical quad is not planar.
    faces = concat(
        [ for (i = [0:n - 1], j = [0:np - 1], t = [0:1])
            let (k = (j + 1) % np, a = i * np, b = (i + 1) * np)
            t == 0 ? [a + j, b + j, b + k] : [a + j, b + k, a + k] ],
        [ [for (j = [0:np - 1]) j] ],                   // start cap
        [ [for (j = [np - 1:-1:0]) n * np + j] ]        // end cap
    );

    polyhedron(points = verts, faces = faces, convexity = 8);
}

// Threaded stud.  It reaches down into the flange so that no face of it is
// coplanar with the flange top.
module threaded_stud() {
    difference() {
        rotate_extrude()                    // one solid, no internal seam
            polygon([
                [0,      hex_t / 2],
                [r_maj,  hex_t / 2],
                [r_maj,  thread_z1 - runout],
                // stops just inside the root so the run-out is not tangent to
                // the groove's inner surface
                [r_root - 0.15, thread_z1],
                [0,      thread_z1]
            ]);
        // the sweep's end caps are kept clear of the stud's own end faces
        thread_groove(hex_t / 2 - 3, thread_z1 + P);
    }
}

// Barb stem and the three barbs.  Starts inside the stud.
module barb() {
    rotate_extrude()
        polygon([
            [0, thread_z1 - 1],
            [barb_min_r, thread_z1 - 1],

            [barb_min_r, barb_z0 + 2],
            [barb_max_r, barb_z0 + 2 + barb_rise],
            [barb_min_r, barb_z0 + 2 + barb_rise],

            [barb_min_r, barb_z0 + 2 + barb_pitch],
            [barb_max_r, barb_z0 + 2 + barb_pitch + barb_rise],
            [barb_min_r, barb_z0 + 2 + barb_pitch + barb_rise],

            [barb_min_r, barb_z0 + 2 + 2 * barb_pitch],
            [barb_max_r, barb_z0 + 2 + 2 * barb_pitch + barb_rise],
            [barb_min_r, barb_z0 + 2 + 2 * barb_pitch + barb_rise],

            [barb_min_r, barb_z1 - tip_ch],
            [tip_r,      barb_z1],
            [0,          barb_z1]
        ]);
}

// --------------------------------------------------------------------- part
module adapter() {
    difference() {
        union() {
            cylinder(h = hex_t, r = hex_r, $fn = 6);
            threaded_stud();
            barb();
        }
        translate([0, 0, -1]) cylinder(h = barb_z1 + 2, r = bore_r);
    }
}

adapter();
