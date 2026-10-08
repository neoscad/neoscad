//! B-spline surfaces ([`BSplineSurface`]): evaluating and projecting onto
//! them, and building the ones blends need.
//!
//! - [`Evaluator`]: points, derivatives to second order, normals and
//!   closest points (point inversion) of a surface.
//! - [`interpolate_curve`] and [`fit_curves`]: cubic B-spline curves
//!   through points, or fitted to curves given as functions within a
//!   tolerance, several on one knot vector (a blend's spine and the two
//!   curves where it touches its faces).
//! - [`interpolate_surface`] and [`fit_surface`]: a cubic B-spline surface
//!   through a grid of points, or fitted to a surface given as a function.
//! - [`canal_surface`]: the rational surface a ball of a given radius
//!   sweeps between two contact curves as its centre runs along a spine,
//!   with exact circular arcs across.
//! - [`ruled_surface`]: the surface of straight lines between two curves
//!   (a chamfer).
//!
//! Every result is deterministic: the same input gives the same bits on
//! every platform.

use crate::bspline;
use crate::math::*;
use crate::model::{BSpline, BSplineSurface};
use crate::nurbs::Spline;

/// `a > b`, false when either is NaN: the checks of the inputs below
/// refuse NaN with everything else they refuse.
fn above(a: f64, b: f64) -> bool {
    a > b
}

/// A surface's point and derivatives at a parameter pair.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfacePoint {
    /// The point.
    pub point: [f64; 3],
    /// `∂S/∂u`.
    pub du: [f64; 3],
    /// `∂S/∂v`.
    pub dv: [f64; 3],
    /// `∂²S/∂u²`.
    pub duu: [f64; 3],
    /// `∂²S/∂u∂v`.
    pub duv: [f64; 3],
    /// `∂²S/∂v²`.
    pub dvv: [f64; 3],
}

/// A [`BSplineSurface`] checked and ready for repeated evaluation and
/// projection (which keeps a grid of seed points, made on first use).
#[derive(Debug)]
pub struct Evaluator(Spline);

impl Evaluator {
    /// Checks the record (degrees, sizes, clamped knots, positive
    /// weights): `Err` says what is wrong with it.
    pub fn new(surface: &BSplineSurface) -> Result<Evaluator, String> {
        Spline::check(surface)?;
        Ok(Evaluator(Spline::new(surface)))
    }

    /// The parameter domain: `([u0, u1], [v0, v1])`.
    pub fn domain(&self) -> ([f64; 2], [f64; 2]) {
        self.0.domain()
    }

    /// The point at `(u, v)`. Outside the domain the end spans'
    /// polynomials continue the surface.
    pub fn eval(&self, u: f64, v: f64) -> [f64; 3] {
        self.0.eval(u, v).arr()
    }

    /// The point and its first and second derivatives at `(u, v)`.
    pub fn derivatives(&self, u: f64, v: f64) -> SurfacePoint {
        let d = self.0.ders(u, v, 2);
        SurfacePoint {
            point: d[0][0].arr(),
            du: d[1][0].arr(),
            dv: d[0][1].arr(),
            duu: d[2][0].arr(),
            duv: d[1][1].arr(),
            dvv: d[0][2].arr(),
        }
    }

    /// The unit natural normal `∂u × ∂v` at `(u, v)`.
    pub fn normal(&self, u: f64, v: f64) -> [f64; 3] {
        self.0.normal(u, v).arr()
    }

    /// The parameters of the point of the patch nearest `p` (within the
    /// domain): from the nearest of a grid of seeds, Newton on the
    /// squared distance with each step kept in the domain and shortened
    /// until the distance does not grow. A surface folded back on itself
    /// within a seed spacing can give a local minimum.
    pub fn project(&self, p: [f64; 3]) -> [f64; 2] {
        let (u, v) = self.0.project(V::from(p), false);
        [u, v]
    }
}

