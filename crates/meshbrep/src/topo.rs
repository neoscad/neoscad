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
    pub fn collapse_short_edges(&mut self, tol: f64) -> usize {
        let mut gone = vec![false; self.edges.len()];
        let mut n = 0;
        for e in 0..self.edges.len() {
            let ed = &self.edges[e];
            if ed.closed() || ed.seam {
                continue;
            }
            let pts = curve::sample(&ed.curve, ed.range, 8);
            let len: f64 = pts.windows(2).map(|w| (w[1] - w[0]).len()).sum();
            if len >= tol {
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
    /// left in, it fails our own check for an outer loop.
    pub fn drop_digons(&mut self) -> Vec<usize> {
        const NONE: usize = usize::MAX;
        let mut replace: Vec<Option<(usize, bool)>> = vec![None; self.edges.len()];
        let mut touched = vec![false; self.edges.len()];
        let mut dropped = Vec::new();
        for (fi, f) in self.faces.iter().enumerate() {
            if f.loops.len() != 1 || f.loops[0].len() != 2 {
                continue;
            }
            let (a, b) = (f.loops[0][0].0, f.loops[0][1].0);
            if a == b || touched[a] || touched[b] {
                continue;
            }
            let (ea, eb) = (&self.edges[a], &self.edges[b]);
            let lines =
                matches!(ea.curve, Curve::Line { .. }) && matches!(eb.curve, Curve::Line { .. });
            let same_ends = ea.v0 != ea.v1
                && ((ea.v0, ea.v1) == (eb.v0, eb.v1) || (ea.v0, ea.v1) == (eb.v1, eb.v0));
            let other = |e: &TEdge| {
                if e.faces[0] == fi {
                    e.faces[1]
                } else {
                    e.faces[0]
                }
            };
            let (fa, fb) = (other(ea), other(eb));
            // The faces either side must be two others, or the edge left
            // would bound one face twice.
            if !lines || !same_ends || fa == fb || [fa, fb].iter().any(|&x| x == NONE || x == fi) {
                continue;
            }
            replace[b] = Some((a, eb.v0 != ea.v0));
            touched[a] = true;
            touched[b] = true;
            dropped.push(fi);
        }
        if dropped.is_empty() {
            return dropped;
        }
        let mut renum = vec![usize::MAX; self.edges.len()];
        let mut kept = Vec::with_capacity(self.edges.len());
        for (i, ed) in std::mem::take(&mut self.edges).into_iter().enumerate() {
            if replace[i].is_none() {
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
