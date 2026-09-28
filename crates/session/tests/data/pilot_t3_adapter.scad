// M24x2 threaded hose-barb adapter, 8 mm through channel.
// Thread z 0..12, hex flange z 12..24 (30 across flats), barb z 24..49.
// Print orientation: thread end down on the bed, barb pointing up.
// Units: mm

$fa = 2;
$fs = 0.3;

// ---- thread (M24 x 2, right hand, real helix) -------------------------
// ISO 60 deg profile, truncated so both the crest and the groove between
// turns stay wider than the 0.4 nozzle. The sharp-V apex sits 0.12 mm
// (axial, per flank) inside the nominal one, which is the running clearance.
P      = 2;       // pitch
TH_L   = 12;      // threaded length
FLANK  = 30;      // flank angle from the radial plane (60 deg included)
R_APEX = 12.0086; // radius of the theoretical sharp-V apex
CREST  = 0.42;    // axial crest flat
GROOVE = 0.42;    // axial gap between turns at the root
HW_C   = CREST / 2;
R_MAJ  = R_APEX - HW_C / tan(FLANK);          // 11.645 -> 23.29 dia
R_MIN  = R_APEX - ((P - GROOVE) / 2) / tan(FLANK); // 10.640 -> 21.28 dia

// ---- hex flange -------------------------------------------------------
AF     = 30;              // across flats
R_HEX  = AF / 2 / cos(30);
HEX_Z0 = TH_L;
HEX_Z1 = 24;
// the underside chamfer leaves the core at 45 deg, so nothing juts out
CH_H   = R_HEX + 0.1 - R_MIN;

// ---- barb -------------------------------------------------------------
BARB_Z0 = HEX_Z1;
BARB_L  = 25;
R_ROOT  = 5.5;    // 11.0 dia root
R_CRST  = 6.5;    // 13.0 dia crest, for 12 ID hose
R_TIP   = 5.2;    // lead-in chamfer at the free end

BORE_R  = 4;      // 8 through channel
TOTAL_H = BARB_Z0 + BARB_L;

HW_R  = (P - GROOVE) / 2;   // axial half width at the root
R_IN  = 8;                  // inner edge of the swept rib, buried in the core

// one turn of the rib, as a closed profile in the (radius, rise) plane
RIB = [[R_IN,  -HW_R], [R_MIN, -HW_R], [R_MAJ, -HW_C],
       [R_MAJ,  HW_C], [R_MIN,  HW_R], [R_IN,   HW_R]];

// the rib runs the full 12 mm and stops on a station at each end, so it is
// never sliced into a feather edge
A0    = HW_R * 360 / P;
A1    = (TH_L - HW_R) * 360 / P;
NST   = round((A1 - A0) / 3);               // ~3 deg between sweep stations
NP    = len(RIB);
STEP  = (A1 - A0) / NST;

// helical sweep: each station is the profile rotated by a and lifted a/360*P
vtx = [for (k = [0:NST], j = [0:NP-1])
         let(a = A0 + k * STEP, p = RIB[j])
         [p[0] * cos(a), p[0] * sin(a), a * P / 360 + p[1]]];

side = [for (k = [0:NST-1], j = [0:NP-1])
          let(n = (j + 1) % NP)
          [k*NP + j, k*NP + n, (k+1)*NP + n, (k+1)*NP + j]];
cap0 = [for (j = [NP-1:-1:0]) j];
cap1 = [for (j = [0:NP-1]) NST*NP + j];

// real helical rib on a plain core
module threaded_shaft() {
    union() {
        cylinder(r = R_MIN, h = TH_L, $fn = 160);
        polyhedron(points = vtx, faces = concat(side, [cap0], [cap1]),
                   convexity = 12);
    }
}

// hex flange with a 45 deg chamfer on its underside
module hex_flange() {
    intersection() {
        translate([0, 0, HEX_Z0])
            cylinder(r = R_HEX, h = HEX_Z1 - HEX_Z0, $fn = 6);
        union() {
            translate([0, 0, HEX_Z0])
                cylinder(r1 = R_MIN, r2 = R_HEX + 0.1, h = CH_H);
            translate([0, 0, HEX_Z0 + CH_H])
                cylinder(r = R_HEX + 0.1, h = HEX_Z1 - HEX_Z0 - CH_H);
        }
    }
}

// three barbs, 7 mm apart; the retaining face behind each crest is 43.6 deg
// from vertical, so the whole barb prints without support
BACK  = 1.05;   // rise of the retaining face
RAMP  = 5.95;   // rise of the lead-in ramp behind it
module barb() {
    rotate_extrude(convexity = 6)
        polygon(concat(
            [[0, BARB_Z0], [R_ROOT, BARB_Z0], [R_ROOT, BARB_Z0 + 1]],
            [for (i = [0:2], p = [[R_CRST, BACK], [R_ROOT, BACK + RAMP]])
               [p[0], BARB_Z0 + 1 + i * (BACK + RAMP) + p[1]]],
            [[R_ROOT, BARB_Z0 + BARB_L - 0.3],
             [R_TIP,  BARB_Z0 + BARB_L],
             [0,      BARB_Z0 + BARB_L]]));
}

difference() {
    union() {
        threaded_shaft();
        hex_flange();
        barb();
    }
    translate([0, 0, -1]) cylinder(r = BORE_R, h = TOTAL_H + 2, $fn = 64);
}
