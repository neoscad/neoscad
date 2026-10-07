//! Small vector algebra and the transcendental functions, all in pure Rust.
//!
//! Every sine, cosine and arc function goes through `libm`, never `f64`'s
//! methods: those call the platform's maths library, which differs in the
//! last place between macOS, glibc and wasm32, and one ulp in an angle
//! changes the digits written to the STEP file.

use std::ops::{Add, Mul, Neg, Sub};

pub(crate) const TAU: f64 = std::f64::consts::TAU;
pub(crate) const PI: f64 = std::f64::consts::PI;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct V {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

pub(crate) const fn v(x: f64, y: f64, z: f64) -> V {
    V { x, y, z }
}

impl Add for V {
    type Output = V;
    fn add(self, o: V) -> V {
        v(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for V {
    type Output = V;
    fn sub(self, o: V) -> V {
        v(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl Neg for V {
    type Output = V;
    fn neg(self) -> V {
        v(-self.x, -self.y, -self.z)
    }
}
impl Mul<f64> for V {
    type Output = V;
    fn mul(self, s: f64) -> V {
        v(self.x * s, self.y * s, self.z * s)
    }
}

impl From<[f64; 3]> for V {
    fn from(a: [f64; 3]) -> V {
        v(a[0], a[1], a[2])
    }
}

impl V {
    pub fn dot(self, o: V) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    pub fn cross(self, o: V) -> V {
        v(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    pub fn len(self) -> f64 {
        self.dot(self).sqrt()
    }
    pub fn norm(self) -> V {
        let l = self.len();
        if l == 0.0 { self } else { self * (1.0 / l) }
    }
    pub fn arr(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
    /// A unit vector perpendicular to `self` (a unit vector), chosen
    /// deterministically from the coordinate axes.
    pub fn perp(self) -> V {
        let a = if self.x.abs() < 0.9 {
            v(1.0, 0.0, 0.0)
        } else {
            v(0.0, 1.0, 0.0)
        };
        (a - self * a.dot(self)).norm()
    }
    /// `self` with the component along the unit vector `a` removed.
    pub fn reject(self, a: V) -> V {
        self - a * self.dot(a)
    }
    /// Lexicographic comparison, for deterministic choices among points.
    pub fn lex_cmp(self, o: V) -> std::cmp::Ordering {
        self.x
            .total_cmp(&o.x)
            .then(self.y.total_cmp(&o.y))
            .then(self.z.total_cmp(&o.z))
    }
}

pub(crate) fn sin(x: f64) -> f64 {
    libm::sin(x)
}
pub(crate) fn cos(x: f64) -> f64 {
    libm::cos(x)
}
pub(crate) fn atan2(y: f64, x: f64) -> f64 {
    libm::atan2(y, x)
}
pub(crate) fn atan(x: f64) -> f64 {
    libm::atan(x)
}

/// Sine of an angle in degrees, exact at multiples of 90°, so that
/// axis-aligned tessellations put their vertices exactly on the axes.
pub(crate) fn sin_deg(d: f64) -> f64 {
    let r = d.rem_euclid(360.0);
    if r == 0.0 || r == 180.0 {
        0.0
    } else if r == 90.0 {
        1.0
    } else if r == 270.0 {
        -1.0
    } else {
        sin(d.to_radians())
    }
}
pub(crate) fn cos_deg(d: f64) -> f64 {
    sin_deg(d + 90.0)
}

/// Solves the `n`×`n` system `a x = b` in place (Gauss–Jordan with partial
/// pivoting). `None` when singular.
pub(crate) fn solve_dense<const N: usize>(
    a: &mut [[f64; N]; N],
    b: &mut [f64; N],
    n: usize,
) -> Option<[f64; N]> {
    for c in 0..n {
        let mut piv = c;
        for r in c + 1..n {
            if a[r][c].abs() > a[piv][c].abs() {
                piv = r;
            }
        }
        if a[piv][c].abs() < 1e-300 {
            return None;
        }
        a.swap(c, piv);
        b.swap(c, piv);
        for r in 0..n {
            if r != c {
                let f = a[r][c] / a[c][c];
                if f != 0.0 {
                    for k in c..n {
                        a[r][k] -= f * a[c][k];
                    }
                    b[r] -= f * b[c];
                }
            }
        }
    }
    let mut x = [0.0; N];
    for i in 0..n {
        x[i] = b[i] / a[i][i];
    }
    Some(x)
}

/// Gauss–Legendre nodes and weights on [-1, 1], 8 points.
pub(crate) const GL8: [(f64, f64); 8] = [
    (-0.960_289_856_497_536_3, 0.101_228_536_290_376_26),
    (-0.796_666_477_413_626_7, 0.222_381_034_453_374_47),
    (-0.525_532_409_916_329, 0.313_706_645_877_887_3),
    (-0.183_434_642_495_649_8, 0.362_683_783_378_362),
    (0.183_434_642_495_649_8, 0.362_683_783_378_362),
    (0.525_532_409_916_329, 0.313_706_645_877_887_3),
    (0.796_666_477_413_626_7, 0.222_381_034_453_374_47),
    (0.960_289_856_497_536_3, 0.101_228_536_290_376_26),
];

/// Union–find with the smaller index as the root, so that roots, and
/// everything numbered from them, do not depend on the order of joins.
#[derive(Debug)]
pub(crate) struct UnionFind(Vec<usize>);

impl UnionFind {
    pub fn new(n: usize) -> UnionFind {
        UnionFind((0..n).collect())
    }
    pub fn find(&mut self, mut a: usize) -> usize {
        while self.0[a] != a {
            self.0[a] = self.0[self.0[a]];
            a = self.0[a];
        }
        a
    }
    pub fn join(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            let (lo, hi) = if a < b { (a, b) } else { (b, a) };
            self.0[hi] = lo;
        }
    }
}