/// The cubic B-spline curve through `points` at the increasing
/// parameters `params` (two points give a segment, three a quadratic), on
/// `[params[0], params[last]]`. Its knots depend only on `params`, so
/// curves interpolated at the same parameters share a knot vector.
pub fn interpolate_curve(points: &[[f64; 3]], params: &[f64]) -> Result<BSpline<3>, String> {
    check_params(points.len(), params)?;
    if points.iter().flatten().any(|x| !x.is_finite()) {
        return Err("a point is not finite".into());
    }
    Ok(bspline::interpolate_at(points, params))
}

fn check_params(n: usize, params: &[f64]) -> Result<(), String> {
    if n < 2 || params.len() != n {
        return Err(format!(
            "{n} points and {} parameters (at least 2 of each, as many of one as the other)",
            params.len()
        ));
    }
    if params.iter().any(|x| !x.is_finite()) || params.windows(2).any(|w| w[1] <= w[0]) {
        return Err("parameters are not increasing".into());
    }
    Ok(())
}

/// Curves fitted on one knot vector, and how far they stray.
#[derive(Clone, Debug, PartialEq)]
pub struct FittedCurves {
    /// The curves, in the order of the functions, all on the same knots.
    pub curves: Vec<BSpline<3>>,
    /// The largest distance found between a curve and its function, at
    /// the midpoints between the interpolated points.
    pub error: f64,
}

/// Cubic B-splines interpolating each of `fs` at the same evenly spaced
/// parameters of `range`, so that they share their knots and each
/// parameter means the same station on all of them (a spine and its two
/// contact curves), with the parameter of the functions. The number of
/// points doubles from 9 until every curve is within `tolerance` of its
/// function at the midpoints, or would pass `max_points`: then `Err`.
pub fn fit_curves(
    fs: &[&dyn Fn(f64) -> [f64; 3]],
    range: [f64; 2],
    tolerance: f64,
    max_points: usize,
) -> Result<FittedCurves, String> {
    if fs.is_empty() || !above(range[1], range[0]) || !above(tolerance, 0.0) {
        return Err("no functions, an empty range or no tolerance".into());
    }
    let mut n = 8usize;
    loop {
        let ts: Vec<f64> = (0..=n)
            .map(|k| range[0] + (range[1] - range[0]) * k as f64 / n as f64)
            .collect();
        let mut curves = Vec::with_capacity(fs.len());
        let mut error = 0.0f64;
        for f in fs {
            let pts: Vec<[f64; 3]> = ts.iter().map(|&t| f(t)).collect();
            let c = interpolate_curve(&pts, &ts)?;
            for k in 0..n {
                let t = 0.5 * (ts[k] + ts[k + 1]);
                error = error.max((V::from(bspline::eval(&c, t)) - V::from(f(t))).len());
            }
            curves.push(c);
        }
        if error <= tolerance {
            return Ok(FittedCurves { curves, error });
        }
        if 2 * n + 1 > max_points {
            return Err(format!(
                "{} points leave the curves {error:.2e} from their functions",
                n + 1
            ));
        }
        n *= 2;
    }
}

/// The cubic B-spline surface through a grid of points:
/// `points[i][j]` at `(us[i], vs[j])`, both increasing. Rows are
/// interpolated along `v`, then their control points along `u` (tensor
/// product interpolation), so the surface passes through every point.
/// Two or three parameters in a direction give degree 1 or 2 there.
pub fn interpolate_surface(
    points: &[Vec<[f64; 3]>],
    us: &[f64],
    vs: &[f64],
) -> Result<BSplineSurface, String> {
    check_params(points.len(), us)?;
    for row in points {
        check_params(row.len(), vs)?;
    }
    if points.iter().flatten().flatten().any(|x| !x.is_finite()) {
        return Err("a point is not finite".into());
    }
    let rows: Vec<BSpline<3>> = points
        .iter()
        .map(|row| bspline::interpolate_at(row, vs))
        .collect();
    let nv = rows[0].control.len();
    let cols: Vec<BSpline<3>> = (0..nv)
        .map(|j| {
            let pts: Vec<[f64; 3]> = rows.iter().map(|r| r.control[j]).collect();
            bspline::interpolate_at(&pts, us)
        })
        .collect();
    let nu = cols[0].control.len();
    Ok(BSplineSurface {
        degree_u: cols[0].degree,
        degree_v: rows[0].degree,
        control: (0..nu)
            .map(|i| cols.iter().map(|c| c.control[i]).collect())
            .collect(),
        weights: None,
        knots_u: cols[0].knots.clone(),
        knots_v: rows[0].knots.clone(),
    })
}

