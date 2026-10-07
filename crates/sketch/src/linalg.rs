//! Dense Householder QR, for the two things the solver needs: damped
//! least-squares steps, and the rank analysis behind the degree-of-freedom
//! and dependency reports.
//!
//! Sketches have tens to a few hundred unknowns, where dense factorisation
//! is cheap and simple. Every loop runs in a fixed order with plain
//! `+ - * / sqrt`, so the results do not depend on the CPU.

/// A column-major matrix.
#[derive(Clone, Debug)]
pub(crate) struct Mat {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f64>,
}

impl Mat {
    pub fn zeros(rows: usize, cols: usize) -> Mat {
        Mat {
            rows,
            cols,
            data: vec![0.0; rows * cols],
        }
    }

    pub fn add(&mut self, r: usize, c: usize, v: f64) {
        self.data[c * self.rows + r] += v;
    }
}

fn sum_squares(v: &[f64]) -> f64 {
    let mut s = 0.0;
    for x in v {
        s += x * x;
    }
    s
}

/// The step δ minimising |Jδ + r|² + μ|δ|², by QR of J stacked on √μ·I.
///
/// The damping keeps the step short when J is singular or the model is
/// poor, and leaves directions J cannot see (free degrees of freedom)
/// alone: their component of δ is zero, which is what keeps an
/// under-constrained point where it was drawn.
pub(crate) fn damped_step(j: &Mat, r: &[f64], mu: f64) -> Vec<f64> {
    let (m, n) = (j.rows, j.cols);
    let big = m + n;
    let mut a = vec![0.0; big * n];
    let damp = mu.sqrt();
    for c in 0..n {
        a[c * big..c * big + m].copy_from_slice(&j.data[c * m..(c + 1) * m]);
        a[c * big + m + c] = damp;
    }
    let mut b = vec![0.0; big];
    for i in 0..m {
        b[i] = -r[i];
    }
    let mut diag = vec![0.0; n];
    let mut v = vec![0.0; big];
    for k in 0..n {
        let col = &a[k * big + k..(k + 1) * big];
        let norm = sum_squares(col).sqrt();
        if norm == 0.0 {
            continue;
        }
        let alpha = if col[0] >= 0.0 { -norm } else { norm };
        let len = big - k;
        v[..len].copy_from_slice(col);
        v[0] -= alpha;
        let vtv = sum_squares(&v[..len]);
        diag[k] = alpha;
        if vtv == 0.0 {
            continue;
        }
        for c in k + 1..n {
            let col = &mut a[c * big + k..(c + 1) * big];
            let mut s = 0.0;
            for i in 0..len {
                s += v[i] * col[i];
            }
            let f = 2.0 * s / vtv;
            for i in 0..len {
                col[i] -= f * v[i];
            }
        }
        let mut s = 0.0;
        for i in 0..len {
            s += v[i] * b[k + i];
        }
        let f = 2.0 * s / vtv;
        for i in 0..len {
            b[k + i] -= f * v[i];
        }
    }
    let mut x = vec![0.0; n];
    for k in (0..n).rev() {
        if diag[k] == 0.0 {
            continue;
        }
        let mut s = b[k];
        for c in k + 1..n {
            s -= a[c * big + k] * x[c];
        }
        x[k] = s / diag[k];
    }
    x
}

/// A Householder reflector I − 2vvᵀ/vᵀv acting on entries `at..`.
#[derive(Clone, Debug)]
struct Reflector {
    at: usize,
    v: Vec<f64>,
    vtv: f64,
}

impl Reflector {
    fn apply(&self, w: &mut [f64]) {
        let w = &mut w[self.at..];
        let mut s = 0.0;
        for (vi, wi) in self.v.iter().zip(w.iter()) {
            s += vi * wi;
        }
        let f = 2.0 * s / self.vtv;
        for (vi, wi) in self.v.iter().zip(w.iter_mut()) {
            *wi -= f * vi;
        }
    }
}

