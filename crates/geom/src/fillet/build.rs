//! Stage F2 of `docs/fillets.md`: the blends of the translational class
//! (straight edges between planes, or cylinders parallel to them), from
//! the child's B-rep to `meshbrep::blend`'s specification, with the checks
//! that run before any boolean.
//!
//! - **The faces** of each edge are its two B-rep faces' exact surfaces,
//!   with their outward sides.
//! - **The ends** (sections 7.1 to 7.4): at each end vertex, the faces and
//!   edges around it decide how the tool ends. Past a tangent edge (a line
//!   running into an arc of a rounded outline) the tool is cut across the
//!   edge; at a simple vertex of three faces, a convex tool runs on into
//!   the air when the third face faces the way the edge leaves (the
//!   material ends) and is cut by that face when the edge runs into it (a
//!   wall), and a concave tool is cut by it either way; two selected edges
//!   there are both extended when convex and mitred when concave; three
//!   are a sphere corner. Anything else is `fillet-unsupported-vertex`.
//! - **The size checks** (section 8): every blend must fit its
//!   cross-section, its strip on each face must stay inside the face, and
//!   two strips on one face must not overlap, before anything is built.
//!   On failure the largest size that fits is found by bisection over the
//!   same checks, so the hint's number is one that passes them.

use std::collections::{BTreeMap, BTreeSet};

use meshbrep::blend::{
    self, BlendEdge, BlendError, BlendFace, BlendSpec, Corner, End, Profile, Section,
};
use meshbrep::{Brep, Face, Surface};

use super::curve::{self, V, add, cross, dot, mul, norm, sub, unit};
use super::{Facts, Sense};

/// Why a call's blends cannot be built: the call is an error and its
/// child stays sharp.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Problem {
    /// A blend does not fit its edge's cross-section or its face.
    TooLarge {
        /// The edge, as an index into the build list.
        edge: usize,
        /// The face's surface kind.
        face: &'static str,
        need: Option<f64>,
        have: Option<f64>,
        best: Option<f64>,
    },
    /// Two blends' strips overlap on a face.
    Overlap {
        edges: [usize; 2],
        face: &'static str,
        need: [f64; 2],
        have: f64,
        best: Option<f64>,
    },
    /// A vertex the blends cannot be joined at.
    Vertex {
        at: V,
        edges: Vec<usize>,
        why: String,
    },
    /// Something the checks above should have caught.
    Failed(String),
}

/// The edges ending at a corner: (edge in the build list, 0 for its start
/// or 1 for its end).
type Ends = Vec<(usize, usize)>;

/// The edges around each vertex and the faces of each edge.
struct Topo {
    edge_faces: Vec<Vec<u32>>,
    vertex_edges: Vec<Vec<u32>>,
}

fn topo(b: &Brep) -> Topo {
    let mut edge_faces: Vec<Vec<u32>> = vec![Vec::new(); b.edges.len()];
    for (fi, f) in b.faces.iter().enumerate() {
        for l in &f.loops {
            for c in &l.coedges {
                let v = &mut edge_faces[c.edge as usize];
                if !v.contains(&(fi as u32)) {
                    v.push(fi as u32);
                }
            }
        }
    }
    let mut vertex_edges: Vec<Vec<u32>> = vec![Vec::new(); b.vertices.len()];
    for (ei, e) in b.edges.iter().enumerate() {
        if e.seam {
            continue;
        }
        vertex_edges[e.start as usize].push(ei as u32);
        if e.end != e.start {
            vertex_edges[e.end as usize].push(ei as u32);
        }
    }
    Topo {
        edge_faces,
        vertex_edges,
    }
}

/// The blend face of B-rep face `f` beside an edge through `p`.
fn blend_face(f: &Face, p: V) -> Option<BlendFace> {
    match &f.surface {
        Surface::Plane { origin, .. } => Some(BlendFace::Plane {
            origin: *origin,
            normal: curve::outward(f, p),
        }),
        Surface::Cylinder {
            origin,
            axis,
            radius,
        } => Some(BlendFace::Cylinder {
            origin: *origin,
            axis: unit(*axis),
            radius: *radius,
            convex: f.same_sense,
        }),
        _ => None,
    }
}

