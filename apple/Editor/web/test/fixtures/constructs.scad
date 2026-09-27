// Every construct of OpenSCAD's grammar (src/core/parser.y), each of which
// must parse without an error node.
include <MCAD/units.scad>
use <MCAD/shapes.scad>

/* A block comment
   over two lines. */
$fn = 32;
size = [10, 20, 30,];
hex = 0xFF + 1e3 + .5 + 2. + 1.5e-3;
8bit = 1; // a digit-led name (deprecated, but accepted)
s = "tab\t quote\" newline\n hex\x41 unicodeé wide\U01F600 slash\\";
r = [0 : 2 : 10];
e = [];
v = [for (i = [0:3]) if (i % 2 == 0) i * i else -i];
c = [for (i = 0, j = 1; i < 5; i = i + 1, j = j * 2) [i, j]];
nested = [each [1, 2], for (a = [1, 2]) let (b = a * 2) each [a, b]];
parens = [for (i = [0:2]) (if (i > 0) i)];
f = function (x, y = 2) x ^ y ^ 2;
g = f(3)(4);
t = true ? false : undef;
bits = ~1 | 2 & 3 << 1 >> 1;
logic = !(1 < 2) || 2 <= 3 && 3 > 2 && 3 >= 3 && 1 != 2;
idx = size[0] + size.x + g.y;
l = let (a = 1, b = a + 1) a + b;
a = assert(l > 0, "positive") l;
q = assert(true);
ec = echo("value", l) l * 2;
neg = -2 ^ 2;

function add(a, b = 1) = a + b;

module part(d = 10, center = false) {
    r = d / 2;
    if (center) {
        translate([0, 0, -r]) cylinder(d = d, h = r * 2);
    } else if (d > 5)
        sphere(r);
    else
        cube(d);
    for (i = [0 : 1]) rotate([0, 0, i * 90]) children(i);
    let (w = d * 2) echo(w);
    assert(d > 0) cube(1);
    echo("done");
}

!part(5);
#part(center = true);
%translate([1, 0, 0]) part();
*part();
{
    module inner() cube();
    inner();
}
;
difference() {
    cube(size, center = true);
    union() {
        sphere(d = 12, $fn = 64);
    }
}
