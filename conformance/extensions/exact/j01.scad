// A joint of BOSL2's stroke(): two cylinders meeting at a sphere of
// their own radius. Each cylinder ends on a great circle of the sphere,
// and the two circles cross where all three surfaces touch.
// volume = 2 pi r^2 h + pi r^3 / 3 - 4 r^3 / 3, r = 0.5, h = 3
// volume: 4.676622007617597
sphere(d = 1);
cylinder(d = 1, h = 3);
rotate([0, 90, 0]) cylinder(d = 1, h = 3);