/// What a selection builds: the specification, and per edge of it the
/// edge's index in [`Facts::edges`].
#[derive(Debug, Clone, PartialEq)]
pub struct Built {
    pub spec: BlendSpec,
    pub edges: Vec<usize>,
}

/// The blend specification of the edges `list` (indices into the facts'
/// edges, all translational) for a call of `profile` and `size`, checked.
pub(crate) fn prepare(
    facts: &Facts,
    list: &[usize],
    profile: Profile,
    size: f64,
) -> Result<Built, Problem> {
    let b = &*facts.brep;
    let t = topo(b);
    let in_list: BTreeMap<u32, usize> = list
        .iter()
        .enumerate()
        .map(|(k, &i)| (facts.edges[i].brep_edge, k))
        .collect();
    let fact = |be: u32| facts.fact_of.get(be as usize).copied().flatten();
    let mut edges = Vec::with_capacity(list.len());
    let mut corners: Vec<Corner> = Vec::new();
    // Corner by vertex: its index and the edges registered so far.
    let mut corner_at: BTreeMap<u32, (usize, Vec<(usize, usize)>)> = BTreeMap::new();
    for (k, &i) in list.iter().enumerate() {
        let e = &facts.edges[i];
        let from = b.vertices[e.vertices[0] as usize];
        let to = b.vertices[e.vertices[1] as usize];
        let dir = unit(sub(to, from));
        let mid = mul(add(from, to), 0.5);
        let fa = &b.faces[e.brep_faces[0] as usize];
        let fb = &b.faces[e.brep_faces[1] as usize];
        let (Some(a), Some(bf)) = (blend_face(fa, mid), blend_face(fb, mid)) else {
            return Err(Problem::Failed(format!(
                "edge {} is not between planes and cylinders",
                k + 1
            )));
        };
        let convex = e.sense == Sense::Convex;
        let mut ends = [End::Open { face: None }, End::Open { face: None }];
        for (end, slot) in ends.iter_mut().enumerate() {
            let vi = e.vertices[end];
            let vp = b.vertices[vi as usize];
            let d_out = if end == 0 { mul(dir, -1.0) } else { dir };
            let around: Vec<u32> = t.vertex_edges[vi as usize]
                .iter()
                .copied()
                .filter(|&x| x != e.brep_edge)
                .collect();
            let ab = [e.brep_faces[0], e.brep_faces[1]];
            // A tangent edge at the vertex touching one of the edge's
            // faces: the edge continues smoothly into another one (a
            // line into an arc of a rounded outline). Cut across it.
            let chain = around.iter().any(|&x| {
                fact(x).is_some_and(|fi| facts.edges[fi as usize].sense == Sense::Smooth)
                    && t.edge_faces[x as usize].iter().any(|f| ab.contains(f))
            });
            if chain {
                *slot = End::Plane {
                    origin: vp,
                    normal: d_out,
                };
                continue;
            }
            let others: BTreeSet<u32> = around
                .iter()
                .flat_map(|&x| t.edge_faces[x as usize].iter().copied())
                .filter(|f| !ab.contains(f))
                .collect();
            let selected: Vec<u32> = around
                .iter()
                .copied()
                .filter(|x| in_list.contains_key(x))
                .collect();
            let vertex_problem = |why: &str| {
                let mut es = vec![k];
                es.extend(selected.iter().map(|x| in_list[x]));
                es.sort_unstable();
                Problem::Vertex {
                    at: vp,
                    edges: es,
                    why: why.to_string(),
                }
            };
            let sense_of = |x: u32| fact(x).map(|fi| facts.edges[fi as usize].sense);
            if selected.iter().any(|&x| sense_of(x) != Some(e.sense)) {
                return Err(vertex_problem(
                    "convex and concave edges meet, which one call cannot round",
                ));
            }
            let simple = others.len() == 1 && around.len() == 2;
            let third = others.iter().next().map(|&f| &b.faces[f as usize]);
            let third_plane = third.filter(|f| matches!(f.surface, Surface::Plane { .. }));
            match (simple, third_plane, selected.len()) {
                (true, Some(f), 0) => {
                    let nf = curve::outward(f, vp);
                    let away = dot(d_out, nf) > 0.0;
                    let origin = match &f.surface {
                        Surface::Plane { origin, .. } => *origin,
                        _ => vp,
                    };
                    *slot = if convex && away {
                        End::Open {
                            face: Some((origin, nf)),
                        }
                    } else {
                        End::Plane {
                            origin,
                            normal: if away { nf } else { mul(nf, -1.0) },
                        }
                    };
                }
                (true, Some(f), 1) => {
                    if convex {
                        // Both extended: the intersection of the two
                        // singly blended solids (section 7.3).
                        let nf = curve::outward(f, vp);
                        let origin = match &f.surface {
                            Surface::Plane { origin, .. } => *origin,
                            _ => vp,
                        };
                        *slot = if dot(d_out, nf) > 0.0 {
                            End::Open {
                                face: Some((origin, nf)),
                            }
                        } else {
                            End::Plane {
                                origin,
                                normal: mul(nf, -1.0),
                            }
                        };
                    } else {
                        // Mitred on the plane bisecting the two edges.
                        let o = &b.edges[selected[0] as usize];
                        let far = if o.start == vi { o.end } else { o.start };
                        let u_o = unit(sub(b.vertices[far as usize], vp));
                        let u_e = mul(d_out, -1.0);
                        let n = unit(sub(u_o, u_e));
                        if norm(sub(u_o, u_e)) < 1e-9 {
                            return Err(vertex_problem("the two edges run on in one line"));
                        }
                        *slot = End::Mitre {
                            origin: vp,
                            normal: n,
                            with: (in_list[&selected[0]], if o.start == vi { 0 } else { 1 }),
                        };
                    }
                }
                (true, Some(_), 2) => {
                    let planes = [&a, &bf]
                        .iter()
                        .all(|f| matches!(f, BlendFace::Plane { .. }));
                    match profile {
                        Profile::Fillet if planes => {
                            let next = corners.len() + corner_at.len();
                            let entry = corner_at.entry(vi).or_insert((next, Vec::new()));
                            entry.1.push((k, end));
                            *slot = End::Corner(entry.0);
                        }
                        Profile::Chamfer if convex => {
                            // Three chamfers on a convex corner: each
                            // extended, they meet in a point.
                            let f = third_plane.expect("matched");
                            let nf = curve::outward(f, vp);
                            let origin = match &f.surface {
                                Surface::Plane { origin, .. } => *origin,
                                _ => vp,
                            };
                            *slot = End::Open {
                                face: Some((origin, nf)),
                            };
                        }
                        _ => {
                            return Err(vertex_problem(if planes {
                                "three concave chamfers meet"
                            } else {
                                "a curved face meets the corner"
                            }));
                        }
                    }
                }
                _ if selected.is_empty() && convex => {
                    // The material ends if every other face there faces
                    // the way the edge leaves.
                    let open = others
                        .iter()
                        .all(|&f| dot(d_out, curve::outward(&b.faces[f as usize], vp)) > 0.0);
                    if open && !others.is_empty() {
                        *slot = End::Open { face: None };
                    } else {
                        return Err(vertex_problem(
                            "the edge runs into a curved face or a vertex of more than three faces",
                        ));
                    }
                }
                _ => {
                    return Err(vertex_problem(
                        "more than three faces or a curved face meet",
                    ));
                }
            }
        }
        edges.push(BlendEdge {
            from,
            to,
            faces: [a, bf],
            face_ids: e.brep_faces,
            convex,
            ends,
        });
    }
    // Corners in vertex order: renumber to the order they were met.
    let mut order: Vec<(usize, u32, Ends)> = corner_at
        .into_iter()
        .map(|(v, (i, es))| (i, v, es))
        .collect();
    order.sort_by_key(|x| x.0);
    for (i, v, es) in order {
        debug_assert_eq!(i, corners.len());
        if es.len() != 3 {
            return Err(Problem::Vertex {
                at: b.vertices[v as usize],
                edges: es.iter().map(|x| x.0).collect(),
                why: "the corner's edges could not all be blended".into(),
            });
        }
        corners.push(Corner {
            vertex: b.vertices[v as usize],
            edges: [es[0], es[1], es[2]],
        });
    }
    let spec = BlendSpec {
        profile,
        size,
        edges,
        corners,
    };
    let built = Built {
        spec,
        edges: list.to_vec(),
    };
    match check(facts, &t, &built) {
        Ok(()) => Ok(built),
        Err(mut p) => {
            let best = largest(facts, &t, &built);
            match &mut p {
                Problem::TooLarge { best: b, .. } | Problem::Overlap { best: b, .. } => *b = best,
                _ => {}
            }
            Err(p)
        }
    }
}

