//! Predicates and intersection (`geom.c`), in `f32` like upstream's
//! `TESSreal`.
//!
//! OpenSCAD's arm64 build compiles libtess2 with clang's default
//! `-ffp-contract=on`, which fuses a multiply feeding an add in the same
//! expression into one FMA (see `eval::fma` for the evidence on the f64
//! side). [`fma`] reproduces that for `f32`, with the same platform rule:
//! fused on `aarch64`, two roundings elsewhere. For `a * b + c * d` clang
//! fuses the first product and rounds the second. Which products fuse was
//! checked by comparing this port with libtess2 built by Apple clang from
//! the reference checkout, on random and degenerate polygons.

use super::{NIL, Tess};

/// `a * b + c` as clang contracts it: fused on `aarch64`.
#[inline]
pub(super) fn fma(a: f32, b: f32, c: f32) -> f32 {
    #[cfg(target_arch = "aarch64")]
    {
        a.mul_add(b, c)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        a * b + c
    }
}

impl Tess {
    #[inline]
    pub(super) fn st(&self, v: u32) -> (f32, f32) {
        let v = &self.v[v];
        (v.s, v.t)
    }

    /// `VertEq`
    #[inline]
    pub(super) fn vert_eq(&self, u: u32, v: u32) -> bool {
        let (us, ut) = self.st(u);
        let (vs, vt) = self.st(v);
        us == vs && ut == vt
    }

    /// `VertLeq`: `u` is lexicographically at or before `v` in (s, t).
    #[inline]
    pub(super) fn vert_leq(&self, u: u32, v: u32) -> bool {
        let (us, ut) = self.st(u);
        let (vs, vt) = self.st(v);
        us < vs || (us == vs && ut <= vt)
    }

    /// `EdgeGoesLeft`
    #[inline]
    pub(super) fn edge_goes_left(&self, e: u32) -> bool {
        self.vert_leq(self.dst(e), self.org(e))
    }

    /// `EdgeGoesRight`
    #[inline]
    pub(super) fn edge_goes_right(&self, e: u32) -> bool {
        self.vert_leq(self.org(e), self.dst(e))
    }

    /// `EdgeIsInternal`: the right face exists and is inside.
    #[inline]
    pub(super) fn edge_is_internal(&self, e: u32) -> bool {
        let r = self.rface(e);
        r != NIL && self.f[r].inside
    }

    /// `tesedgeEval`: the signed distance from edge `uw` to `v` along t,
    /// for `u <= v <= w`.
    pub(super) fn edge_eval(&self, u: u32, v: u32, w: u32) -> f32 {
        let (us, ut) = self.st(u);
        let (vs, vt) = self.st(v);
        let (ws, wt) = self.st(w);
        edge_eval(us, ut, vs, vt, ws, wt)
    }

    /// `tesedgeSign`: a number with the sign of [`Tess::edge_eval`].
    pub(super) fn edge_sign(&self, u: u32, v: u32, w: u32) -> f32 {
        let (us, ut) = self.st(u);
        let (vs, vt) = self.st(v);
        let (ws, wt) = self.st(w);
        edge_sign(us, ut, vs, vt, ws, wt)
    }

    /// `tesedgeIsLocallyDelaunay`: the angles opposite `e` in its two
    /// triangles sum to less than pi (plus upstream's 0.01 slack).
    pub(super) fn edge_is_locally_delaunay(&self, e: u32) -> bool {
        let s = e ^ 1;
        is_locally_delaunay(
            [
                self.st(self.org(self.lnext(e))),
                self.st(self.org(self.lnext(self.lnext(e)))),
                self.st(self.org(e)),
            ],
            [
                self.st(self.org(self.lnext(s))),
                self.st(self.org(self.lnext(self.lnext(s)))),
                self.st(self.org(s)),
            ],
        )
    }

