//! A fast path for convex polygons that gives libtess2's exact output.
//!
//! Most faces OpenSCAD tessellates are convex: the quads of cubes,
//! cylinders, spheres and extrusions, and the caps of cylinders. For a
//! polygon that is strictly convex in libtess2's sweep plane, the sweep
//! (`tessComputeInterior`, most of the work) only marks the polygon's face
//! inside, and its last `FinishRegion` leaves the face's `anEdge` on the
//! edge leaving the rightmost vertex. What follows (the monotone fan, the
//! Delaunay flips and the output order) happens inside that face alone.
//!
//! So this computes the normal, projection and orientation exactly as
//! upstream does and then produces what `tessMeshTessellateMonoRegion`,
//! `tessMeshRefineDelaunay` and `OutputPolymesh` would, by one of three
//! routes, cheapest first:
//!
//! - a quad ([`quad`]): the loops traced once by hand, leaving four cases
//!   picked by comparisons and one Delaunay test;
//! - a polygon with no edge to flip ([`Convex::fan`] and
//!   [`Convex::fan_unchecked`]): the monotone fan on a linked list of the
//!   contour, which only ever cuts ears, testing each diagonal as its
//!   second triangle is cut (or not at all when the vertices lie close to
//!   a circle, see [`shape`]);
//! - otherwise the same steps on a small half-edge mesh of its own
//!   ([`Convex::build`] onwards), which handles the flips.
//!
//! The general mesh ([`super::Tess`]) pays for things this case never
//! needs: the bucket allocator's free lists, the global edge and vertex
//! lists, and the checks that stop a traversal of a broken mesh. That
//! matters because a large mesh is mostly quads and caps. This also skips
//! OpenSCAD's edge bookkeeping (see `openscad.rs`): the triangles of a
//! convex polygon all run the polygon's way round, so the flip test never
//! fires and every edge count returns to zero.
//!
//! "Strictly convex" is decided with a margin, so that none of the
//! sweep's floating-point tests can come out differently from the exact
//! geometry: the sine of every turn must exceed [`MARGIN`] (so no vertex
//! is nearly on the line through its neighbours, and no tip is nearly
//! folded back), every edge must be longer than a millionth of the
//! polygon's extent, and the edges must turn once around. The sweep plane
//! is an axis plane, so s and t are the input coordinates exactly, and
//! every orientation test the sweep evaluates starts from differences of
//! them: its rounding error is about 1e-7 of the products of the lengths
//! involved, a hundredth of the margin. Earlier margins were relative to
//! the coordinates' magnitude (which sent small faces far from the origin,
//! most of a large mesh, down the full sweep) and then to the extent
//! squared (which sent every polygon of more than about 150 sides, a
//! sphere's or a cylinder's caps, down it).
//!
//! Anything else takes the full sweep, and so does any polygon on which
//! the steps here meet something the convex case cannot produce (a face
//! that is not a triangle, a runaway loop): they are checked rather than
//! assumed. The equivalence was checked against the full path on tens of
//! millions of random polygons (quads and n-gons: axis-aligned,
//! grid-snapped, non-planar, far from the origin, nearly degenerate,
//! regular up to 2048 sides) and against OpenSCAD's own libtess2 on about
//! a million more, with no difference.

use super::geom::{edge_sign, is_locally_delaunay};
use super::{compute_normal, compute_normal4, dot, fma, sweep_units};

/// Minimum sine of the turn at each vertex.
const MARGIN: f64 = 1e-5;

/// A null link.
const NIL: u32 = u32::MAX;

/// A half-edge of the convex mesh. Pairs are `2k`, `2k + 1` as in the
/// general mesh; `Lprev` is `Onext->Sym`.
#[derive(Clone, Copy)]
struct Edge {
    lnext: u32,
    onext: u32,
    /// The vertex, as its position in the contour.
    org: u32,
    lface: u32,
    mark: bool,
}

#[derive(Clone, Copy)]
struct Face {
    next: u32,
    prev: u32,
    an_edge: u32,
    inside: bool,
}

/// What [`Convex::axis_pass`] learns: CheckOrientation's area and the
/// contour's bounding box in (s, t).
type AxisPass = (f32, (f32, f32), (f32, f32));

/// Storage for the fast path, reused from polygon to polygon.
#[derive(Default)]
pub(super) struct Convex {
    /// (s, t) per contour point.
    st: Vec<(f32, f32)>,
    e: Vec<Edge>,
    /// Face 0 is the list head, 1 the polygon, 2 the outside.
    f: Vec<Face>,
    stack: Vec<u32>,
    tris: Vec<[u32; 3]>,
    /// The fan's remaining polygon, as a linked list of contour positions
    /// (see [`Convex::fan`]).
    nxt: Vec<u32>,
    prv: Vec<u32>,
    /// For the edge leaving each vertex of the remaining polygon: the
    /// vertex opposite it in the triangle already cut off on its other
    /// side, or NIL for an edge of the contour.
    opp: Vec<u32>,
}