/// The checks before any boolean, at the specification's size.
fn check(facts: &Facts, t: &Topo, built: &Built) -> Result<(), Problem> {
    let spec = &built.spec;
    let b = &*facts.brep;
    let mut sections: Vec<Section> = Vec::with_capacity(spec.edges.len());
    for i in 0..spec.edges.len() {
        match blend::section(spec, i) {
            Ok(s) => sections.push(s),
            Err(BlendError::TooLarge(_)) => {
                let fi = built.edges[i];
                let kinds = facts.edges[fi].faces;
                return Err(Problem::TooLarge {
                    edge: i,
                    face: if kinds[0] == "plane" {
                        kinds[1]
                    } else {
                        kinds[0]
                    },
                    need: None,
                    have: None,
                    best: None,
                });
            }
            Err(e) => return Err(Problem::Failed(e.to_string())),
        }
    }
    // Per (face, edge): the strip's width.
    let mut width: BTreeMap<(u32, u32), f64> = BTreeMap::new();
    for (i, s) in sections.iter().enumerate() {
        let e = &facts.edges[built.edges[i]];
        for k in 0..2 {
            width.insert((e.brep_faces[k], e.brep_edge), s.widths[k]);
        }
    }
    let tol = facts.tolerance;
    for (i, s) in sections.iter().enumerate() {
        let e = &facts.edges[built.edges[i]];
        let be = &spec.edges[i];
        let (from, to) = (be.from, be.to);
        let d = unit(sub(to, from));
        let near: BTreeSet<u32> = e
            .vertices
            .iter()
            .flat_map(|&v| t.vertex_edges[v as usize].iter().copied())
            .collect();
        for k in 0..2 {
            let fid = e.brep_faces[k];
            let face = &b.faces[fid as usize];
            let tangent = s.tangents[k];
            // The way into the face, as a direction (plane) or a turn
            // about the axis (cylinder).
            let into = {
                let w = sub(tangent, from);
                unit(sub(w, mul(d, dot(w, d))))
            };
            let mut best: Option<(f64, u32)> = None;
            for l in &face.loops {
                for c in &l.coedges {
                    let other = c.edge;
                    if other == e.brep_edge || near.contains(&other) {
                        continue;
                    }
                    let oe = &b.edges[other as usize];
                    if oe.seam {
                        continue;
                    }
                    let n = curve::sample_count(oe).max(1);
                    let pts = curve::samples(oe, n);
                    for frac in [0.1, 0.3, 0.5, 0.7, 0.9] {
                        let p = add(from, mul(sub(to, from), frac));
                        for w in pts.windows(2) {
                            let (sa, sb) = (dot(sub(w[0], p), d), dot(sub(w[1], p), d));
                            if (sa > 0.0 && sb > 0.0) || (sa < 0.0 && sb < 0.0) || sa == sb {
                                continue;
                            }
                            let h = add(w[0], mul(sub(w[1], w[0]), sa / (sa - sb)));
                            let dist = match &face.surface {
                                Surface::Cylinder {
                                    origin,
                                    axis,
                                    radius,
                                } => {
                                    let ax = unit(*axis);
                                    let rad = |x: V| {
                                        let q = sub(x, *origin);
                                        unit(sub(q, mul(ax, dot(q, ax))))
                                    };
                                    let (r0, rt, rh) = (rad(p), rad(tangent), rad(h));
                                    // libm's, as everywhere here: the
                                    // same bits on wasm32.
                                    let turn =
                                        |u: V, v: V| libm::atan2(dot(cross(u, v), ax), dot(u, v));
                                    let sign = if turn(r0, rt) >= 0.0 { 1.0 } else { -1.0 };
                                    let mut a = turn(r0, rh) * sign;
                                    if a <= 0.0 {
                                        a += std::f64::consts::TAU;
                                    }
                                    a * radius
                                }
                                _ => dot(sub(h, p), into),
                            };
                            if dist > tol && best.is_none_or(|(x, _)| dist < x) {
                                best = Some((dist, other));
                            }
                        }
                    }
                }
            }
            let Some((have, by)) = best else { continue };
            let need = s.widths[k];
            let theirs = width.get(&(fid, by)).copied();
            match theirs {
                Some(w2) if need + w2 > have - tol => {
                    let j = built
                        .edges
                        .iter()
                        .position(|&fi| facts.edges[fi].brep_edge == by)
                        .unwrap_or(i);
                    return Err(Problem::Overlap {
                        edges: [i.min(j), i.max(j)],
                        face: facts.edges[built.edges[i]].faces[k],
                        need: [need, w2],
                        have,
                        best: None,
                    });
                }
                None if need > have - tol => {
                    return Err(Problem::TooLarge {
                        edge: i,
                        face: facts.edges[built.edges[i]].faces[k],
                        need: Some(need),
                        have: Some(have),
                        best: None,
                    });
                }
                _ => {}
            }
        }
    }
    // The tools themselves, cheaply: end caps that cross are an edge too
    // short for the blend.
    match blend::tools(spec, &|_| 1) {
        Ok(_) => Ok(()),
        Err(BlendError::TooShort(i)) | Err(BlendError::TooLarge(i)) => Err(Problem::TooLarge {
            edge: i,
            face: "edge",
            need: None,
            have: None,
            best: None,
        }),
        Err(e) => Err(Problem::Failed(e.to_string())),
    }
}