/// A surface fitted within a tolerance, and how far it strays.
#[derive(Clone, Debug, PartialEq)]
pub struct FittedSurface {
    /// The surface.
    pub surface: BSplineSurface,
    /// The largest distance found between the surface and what it
    /// stands for.
    pub error: f64,
}

/// A cubic B-spline surface interpolating `f` on an even grid over
/// `u_range` × `v_range`, with `f`'s own parameters. The grid starts at
/// 9 × 9 and doubles in each direction whose midpoints (between grid
/// lines, and at the cells' centres) are further than `tolerance` from
/// `f`, until all are within it; `Err` when the grid would pass
/// `max_points` points.
pub fn fit_surface(
    f: &dyn Fn(f64, f64) -> [f64; 3],
    u_range: [f64; 2],
    v_range: [f64; 2],
    tolerance: f64,
    max_points: usize,
) -> Result<FittedSurface, String> {
    if !above(u_range[1], u_range[0]) || !above(v_range[1], v_range[0]) || !above(tolerance, 0.0) {
        return Err("an empty range or no tolerance".into());
    }
    let lin = |r: [f64; 2], n: usize| -> Vec<f64> {
        (0..=n)
            .map(|k| r[0] + (r[1] - r[0]) * k as f64 / n as f64)
            .collect()
    };
    let (mut nu, mut nv) = (8usize, 8usize);
    loop {
        let (us, vs) = (lin(u_range, nu), lin(v_range, nv));
        let pts: Vec<Vec<[f64; 3]>> = us
            .iter()
            .map(|&u| vs.iter().map(|&v| f(u, v)).collect())
            .collect();
        let surface = interpolate_surface(&pts, &us, &vs)?;
        let ev = Spline::new(&surface);
        let gap = |u: f64, v: f64| (ev.eval(u, v) - V::from(f(u, v))).len();
        let mid = |xs: &[f64], k: usize| 0.5 * (xs[k] + xs[k + 1]);
        let (mut eu, mut evv, mut ec) = (0.0f64, 0.0f64, 0.0f64);
        for i in 0..=nu {
            for j in 0..=nv {
                if i < nu {
                    eu = eu.max(gap(mid(&us, i), vs[j]));
                }
                if j < nv {
                    evv = evv.max(gap(us[i], mid(&vs, j)));
                }
                if i < nu && j < nv {
                    ec = ec.max(gap(mid(&us, i), mid(&vs, j)));
                }
            }
        }
        let error = eu.max(evv).max(ec);
        if error <= tolerance {
            return Ok(FittedSurface { surface, error });
        }
        let more_u = eu > tolerance || (ec > tolerance && eu >= evv);
        let more_v = evv > tolerance || (ec > tolerance && evv >= eu);
        let (nu2, nv2) = (
            if more_u { 2 * nu } else { nu },
            if more_v { 2 * nv } else { nv },
        );
        if (nu2 + 1) * (nv2 + 1) > max_points {
            return Err(format!(
                "a {} by {} grid leaves the surface {error:.2e} from the function",
                nu + 1,
                nv + 1
            ));
        }
        nu = nu2;
        nv = nv2;
    }
}