impl Convex {
    /// Tessellate one contour (at least four points, in input order) if it
    /// is strictly convex in the sweep plane, appending the triangles to
    /// `out` exactly as libtess2 and OpenSCAD's clean-up would produce
    /// them. `face` holds the indices the points came from, which the
    /// triangles are given in. Returns false, having appended nothing, for
    /// any other polygon.
    pub(super) fn tessellate(
        &mut self,
        pts: &[[f32; 3]],
        face: &[u32],
        out: &mut Vec<[u32; 3]>,
    ) -> bool {
        let n = pts.len();
        // Longer contours are rarely convex by the margin; the cap keeps
        // the loop budgets small.
        if !(4..=4096).contains(&n) {
            return false;
        }
        // The sweep plane. The normal matters here only through the axis it
        // picks and its sign on that axis. When every vertex has the same
        // coordinate on one axis (a face of a cube, a cylinder's cap, most
        // faces of an unrotated model), every cross product ComputeNormal
        // forms is zero off that axis, so it picks that axis, and its
        // sign only chooses whether t starts negated. CheckOrientation
        // then makes the contour's area positive either way (negating t
        // negates the area exactly; a zero area is sent to the full path
        // below), so the (s, t) below come out the same up to the sign of
        // a zero t, which nothing here can observe (every test on it
        // compares with zero or subtracts). That saves ComputeNormal's two
        // passes; other faces run it.
        let norm = match constant_axis(pts) {
            Some(k) => {
                let mut v = [0.0; 3];
                v[k] = 1.0;
                v
            }
            // The vertex list after `tessAddContour` holds p1, ...,
            // p(n-1), p0: each new vertex goes before the first one.
            // ComputeNormal's ties follow that order.
            None => compute_normal(&pts[1..], &pts[..1]),
        };
        let (s_unit, t_unit) = sweep_units(norm);
        let proj = |c: &[f32; 3]| (dot(*c, s_unit), dot(*c, t_unit));
        // CheckOrientation: the contour's face, from p0 round in input
        // order, must have a non-negative area, or t is flipped.
        self.st.clear();
        let first = proj(&pts[0]);
        self.st.push(first);
        let mut area = 0.0f32;
        let mut a = first;
        let (mut lo, mut hi) = (first, first);
        for c in &pts[1..] {
            let b = proj(c);
            area = fma(a.0 - b.0, a.1 + b.1, area);
            lo = (lo.0.min(b.0), lo.1.min(b.1));
            hi = (hi.0.max(b.0), hi.1.max(b.1));
            self.st.push(b);
            a = b;
        }
        area = fma(a.0 - first.0, a.1 + first.1, area);
        // A zero area (rounding can cancel it to zero far from the origin)
        // leaves t as the normal's sign set it, which the shortcut above
        // did not compute.
        if area == 0.0 {
            return false;
        }
        if area < 0.0 {
            for p in self.st.iter_mut() {
                p.1 = -p.1;
            }
        }
        // The extent, which negating t does not change.
        let m = (f64::from(hi.0) - f64::from(lo.0)).max(f64::from(hi.1) - f64::from(lo.1));
        let Some(cocircular) = shape(&self.st, m, n > 4) else {
            return false;
        };
        self.finish(face, cocircular, out)
    }

    /// [`Convex::tessellate`] for a contour of five or more points with
    /// the same coordinate on one axis (a cylinder's cap), reading the
    /// mesh's vertices directly: the point gathering and clean-up checks of
    /// `openscad.rs`, the projection and CheckOrientation's area in one
    /// pass, then [`shape`]. False, having appended nothing, if any
    /// condition fails; the caller then takes the general route, which
    /// handles every case.
    pub(super) fn tessellate_axis_aligned(
        &mut self,
        verts: &[[f32; 3]],
        face: &[u32],
        out: &mut Vec<[u32; 3]>,
    ) -> bool {
        let n = face.len();
        if !(5..=4096).contains(&n) {
            return false;
        }
        let (p0, p1, p2) = (
            verts[face[0] as usize],
            verts[face[1] as usize],
            verts[face[2] as usize],
        );
        let Some(k) = (0..3).find(|&k| p0[k] == p1[k] && p1[k] == p2[k]) else {
            return false;
        };
        // One copy of the pass per axis, so that the axis and the sweep
        // plane's unit vectors are constants.
        let pass = match k {
            0 => self.axis_pass::<0>(verts, face),
            1 => self.axis_pass::<1>(verts, face),
            _ => self.axis_pass::<2>(verts, face),
        };
        let Some((area, lo, hi)) = pass else {
            return false;
        };
        if area == 0.0 {
            return false;
        }
        if area < 0.0 {
            for p in self.st.iter_mut() {
                p.1 = -p.1;
            }
        }
        let m = (f64::from(hi.0) - f64::from(lo.0)).max(f64::from(hi.1) - f64::from(lo.1));
        let Some(cocircular) = shape(&self.st, m, true) else {
            return false;
        };
        self.finish(face, cocircular, out)
    }

