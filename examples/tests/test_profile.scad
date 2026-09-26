// A 2D profile: bounding box and area of a flat shape.

// A 20 x 10 plate with two 3 mm holes: 200 - 2 x pi x 1.5².
// @expect bbox [20, 10]
// @expect area 185.86±0.05
module test_plate() {
    difference() {
        square([20, 10]);
        for (x = [5, 15]) translate([x, 5]) circle(d=3, $fn=64);
    }
}

// @expect no-warnings
module test_clean_code() {
    echo(str("plate is ", 20, " mm wide"));
}
