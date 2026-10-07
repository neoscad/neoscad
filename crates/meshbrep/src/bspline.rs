//! Clamped non-rational B-splines in 2 or 3 dimensions: evaluation,
//! derivative and global cubic interpolation (Piegl & Tiller, "The NURBS
//! Book", algorithms A2.1, A2.2 and A9.1).

use crate::model::BSpline;

/// The knot span index `s` with `knots[s] <= t < knots[s + 1]`, clamped to
/// the valid range so that `t` at the end uses the last span.
pub(crate) fn find_span<const D: usize>(b: &BSpline<D>, t: f64) -> usize {
    let p = b.degree as usize;
    let n = b.control.len() - 1;
    if t >= b.knots[n + 1] {
        return n;
    }
    if t <= b.knots[p] {
        return p;
    }
    let (mut lo, mut hi) = (p, n + 1);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if t < b.knots[mid] {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    lo
}

/// The `p + 1` non-zero basis functions at `t` in span `s`.
fn basis_funs(knots: &[f64], s: usize, t: f64, p: usize, out: &mut [f64; 8]) {
    let mut left = [0.0; 8];
    let mut right = [0.0; 8];
    out[0] = 1.0;
    for j in 1..=p {
        left[j] = t - knots[s + 1 - j];
        right[j] = knots[s + j] - t;
        let mut saved = 0.0;
        for r in 0..j {
            let den = right[r + 1] + left[j - r];
            let tmp = if den == 0.0 { 0.0 } else { out[r] / den };
            out[r] = saved + right[r + 1] * tmp;
            saved = left[j - r] * tmp;
        }
        out[j] = saved;
    }
}

pub(crate) fn eval<const D: usize>(b: &BSpline<D>, t: f64) -> [f64; D] {
    let p = b.degree as usize;
    let s = find_span(b, t);
    let mut n = [0.0; 8];
    basis_funs(&b.knots, s, t, p, &mut n);
    let mut out = [0.0; D];
    for (j, nj) in n.iter().enumerate().take(p + 1) {
        let c = &b.control[s - p + j];
        for d in 0..D {
            out[d] += nj * c[d];
        }
    }
    out
}

/// The derivative curve (degree one lower). A degree-0 result is a
/// piecewise constant curve, which `eval` handles.
pub(crate) fn derivative<const D: usize>(b: &BSpline<D>) -> BSpline<D> {
    let p = b.degree as usize;
    let n = b.control.len();
    let mut control = Vec::with_capacity(n.saturating_sub(1));
    for i in 0..n - 1 {
        let den = b.knots[i + p + 1] - b.knots[i + 1];
        let mut q = [0.0; D];
        if den > 0.0 {
            for d in 0..D {
                q[d] = p as f64 * (b.control[i + 1][d] - b.control[i][d]) / den;
            }
        }
        control.push(q);
    }
    BSpline {
        degree: (p - 1) as u32,
        control,
        knots: b.knots[1..b.knots.len() - 1].to_vec(),
    }
}

/// The distinct knot intervals `[a, b]` with `a < b`.
pub(crate) fn spans<const D: usize>(b: &BSpline<D>) -> Vec<(f64, f64)> {
    let p = b.degree as usize;
    let n = b.control.len();
    let mut out = Vec::new();
    for i in p..n {
        let (a, c) = (b.knots[i], b.knots[i + 1]);
        if c > a {
            out.push((a, c));
        }
    }
    out
}

fn dist<const D: usize>(a: &[f64; D], b: &[f64; D]) -> f64 {
    let mut s = 0.0;
    for d in 0..D {
        s += (a[d] - b[d]) * (a[d] - b[d]);
    }
    s.sqrt()
}

/// The cubic B-spline through `pts` (at least 2), parametrised by chord
/// length on [0, 1], with averaged knots. Two points give a segment and
/// three a quadratic.
pub(crate) fn interpolate<const D: usize>(pts: &[[f64; D]]) -> BSpline<D> {
    let n = pts.len();
    debug_assert!(n >= 2);
    let total: f64 = pts.windows(2).map(|w| dist(&w[0], &w[1])).sum();
    let mut u = vec![0.0; n];
    for i in 1..n {
        u[i] = if total > 0.0 {
            u[i - 1] + dist(&pts[i - 1], &pts[i]) / total
        } else {
            i as f64 / (n - 1) as f64
        };
    }
    u[n - 1] = 1.0;
    interpolate_at(pts, &u)
}

/// The cubic B-spline through `pts` at the given increasing parameters
/// `u` (so `eval(result, u[i]) == pts[i]`), on `[u[0], u[n - 1]]`.
///
/// A parameter-space curve built this way shares its edge's parameter,
/// which STEP readers expect: OCCT evaluates a pcurve at the ends of the
/// edge's 3D parameter range and drops it if they miss the vertices
/// (`XSAlgo_AlgoContainer::CheckPCurve`), then projects its own.
pub(crate) fn interpolate_at<const D: usize>(pts: &[[f64; D]], u: &[f64]) -> BSpline<D> {
    let n = pts.len();
    debug_assert!(n >= 2 && u.len() == n);
    if n == 2 {
        return BSpline {
            degree: 1,
            control: vec![pts[0], pts[1]],
            knots: vec![u[0], u[0], u[1], u[1]],
        };
    }
    if n == 3 {
        // The quadratic Bézier through the middle point at its chord
        // parameter.
        let t = ((u[1] - u[0]) / (u[2] - u[0])).clamp(1e-6, 1.0 - 1e-6);
        let mut c = [0.0; D];
        for d in 0..D {
            c[d] = (pts[1][d] - (1.0 - t) * (1.0 - t) * pts[0][d] - t * t * pts[2][d])
                / (2.0 * t * (1.0 - t));
        }
        return BSpline {
            degree: 2,
            control: vec![pts[0], c, pts[2]],
            knots: vec![u[0], u[0], u[0], u[2], u[2], u[2]],
        };
    }
    let p = 3;
    let mut knots = vec![u[0]; p + 1];
    for j in 1..n - p {
        knots.push((j..j + p).map(|i| u[i]).sum::<f64>() / p as f64);
    }
    knots.extend([u[n - 1]; 4]);
    // The collocation matrix is banded (at most p + 1 non-zeros per row,
    // within W of the diagonal) and totally positive, so elimination
    // without pivoting is stable.
    const W: usize = 3;
    let band = |i: usize, j: usize| j + W - i;
    let mut a = vec![[0.0f64; 2 * W + 1]; n];
    let shell = BSpline::<D> {
        degree: 3,
        control: vec![[0.0; D]; n],
        knots: knots.clone(),
    };
    for i in 0..n {
        let s = find_span(&shell, u[i]);
        let mut nb = [0.0; 8];
        basis_funs(&knots, s, u[i], p, &mut nb);
        for (j, v) in nb.iter().enumerate().take(p + 1) {
            let col = s - p + j;
            if col + W >= i && col <= i + W {
                a[i][band(i, col)] = *v;
            }
        }
    }
    let mut b: Vec<[f64; D]> = pts.to_vec();
    for c in 0..n {
        let piv = a[c][band(c, c)];
        for r in c + 1..(c + W + 1).min(n) {
            let f = a[r][band(r, c)] / piv;
            if f != 0.0 {
                for k in c..(c + W + 1).min(n) {
                    a[r][band(r, k)] -= f * a[c][band(c, k)];
                }
                for d in 0..D {
                    b[r][d] -= f * b[c][d];
                }
            }
        }
    }
    let mut x = vec![[0.0f64; D]; n];
    for r in (0..n).rev() {
        for d in 0..D {
            let s: f64 = (r + 1..(r + W + 1).min(n))
                .map(|k| a[r][band(r, k)] * x[k][d])
                .sum();
            x[r][d] = (b[r][d] - s) / a[r][band(r, r)];
        }
    }
    // The ends are the given points exactly (the first and last rows are
    // unit rows), but write them so rounding cannot move them.
    x[0] = pts[0];
    x[n - 1] = pts[n - 1];
    BSpline {
        degree: 3,
        control: x,
        knots,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_passes_through_its_points() {
        let pts: Vec<[f64; 2]> = (0..9)
            .map(|i| {
                let t = i as f64 * 0.4;
                [t, (t * 1.3).sin()]
            })
            .collect();
        let b = interpolate(&pts);
        let total: f64 = pts.windows(2).map(|w| dist(&w[0], &w[1])).sum();
        let mut u = 0.0;
        for i in 0..pts.len() {
            if i > 0 {
                u += dist(&pts[i - 1], &pts[i]) / total;
            }
            let q = eval(&b, u.min(1.0));
            assert!(dist(&q, &pts[i]) < 1e-12, "{i}: {q:?} vs {:?}", pts[i]);
        }
    }

    #[test]
    fn derivative_matches_differences() {
        let pts: Vec<[f64; 3]> = (0..7)
            .map(|i| [i as f64, (i * i) as f64 * 0.1, 1.0])
            .collect();
        let b = interpolate(&pts);
        let d = derivative(&b);
        for k in 1..20 {
            let t = k as f64 / 20.0;
            let h = 1e-6;
            let (a, c) = (eval(&b, t - h), eval(&b, t + h));
            let q = eval(&d, t);
            for i in 0..3 {
                assert!(((c[i] - a[i]) / (2.0 * h) - q[i]).abs() < 1e-5);
            }
        }
    }
}