/// Checks that curves share a degree and a knot vector.
fn compatible(cs: &[&BSpline<3>]) -> Result<(), String> {
    let c0 = cs[0];
    for c in cs {
        let p = c.degree as usize;
        if !(1..=7).contains(&p) || c.control.len() <= p || c.knots.len() != c.control.len() + p + 1
        {
            return Err("a curve's degree, control points and knots do not agree".into());
        }
        if c.degree != c0.degree || c.knots != c0.knots {
            return Err("the curves do not share a degree and a knot vector".into());
        }
        if c.control.iter().flatten().any(|x| !x.is_finite()) {
            return Err("a control point is not finite".into());
        }
    }
    let k = &c0.knots;
    let (p, n) = (c0.degree as usize, c0.control.len());
    if k.windows(2).any(|w| w[1] < w[0])
        || k[..=p].iter().any(|&x| x != k[0])
        || k[n..].iter().any(|&x| x != k[n])
        || k[p] >= k[n]
    {
        return Err("the knots are not clamped and non-decreasing".into());
    }
    Ok(())
}

/// The distance from `q` to the circle of radius `r` about `c` in the
/// plane through `c` with unit normal `n`.
fn circle_distance(q: V, c: V, n: V, r: f64) -> f64 {
    let d = q - c;
    let h = d.dot(n);
    let rho = d.reject(n).len();
    ((rho - r) * (rho - r) + h * h).sqrt()
}

/// The surface a ball of `radius` sweeps as its centre runs along
/// `spine`, between where it touches two faces along `foot_a` and
/// `foot_b` (a rolling-ball blend), as a rational B-spline: across (`u`,
/// from `foot_a` at 0 to `foot_b` at 1) the circular arc in each of the
/// ball's positions, degree 2 with the arc's exact weights; along (`v`)
/// the curves' own degree and knots, which the three must share (fit them
/// together, [`fit_curves`]).
///
/// The rows at `u = 0` and `u = 1` are the foot curves themselves, so
/// the patch's sides are exactly the contact curves it was given. The
/// middle row (the arcs' tangent points and weights, in homogeneous
/// coordinates) is interpolated at the knots' Greville abscissae, so the
/// arcs there are exact; between them the surface is within `error` of
/// the true arcs (and that error includes how far the foot curves are
/// from `radius` off the spine). With a straight spine and constant arc
/// (two planes) it is exact; for a curved spine the error falls with the
/// fourth power of the stations' spacing.
///
/// Each arc must be shorter than a half circle. The natural normal
/// `∂u × ∂v` points out of the ball (away from the spine) where
/// `(foot_a - spine) × (foot_b - spine)` points the way the spine runs,
/// and into it otherwise; swap the feet to flip it.
pub fn canal_surface(
    spine: &BSpline<3>,
    foot_a: &BSpline<3>,
    foot_b: &BSpline<3>,
    radius: f64,
) -> Result<FittedSurface, String> {
    compatible(&[spine, foot_a, foot_b])?;
    if !above(radius, 0.0) || !radius.is_finite() {
        return Err("the radius is not positive".into());
    }
    let p = spine.degree as usize;
    let n = spine.control.len();
    let knots = &spine.knots;
    let g = bspline::greville(knots, p, n);
    let at = |t: f64| {
        (
            V::from(bspline::eval(spine, t)),
            V::from(bspline::eval(foot_a, t)),
            V::from(bspline::eval(foot_b, t)),
        )
    };
    let mut h: Vec<[f64; 4]> = Vec::with_capacity(n);
    for &t in &g {
        let (c, a, b) = at(t);
        let (da, db) = (a - c, b - c);
        let (la, lb) = (da.len(), db.len());
        if !above(la, 0.0) || !above(lb, 0.0) {
            return Err(format!("a foot point is on the spine at {t}"));
        }
        let cos = da.dot(db) / (la * lb);
        if !above(cos, -1.0 + 1e-6) {
            return Err(format!("the arc at {t} is a half circle or more"));
        }
        let w = (0.5 * (1.0 + cos)).sqrt();
        let m = c + (da + db) * (1.0 / (1.0 + cos));
        h.push([m.x * w, m.y * w, m.z * w, w]);
    }
    let mid =
        bspline::interpolate_knots(&h, &g, knots, p).ok_or("the stations do not fit the knots")?;
    if mid.control.iter().any(|q| !above(q[3], 0.0)) {
        return Err("an interpolated weight is not positive".into());
    }
    let surface = BSplineSurface {
        degree_u: 2,
        degree_v: p as u32,
        control: vec![
            foot_a.control.clone(),
            mid.control
                .iter()
                .map(|q| [q[0] / q[3], q[1] / q[3], q[2] / q[3]])
                .collect(),
            foot_b.control.clone(),
        ],
        weights: Some(vec![
            vec![1.0; n],
            mid.control.iter().map(|q| q[3]).collect(),
            vec![1.0; n],
        ]),
        knots_u: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        knots_v: knots.clone(),
    };
    Spline::check(&surface)?;
    // How far the patch is from the true arcs: at four points a knot span
    // along, seven across.
    let ev = Spline::new(&surface);
    let mut error = 0.0f64;
    for (a0, b0) in bspline::spans(spine) {
        for k in 0..=4 {
            let t = a0 + (b0 - a0) * k as f64 / 4.0;
            let (c, a, b) = at(t);
            let nrm = (a - c).cross(b - c).norm();
            error = error
                .max(((a - c).len() - radius).abs())
                .max(((b - c).len() - radius).abs());
            for i in 1..8 {
                let q = ev.eval(i as f64 / 8.0, t);
                error = error.max(circle_distance(q, c, nrm, radius));
            }
        }
    }
    Ok(FittedSurface { surface, error })
}