/// The largest size below the specification's that passes [`check`], as
/// a number a person would write: three significant digits, rounded to
/// nearest when that passes, else down. `None` when nothing passes (a
/// problem that does not depend on the size).
fn largest(facts: &Facts, t: &Topo, built: &Built) -> Option<f64> {
    let at = |s: f64| {
        let mut b = built.clone();
        b.spec.size = s;
        check(facts, t, &b).is_ok()
    };
    let mut lo = 0.0;
    let mut hi = built.spec.size;
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if at(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    if lo <= 0.0 || !at(lo) {
        return None;
    }
    let digits = |x: f64, down: bool| {
        let e = libm::floor(libm::log10(x.abs())) as i32 - 2;
        let p = libm::pow(10.0, f64::from(e));
        let y = if down {
            (x / p).floor()
        } else {
            (x / p).round()
        } * p;
        // Printing-clean: reparse the shortest decimal.
        format!("{:.*}", (-e).max(0) as usize, y)
            .parse::<f64>()
            .unwrap_or(y)
    };
    let near = digits(lo, false);
    if near > 0.0 && near < built.spec.size && at(near) {
        return Some(near);
    }
    let down = digits(lo, true);
    (down > 0.0 && at(down)).then_some(down)
}

/// The tools of a built plan; `segments(sweep)` per fillet arc.
pub(crate) fn tools(
    built: &Built,
    segments: &dyn Fn(f64) -> u32,
) -> Result<Vec<blend::Tool>, String> {
    blend::tools(&built.spec, segments).map_err(|e| e.to_string())
}
