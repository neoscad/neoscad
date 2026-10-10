//! A built plan's tools for the renderers, and the check after the
//! boolean (`docs/fillets.md`, section 8): how much of each blend the
//! result kept.
//!
//! The check is geometric, not by the tools' original IDs: it finds the
//! result's triangles lying on each blend's surface. IDs are rebased when
//! a cached result is reused, so a check by ID would have to follow them;
//! the surfaces do not move, so a warm request reports exactly what a
//! cold one does.

use eval::dump::Keys;
use eval::fillet::FilletNode;
use eval::node::{Node, NodeKind};
use lang::diag::{DiagCode, Severity};
use meshbrep::Surface;
use meshbrep::blend::{BlendFace, End, Path, Source, Tool};
use meshbrep::spline::Evaluator;

use super::curve::{V, add, cross, dot, mul, norm, sub, unit};
use super::{Pass, Plan, PlanDiag, Unavailable, build, facts, plan_with, point_text};
use crate::evaluate::{RenderOptions, Renderer};
use crate::{Geometry, fragments};

/// The tools a fillet node applies, and what the export's cross-check
/// allows for them.
#[derive(Debug, Clone)]
pub struct Blends {
    pub tools: Vec<Tool>,
    /// The largest distance between a blend's polygonal arcs and its
    /// exact surface.
    pub sagitta: f64,
    /// The blends' area (their tools' blend triangles, extensions
    /// included): with `sagitta`, a bound on the volume between the mesh
    /// and the exact model.
    pub area: f64,
    /// `$fn` is set on the call: the export keeps the arcs' polygon (the
    /// rule every curve follows, `docs/step-export.md`).
    pub explicit_fn: bool,
}

/// Segments of a blend arc sweeping `sweep` radians: the call's `$fn`,
/// `$fa` and `$fs` on a circle of its radius, as a sketch arc gets them.
fn segments(f: &FilletNode, sweep: f64) -> u32 {
    fragments::circular_segments_for_angle(&f.disc, f.size, sweep.to_degrees())
        .unwrap_or(1)
        .max(1) as u32
}

/// The tools of fillet node `node`, with arcs of `mult` times the
/// segments (the export render's retries), or `None` when the call builds
/// nothing (no edges, an error): its child is then unchanged. Only a
/// stopped request is an error.
///
/// `conform` is the child's mesh as the tools are applied to it, in the
/// node's own coordinates. A circular edge's tool takes its sections at
/// the angles of the mesh's vertices on that circle (the polygon of the
/// cylinder or cone beside it), in both renders ([`sectioned`]). With
/// `facets` (the normal render, where a straight edge's cylinder faces
/// are OpenSCAD's polygons) each such cylinder is replaced by the facet
/// its tangent line lies on ([`conformed`]); the export render keeps the
/// exact cylinders.
///
/// `pass` says which of a call's passes (`docs/fillets.md`, section
/// 15.6): a call built in one has only a first; the conforming mesh of a
/// second pass is what the first made.
#[allow(clippy::too_many_arguments)]
pub fn blends(
    renderer: &Renderer,
    node: &Node,
    keys: &Keys,
    opts: &RenderOptions,
    mult: u32,
    conform: Option<&[[V; 3]]>,
    facets: bool,
    pass: Pass,
) -> Result<Option<Blends>, Unavailable> {
    let NodeKind::Fillet(f) = &node.kind else {
        return Ok(None);
    };
    let (b, tol) = match pass {
        Pass::FirstAlone(sense) => {
            let facts = match facts(renderer, node, keys, opts, None) {
                Err(Unavailable::Interrupted) => return Err(Unavailable::Interrupted),
                Err(_) => return Ok(None),
                Ok(x) => x,
            };
            let tol = facts.tolerance;
            let p = plan_with(f, Ok(facts.clone()));
            match super::passes::first_alone(f, &p, &facts, sense) {
                Some(b) => (std::sync::Arc::new(b), tol),
                None => return Ok(None),
            }
        }
        Pass::First | Pass::Second => {
            // The renders need the passes, not a checked size hint.
            let Some(p) = super::plan_at(renderer, node, keys, opts, false) else {
                return Ok(None);
            };
            if p.status == super::Status::Interrupted {
                return Err(Unavailable::Interrupted);
            }
            let found = if pass == Pass::First {
                p.build.zip(p.facts.as_ref().map(|x| x.tolerance))
            } else {
                p.second.map(|s| (s.build, s.facts.tolerance))
            };
            match found {
                Some(x) => x,
                None => return Ok(None),
            }
        }
    };
    let (b, warps) = match conform {
        Some(tris) => sectioned(&b, tris, tol),
        None => ((*b).clone(), Vec::new()),
    };
    if facets && let Some(tris) = conform {
        let c = conformed(&b, tris);
        if c != b
            && let Ok(x) = made(f, &c, mult, &warps)
        {
            return Ok(Some(x));
        }
    }
    Ok(made(f, &b, mult, &warps).ok())
}

