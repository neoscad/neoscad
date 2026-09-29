// M24x2 threaded hose-barb adapter
// Print orientation: threaded end down on the bed, barb tip up.
$fn = 96;

// ---------------- parameters ----------------
P      = 2;                        // thread pitch
Rmaj   = 12;                       // M24 major radius
Rmin   = Rmaj - 0.5 * 1.0825 * P;  // ISO minor radius 10.9175
thr_l  = 12;                       // threaded length
Rtrim  = Rmaj + 0.05;              // trim radius, just clear of the crest

af     = 30;                       // hex across flats
Rhex   = af / sqrt(3);             // hex circumradius 17.3205

zc0    = 11.9;                     // flange cone base (slight overlap into thread)
Rc0    = 11.95;                    // flange cone base radius
ch_h   = Rhex - Rc0;               // 45 deg chamfer height
fl_h   = 6;                        // straight hex height
zf1    = zc0 + ch_h + fl_h;        // flange top = barb base

bore   = 8;                        // through-channel

// barb: valley d11, crest d13, three barbs, 25 long
bv     = 5.5;
bc     = 6.5;
barb_l = 25;

// ---------------- helical thread ----------------
// ISO 60 deg profile: crest flat P/8, root flat P/4, depth 5H/8 = 1.0825*P
cw     = P / 16;                   // crest half width
fz     = 0.5 * 1.0825 * P * tan(30);   // flank rise over the flank, 0.625
Rin    = Rmin - 0.4;               // tooth base buried inside the core
hw     = cw + fz + 0.4 * tan(30);  // inner half width, 0.981 < P/2

tp     = [[Rin, -hw], [Rmaj, -cw], [Rmaj, cw], [Rin, hw]];
sp     = 72;                       // steps per revolution
nrev   = (thr_l + 2 * P) / P;      // extra turn each end, trimmed later
nr     = nrev * sp + 1;            // rings

module thread_helix() {
    polyhedron(
        points = [for (i = [0:nr - 1], j = [0:3])
            let (a = 360 * i / sp, zz = -P + P * i / sp + tp[j][1])
                [tp[j][0] * cos(a), tp[j][0] * sin(a), zz]],
        faces = concat(
            [for (i = [0:nr - 2], j = [0:3])
                let (k = (j + 1) % 4)
                    [i * 4 + j, i * 4 + k, (i + 1) * 4 + k, (i + 1) * 4 + j]],
            [[3, 2, 1, 0]],
            [[(nr - 1) * 4 + 0, (nr - 1) * 4 + 1, (nr - 1) * 4 + 2, (nr - 1) * 4 + 3]]
        )
    );
}

module threaded_shank() {
    intersection() {
        // trim to length, with a 45 deg lead-in chamfer at the free end
        rotate_extrude()
            polygon([[0, 0], [Rmaj - 1.0, 0], [Rtrim, 1.05], [Rtrim, thr_l], [0, thr_l]]);
        union() {
            cylinder(r = Rmin, h = thr_l);
            thread_helix();
        }
    }
}

// ---------------- hex flange ----------------
module flange() {
    translate([0, 0, zc0])
        intersection() {
            union() {
                cylinder(r1 = Rc0, r2 = Rhex, h = ch_h);     // 45 deg underside
                translate([0, 0, ch_h]) cylinder(r = Rhex, h = fl_h);
            }
            linear_extrude(height = ch_h + fl_h) circle(r = Rhex, $fn = 6);
        }
}

// ---------------- barb ----------------
// bottom-up: 45 deg retaining face (the only down-facing side), then the
// shallow lead-in ramp that the hose rides over on assembly
module barb() {
    translate([0, 0, zf1])
        rotate_extrude()
            polygon([
                [0, 0], [bv, 0],
                [bv, 2.0], [bc, 3.0], [bc, 3.4], [bv, 9.0],      // barb 1
                [bv, 9.4], [bc, 10.4], [bc, 10.8], [bv, 16.4],   // barb 2
                [bv, 16.8], [bc, 17.8], [bc, 18.2], [bv, 24.2],  // barb 3
                [5.2, barb_l], [0, barb_l]
            ]);
}

// ---------------- assembly ----------------
difference() {
    union() {
        threaded_shank();
        flange();
        barb();
    }
    translate([0, 0, -1]) cylinder(d = bore, h = zf1 + barb_l + 2);
}
