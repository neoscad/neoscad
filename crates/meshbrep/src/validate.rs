//! A structural and geometric validator for [`Brep`], so that tests (and
//! callers) can tell a valid solid from a broken one without a CAD kernel.
//!
//! Validity alone is not enough: the audit found a file that read back as
//! a valid solid with the wrong volume. Callers should also compare
//! [`crate::measure`] with an independent volume (the mesh's).

use crate::bspline;
use crate::curve::{self, CurveEval};
use crate::math::*;
use crate::measure::{face_param, shell_volumes};
use crate::model::Brep;

/// What [`validate`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Validation {
    /// Every problem found, in words. Empty for a valid B-rep.
    pub errors: Vec<String>,
    /// The genus from the Euler–Poincaré formula
    /// `V − E + 2F − L = 2 (S − G)`, if that gives a whole number.
    pub genus: Option<i64>,
    /// The signed volume of each shell (negative for voids), when it could
    /// be integrated.
    pub shell_volumes: Vec<f64>,
}

impl Validation {
    /// Whether no problem was found.
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Checks `brep`:
///
/// - every index is in range;
/// - every loop is closed (each coedge ends where the next starts);
/// - every edge is used by exactly two coedges, in opposite directions
///   (orientation consistency), by two faces, or twice by one face if it
///   is a seam;
/// - every face is in exactly one shell, and shells are edge-connected;
/// - the Euler–Poincaré formula gives a whole, non-negative genus, equal to
///   the input mesh's when the report has it;
/// - geometry, within `tolerance` (model units): curve ends at their
///   vertices, edges on both their faces' surfaces, parameter-space curves
///   present on curved faces with ends at the edge's vertices;
/// - every shell encloses a positive volume, or a negative one if it is a
///   void.
pub fn validate(brep: &Brep, tolerance: f64) -> Validation {
    let mut out = Validation::default();
    let err = |out: &mut Validation, s: String| {
        if out.errors.len() < 100 {
            out.errors.push(s);
        }
    };
    let (nv, ne, nf) = (brep.vertices.len(), brep.edges.len(), brep.faces.len());
    for (i, e) in brep.edges.iter().enumerate() {
        if e.start as usize >= nv || e.end as usize >= nv {
            err(&mut out, format!("edge {i}: vertex out of range"));
        }
        if e.range[1] <= e.range[0] {
            err(&mut out, format!("edge {i}: empty parameter range"));
        }
    }
    let mut uses: Vec<Vec<(usize, bool)>> = vec![Vec::new(); ne];
    let mut nloops = 0i64;
    for (fi, f) in brep.faces.iter().enumerate() {
        if f.loops.is_empty() {
            err(&mut out, format!("face {fi}: no loops"));
        }
        if f.loops.iter().filter(|l| l.outer).count() > 1 {
            err(&mut out, format!("face {fi}: more than one outer loop"));
        }
        for (li, lp) in f.loops.iter().enumerate() {
            nloops += 1;
            if lp.coedges.is_empty() {
                err(&mut out, format!("face {fi} loop {li}: empty"));
                continue;
            }
            for c in &lp.coedges {
                if c.edge as usize >= ne {
                    err(&mut out, format!("face {fi} loop {li}: edge out of range"));
                }
            }
            if out.errors.iter().any(|e| e.contains("out of range")) {
                continue;
            }
            let ends = |c: &crate::model::Coedge| {
                let e = &brep.edges[c.edge as usize];
                if c.forward {
                    (e.start, e.end)
                } else {
                    (e.end, e.start)
                }
            };
            for k in 0..lp.coedges.len() {
                let a = ends(&lp.coedges[k]).1;
                let b = ends(&lp.coedges[(k + 1) % lp.coedges.len()]).0;
                if a != b {
                    err(
                        &mut out,
                        format!("face {fi} loop {li}: open after coedge {k}"),
                    );
                }
                let c = &lp.coedges[k];
                uses[c.edge as usize].push((fi, c.forward));
            }
        }
    }
    if out.errors.iter().any(|e| e.contains("out of range")) {
        return out;
    }
    for (i, u) in uses.iter().enumerate() {
        let seam = brep.edges[i].seam;
        if u.len() != 2 {
            err(
                &mut out,
                format!("edge {i}: used {} times, not twice", u.len()),
            );
        } else if u[0].1 == u[1].1 {
            err(&mut out, format!("edge {i}: both uses run the same way"));
        } else if (u[0].0 == u[1].0) != seam {
            err(
                &mut out,
                format!(
                    "edge {i}: {}",
                    if seam {
                        "a seam used by two faces"
                    } else {
                        "used twice by one face but not a seam"
                    }
                ),
            );
        }
    }
    // Shells.
    let mut shell_of = vec![usize::MAX; nf];
    for (si, sh) in brep.shells.iter().enumerate() {
        for &f in &sh.faces {
            if f as usize >= nf {
                err(&mut out, format!("shell {si}: face out of range"));
            } else if shell_of[f as usize] != usize::MAX {
                err(&mut out, format!("face {f}: in two shells"));
            } else {
                shell_of[f as usize] = si;
            }
        }
    }
    if let Some(f) = shell_of.iter().position(|&s| s == usize::MAX) {
        err(&mut out, format!("face {f}: in no shell"));
    }
    for (i, u) in uses.iter().enumerate() {
        if u.len() == 2 && shell_of[u[0].0] != shell_of[u[1].0] {
            err(&mut out, format!("edge {i}: joins two shells"));
        }
    }
    let mut uf = UnionFind::new(nf);
    for u in &uses {
        if u.len() == 2 {
            uf.join(u[0].0, u[1].0);
        }
    }
    for (si, sh) in brep.shells.iter().enumerate() {
        if let Some(&f0) = sh.faces.first() {
            let r = uf.find(f0 as usize);
            if sh.faces.iter().any(|&f| uf.find(f as usize) != r) {
                err(&mut out, format!("shell {si}: not connected"));
            }
        }
    }
    // Euler–Poincaré.
    let mut used_v = vec![false; nv];
    for e in &brep.edges {
        used_v[e.start as usize] = true;
        used_v[e.end as usize] = true;
    }
    let v_count = used_v.iter().filter(|&&u| u).count() as i64;
    let chi = v_count - ne as i64 + 2 * nf as i64 - nloops;
    if chi % 2 != 0 {
        err(
            &mut out,
            format!("Euler–Poincaré: V − E + 2F − L = {chi} is odd"),
        );
    } else {
        let g = brep.shells.len() as i64 - chi / 2;
        out.genus = Some(g);
        if g < 0 {
            err(&mut out, format!("Euler–Poincaré: genus {g} is negative"));
        }
        let r = &brep.report;
        if r.mesh_components == brep.shells.len() && r.mesh_components > 0 && g != r.mesh_genus {
            err(
                &mut out,
                format!("genus {g} differs from the input mesh's {}", r.mesh_genus),
            );
        }
    }
    // Geometry.
    for (i, e) in brep.edges.iter().enumerate() {
        if e.start != e.end {
            let len: f64 = curve::sample(&e.curve, e.range, 8)
                .windows(2)
                .map(|w| (w[1] - w[0]).len())
                .sum();
            if len < tolerance {
                err(&mut out, format!("edge {i}: degenerate (length {len:.2e})"));
            }
        }
        let (a, b) = (
            curve::eval(&e.curve, e.range[0]),
            curve::eval(&e.curve, e.range[1]),
        );
        let (pa, pb) = (
            V::from(brep.vertices[e.start as usize]),
            V::from(brep.vertices[e.end as usize]),
        );
        let d = (a - pa).len().max((b - pb).len());
        if d > tolerance {
            err(
                &mut out,
                format!("edge {i}: curve ends {d:.2e} from its vertices"),
            );
        }
    }
    for (fi, f) in brep.faces.iter().enumerate() {
        let Some(p) = face_param(f) else {
            err(
                &mut out,
                format!("face {fi}: unsupported surface {}", f.surface.kind()),
            );
            continue;
        };
        for lp in &f.loops {
            for c in &lp.coedges {
                let e = &brep.edges[c.edge as usize];
                let n = curve::sample_count(&e.curve, e.range).min(256);
                let worst = curve::sample(&e.curve, e.range, n)
                    .iter()
                    .map(|&q| p.s.f(q).abs())
                    .fold(0.0, f64::max);
                if worst > tolerance {
                    err(
                        &mut out,
                        format!("face {fi}: edge {} is {worst:.2e} off the surface", c.edge),
                    );
                }
                if !p.periodic() {
                    continue;
                }
                let Some(pc) = &c.pcurve else {
                    err(
                        &mut out,
                        format!("face {fi}: edge {} has no parameter-space curve", c.edge),
                    );
                    continue;
                };
                let [k0, k1] = [pc.knots[0], *pc.knots.last().unwrap_or(&0.0)];
                let (q0, q1) = (bspline::eval(pc, k0), bspline::eval(pc, k1));
                let (s0, s1) = (p.eval(q0[0], q0[1]), p.eval(q1[0], q1[1]));
                let ev = CurveEval::new(&e.curve);
                let (c0, c1) = (ev.at(e.range[0]), ev.at(e.range[1]));
                let d = (s0 - c0).len().max((s1 - c1).len());
                if d > tolerance {
                    err(
                        &mut out,
                        format!(
                            "face {fi}: edge {}'s parameter-space curve ends {d:.2e} from the edge",
                            c.edge
                        ),
                    );
                }
            }
        }
    }
    if out.errors.is_empty() {
        for fi in 0..nf {
            if let Some(e) = face_crossing(brep, fi) {
                err(&mut out, e);
            }
        }
    }
    if out.errors.is_empty() {
        match shell_volumes(brep) {
            Ok(vols) => {
                for (si, (&v, sh)) in vols.iter().zip(&brep.shells).enumerate() {
                    if (v > 0.0) == sh.void || v == 0.0 {
                        err(
                            &mut out,
                            format!(
                                "shell {si}: encloses volume {v:.6}, {}",
                                if sh.void {
                                    "but is a void"
                                } else {
                                    "inside out"
                                }
                            ),
                        );
                    }
                }
                out.shell_volumes = vols;
            }
            Err(e) => err(&mut out, format!("volume: {e}")),
        }
    }
    out
}

/// A face's loops as polylines in its parameter plane (plane coordinates
/// for planes, the parameter-space curves otherwise), `k` times the usual
/// sampling. `None` if a curved face lacks parameter-space curves.
fn face_polylines(b: &Brep, fi: usize, k: usize) -> Option<Vec<Vec<[f64; 2]>>> {
    let f = &b.faces[fi];
    let p = face_param(f)?;
    let mut out = Vec::with_capacity(f.loops.len());
    for lp in &f.loops {
        let mut pts: Vec<[f64; 2]> = Vec::new();
        for c in &lp.coedges {
            let e = &b.edges[c.edge as usize];
            let mut s: Vec<[f64; 2]> = if p.periodic() {
                let pc = c.pcurve.as_ref()?;
                let (t0, t1) = (pc.knots[0], *pc.knots.last()?);
                let n = k * (4 * bspline::spans(pc).len()).clamp(2, 512);
                (0..=n)
                    .map(|i| bspline::eval(pc, t0 + (t1 - t0) * i as f64 / n as f64))
                    .collect()
            } else {
                let n = match e.curve {
                    crate::model::Curve::Line { .. } => 1,
                    _ => k * curve::sample_count(&e.curve, e.range),
                };
                curve::sample(&e.curve, e.range, n)
                    .into_iter()
                    .map(|q| {
                        let d = q - p.o;
                        [d.dot(p.x), d.dot(p.y)]
                    })
                    .collect()
            };
            if !c.forward {
                s.reverse();
            }
            if !pts.is_empty() {
                s.remove(0);
            }
            pts.extend(s);
        }
        out.push(pts);
    }
    Some(out)
}

/// Where a face's boundary crosses itself, in its parameter plane: a face
/// folded over, which is how a mesh whose topology differs from the exact
/// model's shows up once its corners sit at their exact positions (a sliver
/// the exact geometry would have removed). Loops may touch (a pinch at a
/// shared vertex), but not cross.
pub(crate) fn face_crossing(b: &Brep, fi: usize) -> Option<String> {
    // Chords of nearly tangent arcs can cross where the arcs do not, so a
    // crossing found at the usual sampling is confirmed at a finer one.
    let found = |k: usize| -> Option<String> {
        let lines = face_polylines(b, fi, k)?;
        crossing_in(&lines).map(|(a, bb)| {
            format!(
                "face {fi}: boundary crosses itself near ({:.4}, {:.4}) and ({:.4}, {:.4})",
                a[0], a[1], bb[0], bb[1]
            )
        })
    };
    found(1)?;
    found(8)
}

fn crossing_in(lines: &[Vec<[f64; 2]>]) -> Option<([f64; 2], [f64; 2])> {
    // Segments (start, end, loop, index), closed per loop.
    let mut segs: Vec<([f64; 2], [f64; 2], usize, usize)> = Vec::new();
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for (li, l) in lines.iter().enumerate() {
        let n = l.len();
        for i in 0..n {
            let (a, c) = (l[i], l[(i + 1) % n]);
            for d in 0..2 {
                lo[d] = lo[d].min(a[d]);
                hi[d] = hi[d].max(a[d]);
            }
            segs.push((a, c, li, i));
        }
    }
    let ext = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1e-300);
    let eps = 1e-9 * ext;
    let len_of = |li: usize| lines[li].len();
    segs.sort_by(|a, b| a.0[0].min(a.1[0]).total_cmp(&b.0[0].min(b.1[0])));
    let orient = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    };
    for i in 0..segs.len() {
        let (p1, p2, li, ii) = segs[i];
        let maxx = p1[0].max(p2[0]);
        for &(q1, q2, lj, jj) in &segs[i + 1..] {
            if q1[0].min(q2[0]) > maxx + eps {
                break;
            }
            if li == lj {
                let n = len_of(li);
                if (ii + 1) % n == jj || (jj + 1) % n == ii || ii == jj {
                    continue;
                }
            }
            let lp = ((p2[0] - p1[0]).powi(2) + (p2[1] - p1[1]).powi(2)).sqrt();
            let lq = ((q2[0] - q1[0]).powi(2) + (q2[1] - q1[1]).powi(2)).sqrt();
            let (o1, o2) = (orient(p1, p2, q1), orient(p1, p2, q2));
            let (o3, o4) = (orient(q1, q2, p1), orient(q1, q2, p2));
            let (ep, eq) = (eps * lp, eps * lq);
            if ((o1 > ep && o2 < -ep) || (o1 < -ep && o2 > ep))
                && ((o3 > eq && o4 < -eq) || (o3 < -eq && o4 > eq))
            {
                return Some((p1, q1));
            }
        }
    }
    None
}