    /// The pass of [`Convex::tessellate_axis_aligned`] for contours flat
    /// on axis `K`: fills `st` (t not yet oriented) and returns
    /// CheckOrientation's area and the bounding box, or None if the
    /// clean-up would change the face or a point is off the plane.
    fn axis_pass<const K: usize>(&mut self, verts: &[[f32; 3]], face: &[u32]) -> Option<AxisPass> {
        let mut norm = [0.0; 3];
        norm[K] = 1.0;
        let (s_unit, t_unit) = sweep_units(norm);
        let proj = |c: [f32; 3]| (dot(c, s_unit), dot(c, t_unit));
        let n = face.len();
        let p0 = verts[face[0] as usize];
        let level = p0[K];
        self.st.clear();
        let first = proj(p0);
        self.st.push(first);
        // Vertex 0's checks (its neighbours before it wrap round).
        let (ia0, ib0) = (face[n - 2], face[n - 1]);
        let mut bad = (ia0 == ib0) | (ia0 == face[0]) | !first.0.is_finite();
        let (mut ia, mut ib) = (ib0, face[0]);
        let mut area = 0.0f32;
        let (mut lo, mut hi) = (first, first);
        let mut a = first;
        // `extend` over a slice reserves once, where `push` would check the
        // capacity for every point.
        self.st.extend(face[1..].iter().map(|&ic| {
            let p = verts[ic as usize];
            let b = proj(p);
            // A non-finite coordinate makes s non-finite: every coordinate
            // enters the dot product, if only times zero.
            bad |= (ia == ib) | (ia == ic) | (p[K] != level) | !b.0.is_finite();
            (ia, ib) = (ib, ic);
            lo = (lo.0.min(b.0), lo.1.min(b.1));
            hi = (hi.0.max(b.0), hi.1.max(b.1));
            area = fma(a.0 - b.0, a.1 + b.1, area);
            a = b;
            b
        }));
        area = fma(a.0 - first.0, a.1 + first.1, area);
        if bad { None } else { Some((area, lo, hi)) }
    }

    /// The rest of the fast path, once `st` holds the contour in its final
    /// orientation, known to be strictly convex.
    fn finish(&mut self, face: &[u32], cocircular: bool, out: &mut Vec<[u32; 3]>) -> bool {
        self.tris.clear();
        let done = if cocircular {
            self.fan_unchecked()
        } else {
            self.fan()
        };
        if done {
            out.extend(self.tris.iter().map(|t| t.map(|k| face[k as usize])));
            return true;
        }
        self.build();
        self.tris.clear();
        if self.tessellate_mono_region(1) && self.refine_delaunay() && self.output() {
            out.extend(self.tris.iter().map(|t| t.map(|k| face[k as usize])));
            true
        } else {
            false
        }
    }

    #[inline]
    fn lnext(&self, e: u32) -> u32 {
        self.e[e as usize].lnext
    }
    #[inline]
    fn lprev(&self, e: u32) -> u32 {
        self.e[e as usize].onext ^ 1
    }
    #[inline]
    fn org(&self, e: u32) -> u32 {
        self.e[e as usize].org
    }
    #[inline]
    fn dst(&self, e: u32) -> u32 {
        self.e[(e ^ 1) as usize].org
    }
    #[inline]
    fn lface(&self, e: u32) -> u32 {
        self.e[e as usize].lface
    }
    #[inline]
    fn p(&self, v: u32) -> (f32, f32) {
        self.st[v as usize]
    }
    /// `VertLeq`
    #[inline]
    fn vert_leq(&self, u: u32, v: u32) -> bool {
        let (u, v) = (self.p(u), self.p(v));
        u.0 < v.0 || (u.0 == v.0 && u.1 <= v.1)
    }
    #[inline]
    fn edge_goes_left(&self, e: u32) -> bool {
        self.vert_leq(self.dst(e), self.org(e))
    }
    #[inline]
    fn edge_goes_right(&self, e: u32) -> bool {
        self.vert_leq(self.org(e), self.dst(e))
    }
    #[inline]
    fn edge_sign(&self, u: u32, v: u32, w: u32) -> f32 {
        let (u, v, w) = (self.p(u), self.p(v), self.p(w));
        edge_sign(u.0, u.1, v.0, v.1, w.0, w.1)
    }
    /// `EdgeIsInternal`
    #[inline]
    fn edge_is_internal(&self, e: u32) -> bool {
        self.f[self.lface(e ^ 1) as usize].inside
    }

    /// The whole pipeline for the common case where the Delaunay pass
    /// flips nothing, without a half-edge mesh. False, with `tris` in an
    /// unspecified state, if some edge would be flipped (or a loop runs
    /// away); the caller then runs the mesh version, which handles flips.
    ///
    /// `tessMeshTessellateMonoRegion` only ever cuts an ear (three
    /// consecutive vertices) off the face it is fanning, and every ear
    /// becomes a new face just before that face in the face list, starting
    /// at the new diagonal. So the region left is the contour minus the ear
    /// tips, kept here as a linked list, and an edge of it is named by its
    /// origin: `lnext` is `nxt`, `lprev` is `prv`, and the loop below is the
    /// upstream one with those substitutions. The output is the ears in the
    /// order they were cut, then the last triangle, starting at the edge
    /// the last cut left as the face's `anEdge`, which is exactly what
    /// `OutputPolymesh` reads when no edge is flipped.
    ///
    /// No flip happens if every diagonal passes `tesedgeIsLocallyDelaunay`
    /// (each is checked once, and only a flip queues more). A diagonal's
    /// two triangles are known when the second is cut, so each is checked
    /// then.
    fn fan(&mut self) -> bool {
        let n = self.st.len() as u32;
        self.nxt.clear();
        self.nxt.extend(1..n);
        self.nxt.push(0);
        self.prv.clear();
        self.prv.push(n - 1);
        self.prv.extend(0..n - 1);
        self.opp.clear();
        self.opp.resize(n as usize, NIL);
        let mut r = 0;
        for k in 1..n {
            if !self.vert_leq(k, r) {
                r = k;
            }
        }
        let mut budget = 4 * n as usize + 8;
        let mut tick = || {
            budget = budget.saturating_sub(1);
            budget > 0
        };
        let mut up = r;
        while self.vert_leq(self.nxt[up as usize], up) {
            if !tick() {
                return false;
            }
            up = self.prv[up as usize];
        }
        while self.vert_leq(up, self.nxt[up as usize]) {
            if !tick() {
                return false;
            }
            up = self.nxt[up as usize];
        }
        let mut lo = self.prv[up as usize];
        // The origin of the face's `anEdge`: each cut sets it to the new
        // diagonal's far side.
        let mut last = up;
        while self.nxt[up as usize] != lo {
            if !tick() {
                return false;
            }
            if self.vert_leq(self.nxt[up as usize], lo) {
                loop {
                    let m = self.nxt[lo as usize];
                    let w = self.nxt[m as usize];
                    if m == up || !(self.vert_leq(w, m) || self.edge_sign(lo, m, w) <= 0.0) {
                        break;
                    }
                    if !tick() || !self.cut_ear(lo) {
                        return false;
                    }
                    last = lo;
                }
                lo = self.prv[lo as usize];
            } else {
                loop {
                    let p = self.prv[up as usize];
                    if self.nxt[lo as usize] == up
                        || !(self.vert_leq(p, up)
                            || self.edge_sign(self.nxt[up as usize], up, p) >= 0.0)
                    {
                        break;
                    }
                    if !tick() || !self.cut_ear(p) {
                        return false;
                    }
                    last = p;
                    up = p;
                }
                up = self.nxt[up as usize];
            }
        }
        while self.nxt[self.nxt[lo as usize] as usize] != up {
            if !tick() || !self.cut_ear(lo) {
                return false;
            }
            last = lo;
        }
        // The last face: its three edges are all that is left.
        let a = last;
        let b = self.nxt[a as usize];
        let c = self.nxt[b as usize];
        if self.nxt[c as usize] != a {
            return false;
        }
        if !(self.edge_ok(a, c) && self.edge_ok(b, a) && self.edge_ok(c, b)) {
            return false;
        }
        self.tris.push([a, b, c]);
        true
    }

