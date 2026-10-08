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
    /// Findings that are not errors, in words: the B-rep has fewer
    /// handles than the input mesh.
    pub notes: Vec<String>,
    /// The faces the errors are on (an edge's error is on the faces that
    /// use it), sorted, each once. With `Report::face_triangles` they say
    /// which input triangles to build some other way. Errors of the whole
    /// B-rep (its genus) name no face.
    pub error_faces: Vec<u32>,
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
/// - the Euler–Poincaré formula gives a whole, non-negative genus, no more
///   than the input mesh's when the report has it (fewer is a note);
/// - no face's boundary crosses itself, or touches itself with a corner
///   inside one of its edges (within `tolerance`);
/// - geometry, within `tolerance` (model units): curve ends at their
///   vertices, edges on both their faces' surfaces, parameter-space curves
///   present on curved faces with ends at the edge's vertices;
/// - every shell encloses a positive volume, or a negative one if it is a
///   void.
pub fn validate(brep: &Brep, tolerance: f64) -> Validation {
    let mut out = validate_unsorted(brep, tolerance);
    out.error_faces.sort_unstable();
    out.error_faces.dedup();
    out
}

fn validate_unsorted(brep: &Brep, tolerance: f64) -> Validation {
    let mut out = Validation::default();
    let (nv, ne, nf) = (brep.vertices.len(), brep.edges.len(), brep.faces.len());
    // The faces using each edge, for saying where an edge's error is.
    let mut edge_faces: Vec<Vec<u32>> = vec![Vec::new(); ne];
    for (fi, f) in brep.faces.iter().enumerate() {
        for c in f.loops.iter().flat_map(|l| &l.coedges) {
            if let Some(v) = edge_faces.get_mut(c.edge as usize) {
                if !v.contains(&(fi as u32)) {
                    v.push(fi as u32);
                }
            }
        }
    }
    let at_face = |f: usize| vec![f as u32];
    let at_edge = |e: usize| edge_faces.get(e).cloned().unwrap_or_default();
    let err = |out: &mut Validation, s: String, faces: Vec<u32>| {
        if out.errors.len() < 100 {
            out.errors.push(s);
        }
        out.error_faces.extend(faces);
    };
    for (i, e) in brep.edges.iter().enumerate() {
        if e.start as usize >= nv || e.end as usize >= nv {
            err(
                &mut out,
                format!("edge {i}: vertex out of range"),
                at_edge(i),
            );
        }
        if e.range[1] <= e.range[0] {
            err(
                &mut out,
                format!("edge {i}: empty parameter range"),
                at_edge(i),
            );
        }
    }
    let mut uses: Vec<Vec<(usize, bool)>> = vec![Vec::new(); ne];
    let mut nloops = 0i64;
    for (fi, f) in brep.faces.iter().enumerate() {
        if f.loops.is_empty() {
            err(&mut out, format!("face {fi}: no loops"), at_face(fi));
        }
        let outer = f.loops.iter().filter(|l| l.outer).count();
        if outer > 1 {
            err(
                &mut out,
                format!("face {fi}: more than one outer loop"),
                at_face(fi),
            );
        } else if outer == 0
            && !f.loops.is_empty()
            && matches!(
                f.surface,
                crate::model::Surface::Plane { .. } | crate::model::Surface::Faceted
            )
        {
            // A bounded planar region has an outer loop. Without one the
            // face's loops all wind the wrong way for its normal: a face
            // folded through a tunnel of no thickness in the mesh (BOSL2
            // `attachments__084`, which OCCT read back as unorientable).
            err(
                &mut out,
                format!("face {fi}: no outer loop (its loops wind against its normal)"),
                at_face(fi),
            );
        }
        for (li, lp) in f.loops.iter().enumerate() {
            nloops += 1;
            if lp.coedges.is_empty() {
                err(&mut out, format!("face {fi} loop {li}: empty"), at_face(fi));
                continue;
            }
            for c in &lp.coedges {
                if c.edge as usize >= ne {
                    err(
                        &mut out,
                        format!("face {fi} loop {li}: edge out of range"),
                        at_face(fi),
                    );
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
            // A closed edge (a whole circle or ellipse) is a loop by
            // itself. Inside a longer loop it is a hole that touches the
            // boundary at a corner: OCCT splits it off as a wire of its
            // own and rejects the face for wires that nest wrongly (BOSL2
            // `threading__048` written partly as facets, where a facet's
            // plane cut a cone's tip off in an ellipse through one of the
            // facet's corners). On a periodic face a closed edge shares a
            // loop with the seam (a cylinder's rims), which is right.
            let planar = matches!(
                f.surface,
                crate::model::Surface::Plane { .. } | crate::model::Surface::Faceted
            );
            if planar && lp.coedges.len() > 1 {
                for c in &lp.coedges {
                    let e = &brep.edges[c.edge as usize];
                    if e.start == e.end && !e.seam {
                        err(
                            &mut out,
                            format!(
                                "face {fi} loop {li}: closed edge {} inside a longer loop (a hole touching the boundary)",
                                c.edge
                            ),
                            at_edge(c.edge as usize),
                        );
                    }
                }
            }
            // Two straight edges make a loop with no area: they overlap
            // rather than cross, so the crossing check below cannot see it.
            if lp.coedges.len() == 2
                && lp.coedges.iter().all(|c| {
                    matches!(
                        brep.edges[c.edge as usize].curve,
                        crate::model::Curve::Line { .. }
                    )
                })
            {
                err(
                    &mut out,
                    format!("face {fi} loop {li}: two straight edges along one line (no area)"),
                    at_face(fi),
                );
            } else if lp.coedges.len() == 2 {
                // Two curves that run along each other within the
                // tolerance make a sliver of no area too, on any surface
                // (a cone between two facets whose planes cut it almost
                // alike: BOSL2 `threading__048` written partly as facets,
                // which OCCT read back as an unorientable face).
                let (a, b) = (
                    &brep.edges[lp.coedges[0].edge as usize],
                    &brep.edges[lp.coedges[1].edge as usize],
                );
                let same_ends =
                    (a.start, a.end) == (b.start, b.end) || (a.start, a.end) == (b.end, b.start);
                // Not a seam, which a lone sphere's or torus's face runs
                // along both ways.
                let distinct = lp.coedges[0].edge != lp.coedges[1].edge && !a.seam && !b.seam;
                if distinct && same_ends && a.start != a.end && run_along(a, b, tolerance) {
                    err(
                        &mut out,
                        format!("face {fi} loop {li}: two edges along one curve (no area)"),
                        at_face(fi),
                    );
                }
            }
            for k in 0..lp.coedges.len() {
                let a = ends(&lp.coedges[k]).1;
                let b = ends(&lp.coedges[(k + 1) % lp.coedges.len()]).0;
                if a != b {
                    err(
                        &mut out,
                        format!("face {fi} loop {li}: open after coedge {k}"),
                        at_face(fi),
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
                at_edge(i),
            );
        } else if u[0].1 == u[1].1 {
            err(
                &mut out,
                format!("edge {i}: both uses run the same way"),
                at_edge(i),
            );
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
                at_edge(i),
            );
        }
    }
    // Shells.
    let mut shell_of = vec![usize::MAX; nf];
    for (si, sh) in brep.shells.iter().enumerate() {
        for &f in &sh.faces {
            if f as usize >= nf {
                err(
                    &mut out,
                    format!("shell {si}: face out of range"),
                    Vec::new(),
                );
            } else if shell_of[f as usize] != usize::MAX {
                err(&mut out, format!("face {f}: in two shells"), vec![f]);
            } else {
                shell_of[f as usize] = si;
            }
        }
    }
    if let Some(f) = shell_of.iter().position(|&s| s == usize::MAX) {
        err(&mut out, format!("face {f}: in no shell"), at_face(f));
    }
    for (i, u) in uses.iter().enumerate() {
        if u.len() == 2 && shell_of[u[0].0] != shell_of[u[1].0] {
            err(&mut out, format!("edge {i}: joins two shells"), at_edge(i));
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
                err(
                    &mut out,
                    format!("shell {si}: not connected"),
                    sh.faces.clone(),
                );
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
            Vec::new(),
        );
    } else {
        let g = brep.shells.len() as i64 - chi / 2;
        out.genus = Some(g);
        if g < 0 {
            err(
                &mut out,
                format!("Euler–Poincaré: genus {g} is negative"),
                Vec::new(),
            );
        }
        // More handles than the mesh means the B-rep joined what the
        // mesh keeps apart (corners merged that should not be): an error.
        // Fewer can be right. Rounding can leave a mesh with a tunnel of
        // no thickness, where a plane meets one that is equal within
        // the merge tolerance but whose triangles straddle it (a cube
        // standing on a rotated prism, BOSL2 `attachments__079`): the
        // two sides of the tunnel are one exact face, so the B-rep has
        // no handle there and is the exact model's. A tunnel with any
        // volume would change the volume, which callers must compare
        // with the mesh's anyway (see the module documentation).
        let r = &brep.report;
        if r.mesh_components == brep.shells.len() && r.mesh_components > 0 {
            if g > r.mesh_genus {
                err(
                    &mut out,
                    format!("genus {g} is more than the input mesh's {}", r.mesh_genus),
                    Vec::new(),
                );
            } else if g < r.mesh_genus {
                out.notes.push(format!(
                    "genus {g} is less than the input mesh's {} (a tunnel of no thickness in the mesh)",
                    r.mesh_genus
                ));
            }
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
                err(
                    &mut out,
                    format!("edge {i}: degenerate (length {len:.2e})"),
                    at_edge(i),
                );
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
                at_edge(i),
            );
        }
    }
    for (fi, f) in brep.faces.iter().enumerate() {
        let Some(p) = face_param(f) else {
            err(
                &mut out,
                format!("face {fi}: unsupported surface {}", f.surface.kind()),
                at_face(fi),
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
                        at_edge(c.edge as usize),
                    );
                }
                if !p.periodic() {
                    continue;
                }
                let Some(pc) = &c.pcurve else {
                    err(
                        &mut out,
                        format!("face {fi}: edge {} has no parameter-space curve", c.edge),
                        at_face(fi),
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
                        at_face(fi),
                    );
                }
            }
        }
    }
    // A loop of a planar face narrower than the tolerance (a needle
    // triangle of the mesh written as a facet, or a slit whose two sides
    // are not both lines) has no trustworthy area or orientation: its
    // sides overlap within the tolerance. OCCT reads such a wire as
    // crossing itself, or a hole as badly oriented (BOSL2 `distributors`
    // and `example017.scad`, written partly as facets, read back invalid
    // with them). Its width is taken as 4 · area / perimeter, a
    // triangle's height on its long side, a slit's half-width.
    for (fi, f) in brep.faces.iter().enumerate() {
        let planar = matches!(
            f.surface,
            crate::model::Surface::Plane { .. } | crate::model::Surface::Faceted
        );
        if !planar {
            continue;
        }
        let Some(lines) = face_polylines(brep, fi, 1) else {
            continue;
        };
        for (li, pts) in lines.iter().enumerate() {
            if pts.len() < 2 {
                continue;
            }
            let (mut area, mut perimeter) = (0.0, 0.0);
            for k in 0..pts.len() {
                let (p, q) = (pts[k], pts[(k + 1) % pts.len()]);
                area += p[0] * q[1] - p[1] * q[0];
                perimeter += ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt();
            }
            let width = 2.0 * area.abs() / perimeter.max(1e-300);
            if width < tolerance {
                err(
                    &mut out,
                    format!(
                        "face {fi} loop {li}: {width:.2e} wide, narrower than the tolerance (no area)"
                    ),
                    at_face(fi),
                );
            }
        }
        // Each hole inside the outer loop and outside the other holes.
        // OCCT rejects a face whose wires nest otherwise ("invalid
        // imbrication of wires"), and no other check here sees it when no
        // two loops cross. None of the corpora's files has had one, but a
        // face rebuilt partly from facets could. A hole may touch the outer
        // loop or another hole, so a corner within the tolerance of the
        // other loop is not counted. Holes are compared only where their
        // boxes meet: a plate with 400 holes would otherwise compare every
        // pair point by point.
        let Some(outer) = f.loops.iter().position(|l| l.outer) else {
            continue;
        };
        let boxes: Vec<[f64; 4]> = lines
            .iter()
            .map(|q| {
                q.iter().fold(
                    [
                        f64::INFINITY,
                        f64::INFINITY,
                        f64::NEG_INFINITY,
                        f64::NEG_INFINITY,
                    ],
                    |b, p| {
                        [
                            b[0].min(p[0]),
                            b[1].min(p[1]),
                            b[2].max(p[0]),
                            b[3].max(p[1]),
                        ]
                    },
                )
            })
            .collect();
        let meet = |i: usize, j: usize| {
            let (a, b) = (boxes[i], boxes[j]);
            a[0] <= b[2] + tolerance
                && b[0] <= a[2] + tolerance
                && a[1] <= b[3] + tolerance
                && b[1] <= a[3] + tolerance
        };
        for (li, pts) in lines.iter().enumerate() {
            if li == outer {
                continue;
            }
            let strictly = |q: &[[f64; 2]], inside: bool| {
                pts.iter()
                    .any(|&p| polyline_distance(q, p) > tolerance && winds(q, p) == inside)
            };
            let other_holes = lines
                .iter()
                .enumerate()
                .any(|(lj, q)| lj != li && lj != outer && meet(li, lj) && strictly(q, true));
            if strictly(&lines[outer], false) || other_holes {
                err(
                    &mut out,
                    format!(
                        "face {fi} loop {li}: a hole not inside its face's outer loop, or inside another hole"
                    ),
                    at_face(fi),
                );
            }
        }
    }
    if out.errors.is_empty() {
        for fi in 0..nf {
            if let Some(e) = face_crossing(brep, fi).or_else(|| face_touch(brep, fi, tolerance)) {
                err(&mut out, e, at_face(fi));
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
                            sh.faces.clone(),
                        );
                    }
                }
                out.shell_volumes = vols;
            }
            Err(e) => err(&mut out, format!("volume: {e}"), Vec::new()),
        }
    }
    out
}