/// `b` with each circular edge's tool sectioned at the angles of the
/// vertices of `tris` that lie on its circle (within `tol`), when a face
/// beside it is a cylinder or a cone: the vertices of that face's polygon,
/// where the plane or the other face cuts its straight sides. The tool's
/// tangent ring then runs through the polygon's vertices, chord for chord
/// with its facets.
///
/// A sphere's or a torus's rings do not pass through the rim, and the
/// tool's tangent ring on such a face lies on the exact surface, outside
/// the polygon by up to its depth: the boolean then left the tool's side
/// (the cone along the face's normal from the tangent ring) standing in
/// that gap as a band of no width at the polygon's vertices, which
/// reconstruction could not make a face of ("empty parameter range" on
/// a ball sunk part way into a plate). Such an arc is sectioned where its
/// tangent ring crosses the polygon's creases instead, and the tool is
/// conformed to the facets after it is made (the [`Warp`]s returned, as
/// `meshbrep` conforms a curve's tool), so that its ring lies on the
/// polygon, chord for chord with the facets.
fn sectioned(b: &build::Built, tris: &[[V; 3]], tol: f64) -> (build::Built, Vec<Warp>) {
    let mut out = b.clone();
    let mut warps = Vec::new();
    let mut points: Vec<V> = tris.iter().flatten().copied().collect();
    points.sort_by(|x, y| {
        x[0].total_cmp(&y[0])
            .then(x[1].total_cmp(&y[1]))
            .then(x[2].total_cmp(&y[2]))
    });
    points.dedup();
    for (i, e) in out.spec.edges.iter_mut().enumerate() {
        if let Path::Curve { points, facets } = &mut e.path {
            *facets = [0, 1].map(|k| faces_facets(&e.faces[k], points, tris, tol));
            continue;
        }
        let from = e.from;
        let polygonal = e
            .faces
            .iter()
            .any(|f| matches!(f, BlendFace::Cylinder { .. } | BlendFace::Cone { .. }));
        let faces = e.faces.clone();
        let Path::Arc {
            center,
            axis,
            radius,
            sweep,
            sections,
        } = &mut e.path
        else {
            continue;
        };
        let (c, a) = (*center, *axis);
        let radial = |p: V| {
            let q = sub(p, c);
            sub(q, mul(a, dot(q, a)))
        };
        let r0 = radial(from);
        let rho = *radius;
        let u0 = unit(r0);
        let full = *sweep >= std::f64::consts::TAU * (1.0 - 1e-12);
        let angle = |r: V| {
            let mut th = libm::atan2(dot(cross(u0, r), a), dot(u0, r));
            if th < 0.0 {
                th += std::f64::consts::TAU;
            }
            th
        };
        let mut found: Vec<[f64; 2]> = Vec::new();
        if polygonal {
            for &p in &points {
                if dot(sub(p, c), a).abs() > tol {
                    continue;
                }
                let r = radial(p);
                let off = norm(r) - rho;
                if off.abs() > tol {
                    continue;
                }
                let th = angle(r);
                if !full && th >= *sweep {
                    continue;
                }
                found.push([th, off]);
            }
        }
        let mut curved = false;
        for (k, face) in faces.iter().enumerate() {
            if !matches!(face, BlendFace::Sphere { .. } | BlendFace::Torus { .. }) {
                continue;
            }
            let Ok(sec) = meshbrep::blend::section(&b.spec, i) else {
                continue;
            };
            // The arc's points, for the facets near it.
            let n = 64;
            let v0 = cross(a, u0);
            let arc: Vec<V> = (0..=n)
                .map(|j| {
                    let t = *sweep * f64::from(j) / f64::from(n);
                    let (s, co) = (libm::sin(t), libm::cos(t));
                    add(c, add(mul(u0, rho * co), mul(v0, rho * s)))
                })
                .collect();
            let near = faces_facets(face, &arc, tris, tol);
            let Some(w) = Warp::new(face.clone(), near, tol) else {
                continue;
            };
            // Where the tangent ring (the section's tangent point turned
            // about the axis) crosses a crease: a section there, so that
            // each of the ring's chords lies on one facet.
            let h = dot(sub(sec.tangents[k], c), a);
            for &(p, q) in &w.creases {
                let (hp, hq) = (dot(sub(p, c), a) - h, dot(sub(q, c), a) - h);
                if hp * hq > 0.0 || hp == hq {
                    continue;
                }
                let x = add(p, mul(sub(q, p), hp / (hp - hq)));
                let th = angle(radial(x));
                if !full && th >= *sweep {
                    continue;
                }
                found.push([th, 0.0]);
            }
            warps.push(w);
            curved = true;
        }
        if found.is_empty() || (!polygonal && !curved) {
            continue;
        }
        found.sort_by(|x, y| x[0].total_cmp(&y[0]).then(x[1].total_cmp(&y[1])));
        found.dedup_by(|x, y| (x[0] - y[0]).abs() <= 1e-9);
        *sections = found;
    }
    (out, warps)
}

/// The faceted polygon of a sphere or a torus beside an arc, to conform
/// the arc's tool to after it is made: every tool vertex within a band of
/// a few times the polygon's depth of the exact face is moved along the
/// face's normal by the polygon's depth under its foot, in full on the
/// face and fading to nothing at the band's edge. A vertex then keeps the
/// side of the faceted face it had of the exact one, and the tangent ring
/// lies on the facets (as `meshbrep` conforms a curve's tool,
/// `Path::Curve`).
#[derive(Debug, Clone)]
struct Warp {
    face: BlendFace,
    tris: Vec<[V; 3]>,
    /// The polygon's creases: edges shared by two facets at an angle.
    creases: Vec<(V, V)>,
    band: f64,
    tol: f64,
}