    /// `tesedgeIntersect`: the intersection of edges `o1 d1` and `o2 d2`,
    /// as (s, t), guaranteed inside both edges' bounding boxes.
    pub(super) fn edge_intersect(&self, o1: u32, d1: u32, o2: u32, d2: u32) -> (f32, f32) {
        let p = |v: u32| self.st(v);
        let (mut o1, mut d1, mut o2, mut d2) = (p(o1), p(d1), p(o2), p(d2));
        let leq = |u: (f32, f32), v: (f32, f32)| u.0 < v.0 || (u.0 == v.0 && u.1 <= v.1);
        let tleq = |u: (f32, f32), v: (f32, f32)| u.1 < v.1 || (u.1 == v.1 && u.0 <= v.0);

        if !leq(o1, d1) {
            std::mem::swap(&mut o1, &mut d1);
        }
        if !leq(o2, d2) {
            std::mem::swap(&mut o2, &mut d2);
        }
        if !leq(o1, o2) {
            std::mem::swap(&mut o1, &mut o2);
            std::mem::swap(&mut d1, &mut d2);
        }
        let s = if !leq(o2, d1) {
            // Technically no intersection: do our best.
            (o2.0 + d1.0) / 2.0
        } else if leq(d1, d2) {
            let mut z1 = edge_eval(o1.0, o1.1, o2.0, o2.1, d1.0, d1.1);
            let mut z2 = edge_eval(o2.0, o2.1, d1.0, d1.1, d2.0, d2.1);
            if z1 + z2 < 0.0 {
                z1 = -z1;
                z2 = -z2;
            }
            interpolate(z1, o2.0, z2, d1.0)
        } else {
            let mut z1 = edge_sign(o1.0, o1.1, o2.0, o2.1, d1.0, d1.1);
            let mut z2 = -edge_sign(o1.0, o1.1, d2.0, d2.1, d1.0, d1.1);
            if z1 + z2 < 0.0 {
                z1 = -z1;
                z2 = -z2;
            }
            interpolate(z1, o2.0, z2, d2.0)
        };

        // The same for t, with s and t transposed.
        if !tleq(o1, d1) {
            std::mem::swap(&mut o1, &mut d1);
        }
        if !tleq(o2, d2) {
            std::mem::swap(&mut o2, &mut d2);
        }
        if !tleq(o1, o2) {
            std::mem::swap(&mut o1, &mut o2);
            std::mem::swap(&mut d1, &mut d2);
        }
        let t = if !tleq(o2, d1) {
            (o2.1 + d1.1) / 2.0
        } else if tleq(d1, d2) {
            let mut z1 = edge_eval(o1.1, o1.0, o2.1, o2.0, d1.1, d1.0);
            let mut z2 = edge_eval(o2.1, o2.0, d1.1, d1.0, d2.1, d2.0);
            if z1 + z2 < 0.0 {
                z1 = -z1;
                z2 = -z2;
            }
            interpolate(z1, o2.1, z2, d1.1)
        } else {
            let mut z1 = edge_sign(o1.1, o1.0, o2.1, o2.0, d1.1, d1.0);
            let mut z2 = -edge_sign(o1.1, o1.0, d2.1, d2.0, d1.1, d1.0);
            if z1 + z2 < 0.0 {
                z1 = -z1;
                z2 = -z2;
            }
            interpolate(z1, o2.1, z2, d2.1)
        };
        (s, t)
    }
}

/// `tesedgeIsLocallyDelaunay` on the two angles' `calcAngle` arguments:
/// each is the angle at the middle point between the rays to the other
/// two.
#[inline]
pub(super) fn is_locally_delaunay(p: [(f32, f32); 3], q: [(f32, f32); 3]) -> bool {
    clear_delaunay(p, q).unwrap_or_else(|| {
        locally_delaunay(cos_angle(p[0], p[1], p[2]), cos_angle(q[0], q[1], q[2]))
    })
}

