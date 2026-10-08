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
use meshbrep::blend::{End, Source, Tool};

use super::curve::{V, add, cross, dot, mul, norm, sub, unit};
use super::{Plan, PlanDiag, Unavailable, build, facts, plan_with, point_text};
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
/// nothing (no edges, an error, a class not built yet): its child is then
/// unchanged. Only a stopped request is an error.
///
/// `conform` is the child's mesh in the normal render: a fillet's
/// cylinder faces there are OpenSCAD's polygons, so each is replaced by
/// the facet its tangent line lies on ([`conformed`]). The export render
/// passes `None` and keeps the exact cylinders.
pub fn blends(
    renderer: &Renderer,
    node: &Node,
    keys: &Keys,
    opts: &RenderOptions,
    mult: u32,
    conform: Option<&[[V; 3]]>,
) -> Result<Option<Blends>, Unavailable> {
    let NodeKind::Fillet(f) = &node.kind else {
        return Ok(None);
    };
    let facts = match facts(renderer, node, keys, opts) {
        Err(Unavailable::Interrupted) => return Err(Unavailable::Interrupted),
        Err(_) => return Ok(None),
        Ok(x) => x,
    };
    let plan = plan_with(f, Ok(facts));
    let Some(b) = plan.build else {
        return Ok(None);
    };
    if let Some(tris) = conform {
        let c = conformed(&b, tris);
        if c != *b
            && let Ok(x) = made(f, &c, mult)
        {
            return Ok(Some(x));
        }
    }
    Ok(made(f, &b, mult).ok())
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

/// `b` with each fillet's cylinder faces replaced by the planar facet of
/// `tris` (the child's normal mesh) nearest its exact tangent line, facing
/// the same way: the facet of OpenSCAD's polygon the ball touches. Then
/// the tool's tangent line lies in the mesh's face, and the boolean
/// leaves the whole blend, where a tangent on the exact cylinder (outside
/// the inscribed polygon by up to its sagitta) would leave the facet
/// standing over part of the blend, with a crease.
fn conformed(b: &build::Built, tris: &[[V; 3]]) -> build::Built {
    use meshbrep::blend::{BlendFace, Profile};
    let mut out = b.clone();
    if b.spec.profile != Profile::Fillet {
        return out;
    }
    for (i, e) in out.spec.edges.iter_mut().enumerate() {
        if !e
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

fn made(f: &FilletNode, b: &build::Built, mult: u32) -> Result<Blends, String> {
    let seg = |sweep: f64| segments(f, sweep).saturating_mul(mult.max(1));
    let tools = build::tools(b, &seg)?;
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
        _ => f64::INFINITY,
    }
}

/// A tool's blend triangles: the largest distance of their edges'
/// midpoints from the exact surface (the arcs' sagitta), and their area.
fn blend_shape(t: &Tool) -> (f64, f64) {
    let m = &t.mesh;
    let mut dev: f64 = 0.0;
    let mut area = 0.0;
    for (tri, &s) in m.triangles.iter().zip(&m.triangle_surface) {
        if !t.blend.contains(&s) {
            continue;
        }
        let surf = &m.surfaces[s as usize];
        let p = tri.map(|i| m.positions[i as usize]);
        for k in 0..3 {
            let mid = mul(add(p[k], p[(k + 1) % 3]), 0.5);
            dev = dev.max(off_surface(surf, mid));
        }
        dev = dev.max(off_surface(
            surf,
            mul(add(add(p[0], p[1]), p[2]), 1.0 / 3.0),
        ));
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
    let m = f.kind.module();
    // The tools as the normal render made them: conformed to the
    // children's meshes (cached renders), falling back to the exact ones.
    let mut child_tris = Vec::new();
    for c in node.children.iter().filter(|c| !super::is_background(c)) {
        if let Ok(r) = renderer.render(c, keys, opts.clone())
            && let Some(g) = &r.geometry
        {
            child_tris.extend(triangles(g));
        }
    }
    let c = conformed(b, &child_tris);
    let made_now = match made(f, &c, 1) {
        Ok(x) => Ok(x),
        Err(_) => made(f, b, 1),
    };
    let blends = match made_now {
        Ok(x) => x,
        Err(why) => {
            return vec![PlanDiag {
                severity: Severity::Error,
                code: DiagCode::FilletFailed,
                message: format!("{m}(): the blends could not be built: {why}"),
                hints: vec![
                    "this is a limitation or a bug of NeoSCAD; selecting fewer edges may avoid it"
                        .into(),
                ],
                fix: None,
            }];
        }
    };
    let Ok(rendered) = renderer.render(node, keys, opts.clone()) else {
        return Vec::new();
    };
    let Some(g) = rendered.geometry else {
        return Vec::new();
    };
    let tris = triangles(&g);
    let tol = facts.tolerance * 10.0;
    let mut out = Vec::new();
    let name = |src: Source| match src {
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
    for t in &blends.tools {
        let mesh = &t.mesh;
        let (dev, _) = blend_shape(t);
        let band = 2.0 * dev + tol;
        for (&surf, &src) in t.blend.iter().zip(&t.sources) {
            // What the tool should leave of this blend: all of it, less
            // the parts beyond an open end, which lie in the air.
            let mut limits: Vec<(V, V)> = Vec::new();
            if let Source::Edge(i) = src {
                let e = &b.spec.edges[i];
                let d = unit(sub(e.to, e.from));
                for (end, at) in [(0usize, e.from), (1, e.to)] {
                    let out_dir = if end == 0 { mul(d, -1.0) } else { d };
                    if let End::Open { face } = &e.ends[end] {
                        limits.push(match face {
                            Some((o, n)) => (*o, *n),
                            None => (at, out_dir),
                        });
                        // Two extended convex blends at a vertex cut each
                        // other along the plane bisecting their edges
                        // (equal sizes): each keeps its own side (7.3).
                        for (j, o) in b.spec.edges.iter().enumerate() {
                            if j == i {
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
            let surface = &mesh.surfaces[surf as usize];
            let mut found = 0.0;
            for p in &tris {
                let inside = p
                    .iter()
                    .all(|v| (0..3).all(|k| v[k] >= lo[k] && v[k] <= hi[k]));
                if !inside || !p.iter().all(|v| off_surface(surface, *v) <= band) {
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
                // (a face of the child crossing the blend's band).
                let sn = match surface {
                    Surface::Cylinder { origin, axis, .. } => {
                        let q = sub(c, *origin);
                        unit(sub(q, mul(*axis, dot(q, *axis))))
                    }
                    Surface::Sphere { center, .. } => unit(sub(c, *center)),
                    Surface::Plane { normal, .. } => *normal,
                    _ => [0.0; 3],
                };
                if dot(n, sn).abs() >= 0.9 {
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
    out
}
