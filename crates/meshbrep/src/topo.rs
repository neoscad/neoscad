//! The B-rep while it is being built: mutable, with the surfaces and mesh
//! chains each part came from, which the later passes (seams, parameter
//! curves) need and the published [`crate::Brep`] does not carry.

use crate::curve;
use crate::math::*;
use crate::model::{BSpline, Curve};
use crate::surf::{Param, Surf};

#[derive(Clone, Debug)]
pub(crate) struct TEdge {
    pub v0: usize,
    pub v1: usize,
    pub curve: Curve,
    pub range: [f64; 2],
    /// The faces on either side (the face using it forward first). Equal
    /// for a seam.
    pub faces: [usize; 2],
    /// The mesh points the edge was built from (empty for seams).
    pub chain: Vec<V>,
    pub seam: bool,
    /// Largest distance of samples from either surface.
    pub dev: f64,
    /// How far the mesh chain stands off the curve, across the surfaces
    /// (see `Report::max_chain_deviation`); zero where it does not count.
    pub chain_dev: f64,
}

impl TEdge {
    pub fn closed(&self) -> bool {
        self.v0 == self.v1
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TFace {
    pub surf: Surf,
    pub same_sense: bool,
    pub faceted: bool,
    /// Loops of (edge, forward).
    pub loops: Vec<Vec<(usize, bool)>>,
    /// Set by the parametrisation pass.
    pub param: Option<Param>,
    /// Per loop, per coedge: the parameter-space curve (edge direction).
    pub pcurves: Vec<Vec<Option<BSpline<2>>>>,
    pub outer: Vec<bool>,
    /// A torus face's mesh triangles (empty for other surfaces): which
    /// tube and axis angles the face covers decides where its frame puts
    /// the cuts of both periodic coordinates.
    pub tris: Vec<[V; 3]>,
    /// The input triangles the face was built from, so that a failure can
    /// be reported where it is (`Report::face_triangles`).
    pub source: Vec<u32>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Topo {
    pub verts: Vec<V>,
    /// Vertices a seam ends at: they may no longer move.
    pub pinned: Vec<bool>,
    pub edges: Vec<TEdge>,
    pub faces: Vec<TFace>,
    pub notes: Vec<String>,
}

/// How far `p` is from the closed-form edge `e` (a line, circle or
/// ellipse over its range); `None` for a B-spline.
fn distance_to(e: &TEdge, p: V) -> Option<f64> {
    if matches!(e.curve, Curve::BSpline(_)) {
        return None;
    }
    let mut t = curve::param_of(&e.curve, p);
    if !matches!(e.curve, Curve::Line { .. }) {
        // Angles come in (-π, π]; the range may be anywhere.
        while t < e.range[0] {
            t += TAU;
        }
        while t - TAU >= e.range[0] {
            t -= TAU;
        }
    }
    let ends = [e.range[0], e.range[1]].map(|r| (curve::eval(&e.curve, r) - p).len());
    Some(if t <= e.range[1] {
        (curve::eval(&e.curve, t) - p)
            .len()
            .min(ends[0])
            .min(ends[1])
    } else {
        ends[0].min(ends[1])
    })
}

/// Whether two edges with the same ends run along each other within
/// `tol`: samples of each lie on the other where it has a closed form
/// (two B-splines are not compared, and so never coincide here).
fn coincide(a: &TEdge, b: &TEdge, tol: f64) -> bool {
    let along = |x: &TEdge, y: &TEdge| -> Option<bool> {
        let pts = curve::sample(&y.curve, y.range, 16);
        pts.iter()
            .map(|&p| distance_to(x, p).map(|d| d < tol))
            .try_fold(true, |acc, ok| ok.map(|ok| acc && ok))
    };
    match (along(a, b), along(b, a)) {
        (Some(x), Some(y)) => x && y,
        (Some(x), None) | (None, Some(x)) => x,
        (None, None) => false,
    }
}

impl Topo {
    pub fn add_vertex(&mut self, p: V) -> usize {
        self.verts.push(p);
        self.pinned.push(false);
        self.verts.len() - 1
    }

    /// The vertex a coedge starts at.
    pub fn start(&self, (e, fwd): (usize, bool)) -> usize {
        let e = &self.edges[e];
        if fwd { e.v0 } else { e.v1 }
    }

    /// Splits edge `e` at curve parameter `t` (strictly inside its range)
    /// with a new vertex at `p`, updating every loop that uses it. Returns
    /// the new vertex.
    pub fn split_edge(&mut self, e: usize, t: f64, p: V) -> usize {
        let nv = self.add_vertex(p);
        let mut second = self.edges[e].clone();
        second.v0 = nv;
        second.range[0] = t;
        second.chain = Vec::new();
        self.edges[e].v1 = nv;
        self.edges[e].range[1] = t;
        let ne = self.edges.len();
        self.edges.push(second);
        for f in &mut self.faces {
            for lp in &mut f.loops {
                let mut out = Vec::with_capacity(lp.len() + 1);
                for &(x, fwd) in lp.iter() {
                    if x == e {
                        if fwd {
                            out.push((e, true));
                            out.push((ne, true));
                        } else {
                            out.push((ne, false));
                            out.push((e, false));
                        }
                    } else {
                        out.push((x, fwd));
                    }
                }
                *lp = out;
            }
        }
        nv
    }

    /// Collapses open edges shorter than `tol`, joining their two vertices.
    ///
    /// The mesh can leave two corners a hair apart where the exact model
    /// has one: an exact cylinder crossing the edge between two faceted
    /// planes meets it at one point, but the polygonal cylinder crosses
    /// that edge's triangles at two nearby points. Both corners solve onto
    /// the same three surfaces, i.e. the same point, leaving an edge of
    /// length ~1e-13 that readers reject (OCCT: an unorientable face).
    /// Returns how many were collapsed.
    ///
    /// A sliver triangle of the mesh whose three corners solve to two
    /// points (a face flush with a rotated one: the rotation's rounding
    /// leaves Manifold a sliver where the faces meet) has one short edge
    /// between distinct vertices and, once that collapses, another that
    /// starts and ends at the same vertex with no length. Those go too,
    /// in a second pass; the sliver is then a digon ([`Topo::drop_digons`])
    /// or loses its loop altogether.
    pub fn collapse_short_edges(&mut self, tol: f64) -> usize {
        let mut gone = vec![false; self.edges.len()];
        let mut n = 0;
        let length = |ed: &TEdge| -> f64 {
            curve::sample(&ed.curve, ed.range, 8)
                .windows(2)
                .map(|w| (w[1] - w[0]).len())
                .sum()
        };
        for e in 0..self.edges.len() {
            let ed = &self.edges[e];
            if ed.closed() || ed.seam {
                continue;
            }
            if length(ed) >= tol {
                continue;
            }
            let (keep, drop) = (ed.v0, ed.v1);
            for x in &mut self.edges {
                if x.v0 == drop {
                    x.v0 = keep;
                }
                if x.v1 == drop {
                    x.v1 = keep;
                }
            }
            gone[e] = true;
            n += 1;
        }
        for e in 0..self.edges.len() {
            let ed = &self.edges[e];
            if !gone[e]
                && !ed.seam
                && ed.closed()
                && matches!(ed.curve, Curve::Line { .. })
                && length(ed) < tol
            {
                gone[e] = true;
                n += 1;
            }
        }
        if n == 0 {
            return 0;
        }
        let mut renum = vec![usize::MAX; self.edges.len()];
        let mut kept = Vec::with_capacity(self.edges.len() - n);
        for (i, ed) in std::mem::take(&mut self.edges).into_iter().enumerate() {
            if !gone[i] {
                renum[i] = kept.len();
                kept.push(ed);
            }
        }
        self.edges = kept;
        for f in &mut self.faces {
            for lp in &mut f.loops {
                lp.retain(|&(e, _)| !gone[e]);
                for c in lp.iter_mut() {
                    c.0 = renum[c.0];
                }
            }
            f.loops.retain(|lp| !lp.is_empty());
        }
        n
    }

    /// Removes faces that are two straight edges between the same two
    /// vertices: slivers with no area, which is what a thin mesh triangle
    /// becomes once its short edge collapses. Their two edges are one
    /// line, so the faces either side now share the first, and the second
    /// goes. Returns the removed faces' indices, in order.
    ///
    /// They are common on faceted models (twelve files of the stop-rule
    /// corpora had them, `issue1165.scad` among the eligible ones). OCCT
    /// reads most of them, but the face has no area and no orientation:
    /// left in, it fails our own check for an outer loop. A whole face of
    /// two curved edges that run along each other between the same two
    /// vertices goes the same way (see the comment on `lines` below).
    ///
    /// Two digons on the same two edges are a closed bubble of no volume
    /// (a sliver of the mesh between two flush faces, one of them rotated:
    /// `example017.scad`'s tabs in their slots); both go, with their edges.
    /// And a digon that is one loop of a larger face is a slit of no
    /// width in it (the same tabs, where they come through the face): the
    /// loop goes, and the faces either side share one edge.
    pub fn drop_digons(&mut self, tol: f64) -> Vec<usize> {
        const NONE: usize = usize::MAX;
        let mut replace: Vec<Option<(usize, bool)>> = vec![None; self.edges.len()];
        let mut gone = vec![false; self.edges.len()];
        let mut touched = vec![false; self.edges.len()];
        let mut dropped = Vec::new();
        let mut slits: Vec<(usize, usize)> = Vec::new();
        for (fi, f) in self.faces.iter().enumerate() {
            for li in 0..f.loops.len() {
                if f.loops[li].len() != 2 {
                    continue;
                }
                let whole = f.loops.len() == 1;
                let (a, b) = (f.loops[li][0].0, f.loops[li][1].0);
                if a == b || touched[a] || touched[b] {
                    continue;
                }
                let (ea, eb) = (&self.edges[a], &self.edges[b]);
                let same_ends = ea.v0 != ea.v1
                    && ((ea.v0, ea.v1) == (eb.v0, eb.v1) || (ea.v0, ea.v1) == (eb.v1, eb.v0));
                // Two straight edges between the same corners coincide; so
                // do two curves that run along each other, which is what a
                // face of no area between tangent surfaces is: where a
                // cylinder ends on the equator of a sphere of its own
                // radius, the mesh's cap pokes out between the sphere's
                // polygon and its own in slivers, and exactly the cap and
                // both edges of each sliver are the equator (a BOSL2
                // `stroke()` joint). Only whole faces of curves go, not
                // slits.
                let lines = matches!(ea.curve, Curve::Line { .. })
                    && matches!(eb.curve, Curve::Line { .. })
                    || (whole && same_ends && coincide(ea, eb, 10.0 * tol));
                let other = |e: &TEdge| {
                    if e.faces[0] == fi {
                        e.faces[1]
                    } else {
                        e.faces[0]
                    }
                };
                let (fa, fb) = (other(ea), other(eb));
                if whole && lines && same_ends && fa == fb && fa != NONE && fa != fi {
                    let g = &self.faces[fa];
                    let twin = g.loops.len() == 1 && g.loops[0].len() == 2 && {
                        let mut e2 = [g.loops[0][0].0, g.loops[0][1].0];
                        e2.sort_unstable();
                        let mut e1 = [a, b];
                        e1.sort_unstable();
                        e1 == e2
                    };
                    if twin {
                        gone[a] = true;
                        gone[b] = true;
                        touched[a] = true;
                        touched[b] = true;
                        dropped.push(fi);
                        dropped.push(fa);
                    }
                    continue;
                }
                // The faces either side must be two others, or the edge left
                // would bound one face twice.
                if !lines
                    || !same_ends
                    || fa == fb
                    || [fa, fb].iter().any(|&x| x == NONE || x == fi)
                {
                    continue;
                }
                // A slit whose sides are on one surface, facing opposite
                // ways, runs along a fin of no thickness: two flush walls
                // that the exact model cancels and the mesh's rounding
                // kept. Closing the slit would write a solid OCCT rejects
                // (`example017.scad`). A whole digon there is a sliver
                // between the two, which goes as any other
                // (`issue1165.scad`).
                let (ga, gb) = (&self.faces[fa], &self.faces[fb]);
                // The two sides as oriented surfaces: equal planes (a
                // faceted one and an exact one count) facing opposite ways.
                let opposite = match (ga.surf, gb.surf) {
                    (Surf::Plane { n: na, .. }, Surf::Plane { n: nb, .. }) => {
                        let sa = if ga.same_sense { 1.0 } else { -1.0 };
                        let sb = if gb.same_sense { 1.0 } else { -1.0 };
                        ga.surf.same(&gb.surf, tol) && na.dot(nb) * sa * sb < 0.0
                    }
                    (a, b) => a == b && ga.same_sense != gb.same_sense,
                };
                if !whole && opposite {
                    continue;
                }
                replace[b] = Some((a, eb.v0 != ea.v0));
                touched[a] = true;
                touched[b] = true;
                if whole {
                    dropped.push(fi);
                } else {
                    slits.push((fi, li));
                }
            }
        }
        if dropped.is_empty() && slits.is_empty() {
            return dropped;
        }
        // Slit loops first, while the face numbers are still these.
        for &(fi, li) in slits.iter().rev() {
            self.faces[fi].loops.remove(li);
        }
        dropped.sort_unstable();
        dropped.dedup();
        let mut renum = vec![usize::MAX; self.edges.len()];
        let mut kept = Vec::with_capacity(self.edges.len());
        for (i, ed) in std::mem::take(&mut self.edges).into_iter().enumerate() {
            if replace[i].is_none() && !gone[i] {
                renum[i] = kept.len();
                kept.push(ed);
            }
        }
        self.edges = kept;
        let mut k = 0;
        self.faces.retain(|_| {
            let keep = dropped.binary_search(&k).is_err();
            k += 1;
            keep
        });
        for f in &mut self.faces {
            for lp in &mut f.loops {
                for c in lp.iter_mut() {
                    *c = match replace[c.0] {
                        Some((a, flip)) => (renum[a], c.1 != flip),
                        None => (renum[c.0], c.1),
                    };
                }
            }
        }
        // Which faces each edge separates, again (forward user first).
        for e in &mut self.edges {
            e.faces = [NONE, NONE];
        }
        for (fi, f) in self.faces.iter().enumerate() {
            for lp in &f.loops {
                for &(e, fwd) in lp {
                    self.edges[e].faces[usize::from(!fwd)] = fi;
                }
            }
        }
        dropped
    }

    /// Removes faces left with no loops by [`Topo::collapse_short_edges`]
    /// (a sliver whose every edge had no length), other than the lone
    /// spheres and tori that never had one. Returns the removed faces'
    /// indices, in order.
    pub fn drop_empty_faces(&mut self) -> Vec<usize> {
        const NONE: usize = usize::MAX;
        let dropped: Vec<usize> = (0..self.faces.len())
            .filter(|&f| {
                self.faces[f].loops.is_empty()
                    && !matches!(self.faces[f].surf, Surf::Sphere { .. } | Surf::Torus { .. })
            })
            .collect();
        if dropped.is_empty() {
            return dropped;
        }
        let mut k = 0;
        self.faces.retain(|_| {
            let keep = dropped.binary_search(&k).is_err();
            k += 1;
            keep
        });
        for e in &mut self.edges {
            e.faces = [NONE, NONE];
        }
        for (fi, f) in self.faces.iter().enumerate() {
            for lp in &f.loops {
                for &(e, fwd) in lp {
                    self.edges[e].faces[usize::from(!fwd)] = fi;
                }
            }
        }
        dropped
    }

    /// Moves the vertex of the closed conic edge `e` to the point at curve
    /// parameter `t`, keeping the curve.
    pub fn reseat_closed(&mut self, e: usize, t: f64) {
        let p = curve::eval(&self.edges[e].curve, t);
        let ed = &mut self.edges[e];
        match &mut ed.curve {
            Curve::Circle { center, x_axis, .. } => {
                let c = V::from(*center);
                let n = (p - c).norm();
                *x_axis = n.arr();
                ed.range = [0.0, TAU];
            }
            _ => ed.range = [t, t + TAU],
        }
        let v = ed.v0;
        self.verts[v] = p;
    }
}
