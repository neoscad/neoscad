//! Placing points exactly on several surfaces at once.

use crate::math::*;
use crate::surf::Surf;

/// The point on all of `surfs` nearest `p0`: Gauss–Newton with
/// minimum-norm steps, so that with fewer than three surfaces the point
/// moves as little as possible along the set they leave free. Returns the
/// point and the largest |f| left.
///
/// Where two of the surfaces are tangent (a blend along its contact, or
/// faces that nearly touch) the Jacobian is nearly singular and the
/// iteration converges only linearly, a factor of about four a step,
/// until the residual reaches the surfaces' own rounding (for a B-spline
/// face, its projection's), where it wanders without improving. With a
/// B-spline face among them it is stopped there, after [`STALL`] steps
/// without halving the best residual, and the best point is returned:
/// before, those solves ran all 60 steps, each a projection onto the
/// patch per surface, which was most of a fitted blend's reconstruction
/// time, and returned wherever the wandering left them (edges fitted
/// through such points read back from STEP with OCCT's tolerances raised
/// to 5e-5 and volumes 1e-4 off).
pub(crate) fn solve(surfs: &[Surf], p0: V) -> (V, f64) {
    let mut p = p0;
    let k = surfs.len();
    if k == 0 {
        return (p0, 0.0);
    }
    // Each surface's value and gradient at `p`, from one evaluation: a
    // B-spline face's are one projection onto the patch, which is nearly
    // all of a step's cost.
    let mut fg: Vec<(f64, V)> = surfs.iter().map(|s| s.f_grad(p)).collect();
    let resid = |fg: &[(f64, V)]| fg.iter().fold(0.0f64, |m, x| m.max(x.0.abs()));
    let mut res = resid(&fg);
    let mut best = (p, res);
    let mut stalled = 0;
    // Only solves on a B-spline face stop at a stall: there each step is
    // a projection, and the residual's floor is the projection's rounding
    // (about 1e-10). On analytic surfaces a step costs nothing, and at a
    // corner of three nearly tangent faces the residual crawls from 1e-13
    // to 1e-15 over dozens of steps while the point slides 2e-6 along the
    // direction the faces leave nearly free, to where the edges' curves
    // end: stopped early, a box's corner was left 2e-6 off its edges
    // (fillet corpus seed 2, model 508).
    let fitted = surfs.iter().any(|s| matches!(s, Surf::Spline(_)));
    for _ in 0..60 {
        if res < 1e-15 {
            break;
        }
        let step = if k <= 3 {
            // dp = Jᵀ (J Jᵀ + λ)⁻¹ (-f)
            let mut jj = [[0.0; 3]; 3];
            let mut rhs = [0.0; 3];
            for i in 0..k {
                for j in 0..k {
                    jj[i][j] = fg[i].1.dot(fg[j].1) + if i == j { 1e-14 } else { 0.0 };
                }
                rhs[i] = -fg[i].0;
            }
            match solve_dense(&mut jj, &mut rhs, k) {
                Some(y) => (0..k).fold(V::default(), |acc, i| acc + fg[i].1 * y[i]),
                None => break,
            }
        } else {
            let mut jtj = [[0.0; 3]; 3];
            let mut jtf = [0.0; 3];
            for &(fi, g) in &fg {
                let gi = g.arr();
                for a in 0..3 {
                    for b in 0..3 {
                        jtj[a][b] += gi[a] * gi[b];
                    }
                    jtf[a] -= gi[a] * fi;
                }
            }
            for (a, row) in jtj.iter_mut().enumerate() {
                row[a] += 1e-12;
            }
            match solve_dense(&mut jtj, &mut jtf, 3) {
                Some(y) => v(y[0], y[1], y[2]),
                None => break,
            }
        };
        if !step.is_finite() {
            break;
        }
        p = p + step;
        fg = surfs.iter().map(|s| s.f_grad(p)).collect();
        res = resid(&fg);
        if step.len() < 1e-16 * (1.0 + p.len()) {
            break;
        }
        if res < 0.5 * best.1 {
            stalled = 0;
        } else {
            stalled += 1;
        }
        if res < best.1 {
            best = (p, res);
        }
        if stalled >= STALL && fitted {
            return best;
        }
    }
    (p, res)
}

/// How many steps of [`solve`] may pass without halving the best
/// residual before it stops. Newton's quadratic convergence and the
/// linear convergence at a tangency both halve it every step; a stall
/// this long is rounding.
const STALL: usize = 4;

/// A point where `a` and `b` touch (on both, with parallel normals) near
/// `q`: Gauss–Newton on (f_a, f_b, n_a × n_b) with a numeric Jacobian.
/// `None` if it does not converge to one.
pub(crate) fn tangent_point(a: &Surf, b: &Surf, q: V, scale: f64) -> Option<V> {
    let r = |p: V| -> [f64; 5] {
        let c = a.grad(p).cross(b.grad(p));
        [a.f(p), b.f(p), c.x, c.y, c.z]
    };
    let mut p = q;
    let h = 1e-7 * scale.max(1e-300);
    for _ in 0..50 {
        let f = r(p);
        let mut j = [[0.0f64; 3]; 5];
        for k in 0..3 {
            let mut dp = V::default();
            match k {
                0 => dp.x = h,
                1 => dp.y = h,
                _ => dp.z = h,
            }
            let f1 = r(p + dp);
            for i in 0..5 {
                j[i][k] = (f1[i] - f[i]) / h;
            }
        }
        let mut jtj = [[0.0; 3]; 3];
        let mut jtf = [0.0; 3];
        for i in 0..5 {
            for x in 0..3 {
                for y in 0..3 {
                    jtj[x][y] += j[i][x] * j[i][y];
                }
                jtf[x] -= j[i][x] * f[i];
            }
        }
        for (x, row) in jtj.iter_mut().enumerate() {
            row[x] += 1e-14;
        }
        let y = solve_dense(&mut jtj, &mut jtf, 3)?;
        let step = v(y[0], y[1], y[2]);
        if !step.is_finite() {
            return None;
        }
        p = p + step;
        if step.len() < 1e-15 * scale.max(1.0) {
            break;
        }
    }
    let f = r(p);
    let res = f.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    (res < 1e-9 * scale.max(1.0)).then_some(p)
}