/// The rank structure of a set of equations, found by QR of Jᵀ taken one
/// equation at a time in order, without pivoting.
///
/// Taking the equations in order, rather than by size as pivoted QR would,
/// means a dependent equation is always reported against earlier ones:
/// "the second `horizontal` is implied", never the first. The rows must be
/// normalised to unit length, so the tolerance is relative.
#[derive(Clone, Debug, Default)]
pub(crate) struct RankAnalysis {
    /// The number of independent equations.
    pub rank: usize,
    /// The independent equations, in order.
    pub independent: Vec<usize>,
    /// Each dependent equation, with its coefficients over the independent
    /// ones (by position in `independent`): row ≈ Σ c·row_i.
    pub dependent: Vec<(usize, Vec<(usize, f64)>)>,
    /// For each unknown, the length of the projection of its unit vector
    /// onto the null space of J: 0 if the equations pin it, up to 1 if it
    /// is free.
    pub mobility: Vec<f64>,
}

pub(crate) fn rank_analysis(rows: &[Vec<f64>], n: usize, tol: f64) -> RankAnalysis {
    let mut out = RankAnalysis::default();
    let mut refl: Vec<Reflector> = Vec::new();
    // R's columns: column j holds entries 0..=j, fixed once written
    // (later reflectors only touch entries after j).
    let mut r_cols: Vec<Vec<f64>> = Vec::new();
    for (k, row) in rows.iter().enumerate() {
        let mut w = row.clone();
        for h in &refl {
            h.apply(&mut w);
        }
        let r = refl.len();
        let rem = sum_squares(&w[r..]).sqrt();
        if rem <= tol || r == n {
            // Back-substitute R z = w[..r] for the coefficients.
            let mut z = vec![0.0; r];
            for i in (0..r).rev() {
                let mut s = w[i];
                for (j, zj) in z.iter().enumerate().skip(i + 1) {
                    s -= r_cols[j][i] * zj;
                }
                z[i] = s / r_cols[i][i];
            }
            let coef = z
                .into_iter()
                .enumerate()
                .filter(|(_, c)| c.abs() > 1e-9)
                .collect();
            out.dependent.push((k, coef));
            continue;
        }
        let alpha = if w[r] >= 0.0 { -rem } else { rem };
        let mut v = w[r..].to_vec();
        v[0] -= alpha;
        let vtv = sum_squares(&v);
        let mut col = w[..r].to_vec();
        col.push(alpha);
        r_cols.push(col);
        refl.push(Reflector { at: r, v, vtv });
        out.independent.push(k);
    }
    out.rank = refl.len();
    let mut e = vec![0.0; n];
    for i in 0..n {
        e.iter_mut().for_each(|v| *v = 0.0);
        e[i] = 1.0;
        for h in &refl {
            h.apply(&mut e);
        }
        out.mobility.push(sum_squares(&e[out.rank..]).sqrt());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damped_step_solves_a_square_system() {
        // [2 1; 1 3] δ = −r.
        let mut j = Mat::zeros(2, 2);
        j.add(0, 0, 2.0);
        j.add(0, 1, 1.0);
        j.add(1, 0, 1.0);
        j.add(1, 1, 3.0);
        let d = damped_step(&j, &[-3.0, -5.0], 0.0);
        assert!(
            (d[0] - 0.8).abs() < 1e-14 && (d[1] - 1.4).abs() < 1e-14,
            "{d:?}"
        );
    }

    #[test]
    fn damped_step_is_minimal_norm_when_underdetermined() {
        // One equation x + y = 2: the minimal-norm step is (1, 1).
        let mut j = Mat::zeros(1, 2);
        j.add(0, 0, 1.0);
        j.add(0, 1, 1.0);
        let d = damped_step(&j, &[-2.0], 1e-14);
        assert!(
            (d[0] - 1.0).abs() < 1e-9 && (d[1] - 1.0).abs() < 1e-9,
            "{d:?}"
        );
    }

    #[test]
    fn rank_analysis_reports_later_rows_as_dependent() {
        let s = 0.5f64.sqrt();
        let rows = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![s, s, 0.0],
            vec![0.0, 0.0, 1.0],
        ];
        let a = rank_analysis(&rows, 3, 1e-10);
        assert_eq!(a.rank, 3);
        assert_eq!(a.independent, vec![0, 1, 3]);
        assert_eq!(a.dependent.len(), 1);
        let (k, coef) = &a.dependent[0];
        assert_eq!(*k, 2);
        assert_eq!(coef.len(), 2);
        assert!(a.mobility.iter().all(|m| *m < 1e-12));

        let a = rank_analysis(&rows[..1], 3, 1e-10);
        assert_eq!(a.rank, 1);
        assert!(a.mobility[0] < 1e-12);
        assert!((a.mobility[1] - 1.0).abs() < 1e-12);
    }
}