    /// [`Convex::fan`] without the Delaunay tests, for a polygon whose
    /// diagonals all pass them (see [`shape`]): the same steps on local
    /// slices, which is most of the work for a cylinder's cap.
    fn fan_unchecked(&mut self) -> bool {
        let n = self.st.len() as u32;
        self.nxt.clear();
        self.nxt.extend(1..n);
        self.nxt.push(0);
        self.prv.clear();
        self.prv.push(n - 1);
        self.prv.extend(0..n - 1);
        let st = &self.st[..];
        let tris = &mut self.tris;
        let nxt = &mut self.nxt[..];
        let prv = &mut self.prv[..];
        let p = |v: u32| st[v as usize];
        let leq = |u: u32, v: u32| {
            let (u, v) = (p(u), p(v));
            u.0 < v.0 || (u.0 == v.0 && u.1 <= v.1)
        };
        let sign = |u: u32, v: u32, w: u32| {
            let (u, v, w) = (p(u), p(v), p(w));
            edge_sign(u.0, u.1, v.0, v.1, w.0, w.1)
        };
        let mut r = 0;
        for k in 1..n {
            if !leq(k, r) {
                r = k;
            }
        }
        // Every step below either cuts one of the n - 2 ears or moves
        // `lo` or `up` one vertex along the contour.
        let mut budget = 4 * n + 8;
        let mut up = r;
        while leq(nxt[up as usize], up) {
            budget -= 1;
            if budget == 0 {
                return false;
            }
            up = prv[up as usize];
        }
        while leq(up, nxt[up as usize]) {
            budget -= 1;
            if budget == 0 {
                return false;
            }
            up = nxt[up as usize];
        }
        let mut lo = prv[up as usize];
        let mut last = up;
        // `cut_ear` without the checks.
        let mut cut = |nxt: &mut [u32], prv: &mut [u32], q: u32| {
            let m = nxt[q as usize];
            let w = nxt[m as usize];
            tris.push([w, q, m]);
            nxt[q as usize] = w;
            prv[w as usize] = q;
        };
        while nxt[up as usize] != lo {
            budget -= 1;
            if budget == 0 {
                return false;
            }
            if leq(nxt[up as usize], lo) {
                loop {
                    let m = nxt[lo as usize];
                    let w = nxt[m as usize];
                    if m == up || !(leq(w, m) || sign(lo, m, w) <= 0.0) {
                        break;
                    }
                    budget -= 1;
                    if budget == 0 {
                        return false;
                    }
                    cut(nxt, prv, lo);
                    last = lo;
                }
                lo = prv[lo as usize];
            } else {
                loop {
                    let q = prv[up as usize];
                    if nxt[lo as usize] == up
                        || !(leq(q, up) || sign(nxt[up as usize], up, q) >= 0.0)
                    {
                        break;
                    }
                    budget -= 1;
                    if budget == 0 {
                        return false;
                    }
                    cut(nxt, prv, q);
                    last = q;
                    up = q;
                }
                up = nxt[up as usize];
            }
        }
        while nxt[nxt[lo as usize] as usize] != up {
            budget -= 1;
            if budget == 0 {
                return false;
            }
            cut(nxt, prv, lo);
            last = lo;
        }
        let a = last;
        let b = nxt[a as usize];
        let c = nxt[b as usize];
        if nxt[c as usize] != a {
            return false;
        }
        tris.push([a, b, c]);
        true
    }