/// Whether edge `b` stays within `tol` of edge `a` everywhere (sampled
/// densely, so the chords' sagitta is far below `tol` on the short
/// curves where this matters).
fn run_along(a: &crate::model::Edge, b: &crate::model::Edge, tol: f64) -> bool {
    let pa = curve::sample(&a.curve, a.range, 512);
    let pb = curve::sample(&b.curve, b.range, 64);
    pb.iter().all(|&p| {
        pa.windows(2)
            .map(|w| {
                let d = w[1] - w[0];
                let l2 = d.dot(d);
                let t = if l2 > 0.0 {
                    ((p - w[0]).dot(d) / l2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                (p - (w[0] + d * t)).len()
            })
            .fold(f64::INFINITY, f64::min)
            < tol
    })
}

/// Whether the closed polyline `q` winds around `p` (crossing parity).
fn winds(q: &[[f64; 2]], p: [f64; 2]) -> bool {
    let mut inside = false;
    for k in 0..q.len() {
        let (a, b) = (q[k], q[(k + 1) % q.len()]);
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if x > p[0] {
                inside = !inside;
            }
        }
    }
    inside
}

/// The distance from `p` to the closed polyline `q`.
fn polyline_distance(q: &[[f64; 2]], p: [f64; 2]) -> f64 {
    let mut best = f64::INFINITY;
    for k in 0..q.len() {
        let (a, b) = (q[k], q[(k + 1) % q.len()]);
        let d = [b[0] - a[0], b[1] - a[1]];
        let l2 = d[0] * d[0] + d[1] * d[1];
        let t = if l2 > 0.0 {
            (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / l2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (x, y) = (a[0] + d[0] * t - p[0], a[1] + d[1] * t - p[1]);
        best = best.min((x * x + y * y).sqrt());
    }
    best
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

/// Where a corner of a face lies inside another edge of the same face,
/// within `tol` in space: a boundary touching itself in the middle of an
/// edge. Corners that coincide (a pinch) are allowed.
///
/// Manifold keeps two bodies that touch along a line apart with duplicated
/// vertices, and after a rotation its rounding can leave the duplicates on
/// different sides of each other, so that the mesh joins faces the exact
/// model only pinches. Once the vertices sit at their exact positions, a
/// corner lies on an edge of its own face. Counted by its topology the
/// B-rep is still a closed 2-manifold, but a reader that joins by position
/// sees a self-intersecting wire: the rotated Menger sponge
/// (`example024.scad`) read back from OCCT as an open shell with 60 free
/// edges because of this. The test is in space, not in the parameter
/// plane, whose `u` (an angle) and `v` (a length) are not comparable.
pub(crate) fn face_touch(b: &Brep, fi: usize, tol: f64) -> Option<String> {
    let f = &b.faces[fi];
    // (vertex, loop) and (edge, loop) pairs.
    let mut verts: Vec<(u32, usize)> = Vec::new();
    let mut edges: Vec<(u32, usize)> = Vec::new();
    for (li, lp) in f.loops.iter().enumerate() {
        for c in &lp.coedges {
            let e = &b.edges[c.edge as usize];
            verts.push((e.start, li));
            verts.push((e.end, li));
            edges.push((c.edge, li));
        }
    }
    verts.sort_unstable();
    verts.dedup();
    edges.sort_unstable();
    edges.dedup();
    if verts.len() < 3 {
        return None;
    }
    let at = |v: u32| V::from(b.vertices[v as usize]);
    // The corners sorted by x, so that each edge looks only at those in
    // its box: faces of thousands of corners are common (text, `$fn`).
    let mut by_x: Vec<(f64, u32, usize)> = verts.iter().map(|&(v, l)| (at(v).x, v, l)).collect();
    by_x.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    for &(ei, el) in &edges {
        let e = &b.edges[ei as usize];
        let n = curve::sample_count(&e.curve, e.range);
        let pts = curve::sample(&e.curve, e.range, n);
        let mut lo = pts[0];
        let mut hi = pts[0];
        for p in &pts {
            lo = v(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
            hi = v(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
        }
        // The samples' box misses the curve between them by at most the
        // sagitta, which a twentieth of the spacing bounds at this
        // sampling; the pad covers that and the tolerance.
        let pad = tol + 0.05 * (hi - lo).len() / n as f64;
        let (s, t) = (at(e.start), at(e.end));
        let first = by_x.partition_point(|&(x, _, _)| x < lo.x - pad);
        for &(x, vi, vl) in &by_x[first..] {
            if x > hi.x + pad {
                break;
            }
            if vi == e.start || vi == e.end {
                continue;
            }
            let p = at(vi);
            if p.y < lo.y - pad
                || p.z < lo.z - pad
                || p.y > hi.y + pad
                || p.z > hi.z + pad
                || (p - s).len() <= tol
                || (p - t).len() <= tol
            {
                continue;
            }
            if distance_to_curve(&e.curve, e.range, &pts, p) <= tol {
                return Some(format!(
                    "face {fi}: boundary touches itself, a corner at ({:.4}, {:.4}, {:.4}) lies on edge {ei} ({})",
                    p.x,
                    p.y,
                    p.z,
                    if vl == el {
                        "same loop"
                    } else {
                        "another loop"
                    }
                ));
            }
        }
    }
    None
}

/// The distance from `p` to a curve sampled as `pts` over `range`: from
/// the nearest sample's neighbourhood, refined by golden-section search.
fn distance_to_curve(c: &crate::model::Curve, range: [f64; 2], pts: &[V], p: V) -> f64 {
    let n = pts.len() - 1;
    let k = (0..=n)
        .min_by(|&i, &j| (pts[i] - p).len().total_cmp(&(pts[j] - p).len()))
        .expect("samples");
    let step = (range[1] - range[0]) / n as f64;
    let (mut a, mut z) = (
        range[0] + step * k.saturating_sub(1) as f64,
        range[0] + step * (k + 1).min(n) as f64,
    );
    let d = |t: f64| (curve::eval(c, t) - p).len();
    let g = 0.5 * (5f64.sqrt() - 1.0);
    for _ in 0..80 {
        let (m1, m2) = (z - g * (z - a), a + g * (z - a));
        if d(m1) < d(m2) {
            z = m2;
        } else {
            a = m1;
        }
    }
    d(0.5 * (a + z)).min((pts[k] - p).len())
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
