//! Volume and area of a B-rep, integrated on its exact surfaces.
//!
//! By the divergence theorem the volume is ∑ ∬ (σ · n) / 3 over the faces,
//! and by Green's theorem each face's double integral over its region R in
//! (u, v) is the loop integral ∮ G dv over its boundary, with
//! G(u, v) = ∫₀ᵘ F(s, v) ds. Both integrals are Gauss–Legendre on the
//! exact surfaces and the edges' exact curves (planes) or parameter-space
//! curves (curved faces), so the result is limited by quadrature, not by
//! any tessellation: it is exact to about 1e-12 relative. It is the
//! reference the mesh's volume is cross-checked against, and it also
//! checks the parameter-space curves, since a wrong one changes the
//! result.

use crate::Error;
use crate::bspline;
use crate::curve::CurveEval;
use crate::math::*;
use crate::model::{BSpline, Brep, Face};
use crate::surf::{Param, Surf};

/// Volume and area of a B-rep.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Measure {
    /// The enclosed volume (voids subtracted).
    pub volume: f64,
    /// The total surface area.
    pub area: f64,
}

pub(crate) fn face_param(f: &Face) -> Option<Param> {
    let s = Surf::from_public(&f.surface)?;
    Some(Param::new(
        s,
        V::from(f.frame.origin),
        V::from(f.frame.z),
        V::from(f.frame.x),
        f.ref_radius,
    ))
}

/// ∫₀ᵘ F(s, v) ds for the volume and area integrands.
///
/// On a plane, cylinder, cone, sphere or torus the volume integrand
/// σ · (σ_u × σ_v) is `A(v) + B(v) cos u + C(v) sin u` (the frame origin's
/// offset enters only through `cos u` and `sin u`), and the area integrand
/// |σ_u × σ_v| does not depend on `u`. So three evaluations give the
/// integral in closed form, exactly.
fn inner(p: &Param, u: f64, w: f64) -> (f64, f64) {
    let f = |s: f64| {
        let (su, sv) = p.derivs(s, w);
        let n = su.cross(sv);
        (p.eval(s, w).dot(n) / 3.0, n.len())
    };
    let (f0, j) = f(0.0);
    let (fpi, _) = f(PI);
    let (fh, _) = f(PI / 2.0);
    let a = 0.5 * (f0 + fpi);
    let b = 0.5 * (f0 - fpi);
    let c = fh - a;
    (a * u + b * sin(u) + c * (1.0 - cos(u)), j * u)
}

/// The same integral by quadrature, for checking the closed form.
#[cfg(test)]
fn inner_quadrature(p: &Param, u: f64, w: f64) -> (f64, f64) {
    let panels = ((u.abs() / (PI / 4.0)).ceil() as usize).clamp(1, 64);
    let h = u / panels as f64;
    let (mut vol, mut area) = (0.0, 0.0);
    for k in 0..panels {
        let a = k as f64 * h;
        for &(x, wt) in &GL8 {
            let s = a + 0.5 * h * (x + 1.0);
            let (su, sv) = p.derivs(s, w);
            let n = su.cross(sv);
            vol += 0.5 * h * wt * p.eval(s, w).dot(n) / 3.0;
            area += 0.5 * h * wt * n.len();
        }
    }
    (vol, area)
}

/// ∮ G dv along a parametrised boundary piece: `uvd(t)` gives (u, v, dv/dt).
fn along(
    p: &Param,
    t0: f64,
    t1: f64,
    pieces: usize,
    uvd: &dyn Fn(f64) -> (f64, f64, f64),
) -> (f64, f64) {
    let (mut vol, mut area) = (0.0, 0.0);
    let h = (t1 - t0) / pieces as f64;
    for k in 0..pieces {
        let a = t0 + k as f64 * h;
        for &(x, wt) in &GL8 {
            let t = a + 0.5 * h * (x + 1.0);
            let (u, w, dw) = uvd(t);
            if dw == 0.0 {
                continue;
            }
            let (gv, ga) = inner(p, u, w);
            vol += 0.5 * h * wt * gv * dw;
            area += 0.5 * h * wt * ga * dw;
        }
    }
    (vol, area)
}

fn pcurve_integral(p: &Param, pc: &BSpline<2>) -> (f64, f64) {
    let d = bspline::derivative(pc);
    let (mut vol, mut area) = (0.0, 0.0);
    for (a, b) in bspline::spans(pc) {
        let q0 = bspline::eval(pc, a);
        let q1 = bspline::eval(pc, b);
        let len = ((q1[0] - q0[0]).powi(2) + (q1[1] - q0[1]).powi(2)).sqrt();
        let pieces = ((len / 0.5).ceil() as usize).clamp(1, 64);
        let (v, ar) = along(p, a, b, pieces, &|t| {
            let q = bspline::eval(pc, t);
            let dq = bspline::eval(&d, t);
            (q[0], q[1], dq[1])
        });
        vol += v;
        area += ar;
    }
    (vol, area)
}