impl Warp {
    fn new(face: BlendFace, tris: Vec<[V; 3]>, tol: f64) -> Option<Warp> {
        let tris: Vec<[V; 3]> = tris
            .into_iter()
            .filter(|t| norm(cross(sub(t[1], t[0]), sub(t[2], t[0]))) > 0.0)
            .collect();
        if tris.is_empty() {
            return None;
        }
        let mut depth: f64 = 0.0;
        for t in &tris {
            for p in [
                mul(add(add(t[0], t[1]), t[2]), 1.0 / 3.0),
                mul(add(t[0], t[1]), 0.5),
                mul(add(t[1], t[2]), 0.5),
                mul(add(t[2], t[0]), 0.5),
            ] {
                depth = depth.max(face_field(&face, p, tol).map_or(0.0, |x| x.0.abs()));
            }
        }
        if depth <= tol {
            return None;
        }
        let key = |p: V| p.map(f64::to_bits);
        let mut edges: std::collections::BTreeMap<([u64; 3], [u64; 3]), Vec<usize>> =
            std::collections::BTreeMap::new();
        for (i, t) in tris.iter().enumerate() {
            for k in 0..3 {
                let (a, b) = (key(t[k]), key(t[(k + 1) % 3]));
                edges
                    .entry(if a < b { (a, b) } else { (b, a) })
                    .or_default()
                    .push(i);
            }
        }
        let normal = |t: &[V; 3]| unit(cross(sub(t[1], t[0]), sub(t[2], t[0])));
        let creases = edges
            .iter()
            .filter(|(_, ts)| {
                ts.len() == 2 && norm(cross(normal(&tris[ts[0]]), normal(&tris[ts[1]]))) > 1e-9
            })
            .map(|((a, b), _)| (a.map(f64::from_bits), b.map(f64::from_bits)))
            .collect();
        Some(Warp {
            face,
            tris,
            creases,
            band: 4.0 * depth,
            tol,
        })
    }

    /// `x` conformed: moved along the face's normal by the polygon's
    /// depth under its foot, faded across the band.
    fn apply(&self, x: V) -> V {
        let Some((h, n, _)) = face_field(&self.face, x, self.tol) else {
            return x;
        };
        let w = 1.0 - h.abs() / self.band;
        if w <= 0.0 {
            return x;
        }
        let foot = sub(x, mul(n, h));
        // The nearest facet along the normal through the foot, within the
        // band: none (past the polygon given) leaves the vertex alone.
        let down = mul(n, -1.0);
        let mut best: Option<f64> = None;
        for t in &self.tris {
            if let Some(s) = ray_triangle(foot, down, t)
                && s.abs() <= self.band
                && best.is_none_or(|b: f64| s.abs() < b.abs())
            {
                best = Some(s);
            }
        }
        match best {
            Some(d) => sub(x, mul(n, d * w)),
            None => x,
        }
    }
}