    /// [`Convex::tessellate`] for a quad, with fixed-size arrays so that
    /// every loop unrolls: most faces are quads.
    pub(super) fn tessellate_quad(
        &mut self,
        pts: [[f32; 3]; 4],
        face: &[u32],
        out: &mut Vec<[u32; 3]>,
    ) -> bool {
        // As in `tessellate`.
        // Without short-circuits, which would branch on data.
        let same = |k: usize| {
            (pts[1][k] == pts[0][k]) & (pts[2][k] == pts[0][k]) & (pts[3][k] == pts[0][k])
        };
        let axis = if same(0) {
            Some(0)
        } else if same(1) {
            Some(1)
        } else if same(2) {
            Some(2)
        } else {
            None
        };
        let norm = match axis {
            Some(k) => {
                let mut v = [0.0; 3];
                v[k] = 1.0;
                v
            }
            None => compute_normal4(&[pts[1], pts[2], pts[3], pts[0]]),
        };
        let (s_unit, t_unit) = sweep_units(norm);
        let mut st = pts.map(|c| (dot(c, s_unit), dot(c, t_unit)));
        let mut area = 0.0f32;
        for i in 0..4 {
            let (a, b) = (st[i], st[(i + 1) & 3]);
            area = fma(a.0 - b.0, a.1 + b.1, area);
        }
        if area == 0.0 {
            return false;
        }
        // Negating by a multiply, not a branch: -1 * t is exactly -t.
        let flip = if area < 0.0 { -1.0 } else { 1.0 };
        for p in &mut st {
            p.1 *= flip;
        }
        let (mut lo, mut hi) = (st[0], st[0]);
        for p in &st[1..] {
            lo = (lo.0.min(p.0), lo.1.min(p.1));
            hi = (hi.0.max(p.0), hi.1.max(p.1));
        }
        let m = (f64::from(hi.0) - f64::from(lo.0)).max(f64::from(hi.1) - f64::from(lo.1));
        if shape(&st, m, false).is_none() {
            return false;
        }
        let Some(t) = quad(&st) else {
            return false;
        };
        out.extend(t.iter().map(|t| t.map(|k| face[k as usize])));
        true
    }

    /// Cut the ear at `p`, `nxt[p]`, `nxt[nxt[p]]`, as `tessMeshConnect`
    /// does: the ear's face starts at its new diagonal, so it is output as
    /// (far end, p, middle). False if one of the ear's two contour-side
    /// edges fails the Delaunay test.
    fn cut_ear(&mut self, p: u32) -> bool {
        let m = self.nxt[p as usize];
        let w = self.nxt[m as usize];
        if !(self.edge_ok(p, w) && self.edge_ok(m, p)) {
            return false;
        }
        self.tris.push([w, p, m]);
        self.nxt[p as usize] = w;
        self.prv[w as usize] = p;
        self.opp[p as usize] = m;
        true
    }

    /// The edge leaving `u` is now in a second triangle, whose third vertex
    /// is `x`: if it is a diagonal, whether upstream finds it locally
    /// Delaunay.
    fn edge_ok(&self, u: u32, x: u32) -> bool {
        let y = self.opp[u as usize];
        if y == NIL {
            return true;
        }
        let (a, b) = (self.p(u), self.p(self.nxt[u as usize]));
        let (x, y) = (self.p(x), self.p(y));
        is_locally_delaunay([a, x, b], [a, y, b])
    }

    /// The mesh the sweep leaves for a convex contour: the loop of forward
    /// edges `2k` (from point k to k + 1) around the inside face, whose
    /// `anEdge` leaves the rightmost vertex (by `VertLeq`), and their mates
    /// around the outside face.
    fn build(&mut self) {
        let n = self.st.len() as u32;
        let fwd = |k: u32| 2 * (k % n);
        self.e.clear();
        for k in 0..n {
            let prev = fwd(k + n - 1);
            let next = fwd(k + 1);
            self.e.push(Edge {
                lnext: next,
                onext: prev ^ 1,
                org: k,
                lface: 1,
                mark: false,
            });
            self.e.push(Edge {
                lnext: prev ^ 1,
                onext: next,
                org: (k + 1) % n,
                lface: 2,
                mark: false,
            });
        }
        let mut r = 0;
        for k in 1..n {
            if !self.vert_leq(k, r) {
                r = k;
            }
        }
        self.f.clear();
        self.f.push(Face {
            next: 1,
            prev: 2,
            an_edge: NIL,
            inside: false,
        });
        self.f.push(Face {
            next: 2,
            prev: 0,
            an_edge: fwd(r),
            inside: true,
        });
        self.f.push(Face {
            next: 0,
            prev: 1,
            an_edge: 1,
            inside: false,
        });
    }

    /// `Splice(a, b)`
    fn splice(&mut self, a: u32, b: u32) {
        let a_onext = self.e[a as usize].onext;
        let b_onext = self.e[b as usize].onext;
        self.e[(a_onext ^ 1) as usize].lnext = b;
        self.e[(b_onext ^ 1) as usize].lnext = a;
        self.e[a as usize].onext = b_onext;
        self.e[b as usize].onext = a_onext;
    }