/// `tesedgeIsLocallyDelaunay` decided cheaply where the answer is clear:
/// Some(answer) only if upstream computes that answer, None in the narrow
/// band where the caller must run the exact test.
///
/// Upstream sums the two angles (alpha, beta) through `float` cosines and
/// `acos`, and compares the sum with pi + 0.01. Taking the angles between
/// the same rounded `float` difference vectors, the cosine it computes is
/// within 3.6e-7 of the true one (a fused dot product off by at most 2^-23
/// of the lengths' product, a denominator from two square roots and a
/// product off by 3 * 2^-24, and the division's rounding), which moves the
/// `acos` by at most `acos(1 - 3.6e-7)`, under 8.5e-4, since `acos` is
/// steepest at the ends. With the `float` roundings of the angles and of
/// their sum, upstream's sum is within 1.71e-3 of alpha + beta. So
/// alpha + beta at most pi + 0.0078 means upstream finds the edge Delaunay
/// (its sum stays under pi + 0.0096), and at least pi + 0.012 means it
/// does not (its sum stays over pi + 0.0103).
///
/// That is decided without square roots from the products below, exact in
/// `f64` up to one rounding of each sum: with `D = dot` and `X = |cross|`
/// for each angle, `C = Da Db - Xa Xb` and `S = Xa Db + Da Xb` are the
/// cosine and sine of alpha + beta times the same positive factor. Both
/// angles at most pi/2 (both dots non-negative) means true. Otherwise the
/// sum is past pi/2; it is at most pi + 0.0078 if it is short of 3 pi / 2
/// (C < 0) with a sine at least -sin(0.0078) of the magnitude, and at
/// least pi + 0.012 if the sine is negative and either the sum is past
/// 3 pi / 2 (C >= 0) or the sine is at most -0.012 of the magnitude
/// (below -sin(0.012)). Values that overflow or underflow fail every
/// comparison and fall through.
#[inline]
fn clear_delaunay(p: [(f32, f32); 3], q: [(f32, f32); 3]) -> Option<bool> {
    let angle = |[p0, p1, p2]: [(f32, f32); 3]| {
        let u = (f64::from(p2.0 - p1.0), f64::from(p2.1 - p1.1));
        let v = (f64::from(p0.0 - p1.0), f64::from(p0.1 - p1.1));
        (u.0 * v.0 + u.1 * v.1, (u.0 * v.1 - u.1 * v.0).abs())
    };
    let (da, xa) = angle(p);
    let (db, xb) = angle(q);
    let c = da * db - xa * xb;
    let s = xa * db + da * xb;
    let mag2 = s * s + c * c;
    // Evaluated in full and combined without short-circuits: which case
    // applies varies from edge to edge, and branches would mispredict.
    let yes = (da >= 0.0) & (db >= 0.0) | (c < 0.0) & ((s >= 0.0) | (s * s <= 6.084e-5 * mag2));
    let no = (s < 0.0) & ((c >= 0.0) | (s * s >= 1.44e-4 * mag2));
    if yes {
        Some(true)
    } else if no {
        Some(false)
    } else {
        None
    }
}

/// `calcAngle`'s cosine, before the `acos`: of the angle at `p1`
/// between `p1->p2` and `p1->p0`, clamped to [-1, 1] (NaN passes through,
/// as upstream's comparisons let it).
#[inline]
fn cos_angle(p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) -> f32 {
    let a = [p2.0 - p1.0, p2.1 - p1.1];
    let b = [p0.0 - p1.0, p0.1 - p1.1];
    let mut num = fma(a[0], b[0], a[1] * b[1]);
    let la = f64::from(fma(a[0], a[0], a[1] * a[1])).sqrt();
    let lb = f64::from(fma(b[0], b[0], b[1] * b[1])).sqrt();
    let den = (la * lb) as f32;
    if f64::from(den) > 0.0 {
        num /= den;
    }
    if f64::from(num) < -1.0 {
        num = -1.0;
    }
    if f64::from(num) > 1.0 {
        num = 1.0;
    }
    num
}

/// The end of `tesedgeIsLocallyDelaunay`, given the two angles' cosines:
/// `acos(ca) + acos(cb) < pi + 0.01`, where upstream calls the `double`
/// `acos` on each `float` cosine and adds the results as `float`s.
///
/// The `acos` calls are most of the cost of the Delaunay pass, and most
/// edges are clearly Delaunay, so they are skipped when `ca + cb >= -d`
/// (exact in `f64`, with d = 1e-5). Then `cb >= -ca - d`, so `acos(cb)` is
/// at most `acos(-ca)` plus the most `acos` can grow over an interval of
/// width d, which is `acos(1 - d)` (at the ends, where it is steepest),
/// about 0.0045. With `acos(-ca) = pi - acos(ca)` the true sum is under
/// pi + 0.0045, and the rounding of `acos` and of the `float` sum (under
/// 1e-6) cannot carry it past upstream's 0.01 slack. The answer is the one
/// upstream computes; only the work is saved. Vertices on a circle (every
/// regular polygon) sum to pi exactly, so this is the common case there.
#[inline]
fn locally_delaunay(ca: f32, cb: f32) -> bool {
    if f64::from(ca) + f64::from(cb) >= -1e-5 {
        return true;
    }
    let a = f64::from(ca).acos() as f32;
    let b = f64::from(cb).acos() as f32;
    f64::from(a + b) < std::f64::consts::PI + 0.01
}