/// The volume and area contributions of one face.
pub(crate) fn face_integrals(b: &Brep, fi: usize) -> Result<(f64, f64), Error> {
    let f = &b.faces[fi];
    let p = face_param(f).ok_or(Error::Unsupported(f.surface.kind()))?;
    let s = if f.same_sense { 1.0 } else { -1.0 };
    let (mut vol, mut area) = (0.0, 0.0);
    for lp in &f.loops {
        for c in &lp.coedges {
            let e = &b.edges[c.edge as usize];
            let sign = if c.forward { 1.0 } else { -1.0 };
            let (v, a) =
                if let (Surf::Plane { .. }, crate::model::Curve::Line { .. }) = (p.s, &e.curve) {
                    // A straight edge on a plane: the integrand is constant
                    // there (σ · n = o · n), so the loop integral is the
                    // trapezoid's, exactly. Faceted models are thousands of
                    // these, and quadrature cost a third of their export.
                    let ev = CurveEval::new(&e.curve);
                    let uv = |t: f64| {
                        let q = ev.at(t) - p.o;
                        (q.dot(p.x), q.dot(p.y))
                    };
                    let ((u0, w0), (u1, w1)) = (uv(e.range[0]), uv(e.range[1]));
                    let g = 0.5 * (u0 + u1) * (w1 - w0);
                    (p.o.dot(p.x.cross(p.y)) / 3.0 * g, g)
                } else if p.periodic() {
                    let pc = c.pcurve.as_ref().ok_or_else(|| {
                        Error::Reconstruction(format!(
                            "face {fi}: an edge has no parameter-space curve"
                        ))
                    })?;
                    pcurve_integral(&p, pc)
                } else {
                    let ev = CurveEval::new(&e.curve);
                    let [t0, t1] = e.range;
                    let pieces = match &e.curve {
                        crate::model::Curve::Line { .. } => 1,
                        crate::model::Curve::BSpline(bs) => bspline::spans(bs).len().max(1),
                        _ => (((t1 - t0).abs() / 0.5).ceil() as usize).clamp(1, 64),
                    };
                    // B-spline pieces follow its knot spans.
                    if let crate::model::Curve::BSpline(bs) = &e.curve {
                        let mut acc = (0.0, 0.0);
                        for (a, bb) in bspline::spans(bs) {
                            let (a, bb) = (a.max(t0), bb.min(t1));
                            if bb <= a {
                                continue;
                            }
                            let r = along(&p, a, bb, 1, &|t| {
                                let q = ev.at(t) - p.o;
                                (q.dot(p.x), q.dot(p.y), ev.deriv(t).dot(p.y))
                            });
                            acc.0 += r.0;
                            acc.1 += r.1;
                        }
                        acc
                    } else {
                        along(&p, t0, t1, pieces, &|t| {
                            let q = ev.at(t) - p.o;
                            (q.dot(p.x), q.dot(p.y), ev.deriv(t).dot(p.y))
                        })
                    }
                };
            vol += sign * v;
            area += sign * a;
        }
    }
    Ok((vol, s * area))
}

/// The volume and area of `brep`, integrated on its exact geometry.
///
/// Fails if a curved face lacks parameter-space curves (they bound the
/// region integrated) or uses a surface kind not supported yet.
pub fn measure(brep: &Brep) -> Result<Measure, Error> {
    let mut m = Measure::default();
    for fi in 0..brep.faces.len() {
        let (v, a) = face_integrals(brep, fi)?;
        m.volume += v;
        m.area += a;
    }
    Ok(m)
}

/// The signed volume enclosed by each shell (negative for a void).
pub(crate) fn shell_volumes(brep: &Brep) -> Result<Vec<f64>, Error> {
    let mut out = Vec::with_capacity(brep.shells.len());
    for sh in &brep.shells {
        let mut v = 0.0;
        for &f in &sh.faces {
            v += face_integrals(brep, f as usize)?.0;
        }
        out.push(v);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_form_inner_integral_matches_quadrature() {
        let o = v(1.5, -2.0, 0.7);
        let z = v(0.3, 0.4, 0.866).norm();
        let x = z.perp();
        let surfs = [
            Surf::Plane { o, n: z },
            Surf::Cyl { o, a: z, r: 2.5 },
            Surf::Cone {
                apex: o,
                a: z,
                k: 0.7,
            },
            Surf::Sphere { c: o, r: 3.0 },
            // A torus's integrand has the same form in u: its tube angle
            // v enters only through A, B and C.
            Surf::Torus {
                c: o,
                a: z,
                big: 4.0,
                r: 1.5,
            },
        ];
        for s in surfs {
            let p = Param::new(s, o + v(0.2, 0.1, -0.3), z, x, 1.2);
            for &(u, w) in &[(0.3, 0.2), (2.0, -0.5), (6.2, 1.1), (-1.0, 0.4)] {
                let (a, b) = (inner(&p, u, w), inner_quadrature(&p, u, w));
                assert!(
                    (a.0 - b.0).abs() < 1e-9 * (1.0 + b.0.abs()),
                    "{s:?} vol {a:?} {b:?}"
                );
                assert!(
                    (a.1 - b.1).abs() < 1e-9 * (1.0 + b.1.abs()),
                    "{s:?} area {a:?} {b:?}"
                );
            }
        }
    }
}