    /// `tessMeshConnect`, for two edges of the same face (the only way the
    /// monotone fan uses it); None otherwise, or if the new face's loop
    /// does not close.
    fn connect(&mut self, e_org: u32, e_dst: u32) -> Option<u32> {
        let lf = self.lface(e_org);
        if self.lface(e_dst) != lf {
            return None;
        }
        // `MakeEdge`: a pair forming its own loop.
        let e_new = self.e.len() as u32;
        let e_sym = e_new ^ 1;
        let blank = Edge {
            lnext: 0,
            onext: 0,
            org: 0,
            lface: lf,
            mark: false,
        };
        self.e.push(Edge {
            lnext: e_sym,
            onext: e_new,
            ..blank
        });
        self.e.push(Edge {
            lnext: e_new,
            onext: e_sym,
            ..blank
        });
        self.splice(e_new, self.lnext(e_org));
        self.splice(e_sym, e_dst);
        self.e[e_new as usize].org = self.dst(e_org);
        self.e[e_sym as usize].org = self.org(e_dst);
        self.f[lf as usize].an_edge = e_sym;
        // `MakeFace(eNew, lf)`: a new face before lf in the list.
        let f_new = self.f.len() as u32;
        let f_prev = self.f[lf as usize].prev;
        let inside = self.f[lf as usize].inside;
        self.f.push(Face {
            next: lf,
            prev: f_prev,
            an_edge: e_new,
            inside,
        });
        self.f[f_prev as usize].next = f_new;
        self.f[lf as usize].prev = f_new;
        let mut e = e_new;
        let mut steps = self.st.len();
        loop {
            self.e[e as usize].lface = f_new;
            e = self.lnext(e);
            if e == e_new {
                break;
            }
            steps = steps.checked_sub(1)?;
        }
        Some(e_new)
    }

    /// `tessMeshTessellateMonoRegion`: fan the polygon into triangles,
    /// from right to left. False if a loop runs away.
    fn tessellate_mono_region(&mut self, face: u32) -> bool {
        // Each loop step either walks the boundary once round or adds one
        // of the n - 3 diagonals.
        let mut budget = 4 * self.st.len() + 8;
        let mut tick = || {
            budget = budget.saturating_sub(1);
            budget > 0
        };
        let mut up = self.f[face as usize].an_edge;
        while self.vert_leq(self.dst(up), self.org(up)) {
            if !tick() {
                return false;
            }
            up = self.lprev(up);
        }
        while self.vert_leq(self.org(up), self.dst(up)) {
            if !tick() {
                return false;
            }
            up = self.lnext(up);
        }
        let mut lo = self.lprev(up);
        while self.lnext(up) != lo {
            if !tick() {
                return false;
            }
            if self.vert_leq(self.dst(up), self.org(lo)) {
                // up->Dst is on the left: form triangles from lo->Org.
                while self.lnext(lo) != up
                    && (self.edge_goes_left(self.lnext(lo))
                        || self.edge_sign(self.org(lo), self.dst(lo), self.dst(self.lnext(lo)))
                            <= 0.0)
                {
                    if !tick() {
                        return false;
                    }
                    let Some(e) = self.connect(self.lnext(lo), lo) else {
                        return false;
                    };
                    lo = e ^ 1;
                }
                lo = self.lprev(lo);
            } else {
                // lo->Org is on the left: form CCW triangles from up->Dst.
                while self.lnext(lo) != up
                    && (self.edge_goes_right(self.lprev(up))
                        || self.edge_sign(self.dst(up), self.org(up), self.org(self.lprev(up)))
                            >= 0.0)
                {
                    if !tick() {
                        return false;
                    }
                    let Some(e) = self.connect(up, self.lprev(up)) else {
                        return false;
                    };
                    up = e ^ 1;
                }
                up = self.lnext(up);
            }
        }
        // Fan the rest from the leftmost vertex.
        while self.lnext(self.lnext(lo)) != up {
            if !tick() {
                return false;
            }
            let Some(e) = self.connect(self.lnext(lo), lo) else {
                return false;
            };
            lo = e ^ 1;
        }
        true
    }

    /// `tesedgeIsLocallyDelaunay`
    fn locally_delaunay(&self, e: u32) -> bool {
        let s = e ^ 1;
        is_locally_delaunay(
            [
                self.p(self.org(self.lnext(e))),
                self.p(self.org(self.lnext(self.lnext(e)))),
                self.p(self.org(e)),
            ],
            [
                self.p(self.org(self.lnext(s))),
                self.p(self.org(self.lnext(self.lnext(s)))),
                self.p(self.org(s)),
            ],
        )
    }

    /// `tessMeshFlipEdge`. Vertex `anEdge`s are not kept: nothing after
    /// the sweep reads them.
    fn flip(&mut self, edge: u32) {
        let a0 = edge;
        let a1 = self.lnext(a0);
        let a2 = self.lnext(a1);
        let b0 = edge ^ 1;
        let b1 = self.lnext(b0);
        let b2 = self.lnext(b1);
        let a_opp = self.org(a2);
        let b_opp = self.org(b2);
        let fa = self.lface(a0);
        let fb = self.lface(b0);
        let e = &mut self.e;
        e[a0 as usize].org = b_opp;
        e[a0 as usize].onext = b1 ^ 1;
        e[b0 as usize].org = a_opp;
        e[b0 as usize].onext = a1 ^ 1;
        e[a2 as usize].onext = b0;
        e[b2 as usize].onext = a0;
        e[b1 as usize].onext = a2 ^ 1;
        e[a1 as usize].onext = b2 ^ 1;
        e[a0 as usize].lnext = a2;
        e[a2 as usize].lnext = b1;
        e[b1 as usize].lnext = a0;
        e[b0 as usize].lnext = b2;
        e[b2 as usize].lnext = a1;
        e[a1 as usize].lnext = b0;
        e[a1 as usize].lface = fb;
        e[b1 as usize].lface = fa;
        self.f[fa as usize].an_edge = a0;
        self.f[fb as usize].an_edge = b0;
    }