/// Where the line through `o` along unit `d` meets triangle `t`: the
/// parameter along the line, if it meets it (its edges included).
fn ray_triangle(o: V, d: V, t: &[V; 3]) -> Option<f64> {
    let e1 = sub(t[1], t[0]);
    let e2 = sub(t[2], t[0]);
    let p = cross(d, e2);
    let det = dot(e1, p);
    if det.abs() <= 1e-300 {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub(o, t[0]);
    let u = dot(s, p) * inv;
    let q = cross(s, e1);
    let w = dot(d, q) * inv;
    let eps = 1e-9;
    if u < -eps || w < -eps || u + w > 1.0 + eps {
        return None;
    }
    Some(dot(e2, q) * inv)
}

/// A curved face's signed distance at `p` (positive outside the
/// material), its outward normal, and its least radius of curvature;
/// `None` for a plane.
fn face_field(f: &BlendFace, p: V, tol: f64) -> Option<(f64, V, f64)> {
    match *f {
        BlendFace::Plane { .. } => None,
        BlendFace::Cylinder {
            origin,
            axis,
            radius,
            convex,
        } => {
            let q = sub(p, origin);
            let r = sub(q, mul(axis, dot(q, axis)));
            let s = if convex { 1.0 } else { -1.0 };
            Some(((norm(r) - radius) * s, mul(unit(r), s), radius))
        }
        BlendFace::Cone {
            apex,
            axis,
            slope,
            convex,
        } => {
            let q = sub(p, apex);
            let h = dot(q, axis);
            let r = sub(q, mul(axis, h));
            let l = (1.0 + slope * slope).sqrt();
            let s = if convex { 1.0 } else { -1.0 };
            let n = mul(sub(unit(r), mul(axis, slope)), s / l);
            Some(((norm(r) - slope * h) * s / l, n, (norm(r) * l).max(tol)))
        }
        BlendFace::Sphere {
            center,
            radius,
            convex,
        } => {
            let q = sub(p, center);
            let s = if convex { 1.0 } else { -1.0 };
            Some(((norm(q) - radius) * s, mul(unit(q), s), radius))
        }
        BlendFace::Torus {
            center,
            axis,
            major_radius,
            minor_radius,
            convex,
        } => {
            let q = sub(p, center);
            let tube = add(
                center,
                mul(unit(sub(q, mul(axis, dot(q, axis)))), major_radius),
            );
            let d = sub(p, tube);
            let s = if convex { 1.0 } else { -1.0 };
            Some(((norm(d) - minor_radius) * s, mul(unit(d), s), minor_radius))
        }
    }
}

/// The triangles of `tris` (the mesh a tool is applied to) that are the
/// faceted face `f` near a curve edge (`points`): their vertices and
/// centroid within the polygon's depth under the exact face (measured
/// on the polygon's own chords, those of its edges whose ends are on the
/// exact face), facing the way the face does (its outward normal and the
/// triangle's agree), and near the edge (within the box of its points, grown by
/// twice their spread across, enough for any blend that fits). A plane
/// gives none: its facets are the plane. The curve's tool is conformed
/// to them (`meshbrep::blend::Path::Curve`).
fn faces_facets(f: &BlendFace, points: &[V], tris: &[[V; 3]], tol: f64) -> Vec<[V; 3]> {
    let field = |p: V| face_field(f, p, tol);
    if matches!(f, BlendFace::Plane { .. }) || points.is_empty() {
        return Vec::new();
    }
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in points {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let spread = (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max);
    let grow = spread.max(tol);
    let lo = lo.map(|x| x - grow);
    let hi = hi.map(|x| x + grow);
    // Candidates: near the edge, facing the way the face does.
    let candidates: Vec<[V; 3]> = tris
        .iter()
        .filter(|t| {
            // Near the edge: the triangle's box meets the grown box.
            (0..3).all(|k| {
                let a = t[0][k].min(t[1][k]).min(t[2][k]);
                let b = t[0][k].max(t[1][k]).max(t[2][k]);
                b >= lo[k] && a <= hi[k]
            })
        })
        .filter(|t| {
            let c = mul(add(add(t[0], t[1]), t[2]), 1.0 / 3.0);
            let Some((_, n, _)) = field(c) else {
                return false;
            };
            let tn = cross(sub(t[1], t[0]), sub(t[2], t[0]));
            norm(tn) > 0.0 && dot(unit(tn), n) >= 0.75
        })
        .copied()
        .collect();
    // How deep the polygon's facets lie under the exact face: the deepest
    // midpoint of a candidate's edge between two of the polygon's own
    // vertices (on the exact face), with a margin for a facet's middle.
    // A bound from the coarsest polygon instead takes in nearby faces
    // that are not the polygon's (a branch's top cap over the top of the
    // rod it stands on), and the tool then followed them up the branch.
    let on = |v: V| field(v).is_some_and(|(d, _, _)| d.abs() <= tol);
    let mut chord: f64 = 0.0;
    for t in &candidates {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            if on(a) && on(b) {
                let m = mul(add(a, b), 0.5);
                chord = chord.max(field(m).map_or(0.0, |(d, _, _)| d.abs()));
            }
        }
    }
    if chord <= 0.0 {
        return Vec::new();
    }
    // Inscribed polygons lie on the axis's side: in the material of a
    // rod, in the air of a hole.
    let band = 1.5 * chord + tol;
    candidates
        .into_iter()
        .filter(|t| {
            let c = mul(add(add(t[0], t[1]), t[2]), 1.0 / 3.0);
            t.iter()
                .chain(std::iter::once(&c))
                .all(|&v| field(v).is_some_and(|(d, _, _)| d.abs() <= band))
        })
        .collect()
}

/// The distance from `p` to triangle `t`.
fn to_triangle(p: V, t: &[V; 3]) -> f64 {
    let [a, b, c] = *t;
    let n = cross(sub(b, a), sub(c, a));
    let l = norm(n);
    if l > 0.0 {
        let n = mul(n, 1.0 / l);
        let h = dot(sub(p, a), n);
        let q = sub(p, mul(n, h));
        let inside = [(a, b), (b, c), (c, a)]
            .iter()
            .all(|&(u, v)| dot(cross(sub(v, u), sub(q, u)), n) >= 0.0);
        if inside {
            return h.abs();
        }
    }
    let seg = |u: V, v: V| {
        let w = sub(v, u);
        let ww = dot(w, w);
        let s = if ww > 0.0 {
            (dot(sub(p, u), w) / ww).clamp(0.0, 1.0)
        } else {
            0.0
        };
        norm(sub(p, add(u, mul(w, s))))
    };
    seg(a, b).min(seg(b, c)).min(seg(c, a))
}

/// `b` with each straight edge's cylinder faces replaced by the planar facet of
/// `tris` (the child's normal mesh) nearest its exact tangent line, facing
/// the same way: the facet of OpenSCAD's polygon the ball touches. Then
/// the tool's tangent line lies in the mesh's face, and the boolean
/// leaves the whole blend, where a tangent on the exact cylinder (outside
/// the inscribed polygon by up to its sagitta) would leave the facet
/// standing over part of the blend, with a crease.
fn conformed(b: &build::Built, tris: &[[V; 3]]) -> build::Built {
    let mut out = b.clone();
    for (i, e) in out.spec.edges.iter_mut().enumerate() {
        // A circular edge's coaxial cylinder is conformed by its
        // sections instead ([`sectioned`]).
        if !matches!(e.path, Path::Line)
            || !e
                .faces
                .iter()
                .any(|f| matches!(f, BlendFace::Cylinder { .. }))
        {
            continue;
        }
        let Ok(sec) = meshbrep::blend::section(&b.spec, i) else {
            continue;
        };
        let d = unit(sub(e.to, e.from));
        for k in 0..2 {
            let BlendFace::Cylinder {
                origin,
                axis,
                convex,
                ..
            } = e.faces[k]
            else {
                continue;
            };
            let t = sec.tangents[k];
            let q = sub(t, origin);
            let rad = unit(sub(q, mul(axis, dot(q, axis))));
            let nu = if convex { rad } else { mul(rad, -1.0) };
            let mut best: Option<(f64, V, V)> = None;
            for tri in tris {
                let n = cross(sub(tri[1], tri[0]), sub(tri[2], tri[0]));
                let l = norm(n);
                if l <= 0.0 {
                    continue;
                }
                let n = mul(n, 1.0 / l);
                if dot(n, nu) < 0.95 || dot(n, d).abs() > 1e-9 {
                    continue;
                }
                let dist = to_triangle(t, tri);
                if best.is_none_or(|(x, _, _)| dist < x) {
                    best = Some((dist, tri[0], n));
                }
            }
            if let Some((_, o, n)) = best {
                e.faces[k] = BlendFace::Plane {
                    origin: o,
                    normal: n,
                };
            }
        }
    }
    out
}

fn made(f: &FilletNode, b: &build::Built, mult: u32, warps: &[Warp]) -> Result<Blends, String> {
    let seg = |sweep: f64| segments(f, sweep).saturating_mul(mult.max(1));
    // Arcs with no polygon to conform to are revolved through the call's
    // own segments for a circle of their radius, a multiple of 4 so that
    // an axis-aligned rim has sections on the axes.
    let mut b = b.clone();
    for e in &mut b.spec.edges {
        if let meshbrep::blend::Path::Arc {
            radius,
            sweep,
            sections,
            ..
        } = &mut e.path
            && sections.is_empty()
        {
            let n = fragments::circular_segments(&f.disc, *radius)
                .unwrap_or(32)
                .max(3) as u32;
            let n = n.saturating_mul(mult.max(1)).div_ceil(4).max(1) * 4;
            let step = std::f64::consts::TAU / f64::from(n);
            *sections = (0..n)
                .map(|j| step * f64::from(j))
                .filter(|&t| t < *sweep)
                .map(|t| [t, 0.0])
                .collect();
        }
    }
    let mut tools = build::tools(&b, &seg)?;
    // Conform the tools beside spheres and tori to their polygons
    // ([`sectioned`]); before the sagitta is measured, which then counts
    // the depth the vertices moved by.
    if !warps.is_empty() {
        for t in &mut tools {
            for p in &mut t.mesh.positions {
                for w in warps {
                    *p = w.apply(*p);
                }
            }
        }
    }
    let mut sagitta: f64 = 0.0;
    let mut area = 0.0;
    for t in &tools {
        let (dev, a) = blend_shape(t);
        sagitta = sagitta.max(dev);
        area += a;
    }
    Ok(Blends {
        tools,
        sagitta,
        area,
        explicit_fn: f.disc.fn_ > 0.0 || !f.disc.fn_.is_finite(),
    })
}

fn tri_area(a: V, b: V, c: V) -> f64 {
    0.5 * norm(cross(sub(b, a), sub(c, a)))
}

/// The blend surfaces of a tool, ready to measure points against: a
/// B-spline blend (between curved faces) needs an evaluator, made once.
struct Exact<'a> {
    s: &'a Surface,
    ev: Option<Evaluator>,
}

