//! B-spline surfaces as reconstruction works with them: evaluation with
//! derivatives to second order (Piegl & Tiller, "The NURBS Book",
//! algorithms A2.3, A3.6 and A4.4), and point inversion (closest point,
//! section 6.1 there).
//!
//! Everything is plain `f64` arithmetic in a fixed order, with no fused
//! multiply-add and no platform maths (only `sqrt`, which IEEE 754 rounds
//! correctly everywhere), so the same input gives the same bits on every
//! target, wasm32 included.

use std::sync::OnceLock;

use crate::math::*;
use crate::model::{BSpline, BSplineSurface};

/// The highest degree handled: the basis arrays are fixed at 8 entries.
pub(crate) const MAX_DEGREE: u32 = 7;

/// A B-spline surface ready for repeated evaluation and projection.
#[derive(Debug)]
pub(crate) struct Spline {
    /// The record it was made from (for writing, and for equality).
    pub public: BSplineSurface,
    pub pu: usize,
    pub pv: usize,
    pub nu: usize,
    pub nv: usize,
    /// Homogeneous control points `(w x, w y, w z, w)`, `i * nv + j`.
    pw: Vec<[f64; 4]>,
    pub ku: Vec<f64>,
    pub kv: Vec<f64>,
    /// Whether the record is a usable surface (see [`Spline::check`]).
    pub valid: bool,
    /// The largest extent of the control net: the scale its tolerances
    /// are relative to.
    pub size: f64,
    /// Seeds for projection: a grid of surface points with their
    /// parameters, made on first use. A grid, not the control net, so
    /// that a seed's parameters are where its point is.
    seeds: OnceLock<Vec<(f64, f64, V)>>,
}

impl PartialEq for Spline {
    fn eq(&self, o: &Spline) -> bool {
        self.public == o.public
    }
}

/// The surface and its derivatives at a point: `d[k][l]` is
/// `∂^(k+l) S / ∂u^k ∂v^l`, for `k + l` up to the order asked for.
pub(crate) type Ders = [[V; 3]; 3];

