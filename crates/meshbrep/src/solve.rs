//! Placing points exactly on several surfaces at once.

use crate::math::*;
use crate::surf::Surf;

/// The point on all of `surfs` nearest `p0`: Gauss–Newton with
/// minimum-norm steps, so that with fewer than three surfaces the point
/// moves as little as possible along the set they leave free. Returns the
/// point and the largest |f| left.
pub(crate) fn solve(surfs: &[Surf], p0: V) -> (V, f64) {
    let mut p = p0;
    let k = surfs.len();
    if k == 0 {
        return (p0, 0.0);
    }
    for _ in 0..60 {
        let res = surfs.iter().fold(0.0f64, |m, s| m.max(s.f(p).abs()));
        if res < 1e-15 {
            break;
        }
        let step = if k <= 3 {
            // dp = Jᵀ (J Jᵀ + λ)⁻¹ (-f)
            let mut jj = [[0.0; 3]; 3];
            let mut rhs = [0.0; 3];
            let g: Vec<V> = surfs.iter().map(|s| s.grad(p)).collect();
            for i in 0..k {
                for j in 0..k {
                    jj[i][j] = g[i].dot(g[j]) + if i == j { 1e-14 } else { 0.0 };
                }
                rhs[i] = -surfs[i].f(p);
            }
            match solve_dense(&mut jj, &mut rhs, k) {
                Some(y) => (0..k).fold(V::default(), |acc, i| acc + g[i] * y[i]),
                None => break,
            }
        } else {
            let mut jtj = [[0.0; 3]; 3];
            let mut jtf = [0.0; 3];
            for s in surfs {
                let gi = s.grad(p).arr();
                let fi = s.f(p);
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
        if step.len() < 1e-16 * (1.0 + p.len()) {
            break;
        }
    }
    let res = surfs.iter().fold(0.0f64, |m, s| m.max(s.f(p).abs()));
    (p, res)
}

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
