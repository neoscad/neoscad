// NeoSCAD hero model: a BOSL2 planetary gearbox.
// camera: 0,0,-14,64,0,24,390
//
// Herringbone sun, four herringbone planets and a herringbone ring gear,
// all laid out by BOSL2's planetary_gears() so the teeth really mesh, on
// a spoked carrier with a quarter of the ring cut away to show the
// train. Heavy on purpose (helical tooth surfaces, many booleans), to
// time a full render against OpenSCAD. Needs BOSL2 on OPENSCADPATH
// (.reference/BOSL2); scripts/apple/build-hero.sh sets it.

include <BOSL2/std.scad>
include <BOSL2/gears.scad>
include <BOSL2/isosurface.scad>

$fn = 96;

mod = 2;
helical = 24;
thick = 14;

g = planetary_gears(mod = mod, n = 4, max_teeth = 72, sun_ring = 3.5, helical = helical);
sun = g[0];
ring = g[1];
planets = g[2];

// Sun: a herringbone gear on a hub, with a keyed bore.
color("#ff5a6e")
difference() {
    union() {
        spur_gear(mod = mod, teeth = sun[1], profile_shift = sun[2], helical = helical,
                  herringbone = true, thickness = thick, gear_spin = sun[3], shaft_diam = 0);
        up(thick / 2) cyl(h = 6, d = 16, anchor = BOT, rounding2 = 1.5);
    }
    cyl(h = 60, d = 8);
    right(4) cuboid([3, 3, 60]);
}

// Planets: herringbone (opposite hand), each lightened by six round holes.
color("#18b3cc")
for (i = idx(planets[4]))
    move(planets[4][i])
    difference() {
        spur_gear(mod = mod, teeth = planets[1], profile_shift = planets[2], helical = -helical,
                  herringbone = true, thickness = thick, gear_spin = planets[3][i], shaft_diam = 6);
        zrot_copies(n = 6, r = planets[1] * mod * 0.27) cyl(h = 40, d = planets[1] * mod * 0.16);
    }

// Ring: a herringbone ring gear with a quarter cut away (the octant-style
// cut the icon concepts use), so the train inside is visible.
color("#4b3fd1")
difference() {
    ring_gear(mod = mod, teeth = ring[1], profile_shift = ring[2], helical = helical,
              herringbone = true, thickness = thick, gear_spin = ring[3], backing = 7);
    cutaway();
}

// The wedge facing the camera, removed from the ring and the plinth.
module cutaway() {
    linear_extrude(height = 100, center = true)
        polygon([[0, 0], [300, 0], [300 * cos(-80), 300 * sin(-80)]]);
}

// Plinth: a disc of gyroid lattice (BOSL2's isosurface, a marching-cubes
// mesh computed in the OpenSCAD language itself) inside a solid rim. The
// isosurface is most of the evaluation cost, which is the point: it is
// script work, not geometry kernel work.
function gyroid(p, w) = let(q = 360 / w * p)
    sin(q.x) * cos(q.y) + sin(q.y) * cos(q.z) + sin(q.z) * cos(q.x);
rim = ring[1] * mod / 2 + 2 * mod + 7;
base = 18;
color("#7d86b8")
down(thick / 2 + 6)
difference() {
    union() {
        intersection() {
            cyl(h = base, r = rim - 2, anchor = TOP, $fn = 180);
            isosurface(function(x, y, z) gyroid([x, y, z], 24), [-0.45, 0.45],
                       [[-rim, -rim, -base - 1], [rim, rim, 1]], voxel_size = 1.6);
        }
        difference() {
            cyl(h = base, r = rim, anchor = TOP, rounding1 = 3, $fn = 180);
            cyl(h = 3 * base, r = rim - 3, $fn = 180);
        }
        cyl(h = 3, r = rim, anchor = TOP, $fn = 180);
    }
    cutaway();
}

// Carrier: a plate below the gears, spoked, with a pin up through each
// planet.
color("#b8bfd6")
down(thick / 2 + 1)
difference() {
    union() {
        cyl(h = 4, r = norm(planets[4][0]) + 10, anchor = TOP, rounding = 1.2);
        for (p = planets[4]) move(p) cyl(h = thick + 6, d = 5.6, anchor = BOT);
    }
    cyl(h = 20, d = 26);
    for (a = [0 : 3]) zrot(45 + a * 90) right(norm(planets[4][0]) * 0.62) cyl(h = 20, d = 14);
}