impl<'a> Exact<'a> {
    fn new(s: &'a Surface) -> Exact<'a> {
        let ev = match s {
            Surface::BSpline(b) => Evaluator::new(b).ok(),
            _ => None,
        };
        Exact { s, ev }
    }

    /// Distance of `p` from the surface.
    fn off(&self, p: V) -> f64 {
        match &self.ev {
            Some(ev) => {
                let [u, v] = ev.project(p);
                norm(sub(ev.eval(u, v), p))
            }
            None => off_surface(self.s, p),
        }
    }

    /// Its unit normal at (or near) `p`, either way.
    fn normal(&self, p: V) -> V {
        match &self.ev {
            Some(ev) => {
                let [u, v] = ev.project(p);
                ev.normal(u, v)
            }
            None => surface_normal(self.s, p),
        }
    }
}

/// Distance of `p` from the exact blend surface `s`.
fn off_surface(s: &Surface, p: V) -> f64 {
    match s {
        Surface::Cylinder {
            origin,
            axis,
            radius,
        } => {
            let q = sub(p, *origin);
            let r = norm(sub(q, mul(*axis, dot(q, *axis))));
            (r - radius).abs()
        }
        Surface::Sphere { center, radius } => (norm(sub(p, *center)) - radius).abs(),
        Surface::Plane { origin, normal } => dot(sub(p, *origin), *normal).abs(),
        Surface::Torus {
            center,
            axis,
            major_radius,
            minor_radius,
        } => (norm(sub(p, tube_point(*center, *axis, *major_radius, p))) - minor_radius).abs(),
        Surface::Cone { apex, axis, slope } => {
            // The distance from the generator in `p`'s meridian.
            let q = sub(p, *apex);
            let h = dot(q, *axis);
            let r = norm(sub(q, mul(*axis, h)));
            let c = 1.0 / (1.0 + slope * slope).sqrt();
            ((r - slope * h) * c).abs()
        }
        _ => f64::INFINITY,
    }
}

/// The point of a torus's tube circle (about `axis` through `center`, of
/// radius `major`) in `p`'s meridian.
fn tube_point(center: V, axis: V, major: f64, p: V) -> V {
    let q = sub(p, center);
    let r = unit(sub(q, mul(axis, dot(q, axis))));
    add(center, mul(r, major))
}

/// The unit normal of the blend surface `s` at `p` (either way), for
/// telling a triangle on it from one across it.
fn surface_normal(s: &Surface, p: V) -> V {
    match s {
        Surface::Cylinder { origin, axis, .. } => {
            let q = sub(p, *origin);
            unit(sub(q, mul(*axis, dot(q, *axis))))
        }
        Surface::Sphere { center, .. } => unit(sub(p, *center)),
        Surface::Plane { normal, .. } => *normal,
        Surface::Torus {
            center,
            axis,
            major_radius,
            ..
        } => unit(sub(p, tube_point(*center, *axis, *major_radius, p))),
        Surface::Cone { apex, axis, slope } => {
            let q = sub(p, *apex);
            let radial = unit(sub(q, mul(*axis, dot(q, *axis))));
            unit(sub(radial, mul(*axis, *slope)))
        }
        _ => [0.0; 3],
    }
}

/// A tool's blend triangles: the largest distance of their edges'
/// midpoints from the exact surface (the arcs' sagitta), and their area.
fn blend_shape(t: &Tool) -> (f64, f64) {
    let m = &t.mesh;
    let mut dev: f64 = 0.0;
    let mut area = 0.0;
    let exact: Vec<(u32, Exact)> = t
        .blend
        .iter()
        .map(|&b| (b, Exact::new(&m.surfaces[b as usize])))
        .collect();
    for (tri, &s) in m.triangles.iter().zip(&m.triangle_surface) {
        let Some((_, ex)) = exact.iter().find(|x| x.0 == s) else {
            continue;
        };
        let surf = &m.surfaces[s as usize];
        let p = tri.map(|i| m.positions[i as usize]);
        for k in 0..3 {
            let mid = mul(add(p[k], p[(k + 1) % 3]), 0.5);
            dev = dev.max(ex.off(mid));
        }
        dev = dev.max(ex.off(mul(add(add(p[0], p[1]), p[2]), 1.0 / 3.0)));
        // A sphere's facet is furthest inside where the centre's
        // perpendicular meets it, which no edge midpoint shows.
        if let Surface::Sphere { center, radius } = surf {
            let n = unit(cross(sub(p[1], p[0]), sub(p[2], p[0])));
            dev = dev.max(radius - dot(sub(p[0], *center), n).abs());
        }
        area += tri_area(p[0], p[1], p[2]);
    }
    (dev, area)
}

/// Whether triangle `p` lies in the plane of triangle `f` (within `eps`)
/// with its centroid inside it.
fn piece_of(p: &[V; 3], f: &[V; 3], eps: f64) -> bool {
    let n = cross(sub(f[1], f[0]), sub(f[2], f[0]));
    let l = norm(n);
    if l <= 0.0 {
        return false;
    }
    let n = mul(n, 1.0 / l);
    if p.iter().any(|v| dot(sub(*v, f[0]), n).abs() > eps) {
        return false;
    }
    let c = mul(add(add(p[0], p[1]), p[2]), 1.0 / 3.0);
    (0..3).all(|k| dot(cross(sub(f[(k + 1) % 3], f[k]), sub(c, f[k])), n) >= -eps * l)
}

/// The least cosine between a tool's blend triangles' normals and its
/// surface's normals at their corners, less a margin: what a triangle of
/// the result on the blend can turn by.
fn facing(t: &Tool) -> f64 {
    let m = &t.mesh;
    let mut worst: f64 = 1.0;
    let exact: Vec<(u32, Exact)> = t
        .blend
        .iter()
        .map(|&b| (b, Exact::new(&m.surfaces[b as usize])))
        .collect();
    for (tri, &s) in m.triangles.iter().zip(&m.triangle_surface) {
        let Some((_, ex)) = exact.iter().find(|x| x.0 == s) else {
            continue;
        };
        let p = tri.map(|i| m.positions[i as usize]);
        let n = unit(cross(sub(p[1], p[0]), sub(p[2], p[0])));
        for v in p {
            worst = worst.min(dot(n, ex.normal(v)).abs());
        }
    }
    (worst - 0.05).max(0.5)
}

/// The area of triangle `p` on the side of each plane `(o, n)` where
/// `n · (x - o) <= 0`.
fn clipped_area(p: [V; 3], planes: &[(V, V)]) -> f64 {
    let mut poly: Vec<V> = p.to_vec();
    for &(o, n) in planes {
        let mut out = Vec::with_capacity(poly.len() + 1);
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (da, db) = (dot(sub(a, o), n), dot(sub(b, o), n));
            if da <= 0.0 {
                out.push(a);
            }
            if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
                out.push(add(a, mul(sub(b, a), da / (da - db))));
            }
        }
        poly = out;
        if poly.len() < 3 {
            return 0.0;
        }
    }
    (1..poly.len() - 1)
        .map(|i| tri_area(poly[0], poly[i], poly[i + 1]))
        .sum()
}