    /// `tessMeshRefineDelaunay`, as [`super::Tess::refine_delaunay`] does
    /// it (three of the four edges around a flip re-queued, as upstream).
    /// False if a face is not a triangle or the flips run away.
    fn refine_delaunay(&mut self) -> bool {
        self.stack.clear();
        let mut f = self.f[0].next;
        while f != 0 {
            let face = self.f[f as usize];
            if face.inside {
                let start = face.an_edge;
                let mut e = start;
                let mut len = 0;
                loop {
                    let internal = self.edge_is_internal(e);
                    self.e[e as usize].mark = internal;
                    if internal && !self.e[(e ^ 1) as usize].mark {
                        self.stack.push(e);
                    }
                    e = self.lnext(e);
                    len += 1;
                    if e == start {
                        break;
                    }
                    if len == 3 {
                        return false;
                    }
                }
                if len != 3 {
                    return false;
                }
            }
            f = face.next;
        }
        // The flips terminate on a real triangulation, but they are
        // counted all the same.
        let n = self.st.len();
        let mut budget = 64 * n * n + 64;
        while let Some(e) = self.stack.pop() {
            budget = match budget.checked_sub(1) {
                Some(b) => b,
                None => return false,
            };
            self.e[e as usize].mark = false;
            self.e[(e ^ 1) as usize].mark = false;
            if !self.locally_delaunay(e) {
                self.flip(e);
                let edges = [self.lnext(e), self.lprev(e), self.lnext(e ^ 1)];
                for x in edges {
                    if !self.e[x as usize].mark && self.edge_is_internal(x) {
                        self.e[x as usize].mark = true;
                        self.e[(x ^ 1) as usize].mark = true;
                        self.stack.push(x);
                    }
                }
            }
        }
        true
    }

    /// `OutputPolymesh` for triangles, reading each output vertex straight
    /// back to its contour position (no vertex is created here).
    fn output(&mut self) -> bool {
        let mut f = self.f[0].next;
        while f != 0 {
            let face = self.f[f as usize];
            if face.inside {
                let a = face.an_edge;
                let b = self.lnext(a);
                let c = self.lnext(b);
                if self.lnext(c) != a {
                    return false;
                }
                self.tris.push([self.org(a), self.org(b), self.org(c)]);
            }
            f = face.next;
        }
        true
    }
}

/// [`Convex::fan`] and the Delaunay pass unrolled for four vertices,
/// the commonest face: the two triangles, as positions, or None if a step
/// would go other than the convex case lets it (the caller then takes the
/// general path).
///
/// Name the vertices r (the rightmost), a, b and c in contour order. The
/// fan starts with `up` leaving r and `lo` entering it, and which ear it
/// cuts first depends only on the order of a, b and c by `VertLeq` (four
/// cases, each traced through the loop in `fan`), plus one `EdgeSign` in
/// two of them that convexity decides but is evaluated anyway. The ear
/// (w, p, m) and the triangle left (p, w, x) share the diagonal w p, the
/// only edge the Delaunay pass tests. If it fails, `tessMeshFlipEdge` on
/// the ear's side of it leaves (x, m, w) and (m, x, p), in the same face
/// order.
fn quad(st: &[(f32, f32); 4]) -> Option<[[u32; 3]; 2]> {
    let leq = |u: u32, v: u32| {
        let (u, v) = (st[u as usize], st[v as usize]);
        u.0 < v.0 || (u.0 == v.0 && u.1 <= v.1)
    };
    let sign = |u: u32, v: u32, w: u32| {
        let (u, v, w) = (st[u as usize], st[v as usize], st[w as usize]);
        edge_sign(u.0, u.1, v.0, v.1, w.0, w.1)
    };
    // Every choice below is made with selects rather than branches: which
    // case a quad falls in depends on its orientation, which on a curved
    // surface changes from face to face and would mispredict.
    let mut r = 0;
    for k in 1..4 {
        r = if leq(k, r) { r } else { k };
    }
    let (a, b, c) = ((r + 1) & 3, (r + 2) & 3, (r + 3) & 3);
    let (ac, ab, bc) = (leq(a, c), leq(a, b), leq(b, c));
    // The cases, as (w, p, m, x):
    //   a <= c, a <= b: `lo` steps to b, then cuts b c r from b, if
    //     EdgeSign(b, c, r) <= 0: (r, b, c, a);
    //   a <= c, b < a: `lo` steps to b, then `up` cuts c r a from c:
    //     (a, c, r, b);
    //   c < a, b <= c: `up` steps to a, then `lo` cuts c r a from c:
    //     (a, c, r, b);
    //   c < a, c < b: `up` steps to a, then cuts r a b from r, if
    //     EdgeSign(b, a, r) >= 0: (b, r, a, c).
    // The other halves of those conditions hold because r is rightmost.
    // Each tuple is the first one turned by `shift` places.
    let first = ac && ab;
    let last = !ac && !bc;
    // Written so that a NaN sign fails, as it fails upstream's test.
    let first_cuts = sign(b, c, r) <= 0.0;
    let last_cuts = sign(b, a, r) >= 0.0;
    let ok = (!first || first_cuts) && (!last || last_cuts);
    if !ok {
        return None;
    }
    let shift = u32::from(!first) + u32::from(last);
    let [w, p, m, x] = [0, 2, 3, 1].map(|o| (r + o + shift) & 3);
    let q = |v: u32| st[v as usize];
    let keep = is_locally_delaunay([q(w), q(m), q(p)], [q(w), q(x), q(p)]);
    let pick = |y: u32, n: u32| if keep { y } else { n };
    Some([
        [pick(w, x), pick(p, m), pick(m, w)],
        [pick(p, m), pick(w, x), pick(x, p)],
    ])
}