/// `tesedgeEval` on raw coordinates; with s and t swapped it is
/// `testransEval`.
#[inline]
fn edge_eval(us: f32, ut: f32, vs: f32, vt: f32, ws: f32, wt: f32) -> f32 {
    let gap_l = vs - us;
    let gap_r = ws - vs;
    if gap_l + gap_r > 0.0 {
        if gap_l < gap_r {
            fma(ut - wt, gap_l / (gap_l + gap_r), vt - ut)
        } else {
            fma(wt - ut, gap_r / (gap_l + gap_r), vt - wt)
        }
    } else {
        // Vertical line.
        0.0
    }
}

/// `tesedgeSign` on raw coordinates; with s and t swapped it is
/// `testransSign`.
#[inline]
pub(super) fn edge_sign(us: f32, ut: f32, vs: f32, vt: f32, ws: f32, wt: f32) -> f32 {
    let gap_l = vs - us;
    let gap_r = ws - vs;
    if gap_l + gap_r > 0.0 {
        fma(vt - wt, gap_l, (vt - ut) * gap_r)
    } else {
        0.0
    }
}

/// `RealInterpolate(a, x, b, y)`: `(b*x + a*y) / (a+b)`, clamping negative
/// weights to zero, or the midpoint when both are zero.
#[inline]
fn interpolate(a: f32, x: f32, b: f32, y: f32) -> f32 {
    let a = if a < 0.0 { 0.0 } else { a };
    let b = if b < 0.0 { 0.0 } else { b };
    if a <= b {
        if b == 0.0 {
            (x + y) / 2.0
        } else {
            fma(y - x, a / (a + b), x)
        }
    } else {
        fma(x - y, b / (a + b), y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tesedgeIsLocallyDelaunay` with nothing skipped.
    fn upstream(p: [(f32, f32); 3], q: [(f32, f32); 3]) -> bool {
        let a = f64::from(cos_angle(p[0], p[1], p[2])).acos() as f32;
        let b = f64::from(cos_angle(q[0], q[1], q[2])).acos() as f32;
        f64::from(a + b) < std::f64::consts::PI + 0.01
    }

    /// The shortcuts (`clear_delaunay` and the `acos` skip in
    /// `locally_delaunay`) never give an answer upstream would not, on
    /// quads near a circle (where the sum is near pi and the answer turns
    /// on the slack), slivers, and random ones, at several scales and far
    /// from the origin.
    #[test]
    fn the_delaunay_shortcuts_agree_with_upstream() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let (mut decided, mut total) = (0, 0);
        for i in 0..400_000 {
            let scale = [1e-3, 1.0, 50.0, 1e4][i % 4];
            let off = [0.0, 3.0, 1e3, 1e5][(i / 4) % 4];
            // Four points in order round a circle, moved off it by a noise
            // that ranges from none to large.
            let noise = [0.0, 1e-7, 1e-4, 1e-2, 0.3][(i / 16) % 5];
            let mut ang = [0.0; 4];
            for a in &mut ang {
                *a = rnd() * std::f64::consts::TAU;
            }
            ang.sort_by(f64::total_cmp);
            let pt = |a: f64, r: &mut dyn FnMut() -> f64| {
                let (x, y) = (a.cos() + noise * (r() - 0.5), a.sin() + noise * (r() - 0.5));
                ((x * scale + off) as f32, (y * scale - off) as f32)
            };
            let [a, x, b, y] = ang.map(|t| pt(t, &mut rnd));
            // The diagonal a b with x on one side and y on the other.
            let (p, q) = ([b, x, a], [a, y, b]);
            let want = upstream(p, q);
            total += 1;
            if let Some(got) = clear_delaunay(p, q) {
                decided += 1;
                assert_eq!(got, want, "{p:?} {q:?}");
            }
            assert_eq!(is_locally_delaunay(p, q), want, "{p:?} {q:?}");
        }
        // Most are decided without square roots; the rest still agree.
        assert!(decided * 10 > total * 9, "{decided} of {total}");
    }
}