/// The result's triangles, from a normal render's geometry.
pub(crate) fn triangles(g: &Geometry) -> Vec<[V; 3]> {
    match g {
        Geometry::Manifold(m) => {
            let gl = m.manifold.get_mesh_gl64(-1);
            let np = (gl.num_prop as usize).max(3);
            let at = |i: u64| {
                let k = i as usize * np;
                [
                    gl.vert_properties[k],
                    gl.vert_properties[k + 1],
                    gl.vert_properties[k + 2],
                ]
            };
            gl.tri_verts
                .chunks(3)
                .map(|c| [at(c[0]), at(c[1]), at(c[2])])
                .collect()
        }
        Geometry::PolySet(ps) => {
            let mut out = Vec::new();
            for f in &ps.faces {
                for i in 1..f.len().saturating_sub(1) {
                    out.push([f[0], f[i], f[i + 1]].map(|k| ps.vertices[k as usize]));
                }
            }
            out
        }
        Geometry::Polygon2d(_) => Vec::new(),
    }
}

/// The checks after the boolean, for a plan that built its blends: each
/// blend's area in the normal render's result against what its tool
/// should leave. Less is `fillet-interrupted` (info: something else in
/// the child cut into it, often on purpose); almost none is
/// `fillet-failed` (an error: the result is not what the call asked
/// for). Tools that could not be made at all are `fillet-failed` too.
///
/// A call built in two passes (`docs/fillets.md`, section 15.6) is
/// checked pass by pass, as two nested calls would be: the first pass's
/// blends on what it made (the stage the normal render kept), the
/// second's on the result, each pass's tools conformed to the mesh they
/// were applied to.
pub fn blend_diags(
    renderer: &Renderer,
    node: &Node,
    keys: &Keys,
    opts: &RenderOptions,
    plan: &Plan,
) -> Vec<PlanDiag> {
    let (NodeKind::Fillet(f), Some(b), Some(facts)) = (&node.kind, &plan.build, &plan.facts) else {
        return Vec::new();
    };
    // The first pass's tools as the normal render made them: conformed to
    // the children's meshes (cached renders).
    let mut child_tris = Vec::new();
    for c in node.children.iter().filter(|c| !super::is_background(c)) {
        if let Ok(r) = renderer.render(c, keys, opts.clone())
            && let Some(g) = &r.geometry
        {
            child_tris.extend(triangles(g));
        }
    }
    let Ok(rendered) = renderer.render(node, keys, opts.clone()) else {
        return Vec::new();
    };
    let Some(g) = rendered.geometry else {
        return Vec::new();
    };
    let result = triangles(&g);
    let first_name = |src: Source| match src {
        Source::Edge(i) => {
            let fi = b.edges[i];
            let k = plan
                .selected
                .iter()
                .position(|&s| s == fi)
                .map_or(0, |x| x + 1);
            format!("edge {k}")
        }
        Source::Corner(c) => format!("the corner at {}", point_text(b.spec.corners[c].vertex)),
    };
    let Some(second) = &plan.second else {
        let mut out = Vec::new();
        measure(
            f,
            b,
            facts.tolerance,
            &child_tris,
            &result,
            &first_name,
            &mut out,
        );
        return out;
    };
    // What the first pass made: kept by the normal render. A cached result
    // whose stage has been evicted is computed again, which keeps it.
    let key = keys.get(node);
    let stage = renderer.fillet_stage(key).or_else(|| {
        renderer.forget(key);
        renderer.render(node, keys, opts.clone()).ok()?;
        renderer.fillet_stage(key)
    });
    let Some(stage) = stage else {
        return Vec::new();
    };
    let stage = triangles(&stage);
    let mut out = Vec::new();
    measure(
        f,
        b,
        facts.tolerance,
        &child_tris,
        &stage,
        &first_name,
        &mut out,
    );
    let sb = &second.build;
    let second_name = |src: Source| match src {
        Source::Edge(i) => {
            let k = second.selected.iter().position(|&s| s == sb.edges[i]);
            match k.and_then(|k| second.origin[k]) {
                Some(o) => {
                    let n = plan
                        .selected
                        .iter()
                        .position(|&s| s == o)
                        .map_or(0, |x| x + 1);
                    format!("edge {n}")
                }
                None => format!(
                    "the edge the first pass made ({})",
                    super::edge_text(&second.facts.edges[sb.edges[i]])
                ),
            }
        }
        Source::Corner(c) => format!("the corner at {}", point_text(sb.spec.corners[c].vertex)),
    };
    measure(
        f,
        sb,
        second.facts.tolerance,
        &stage,
        &result,
        &second_name,
        &mut out,
    );
    out
}

