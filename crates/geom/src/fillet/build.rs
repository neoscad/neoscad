//! Stages F2 and F3 of `docs/fillets.md`: the blends of the translational
//! class (straight edges between planes, or cylinders parallel to them)
//! and the rotational class (circles and arcs between surfaces of
//! revolution about one axis), from the child's B-rep to
//! `meshbrep::blend`'s specification, with the checks that run before any
//! boolean.
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
//!   are a sphere corner. Where two selected edges run on into each other
//!   (lines and arcs of one outline, or the pieces of one circle) their
//!   tools are joined. An arc ends only so, or on a plane through its
//!   axis. Anything else is `fillet-unsupported-vertex`.
//! - **The size checks** (section 8): every blend must fit its
//!   cross-section, its strip on each face must stay inside the face, and
//!   two strips on one face must not overlap, before anything is built.
//!   On failure the largest size that fits is found by bisection over the
//!   same checks, and the hint offers a little less, which passes them.

use std::collections::{BTreeMap, BTreeSet};

use meshbrep::blend::{
    self, BlendEdge, BlendError, BlendFace, BlendSpec, Corner, End, Path, Profile, Section,
};
use meshbrep::{Brep, Curve, Edge, Face, Surface};

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
        /// The size the hint writes, and the largest that fits.
        best: Option<[f64; 2]>,
    },
    /// Two blends' strips overlap on a face.
    Overlap {
        edges: [usize; 2],
        face: &'static str,
        need: [f64; 2],
        have: f64,
        /// The size the hint writes, and the largest that fits.
        best: Option<[f64; 2]>,
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

/// The blend face of B-rep face `f` beside an edge through `p`: its exact
/// surface and which side the material is on.
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
        Surface::Cone { apex, axis, slope } => Some(BlendFace::Cone {
            apex: *apex,
            axis: unit(*axis),
            slope: *slope,
            convex: f.same_sense,
        }),
        Surface::Sphere { center, radius } => Some(BlendFace::Sphere {
            center: *center,
            radius: *radius,
            convex: f.same_sense,
        }),
        Surface::Torus {
            center,
            axis,
            major_radius,
            minor_radius,
        } => Some(BlendFace::Torus {
            center: *center,
            axis: unit(*axis),
            major_radius: *major_radius,
            minor_radius: *minor_radius,
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

/// An arc's axis: its circle's centre, the unit axis it turns
/// counter-clockwise about from its start to its end, the angle, and the
/// radius.
fn arc_of(e: &Edge) -> Option<(V, V, f64, f64)> {
    let Curve::Circle {
        center,
        normal,
        radius,
        ..
    } = &e.curve
    else {
        return None;
    };
    let [t0, t1] = e.range;
    let n = unit(*normal);
    if t1 >= t0 {
        Some((*center, n, t1 - t0, *radius))
    } else {
        Some((*center, mul(n, -1.0), t0 - t1, *radius))
    }
}

/// The unit direction in which B-rep edge `x` leaves vertex `v` (one of
/// its ends): along a line, or along an arc's tangent there.
fn leaving(b: &Brep, x: u32, v: u32) -> V {
    let e = &b.edges[x as usize];
    let at_start = e.start == v;
    let t = if at_start { e.range[0] } else { e.range[1] };
    let d = curve::tangent(&e.curve, t, e.range);
    if at_start { d } else { mul(d, -1.0) }
}

/// The blend specification of the edges `list` (indices into the facts'
/// edges, translational or rotational) for a call of `profile` and
/// `size`, checked.
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
        let be = &b.edges[e.brep_edge as usize];
        let from = b.vertices[e.vertices[0] as usize];
        let to = b.vertices[e.vertices[1] as usize];
        let arc = arc_of(be);
        let fa = &b.faces[e.brep_faces[0] as usize];
        let fb = &b.faces[e.brep_faces[1] as usize];
        // The faces' outward sides, where the edge starts (a line's
        // middle: planes are planes).
        let at = if arc.is_some() {
            from
        } else {
            mul(add(from, to), 0.5)
        };
        let (Some(a), Some(bf)) = (blend_face(fa, at), blend_face(fb, at)) else {
            return Err(Problem::Failed(format!(
                "edge {} is not between planes and surfaces of revolution",
                k + 1
            )));
        };
        let convex = e.sense == Sense::Convex;
        let mut ends = [End::Open { face: None }, End::Open { face: None }];
        let closed = be.start == be.end;
        for (end, slot) in ends.iter_mut().enumerate() {
            if closed {
                // A whole circle has no ends.
                break;
            }
            let vi = e.vertices[end];
            let vp = b.vertices[vi as usize];
            // Out of the edge past this end.
            let d_out = mul(leaving(b, e.brep_edge, vi), -1.0);
            let around: Vec<u32> = t.vertex_edges[vi as usize]
                .iter()
                .copied()
                .filter(|&x| x != e.brep_edge)
                .collect();
            let ab = [e.brep_faces[0], e.brep_faces[1]];
            let selected: Vec<u32> = around
                .iter()
                .copied()
                .filter(|x| in_list.contains_key(x))
                .collect();
            // A tangent edge at the vertex touching one of the edge's
            // faces: the edge continues smoothly into another one (a
            // line into an arc of a rounded outline). Cut across it, and
            // when the edge it runs on into is selected too, join the
            // two tools there (7.2).
            // Or the edge runs on into another edge between the same two
            // faces: one circle split at the vertices where its faces'
            // seams reach it (a countersink's cone meeting its hole).
            let chain = around.iter().any(|&x| {
                let faces = &t.edge_faces[x as usize];
                (fact(x).is_some_and(|fi| facts.edges[fi as usize].sense == Sense::Smooth)
                    && faces.iter().any(|f| ab.contains(f)))
                    || (faces.len() == 2
                        && ab.iter().all(|f| faces.contains(f))
                        && dot(leaving(b, x, vi), d_out) > 1.0 - 1e-6)
            });
            if chain {
                let next = selected.iter().copied().find(|&x| {
                    let shares = t.edge_faces[x as usize].iter().any(|f| ab.contains(f));
                    let same = fact(x).is_some_and(|fi| facts.edges[fi as usize].sense == e.sense);
                    let ox = &b.edges[x as usize];
                    shares
                        && same
                        && ox.start != ox.end
                        && dot(leaving(b, x, vi), d_out) > 1.0 - 1e-6
                });
                *slot = match (next, arc) {
                    (Some(x), _) => End::Chain {
                        with: (
                            in_list[&x],
                            if b.edges[x as usize].start == vi {
                                0
                            } else {
                                1
                            },
                        ),
                    },
                    // Across the edge at the vertex: for an arc, the
                    // plane through its axis there, taken through the
                    // vertex itself, which is the neighbour's too.
                    (None, _) => End::Plane {
                        origin: vp,
                        normal: d_out,
                    },
                };
                continue;
            }
            let others: BTreeSet<u32> = around
                .iter()
                .flat_map(|&x| t.edge_faces[x as usize].iter().copied())
                .filter(|f| !ab.contains(f))
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
            if let Some((c, axis, _, _)) = arc {
                // An arc ends on a plane through its axis (a half hole at
                // a plate's edge), into the air or against it.
                let through = third_plane.filter(|f| {
                    let Surface::Plane { origin, normal } = &f.surface else {
                        return false;
                    };
                    let n = unit(*normal);
                    dot(n, axis).abs() <= 1e-6 && dot(sub(c, *origin), n).abs() <= facts.tolerance
                });
                match (simple, through, selected.len()) {
                    (true, Some(f), 0) => {
                        let nf = curve::outward(f, vp);
                        let away = dot(d_out, nf) > 0.0;
                        *slot = if convex && away {
                            End::Open {
                                face: Some((c, nf)),
                            }
                        } else {
                            End::Plane {
                                origin: c,
                                normal: if away { nf } else { mul(nf, -1.0) },
                            }
                        };
                    }
                    _ => {
                        return Err(vertex_problem(if selected.is_empty() {
                            "an arc's blend ends only where it runs on smoothly or on a plane through its axis"
                        } else {
                            "an arc's blend meets another blend only where the two run on smoothly"
                        }));
                    }
                }
                continue;
            }
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
                (true, Some(f), 1)
                    if !matches!(b.edges[selected[0] as usize].curve, Curve::Circle { .. }) =>
                {
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
                (true, Some(_), 2)
                    if selected
                        .iter()
                        .all(|&x| !matches!(b.edges[x as usize].curve, Curve::Circle { .. })) =>
                {
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
                _ if selected
                    .iter()
                    .any(|&x| matches!(b.edges[x as usize].curve, Curve::Circle { .. })) =>
                {
                    return Err(vertex_problem(
                        "an arc's blend meets another blend only where the two run on smoothly",
                    ));
                }
                _ => {
                    return Err(vertex_problem(
                        "more than three faces or a curved face meet",
                    ));
                }
            }
        }
        let (path, margin) = match arc {
            Some((c, axis, sweep, radius)) => (
                Path::Arc {
                    center: c,
                    axis,
                    radius,
                    sweep: sweep.min(std::f64::consts::TAU),
                    sections: Vec::new(),
                },
                (!convex).then(|| wall(facts, e, c, axis, size)).flatten(),
            ),
            None => (Path::Line, None),
        };
        edges.push(BlendEdge {
            from,
            to,
            faces: [a, bf],
            face_ids: e.brep_faces,
            convex,
            ends,
            path,
            margin,
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

/// How far a concave arc's tool may overlap into the material behind its
/// curved face: a coaxial face of the child just behind it (the inside of
/// a tube, a lip's inner wall) limits it to half the gap, so the tool's
/// overlap never reaches through the wall and adds material on its far
/// side. `None` when nothing coaxial is that close.
fn wall(facts: &Facts, e: &super::EdgeFact, c: V, axis: V, size: f64) -> Option<f64> {
    let b = &*facts.brep;
    let rho = {
        let q = sub(e.from, c);
        norm(sub(q, mul(axis, dot(q, axis))))
    };
    let mut best: Option<f64> = None;
    for (fi, f) in b.faces.iter().enumerate() {
        if e.brep_faces.contains(&(fi as u32)) {
            continue;
        }
        let (origin, ax, radius) = match &f.surface {
            Surface::Cylinder {
                origin,
                axis,
                radius,
            } => (*origin, unit(*axis), *radius),
            _ => continue,
        };
        let off = sub(origin, c);
        let on_axis = norm(sub(off, mul(axis, dot(off, axis)))) <= facts.tolerance;
        if !on_axis || norm(cross(ax, axis)) > 1e-6 {
            continue;
        }
        let gap = (rho - radius).abs();
        if gap > facts.tolerance && gap < 2.0 * size {
            best = Some(best.map_or(0.5 * gap, |x: f64| x.min(0.5 * gap)));
        }
    }
    best
}

/// One place along an edge where the checks look across it: the point,
/// the edge's direction there, and the tangent points of its blend in
/// that cross-section.
struct Across {
    p: V,
    d: V,
    tangents: [V; 2],
}

/// Turns `p` about the axis through `c` along unit `a` by `t` radians
/// (`libm`'s trig, the same bits on wasm32).
fn turn(p: V, c: V, a: V, t: f64) -> V {
    let q = sub(p, c);
    let along = mul(a, dot(q, a));
    let r = sub(q, along);
    let s = cross(a, r);
    add(
        c,
        add(along, add(mul(r, libm::cos(t)), mul(s, libm::sin(t)))),
    )
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
        let near: BTreeSet<u32> = e
            .vertices
            .iter()
            .flat_map(|&v| t.vertex_edges[v as usize].iter().copied())
            .collect();
        let arc = match &be.path {
            Path::Arc {
                center,
                axis,
                sweep,
                ..
            } => Some((*center, *axis, *sweep)),
            Path::Line => None,
        };
        // Where to look across the edge.
        let at = |frac: f64| -> Across {
            match arc {
                None => Across {
                    p: add(from, mul(sub(to, from), frac)),
                    d: unit(sub(to, from)),
                    tangents: s.tangents,
                },
                Some((c, a, sweep)) => {
                    let th = sweep * frac;
                    let p = turn(from, c, a, th);
                    let r = sub(p, c);
                    Across {
                        p,
                        d: unit(cross(a, sub(r, mul(a, dot(r, a))))),
                        tangents: s.tangents.map(|x| turn(x, c, a, th)),
                    }
                }
            }
        };
        let fracs: Vec<f64> = match arc {
            None => vec![0.1, 0.3, 0.5, 0.7, 0.9],
            Some((_, _, sweep)) => {
                let n = ((64.0 * sweep / std::f64::consts::TAU).ceil() as usize).max(8);
                (0..n).map(|j| (j as f64 + 0.5) / n as f64).collect()
            }
        };
        for k in 0..2 {
            let fid = e.brep_faces[k];
            let face = &b.faces[fid as usize];
            // How far into face `k` the point `h` of its boundary is, from
            // the edge at `x` (on its cross-section through `h`): a
            // distance on a plane (and, for an arc, along a cylinder's or
            // a cone's generator), an arc length on a cylinder beside a
            // line or on a sphere or torus beside an arc. `None` when `h`
            // is not on the blend's side.
            let dist = |x: &Across, h: V| -> Option<f64> {
                let into = {
                    let w = sub(x.tangents[k], x.p);
                    unit(sub(w, mul(x.d, dot(w, x.d))))
                };
                let round = |centre: V, radius: f64, axis: V| {
                    // The turn about `axis` from the edge to `h`, the way
                    // the strip goes.
                    let rad = |y: V| {
                        let q = sub(y, centre);
                        unit(sub(q, mul(axis, dot(q, axis))))
                    };
                    let (r0, rt, rh) = (rad(x.p), rad(x.tangents[k]), rad(h));
                    // libm's, as everywhere here: the same bits on wasm32.
                    let turn = |u: V, v: V| libm::atan2(dot(cross(u, v), axis), dot(u, v));
                    let sign = if turn(r0, rt) >= 0.0 { 1.0 } else { -1.0 };
                    let mut a = turn(r0, rh) * sign;
                    if a <= 0.0 {
                        a += std::f64::consts::TAU;
                    }
                    a * radius
                };
                let v = match (&face.surface, arc) {
                    (
                        Surface::Cylinder {
                            origin,
                            axis,
                            radius,
                        },
                        None,
                    ) => round(*origin, *radius, unit(*axis)),
                    (Surface::Sphere { center, radius }, Some(_)) => round(*center, *radius, x.d),
                    (
                        Surface::Torus {
                            center,
                            major_radius,
                            minor_radius,
                            ..
                        },
                        Some((_, a, _)),
                    ) => {
                        let q = sub(x.p, *center);
                        let r = unit(sub(q, mul(a, dot(q, a))));
                        round(add(*center, mul(r, *major_radius)), *minor_radius, x.d)
                    }
                    _ => dot(sub(h, x.p), into),
                };
                (v > tol).then_some(v)
            };
            let mut best: Option<(f64, u32)> = None;
            let mut consider = |v: Option<f64>, by: u32| {
                if let Some(v) = v
                    && best.is_none_or(|(x, _)| v < x)
                {
                    best = Some((v, by));
                }
            };
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
                    // Rays across the edge at fixed places.
                    for &frac in &fracs {
                        let x = at(frac);
                        for w in pts.windows(2) {
                            let (sa, sb) = (dot(sub(w[0], x.p), x.d), dot(sub(w[1], x.p), x.d));
                            if (sa > 0.0 && sb > 0.0) || (sa < 0.0 && sb < 0.0) || sa == sb {
                                continue;
                            }
                            let h = add(w[0], mul(sub(w[1], w[0]), sa / (sa - sb)));
                            if let Some((c, a, _)) = arc {
                                // The meridian plane is two half-planes:
                                // only the edge's own counts.
                                let q = sub(h, c);
                                let r = sub(x.p, c);
                                if dot(sub(q, mul(a, dot(q, a))), sub(r, mul(a, dot(r, a)))) <= 0.0
                                {
                                    continue;
                                }
                            }
                            consider(dist(&x, h), other);
                        }
                    }
                    // Around an arc, also every boundary point in its own
                    // meridian, and each segment's point nearest the axis:
                    // the closest approach between the rays above.
                    if let Some((c, a, sweep)) = arc {
                        let r0 = {
                            let q = sub(from, c);
                            unit(sub(q, mul(a, dot(q, a))))
                        };
                        let mut cands: Vec<V> = pts.clone();
                        for w in pts.windows(2) {
                            let dv = sub(w[1], w[0]);
                            let rel = |y: V| {
                                let q = sub(y, c);
                                sub(q, mul(a, dot(q, a)))
                            };
                            let (p0, dr) = (rel(w[0]), sub(rel(w[1]), rel(w[0])));
                            let dd = dot(dr, dr);
                            if dd > 0.0 {
                                let s = (-dot(p0, dr) / dd).clamp(0.0, 1.0);
                                cands.push(add(w[0], mul(dv, s)));
                            }
                        }
                        for h in cands {
                            let q = sub(h, c);
                            let rq = sub(q, mul(a, dot(q, a)));
                            if norm(rq) <= tol {
                                continue;
                            }
                            let th = libm::atan2(dot(cross(r0, rq), a), dot(r0, rq));
                            let th = if th < 0.0 {
                                th + std::f64::consts::TAU
                            } else {
                                th
                            };
                            if th > sweep {
                                continue;
                            }
                            consider(dist(&at(th / sweep), h), other);
                        }
                    } else {
                        // Along a line, every boundary point across from
                        // it too: a hole in the face beside the edge comes
                        // nearest between the five rays. A circle's
                        // nearest point to the edge is taken exactly.
                        let len = norm(sub(to, from));
                        let d = unit(sub(to, from));
                        let mut cands: Vec<V> = pts.clone();
                        if let Curve::Circle {
                            center,
                            normal,
                            x_axis,
                            radius,
                        } = &oe.curve
                        {
                            let x = at(0.5);
                            let w = {
                                let v = sub(x.tangents[k], x.p);
                                let v = sub(v, mul(d, dot(v, d)));
                                let n = unit(*normal);
                                unit(sub(v, mul(n, dot(v, n))))
                            };
                            if norm(w) > 0.0 {
                                let h = sub(*center, mul(w, *radius));
                                let y = cross(unit(*normal), *x_axis);
                                let q = sub(h, *center);
                                let mut t = libm::atan2(dot(q, y), dot(q, *x_axis));
                                let [t0, t1] = oe.range;
                                while t < t0 {
                                    t += std::f64::consts::TAU;
                                }
                                if t <= t1 {
                                    cands.push(h);
                                }
                            }
                        }
                        for h in cands {
                            let s = dot(sub(h, from), d) / len;
                            if s > 0.0 && s < 1.0 {
                                consider(dist(&at(s), h), other);
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

/// The size a hint offers, and the largest below the specification's
/// that passes [`check`], each as a number a person would write (three
/// significant digits). The offer is 5% under the largest, rounded to
/// nearest when that passes, else down: at the largest itself the blends
/// leave a sliver of face between them or beside a face's edge, which
/// prints as nothing and which the exact export's reconstruction, near
/// tangent along its whole length, does not survive. `None` when nothing
/// passes (a problem that does not depend on the size).
fn largest(facts: &Facts, t: &Topo, built: &Built) -> Option<[f64; 2]> {
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
    // Three significant digits, rounded down (-1), to nearest (0) or up.
    let digits = |x: f64, way: i8| {
        let e = libm::floor(libm::log10(x.abs())) as i32 - 2;
        let p = libm::pow(10.0, f64::from(e));
        let y = match way {
            -1 => (x / p).floor(),
            0 => (x / p).round(),
            _ => (x / p).ceil(),
        } * p;
        // Printing-clean: reparse the shortest decimal.
        format!("{:.*}", (-e).max(0) as usize, y)
            .parse::<f64>()
            .unwrap_or(y)
    };
    let limit = digits(lo, 1);
    let room = 0.95 * lo;
    let near = digits(room, 0);
    if near > 0.0 && near < built.spec.size && at(near) {
        return Some([near, limit]);
    }
    let down = digits(room, -1);
    (down > 0.0 && at(down)).then_some([down, limit])
}

/// The tools of a built plan; `segments(sweep)` per fillet arc.
pub(crate) fn tools(
    built: &Built,
    segments: &dyn Fn(f64) -> u32,
) -> Result<Vec<blend::Tool>, String> {
    blend::tools(&built.spec, segments).map_err(|e| e.to_string())
}