/// The knot span `s` with `knots[s] <= t < knots[s + 1]`, clamped to the
/// valid spans so that the domain's end (and points beyond it, which then
/// extend the end span's polynomial) use the end spans.
fn find_span(knots: &[f64], n: usize, p: usize, t: f64) -> usize {
    if t >= knots[n] {
        // The last span with positive length.
        let mut s = n - 1;
        while s > p && knots[s] >= knots[s + 1] {
            s -= 1;
        }
        return s;
    }
    if t <= knots[p] {
        return p;
    }
    let (mut lo, mut hi) = (p, n);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if t < knots[mid] {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    lo
}

/// The non-zero basis functions at `t` in span `s` and their derivatives
/// up to `n` (A2.3): `out[k][j]` is the k-th derivative of `N_{s-p+j}`.
/// Derivatives above the degree are zero.
fn ders_basis(knots: &[f64], s: usize, t: f64, p: usize, n: usize, out: &mut [[f64; 8]; 3]) {
    let mut ndu = [[0.0f64; 8]; 8];
    let mut left = [0.0f64; 8];
    let mut right = [0.0f64; 8];
    ndu[0][0] = 1.0;
    for j in 1..=p {
        left[j] = t - knots[s + 1 - j];
        right[j] = knots[s + j] - t;
        let mut saved = 0.0;
        for r in 0..j {
            ndu[j][r] = right[r + 1] + left[j - r];
            let temp = ndu[r][j - 1] / ndu[j][r];
            ndu[r][j] = saved + right[r + 1] * temp;
            saved = left[j - r] * temp;
        }
        ndu[j][j] = saved;
    }
    *out = [[0.0; 8]; 3];
    for j in 0..=p {
        out[0][j] = ndu[j][p];
    }
    let n = n.min(p).min(2);
    let mut a = [[0.0f64; 8]; 2];
    for r in 0..=p {
        let (mut s1, mut s2) = (0usize, 1usize);
        a[0][0] = 1.0;
        for k in 1..=n {
            let mut d = 0.0;
            let rk = r as isize - k as isize;
            let pk = p - k;
            if rk >= 0 {
                a[s2][0] = a[s1][0] / ndu[pk + 1][rk as usize];
                d = a[s2][0] * ndu[rk as usize][pk];
            }
            let j1: usize = if rk >= -1 { 1 } else { (-rk) as usize };
            let j2: usize = if r as isize - 1 <= pk as isize {
                k - 1
            } else {
                p - r
            };
            for j in j1..=j2 {
                let idx = (rk + j as isize) as usize;
                a[s2][j] = (a[s1][j] - a[s1][j - 1]) / ndu[pk + 1][idx];
                d += a[s2][j] * ndu[idx][pk];
            }
            if r <= pk {
                a[s2][k] = -a[s1][k - 1] / ndu[pk + 1][r];
                d += a[s2][k] * ndu[r][pk];
            }
            out[k][r] = d;
            std::mem::swap(&mut s1, &mut s2);
        }
    }
    let mut f = p as f64;
    for k in 1..=n {
        for j in 0..=p {
            out[k][j] *= f;
        }
        f *= (p - k) as f64;
    }
}

impl Spline {
    /// The working form of a record. A malformed record gives a spline
    /// with `valid` false, which must not be evaluated.
    pub fn new(b: &BSplineSurface) -> Spline {
        let mut s = Spline {
            public: b.clone(),
            pu: b.degree_u as usize,
            pv: b.degree_v as usize,
            nu: b.control.len(),
            nv: b.control.first().map_or(0, |r| r.len()),
            pw: Vec::new(),
            ku: b.knots_u.clone(),
            kv: b.knots_v.clone(),
            valid: false,
            size: 0.0,
            seeds: OnceLock::new(),
        };
        if Spline::check(b).is_err() {
            return s;
        }
        let mut lo = V::from(b.control[0][0]);
        let mut hi = lo;
        for (i, row) in b.control.iter().enumerate() {
            for (j, &c) in row.iter().enumerate() {
                let w = b.weights.as_ref().map_or(1.0, |w| w[i][j]);
                s.pw.push([c[0] * w, c[1] * w, c[2] * w, w]);
                lo = v(lo.x.min(c[0]), lo.y.min(c[1]), lo.z.min(c[2]));
                hi = v(hi.x.max(c[0]), hi.y.max(c[1]), hi.z.max(c[2]));
            }
        }
        let d = hi - lo;
        s.size = d.x.max(d.y).max(d.z);
        s.valid = true;
        s
    }

    /// Why a record is not a usable surface, if it is not.
    pub fn check(b: &BSplineSurface) -> Result<(), String> {
        let (pu, pv) = (b.degree_u, b.degree_v);
        if !(1..=MAX_DEGREE).contains(&pu) || !(1..=MAX_DEGREE).contains(&pv) {
            return Err(format!(
                "degrees {pu} and {pv} (1 to {MAX_DEGREE} supported)"
            ));
        }
        let (pu, pv) = (pu as usize, pv as usize);
        let nu = b.control.len();
        let nv = b.control.first().map_or(0, |r| r.len());
        if nu <= pu || nv <= pv {
            return Err(format!(
                "a {nu} by {nv} control net is too small for degrees {pu} and {pv}"
            ));
        }
        if b.control.iter().any(|r| r.len() != nv) {
            return Err("control net rows differ in length".into());
        }
        if b.control.iter().flatten().flatten().any(|x| !x.is_finite()) {
            return Err("a control point is not finite".into());
        }
        if let Some(w) = &b.weights {
            if w.len() != nu || w.iter().any(|r| r.len() != nv) {
                return Err("weights differ in shape from the control net".into());
            }
            if w.iter().flatten().any(|&x| !(x.is_finite() && x > 0.0)) {
                return Err("a weight is not positive".into());
            }
        }
        for (name, k, n, p) in [("u", &b.knots_u, nu, pu), ("v", &b.knots_v, nv, pv)] {
            if k.len() != n + p + 1 {
                return Err(format!(
                    "{} knots along {name}, {} expected",
                    k.len(),
                    n + p + 1
                ));
            }
            if k.iter().any(|x| !x.is_finite()) || k.windows(2).any(|w| w[1] < w[0]) {
                return Err(format!("knots along {name} are not non-decreasing"));
            }
            if k[..=p].iter().any(|&x| x != k[0]) || k[n..].iter().any(|&x| x != k[n]) {
                return Err(format!("knots along {name} are not clamped"));
            }
            if k[p] >= k[n] {
                return Err(format!("the domain along {name} is empty"));
            }
            // Exactly `p + 1` copies of each end knot, so that the
            // boundary rows are the patch's edges.
            if k[p + 1] <= k[p] || k[n - 1] >= k[n] {
                return Err(format!(
                    "an end knot along {name} repeats more than {} times",
                    p + 1
                ));
            }
            // An interior knot repeated more than the degree breaks the
            // surface apart.
            let mut run = 1;
            for i in p + 1..n {
                run = if k[i] == k[i - 1] { run + 1 } else { 1 };
                if run > p {
                    return Err(format!("an interior knot along {name} repeats {run} times"));
                }
            }
        }
        Ok(())
    }

    /// The parameter domain: `([u0, u1], [v0, v1])`.
    pub fn domain(&self) -> ([f64; 2], [f64; 2]) {
        (
            [self.ku[self.pu], self.ku[self.nu]],
            [self.kv[self.pv], self.kv[self.nv]],
        )
    }

    /// The surface and its derivatives up to `order` (at most 2) at
    /// `(u, v)`. Outside the domain the end spans' polynomials extend it.
    pub fn ders(&self, u: f64, v: f64, order: usize) -> Ders {
        let order = order.min(2);
        let (pu, pv) = (self.pu, self.pv);
        let su = find_span(&self.ku, self.nu, pu, u);
        let sv = find_span(&self.kv, self.nv, pv, v);
        let mut bu = [[0.0; 8]; 3];
        let mut bv = [[0.0; 8]; 3];
        ders_basis(&self.ku, su, u, pu, order, &mut bu);
        ders_basis(&self.kv, sv, v, pv, order, &mut bv);
        // Homogeneous derivatives A[k][l].
        let mut a = [[[0.0f64; 4]; 3]; 3];
        for k in 0..=order {
            for l in 0..=order - k {
                let mut acc = [0.0f64; 4];
                for i in 0..=pu {
                    let row = (su - pu + i) * self.nv;
                    let mut t = [0.0f64; 4];
                    for j in 0..=pv {
                        let c = &self.pw[row + sv - pv + j];
                        let b = bv[l][j];
                        for d in 0..4 {
                            t[d] += b * c[d];
                        }
                    }
                    let b = bu[k][i];
                    for d in 0..4 {
                        acc[d] += b * t[d];
                    }
                }
                a[k][l] = acc;
            }
        }
        // The rational surface's derivatives (A4.4); for weights all 1 the
        // weight derivatives vanish and this is the non-rational case.
        let bin = |n: usize, k: usize| -> f64 {
            match (n, k) {
                (2, 1) => 2.0,
                _ => 1.0,
            }
        };
        let mut s = [[V::default(); 3]; 3];
        let w = |k: usize, l: usize| a[k][l][3];
        for k in 0..=order {
            for l in 0..=order - k {
                let mut x = V::from([a[k][l][0], a[k][l][1], a[k][l][2]]);
                for j in 1..=l {
                    x = x - s[k][l - j] * (bin(l, j) * w(0, j));
                }
                for i in 1..=k {
                    x = x - s[k - i][l] * (bin(k, i) * w(i, 0));
                    let mut x2 = V::default();
                    for j in 1..=l {
                        x2 = x2 + s[k - i][l - j] * (bin(l, j) * w(i, j));
                    }
                    x = x - x2 * bin(k, i);
                }
                s[k][l] = x * (1.0 / w(0, 0));
            }
        }
        s
    }

    pub fn eval(&self, u: f64, v: f64) -> V {
        self.ders(u, v, 0)[0][0]
    }

    /// The unit natural normal `∂u × ∂v` at `(u, v)`.
    pub fn normal(&self, u: f64, v: f64) -> V {
        let d = self.ders(u, v, 1);
        d[1][0].cross(d[0][1]).norm()
    }

    fn seeds(&self) -> &[(f64, f64, V)] {
        self.seeds.get_or_init(|| {
            let (du, dv) = self.domain();
            let us = sample_params(&self.ku, self.pu, self.nu, du);
            let vs = sample_params(&self.kv, self.pv, self.nv, dv);
            let mut out = Vec::with_capacity(us.len() * vs.len());
            for &u in &us {
                for &v in &vs {
                    out.push((u, v, self.eval(u, v)));
                }
            }
            out
        })
    }

    /// How far past each end of the domain projection may go when
    /// `extend` is asked for: half the end span, at most a tenth of the
    /// domain. Beyond the boundary the end span's polynomial continues the
    /// surface smoothly (a blend's arc continues as the same circle), so
    /// the implicit form stays smooth across a face's edge on the
    /// boundary, which is where vertices are solved.
    fn margins(knots: &[f64], p: usize, n: usize) -> [f64; 2] {
        let (lo, hi) = (knots[p], knots[n]);
        let first = knots[p + 1..=n]
            .iter()
            .copied()
            .find(|&k| k > lo)
            .unwrap_or(hi);
        let last = knots[p..n]
            .iter()
            .rev()
            .copied()
            .find(|&k| k < hi)
            .unwrap_or(lo);
        let tenth = 0.1 * (hi - lo);
        [
            (0.5 * (first - lo)).min(tenth),
            (0.5 * (hi - last)).min(tenth),
        ]
    }

    /// The parameters of the point of the surface nearest `p`: the nearest
    /// seed, then Newton on the squared distance (Gauss–Newton where its
    /// Hessian is not positive definite, far from the surface), each step
    /// kept inside the bounds and shortened until the distance does not
    /// grow. With `extend`, the bounds reach a little past the domain.
    pub fn project(&self, p: V, extend: bool) -> (f64, f64) {
        let ((u0, u1), (v0, v1)) = {
            let (du, dv) = self.domain();
            if extend {
                let mu = Spline::margins(&self.ku, self.pu, self.nu);
                let mv = Spline::margins(&self.kv, self.pv, self.nv);
                (
                    (du[0] - mu[0], du[1] + mu[1]),
                    (dv[0] - mv[0], dv[1] + mv[1]),
                )
            } else {
                ((du[0], du[1]), (dv[0], dv[1]))
            }
        };
        let mut best = (0.0, 0.0, f64::INFINITY);
        for &(u, v, q) in self.seeds() {
            let d = (q - p).dot(q - p);
            if d < best.2 {
                best = (u, v, d);
            }
        }
        let (mut u, mut v) = (best.0, best.1);
        let mut d = self.ders(u, v, 2);
        let mut r = d[0][0] - p;
        let mut d2 = r.dot(r);
        let tiny = 4.0 * f64::EPSILON * (self.size + p.len());
        for _ in 0..60 {
            let (su, sv) = (d[1][0], d[0][1]);
            let g = [su.dot(r), sv.dot(r)];
            let mut h = [
                [su.dot(su) + d[2][0].dot(r), su.dot(sv) + d[1][1].dot(r)],
                [0.0, sv.dot(sv) + d[0][2].dot(r)],
            ];
            h[1][0] = h[0][1];
            let det = |h: &[[f64; 2]; 2]| h[0][0] * h[1][1] - h[0][1] * h[1][0];
            if !(h[0][0] > 0.0 && h[1][1] > 0.0 && det(&h) > 0.0) {
                h = [[su.dot(su), su.dot(sv)], [su.dot(sv), sv.dot(sv)]];
            }
            let dt = det(&h);
            if dt <= 0.0 || dt.is_nan() {
                break;
            }
            let mut du = -(h[1][1] * g[0] - h[0][1] * g[1]) / dt;
            let mut dv = -(h[0][0] * g[1] - h[1][0] * g[0]) / dt;
            // A bound reached in one direction: the other moves alone.
            let (nu, nv) = (u + du, v + dv);
            let (cu, cv) = (nu.clamp(u0, u1), nv.clamp(v0, v1));
            if cu != nu && cv == nv && h[1][1] > 0.0 {
                du = cu - u;
                dv = (v - g[1] / h[1][1]).clamp(v0, v1) - v;
            } else if cv != nv && cu == nu && h[0][0] > 0.0 {
                dv = cv - v;
                du = (u - g[0] / h[0][0]).clamp(u0, u1) - u;
            } else {
                du = cu - u;
                dv = cv - v;
            }
            let mut f = 1.0;
            let mut moved = false;
            for _ in 0..30 {
                let (tu, tv) = (u + du * f, v + dv * f);
                let td = self.ders(tu, tv, 2);
                let tr = td[0][0] - p;
                let t2 = tr.dot(tr);
                if t2 <= d2 {
                    let step = (td[0][0] - d[0][0]).len();
                    u = tu;
                    v = tv;
                    d = td;
                    r = tr;
                    d2 = t2;
                    moved = step > tiny;
                    break;
                }
                f *= 0.5;
            }
            if !moved {
                break;
            }
        }
        (u, v)
    }

    /// The signed distance of `p` along the natural normal at its
    /// projection (the projection allowed a little past the domain).
    pub fn f(&self, p: V) -> f64 {
        let (u, v) = self.project(p, true);
        let d = self.ders(u, v, 1);
        let n = d[1][0].cross(d[0][1]).norm();
        (p - d[0][0]).dot(n)
    }

    /// The unit natural normal at `p`'s projection: the gradient of
    /// [`Spline::f`].
    pub fn grad(&self, p: V) -> V {
        let (u, v) = self.project(p, true);
        self.normal(u, v)
    }

    /// Whether `o` is the same surface: the same degrees and knots
    /// (within 1e-12 relative), weights within 1e-12 relative, and control
    /// points within `tol`.
    pub fn same(&self, o: &Spline, tol: f64) -> bool {
        let (a, b) = (&self.public, &o.public);
        let close = |x: f64, y: f64| (x - y).abs() <= 1e-12 * (1.0 + x.abs().max(y.abs()));
        a.degree_u == b.degree_u
            && a.degree_v == b.degree_v
            && self.nu == o.nu
            && self.nv == o.nv
            && a.knots_u.len() == b.knots_u.len()
            && a.knots_v.len() == b.knots_v.len()
            && a.knots_u.iter().zip(&b.knots_u).all(|(&x, &y)| close(x, y))
            && a.knots_v.iter().zip(&b.knots_v).all(|(&x, &y)| close(x, y))
            && self.pw.iter().zip(&o.pw).all(|(p, q)| {
                close(p[3], q[3])
                    && (V::from([p[0], p[1], p[2]]) * (1.0 / p[3])
                        - V::from([q[0], q[1], q[2]]) * (1.0 / q[3]))
                        .len()
                        < tol
            })
    }

    /// The point on the boundary curve at fixed `u = at` (or fixed
    /// `v = at`) with the other parameter `t`, and its derivative in `t`.
    pub fn iso(&self, fixed_u: bool, at: f64, t: f64) -> (V, V) {
        if fixed_u {
            let d = self.ders(at, t, 1);
            (d[0][0], d[0][1])
        } else {
            let d = self.ders(t, at, 1);
            (d[0][0], d[1][0])
        }
    }

    /// The domain of the free parameter along an iso-curve.
    pub fn iso_range(&self, fixed_u: bool) -> [f64; 2] {
        let (du, dv) = self.domain();
        if fixed_u { dv } else { du }
    }

    /// The parameters at which an iso-curve is sampled: four per knot
    /// span, at most 64 in all, and the ends.
    pub fn iso_samples(&self, fixed_u: bool) -> Vec<f64> {
        if fixed_u {
            sample_params(&self.kv, self.pv, self.nv, self.domain().1)
        } else {
            sample_params(&self.ku, self.pu, self.nu, self.domain().0)
        }
    }

    /// A boundary of the patch as an exact non-rational curve, oriented
    /// with the free parameter: its row of control points, when the
    /// weights along it are all equal (a polynomial boundary). `at` must
    /// be an end of the domain.
    pub fn boundary_curve(&self, fixed_u: bool, at: f64) -> Option<BSpline<3>> {
        let (du, dv) = self.domain();
        let pts: Vec<[f64; 4]> = if fixed_u {
            let i = if at == du[0] {
                0
            } else if at == du[1] {
                self.nu - 1
            } else {
                return None;
            };
            (0..self.nv).map(|j| self.pw[i * self.nv + j]).collect()
        } else {
            let j = if at == dv[0] {
                0
            } else if at == dv[1] {
                self.nv - 1
            } else {
                return None;
            };
            (0..self.nu).map(|i| self.pw[i * self.nv + j]).collect()
        };
        let w0 = pts[0][3];
        if pts.iter().any(|q| q[3] != w0) {
            return None;
        }
        Some(BSpline {
            degree: if fixed_u { self.pv } else { self.pu } as u32,
            control: pts
                .iter()
                .map(|q| [q[0] / q[3], q[1] / q[3], q[2] / q[3]])
                .collect(),
            knots: if fixed_u {
                self.kv.clone()
            } else {
                self.ku.clone()
            },
        })
    }

    /// The distinct interior knots along `u` (`along_u`) or `v`: where the
    /// surface's derivatives may jump, so where quadrature splits.
    pub fn breaks(&self, along_u: bool) -> Vec<f64> {
        let (k, p, n) = if along_u {
            (&self.ku, self.pu, self.nu)
        } else {
            (&self.kv, self.pv, self.nv)
        };
        let mut out: Vec<f64> = Vec::new();
        for &x in &k[p + 1..n] {
            if x > k[p] && x < k[n] && out.last() != Some(&x) {
                out.push(x);
            }
        }
        out
    }
}

/// Parameters across a domain: `m` per knot span (four, fewer when there
/// are many spans, so that there are at most about 64), and the end.
fn sample_params(knots: &[f64], p: usize, n: usize, dom: [f64; 2]) -> Vec<f64> {
    let spans: Vec<(f64, f64)> = (p..n)
        .map(|i| (knots[i], knots[i + 1]))
        .filter(|(a, b)| b > a)
        .collect();
    let m = (64 / spans.len().max(1)).clamp(1, 4);
    let mut out = Vec::with_capacity(spans.len() * m + 1);
    for &(a, b) in &spans {
        for k in 0..m {
            out.push(a + (b - a) * k as f64 / m as f64);
        }
    }
    out.push(dom[1]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quarter of the cylinder of radius 2 about z, as a rational
    /// quadratic in `u` and linear in `v` over z in [0, 3].
    fn quarter_cylinder() -> BSplineSurface {
        let w = std::f64::consts::FRAC_1_SQRT_2;
        BSplineSurface {
            degree_u: 2,
            degree_v: 1,
            control: vec![
                vec![[2.0, 0.0, 0.0], [2.0, 0.0, 3.0]],
                vec![[2.0, 2.0, 0.0], [2.0, 2.0, 3.0]],
                vec![[0.0, 2.0, 0.0], [0.0, 2.0, 3.0]],
            ],
            weights: Some(vec![vec![1.0, 1.0], vec![w, w], vec![1.0, 1.0]]),
            knots_u: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            knots_v: vec![0.0, 0.0, 1.0, 1.0],
        }
    }

    #[test]
    fn a_rational_quarter_cylinder_is_exact() {
        let s = Spline::new(&quarter_cylinder());
        assert!(s.valid);
        for i in 0..=10 {
            for j in 0..=4 {
                let (u, w) = (i as f64 / 10.0, j as f64 / 4.0);
                let p = s.eval(u, w);
                assert!(((p.x * p.x + p.y * p.y).sqrt() - 2.0).abs() < 1e-14);
                assert!((p.z - 3.0 * w).abs() < 1e-14);
                // The normal points away from the axis.
                let n = s.normal(u, w);
                let radial = v(p.x, p.y, 0.0).norm();
                assert!((n.dot(radial) - 1.0).abs() < 1e-12, "{n:?} {radial:?}");
            }
        }
    }

    #[test]
    fn derivatives_match_differences() {
        let mut b = quarter_cylinder();
        // Bend it so that every second derivative is non-zero.
        b.control[1][1] = [2.5, 2.2, 3.4];
        let s = Spline::new(&b);
        let h = 1e-5;
        for &(u, w) in &[(0.3, 0.4), (0.7, 0.9), (0.05, 0.5)] {
            let d = s.ders(u, w, 2);
            let du = (s.eval(u + h, w) - s.eval(u - h, w)) * (0.5 / h);
            let dv = (s.eval(u, w + h) - s.eval(u, w - h)) * (0.5 / h);
            assert!((du - d[1][0]).len() < 1e-8, "{du:?} {:?}", d[1][0]);
            assert!((dv - d[0][1]).len() < 1e-8);
            let d1 = |u: f64, w: f64| s.ders(u, w, 1);
            let duu = (d1(u + h, w)[1][0] - d1(u - h, w)[1][0]) * (0.5 / h);
            let duv = (d1(u, w + h)[1][0] - d1(u, w - h)[1][0]) * (0.5 / h);
            let dvv = (d1(u, w + h)[0][1] - d1(u, w - h)[0][1]) * (0.5 / h);
            assert!((duu - d[2][0]).len() < 1e-6, "{duu:?} {:?}", d[2][0]);
            assert!((duv - d[1][1]).len() < 1e-6);
            assert!((dvv - d[0][2]).len() < 1e-6);
        }
    }

    #[test]
    fn projection_finds_the_nearest_point() {
        let s = Spline::new(&quarter_cylinder());
        for &(u, w, off) in &[(0.2, 0.3, 0.5), (0.8, 0.6, -0.4), (0.5, 1.0, 0.1)] {
            let p0 = s.eval(u, w);
            let p = p0 + s.normal(u, w) * off;
            let (pu, pv) = s.project(p, false);
            assert!((s.eval(pu, pv) - p0).len() < 1e-12, "{u} {w}: {pu} {pv}");
            assert!((s.f(p) - off).abs() < 1e-12);
        }
        // Past the end of the domain the projection stops at the boundary
        // unless extended, when the arc continues.
        let p = v(2.0 * 0.995, -2.0 * 0.0998, 1.0);
        let (pu, _) = s.project(p, false);
        assert_eq!(pu, 0.0);
        assert!(s.f(p).abs() < 1e-4);
    }

    #[test]
    fn malformed_records_are_refused() {
        let mut b = quarter_cylinder();
        b.knots_u = vec![0.0, 0.0, 0.5, 1.0, 1.0, 1.0];
        assert!(Spline::check(&b).is_err());
        let mut b = quarter_cylinder();
        b.weights.as_mut().unwrap()[1][0] = 0.0;
        assert!(Spline::check(&b).is_err());
        let mut b = quarter_cylinder();
        b.control[1].pop();
        assert!(Spline::check(&b).is_err());
        assert!(!Spline::new(&b).valid);
    }
}