/// [`blend_diags`] for one pass: the tools of `b` conformed to `conform`
/// (the mesh they were applied to), measured on `tris` (what came of it).
fn measure(
    f: &FilletNode,
    b: &build::Built,
    tolerance: f64,
    conform: &[[V; 3]],
    tris: &[[V; 3]],
    name: &dyn Fn(Source) -> String,
    out: &mut Vec<PlanDiag>,
) {
    let m = f.kind.module();
    let (sb, warps) = sectioned(b, conform, tolerance);
    let c = conformed(&sb, conform);
    let made_now = match made(f, &c, 1, &warps) {
        Ok(x) => Ok(x),
        Err(_) => made(f, &sb, 1, &warps),
    };
    let blends = match made_now {
        Ok(x) => x,
        Err(why) => {
            out.push(PlanDiag {
                severity: Severity::Error,
                code: DiagCode::FilletFailed,
                message: format!("{m}(): the blends could not be built: {why}"),
                hints: vec![
                    "this is a limitation or a bug of NeoSCAD; selecting fewer edges may avoid it"
                        .into(),
                ],
                fix: None,
            });
            return;
        }
    };
    let tol = tolerance * 10.0;
    for t in &blends.tools {
        let mesh = &t.mesh;
        let (dev, _) = blend_shape(t);
        // How far a facet of this tool turns from the surface's normal:
        // a coarse blend's facets turn by up to half its segments' angle,
        // and a piece of one must still count as lying on the blend.
        let facing = facing(t).min(0.9);
        let band = 2.0 * dev + tol;
        for (&surf, &src) in t.blend.iter().zip(&t.sources) {
            // What the tool should leave of this blend: all of it, less
            // the parts beyond an open end, which lie in the air.
            let mut limits: Vec<(V, V)> = Vec::new();
            if let Source::Edge(i) = src {
                let e = &b.spec.edges[i];
                // A curve leaves its ends along its own direction there.
                let d_end = |end: usize| match &e.path {
                    Path::Curve { points, .. } if points.len() >= 2 => {
                        let n = points.len();
                        if end == 0 {
                            unit(sub(points[1], points[0]))
                        } else {
                            unit(sub(points[n - 1], points[n - 2]))
                        }
                    }
                    _ => unit(sub(e.to, e.from)),
                };
                for (end, at) in [(0usize, e.from), (1, e.to)] {
                    let d = d_end(end);
                    let out_dir = if end == 0 { mul(d, -1.0) } else { d };
                    if let End::Open { face } = &e.ends[end] {
                        limits.push(match face {
                            Some((o, n)) => (*o, *n),
                            None => (at, out_dir),
                        });
                        // Two extended convex blends at a vertex cut each
                        // other along the plane bisecting their edges
                        // (equal sizes): each keeps its own side (7.3).
                        // Only straight edges meet so.
                        for (j, o) in b.spec.edges.iter().enumerate() {
                            if j == i
                                || !matches!(e.path, meshbrep::blend::Path::Line)
                                || !matches!(o.path, meshbrep::blend::Path::Line)
                            {
                                continue;
                            }
                            let far = if o.from == at {
                                o.to
                            } else if o.to == at {
                                o.from
                            } else {
                                continue;
                            };
                            let u_o = unit(sub(far, at));
                            let u_e = mul(out_dir, -1.0);
                            let n = sub(u_o, u_e);
                            if norm(n) > 1e-9 {
                                limits.push((at, unit(n)));
                            }
                        }
                    }
                }
            }
            let mut lo = [f64::INFINITY; 3];
            let mut hi = [f64::NEG_INFINITY; 3];
            let mut expected = 0.0;
            for (tri, &s) in mesh.triangles.iter().zip(&mesh.triangle_surface) {
                if s != surf {
                    continue;
                }
                let p = tri.map(|i| mesh.positions[i as usize]);
                for v in &p {
                    for k in 0..3 {
                        lo[k] = lo[k].min(v[k] - band);
                        hi[k] = hi[k].max(v[k] + band);
                    }
                }
                expected += clipped_area(p, &limits);
            }
            if expected <= 0.0 {
                continue;
            }
            let surface = Exact::new(&mesh.surfaces[surf as usize]);
            // The tool's own facets on this blend: what the boolean keeps
            // of the blend are pieces of them, in their planes.
            let facets: Vec<[V; 3]> = mesh
                .triangles
                .iter()
                .zip(&mesh.triangle_surface)
                .filter(|(_, s)| **s == surf)
                .map(|(t, _)| t.map(|i| mesh.positions[i as usize]))
                .collect();
            let flat = tolerance * 0.1;
            let mut found = 0.0;
            for p in tris {
                let inside = p
                    .iter()
                    .all(|v| (0..3).all(|k| v[k] >= lo[k] && v[k] <= hi[k]));
                if !inside || !p.iter().all(|v| surface.off(*v) <= band) {
                    continue;
                }
                let n = cross(sub(p[1], p[0]), sub(p[2], p[0]));
                let a = 0.5 * norm(n);
                if a <= 0.0 {
                    continue;
                }
                let n = mul(n, 0.5 / a);
                let c = mul(add(add(p[0], p[1]), p[2]), 1.0 / 3.0);
                // Facing along the surface's normal there, not across it
                // (a face of the child crossing the blend's band); or,
                // where the tool's facets turn further from the surface
                // than that allows (a coarse polygon revolved), a piece
                // of one of its facets.
                let sn = surface.normal(c);
                if dot(n, sn).abs() >= facing || facets.iter().any(|f| piece_of(p, f, flat)) {
                    found += a;
                }
            }
            let ratio = found / expected;
            if ratio < 0.02 {
                out.push(PlanDiag {
                    severity: Severity::Error,
                    code: DiagCode::FilletFailed,
                    message: format!(
                        "{m}(): the blend of {} is missing from the result, which does not match the selection; the child is not rounded there",
                        name(src)
                    ),
                    hints: vec![
                        "this is a limitation or a bug of NeoSCAD; selecting fewer edges may avoid it"
                            .into(),
                    ],
                    fix: None,
                });
            } else if ratio < 0.98 {
                out.push(PlanDiag {
                    severity: Severity::Info,
                    code: DiagCode::FilletInterrupted,
                    message: format!(
                        "{m}(): {}% of the blend of {} remains: other geometry of the child cuts into it",
                        (ratio * 100.0).round(),
                        name(src)
                    ),
                    hints: vec![
                        "often intended (a hole or a slot crossing the rounded edge); if not, check what overlaps it".into(),
                    ],
                    fix: None,
                });
            }
        }
    }
}