/// The centre and squared radius of the circle through three points (of
/// three vertices spread round a polygon, for [`shape`]'s test); NaN or
/// infinite when they are collinear.
fn circle(p0: (f64, f64), p1: (f64, f64), p2: (f64, f64)) -> ((f64, f64), f64) {
    let a = (p1.0 - p0.0, p1.1 - p0.1);
    let b = (p2.0 - p0.0, p2.1 - p0.1);
    let d = 2.0 * (a.0 * b.1 - a.1 * b.0);
    let (a2, b2) = (a.0 * a.0 + a.1 * a.1, b.0 * b.0 + b.1 * b.1);
    let c = (
        p0.0 + (b.1 * a2 - a.1 * b2) / d,
        p0.1 + (a.0 * b2 - b.0 * a2) / d,
    );
    (c, (p0.0 - c.0).powi(2) + (p0.1 - c.1).powi(2))
}

/// The axis on which every point has the same coordinate, if any.
fn constant_axis(pts: &[[f32; 3]]) -> Option<usize> {
    let p0 = pts[0];
    (0..3).find(|&k| pts.iter().all(|p| p[k] == p0[k]))
}

/// Whether the contour is strictly convex, and if so whether its
/// vertices are nearly on one circle, in one pass over (s, t) in `f64`.
/// `m` is the contour's extent; `circle` asks for the second answer
/// (false is always a safe one).
///
/// Strictly convex: the loop turns left by a clear margin at every vertex
/// (see [`MARGIN`]) and goes round exactly once.
///
/// Nearly on a circle: then every diagonal of every triangulation passes
/// upstream's `tesedgeIsLocallyDelaunay`, which saves testing each one.
/// A regular polygon (a cylinder's cap) is the common case. Four points on
/// a circle, in order round it, have opposite angles summing to pi
/// exactly. Here every vertex is within `delta` of a circle round a centre
/// inside the polygon (every turn about it positive), so moving each
/// vertex radially onto the circle keeps their order, and the moved
/// points have that property for every diagonal. Moving back changes a
/// ray from one vertex to another, whose moved length is at least
/// `emin - 2 delta` (the shortest edge, less the moves: on a circle no two
/// points are closer than adjacent ones), by at most
/// `asin(2 delta / (emin - 2 delta))`, so each angle by twice that and the
/// sum of two by four times. With `delta <= 3e-4 emin` the sum stays under
/// pi + 0.0038, and upstream's own rounding (see `geom::clear_delaunay`)
/// adds under 1.8e-3: well inside its 0.01 slack. The `f64` rounding of
/// this test is far below its margins, given the `emin` floor.
fn shape(st: &[(f32, f32)], m: f64, circle: bool) -> Option<bool> {
    // Within this range no product the sweep or the Delaunay test forms
    // from differences of vertices underflows or overflows a `float`.
    if !(1e-12..=1e15).contains(&m) {
        return None;
    }
    let min_edge2 = 1e-12 * m * m;
    let n = st.len();
    let p = |i: usize| (f64::from(st[i].0), f64::from(st[i].1));
    let mut prev = p(n - 1);
    let mut u = (prev.0 - p(n - 2).0, prev.1 - p(n - 2).1);
    let mut uu = u.0 * u.0 + u.1 * u.1;
    let k2 = MARGIN * MARGIN;
    let mut emin2 = f64::INFINITY;
    // Edges leaving the half-plane of directions with s > 0. Every turn is
    // left and under pi, so the direction advances monotonically, can
    // never step over the other half-plane, and leaves this one once per
    // revolution.
    let mut exits = 0u32;
    let mut was_right = u.0 > 0.0;
    for &(s, t) in st {
        let q = (f64::from(s), f64::from(t));
        // The edge into q, and the turn at the vertex before it.
        let v = (q.0 - prev.0, q.1 - prev.1);
        let cross = u.0 * v.1 - u.1 * v.0;
        let vv = v.0 * v.0 + v.1 * v.1;
        if !((cross > 0.0) & (cross * cross > k2 * uu * vv) & (vv > min_edge2)) {
            return None;
        }
        let right = v.0 > 0.0;
        exits += u32::from(was_right & !right);
        was_right = right;
        emin2 = emin2.min(vv);
        prev = q;
        u = v;
        uu = vv;
    }
    if exits != 1 {
        return None;
    }
    if !circle {
        return Some(false);
    }
    // The circle through three vertices spread round the polygon.
    let (c, r2) = self::circle(p(0), p(n / 3), p(2 * n / 3));
    if !(r2.is_finite() && r2 > 0.0) {
        return Some(false);
    }
    let floor = 1e-9 * r2;
    let mut dev = 0.0f64;
    let mut inside = true;
    let last = p(n - 1);
    let mut wp = (last.0 - c.0, last.1 - c.1);
    for &(s, t) in st {
        let w = (f64::from(s) - c.0, f64::from(t) - c.1);
        inside &= wp.0 * w.1 - wp.1 * w.0 > floor;
        dev = dev.max((w.0 * w.0 + w.1 * w.1 - r2).abs());
        wp = w;
    }
    // |d - R| = |d^2 - R^2| / (d + R) <= |d^2 - R^2| / R.
    let r = r2.sqrt();
    let emin = emin2.sqrt();
    Some(inside && emin >= 1e-9 * r && dev / r <= 3e-4 * emin)
}