/// The ruled surface between two curves (a chamfer between two contact
/// curves): degree 1 across (`u`, from `a` at 0 to `b` at 1), the curves'
/// own degree and knots along, which they must share. Exact: each line
/// joins the points of equal parameter.
pub fn ruled_surface(a: &BSpline<3>, b: &BSpline<3>) -> Result<BSplineSurface, String> {
    compatible(&[a, b])?;
    let surface = BSplineSurface {
        degree_u: 1,
        degree_v: a.degree,
        control: vec![a.control.clone(), b.control.clone()],
        weights: None,
        knots_u: vec![0.0, 0.0, 1.0, 1.0],
        knots_v: a.knots.clone(),
    };
    Spline::check(&surface)?;
    Ok(surface)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn circle(r: f64, z: f64) -> impl Fn(f64) -> [f64; 3] {
        move |t: f64| [r * cos(t), r * sin(t), z]
    }

    #[test]
    fn fitted_curves_share_knots_and_meet_the_tolerance() {
        let (a, b) = (circle(10.0, 0.0), circle(7.0, 3.0));
        let f = fit_curves(&[&a, &b], [0.0, PI / 2.0], 1e-8, 10_000).unwrap();
        assert!(f.error <= 1e-8);
        assert_eq!(f.curves[0].knots, f.curves[1].knots);
        let q = bspline::eval(&f.curves[0], 0.3);
        assert!((V::from(q) - V::from(a(0.3))).len() < 1e-8);
    }

    #[test]
    fn a_fitted_surface_meets_the_tolerance() {
        let f = |u: f64, v: f64| [u, v, 0.3 * sin(u) * cos(2.0 * v)];
        let fit = fit_surface(&f, [0.0, 3.0], [0.0, 2.0], 1e-7, 100_000).unwrap();
        assert!(fit.error <= 1e-7);
        let ev = Evaluator::new(&fit.surface).unwrap();
        let q = ev.eval(1.234, 0.567);
        assert!((V::from(q) - V::from(f(1.234, 0.567))).len() < 1e-6);
        let [u, v] = ev.project([1.234, 0.567, 1.0]);
        let n = ev.normal(u, v);
        // The nearest point's normal points back at the point.
        let d = (V::from([1.234, 0.567, 1.0]) - V::from(ev.eval(u, v))).norm();
        assert!(V::from(n).cross(d).len() < 1e-9);
    }

    #[test]
    fn a_canal_surface_with_a_straight_spine_is_a_cylinder() {
        // A ball of radius 2 rolling along x in the corner of z = 0 and
        // y = 0, on the side y, z > 0.
        let line = |y: f64, z: f64| move |t: f64| [t, y, z];
        let (s, a, b) = (line(2.0, 2.0), line(2.0, 0.0), line(0.0, 2.0));
        let f = fit_curves(&[&s, &a, &b], [0.0, 10.0], 1e-9, 1000).unwrap();
        let c = canal_surface(&f.curves[0], &f.curves[1], &f.curves[2], 2.0).unwrap();
        assert!(c.error < 1e-12, "{}", c.error);
        let ev = Evaluator::new(&c.surface).unwrap();
        for i in 0..=8 {
            for j in 0..=8 {
                let q = V::from(ev.eval(i as f64 / 8.0, 10.0 * j as f64 / 8.0));
                let r = ((q.y - 2.0).powi(2) + (q.z - 2.0).powi(2)).sqrt();
                assert!((r - 2.0).abs() < 1e-13);
            }
        }
        // (a - s) × (b - s) = (0, 0, -2) × (0, -2, 0) points along -x,
        // against the spine: the normal points into the ball.
        let q = V::from(ev.eval(0.5, 5.0));
        let n = V::from(ev.normal(0.5, 5.0));
        assert!(n.dot(v(5.0, 2.0, 2.0) - q) > 0.0);
    }

    #[test]
    fn a_canal_surface_round_a_circle_is_near_the_torus() {
        // The fillet of radius 2 between the plane z = 0 and a boss of
        // radius 5 about z: its spine is the circle of radius 7 at z = 2.
        let s = |t: f64| [7.0 * cos(t), 7.0 * sin(t), 2.0];
        let a = |t: f64| [7.0 * cos(t), 7.0 * sin(t), 0.0];
        let b = |t: f64| [5.0 * cos(t), 5.0 * sin(t), 2.0];
        let f = fit_curves(&[&s, &a, &b], [0.0, PI / 2.0], 1e-9, 10_000).unwrap();
        let c = canal_surface(&f.curves[0], &f.curves[1], &f.curves[2], 2.0).unwrap();
        assert!(c.error < 1e-8, "{}", c.error);
        let ev = Evaluator::new(&c.surface).unwrap();
        for i in 0..=8 {
            for j in 0..=16 {
                let q = V::from(ev.eval(i as f64 / 8.0, PI / 2.0 * j as f64 / 16.0));
                let rho = (q.x * q.x + q.y * q.y).sqrt();
                let d = ((rho - 7.0).powi(2) + (q.z - 2.0).powi(2)).sqrt();
                assert!((d - 2.0).abs() < 1e-8, "{d}");
            }
        }
    }

    #[test]
    fn mismatched_curves_are_refused() {
        let a = interpolate_curve(&[[0.0; 3], [1.0, 0.0, 0.0]], &[0.0, 1.0]).unwrap();
        let b = interpolate_curve(&[[0.0; 3], [1.0, 1.0, 0.0]], &[0.0, 2.0]).unwrap();
        assert!(ruled_surface(&a, &b).is_err());
        let b = interpolate_curve(&[[0.0, 0.0, 1.0], [1.0, 1.0, 1.0]], &[0.0, 1.0]).unwrap();
        assert!(ruled_surface(&a, &b).is_ok());
    }
}
