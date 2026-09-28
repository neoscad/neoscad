//! A port of libtess2, the SGI GLU tessellator as Mikko Mononen repackaged
//! it, in the version OpenSCAD vendors (`src/ext/libtess2`), together with
//! OpenSCAD's wrapper around it (`GeometryUtils::tessellatePolygonWithHoles`,
//! in `openscad.rs`).
//!
//! OpenSCAD splits every polygon face of a mesh into triangles with
//! libtess2 before export, display or conversion to Manifold
//! (`PolySetUtils::tessellate_faces`). Which diagonals it picks shows in
//! STL and OBJ bytes and in the shading of non-planar faces, so this port
//! follows upstream operation for operation: the same mesh edits in the
//! same order, the same event-queue tie-breaking, `f32` arithmetic with
//! the multiply-adds fused where clang fuses them. Only the subset
//! OpenSCAD uses is ported: one contour per call, the odd winding rule, an
//! automatically computed normal, and `TESS_CONSTRAINED_DELAUNAY_TRIANGLES`
//! output (triangles, refined by edge flips).
//!
//! libtess2 is under the SGI Free Software License B (version 2.0), which
//! is GPL-compatible:
//!
//! > SGI FREE SOFTWARE LICENSE B (Version 2.0, Sept. 18, 2008)
//! > Copyright (C) [dates of first publication] Silicon Graphics, Inc.
//! > All Rights Reserved.
//! >
//! > Permission is hereby granted, free of charge, to any person obtaining
//! > a copy of this software and associated documentation files (the
//! > "Software"), to deal in the Software without restriction, including
//! > without limitation the rights to use, copy, modify, merge, publish,
//! > distribute, sublicense, and/or sell copies of the Software, and to
//! > permit persons to whom the Software is furnished to do so, subject to
//! > the following conditions:
//! >
//! > The above copyright notice including the dates of first publication
//! > and either this permission notice or a reference to
//! > http://oss.sgi.com/projects/FreeB/ shall be included in all copies or
//! > substantial portions of the Software.
//! >
//! > THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
//! > OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! > MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
//! > IN NO EVENT SHALL SILICON GRAPHICS, INC. BE LIABLE FOR ANY CLAIM,
//! > DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
//! > OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR
//! > THE USE OR OTHER DEALINGS IN THE SOFTWARE.
//! >
//! > Except as contained in this notice, the name of Silicon Graphics, Inc.
//! > shall not be used in advertising or otherwise to promote the sale, use
//! > or other dealings in this Software without prior written authorization
//! > from Silicon Graphics, Inc.
//!
//! Original code by Eric Veach (1994); libtess2 by Mikko Mononen (2009).

mod arena;
mod convex;
mod cxxmap;
mod geom;
mod mesh;
mod openscad;
mod pq;
mod sweep;
#[cfg(test)]
mod tests;

pub use openscad::Tessellator;

use arena::Arena;
use geom::fma;

/// A null link (upstream's `NULL`).
const NIL: u32 = u32::MAX;

/// The tessellation failed: the event queue could not grow (see `pq.rs`),
/// or the mesh broke (see `arena.rs`). The polygon yields no triangles, as
/// when upstream's `tessTesselate` returns 0.
#[derive(Debug)]
struct Overflow;

#[derive(Clone, Copy)]
struct Vertex {
    next: u32,
    prev: u32,
    an_edge: u32,
    coords: [f32; 3],
    s: f32,
    t: f32,
    pq_handle: i32,
    /// Output number (`TESS_UNDEF`: -1).
    n: i32,
    /// Input number, or -1 for a vertex the tessellator created.
    idx: i32,
}

impl Default for Vertex {
    fn default() -> Self {
        Vertex {
            next: NIL,
            prev: NIL,
            an_edge: NIL,
            coords: [0.0; 3],
            s: 0.0,
            t: 0.0,
            pq_handle: 0,
            n: -1,
            idx: -1,
        }
    }
}

#[derive(Clone, Copy)]
struct Face {
    next: u32,
    prev: u32,
    an_edge: u32,
    inside: bool,
}

impl Default for Face {
    fn default() -> Self {
        Face {
            next: NIL,
            prev: NIL,
            an_edge: NIL,
            inside: false,
        }
    }
}

#[derive(Clone, Copy)]
struct HalfEdge {
    /// The global edge list; `prev` is `Sym->next`.
    next: u32,
    onext: u32,
    lnext: u32,
    org: u32,
    lface: u32,
    active_region: u32,
    winding: i32,
    /// Queued for the Delaunay edge flips.
    mark: bool,
}

impl Default for HalfEdge {
    fn default() -> Self {
        HalfEdge {
            next: NIL,
            onext: NIL,
            lnext: NIL,
            org: NIL,
            lface: NIL,
            active_region: NIL,
            winding: 0,
            mark: false,
        }
    }
}

/// `ActiveRegion`: the region between two adjacent edges crossing the
/// sweep line.
#[derive(Clone, Copy)]
struct Region {
    e_up: u32,
    node_up: u32,
    winding_number: i32,
    inside: bool,
    dirty: bool,
    fix_upper_edge: bool,
}

impl Default for Region {
    fn default() -> Self {
        Region {
            e_up: NIL,
            node_up: NIL,
            winding_number: 0,
            inside: false,
            dirty: false,
            fix_upper_edge: false,
        }
    }
}

/// `DictNode`
#[derive(Clone, Copy)]
struct DictNode {
    key: u32,
    next: u32,
    prev: u32,
}

impl Default for DictNode {
    fn default() -> Self {
        DictNode {
            key: NIL,
            next: NIL,
            prev: NIL,
        }
    }
}

/// `TESStesselator`, with its storage kept between polygons so that
/// tessellating a mesh does not allocate per face.
struct Tess {
    v: Arena<Vertex>,
    f: Arena<Face>,
    e: Arena<HalfEdge>,
    r: Arena<Region>,
    d: Arena<DictNode>,
    /// Whether a link was null where upstream would crash (see
    /// `arena.rs`); the arenas share it.
    broken: std::rc::Rc<std::cell::Cell<bool>>,
    /// Steps left before a traversal is taken to be spinning (see
    /// [`Tess::tick`]).
    fuel: std::cell::Cell<u64>,
    pq: pq::Pq,
    /// The current sweep event.
    event: u32,
    /// A vertex outside the lists, for the intersection point
    /// `CheckForIntersect` keeps on its stack.
    scratch: u32,
    bmin: [f32; 2],
    bmax: [f32; 2],
    stack: Vec<u32>,
    /// Scratch coordinates (vertex list order) for `ComputeNormal`.
    coords: Vec<[f32; 3]>,
    /// `tessGetElements`: three output vertex numbers per triangle.
    elements: Vec<i32>,
    /// `tessGetVertexIndices`: the input number of each output vertex.
    vertex_indices: Vec<i32>,
}

impl Default for Tess {
    fn default() -> Self {
        // Heads before each pool: vHead and the scratch vertex, fHead,
        // eHead/eHeadSym, and the dictionary head. Bucket sizes are
        // `tessNewTess`'s defaults (OpenSCAD zeroes `TESSalloc`).
        let broken = std::rc::Rc::new(std::cell::Cell::new(false));
        Tess {
            v: Arena::new(2, 1, 512, broken.clone()),
            f: Arena::new(1, 1, 256, broken.clone()),
            e: Arena::new(2, 2, 512, broken.clone()),
            r: Arena::new(0, 1, 256, broken.clone()),
            d: Arena::new(1, 1, 512, broken.clone()),
            broken,
            fuel: std::cell::Cell::new(0),
            pq: pq::Pq::default(),
            event: NIL,
            scratch: 1,
            bmin: [0.0; 2],
            bmax: [0.0; 2],
            stack: Vec::new(),
            coords: Vec::new(),
            elements: Vec::new(),
            vertex_indices: Vec::new(),
        }
    }
}

impl Tess {
    /// Whether a link was null where upstream would crash (see `arena.rs`).
    #[inline]
    fn broken(&self) -> bool {
        self.broken.get()
    }

    /// One step of a traversal loop. False, ending the loop, once the mesh
    /// broke or the budget ran out: a broken mesh can have links that never
    /// lead back to where a loop started, and upstream would spin or crash
    /// there. The budget is far above what a real sweep takes (a few
    /// hundred steps per vertex).
    #[inline]
    fn tick(&self) -> bool {
        let f = self.fuel.get();
        if f == 0 || self.broken() {
            return false;
        }
        self.fuel.set(f - 1);
        true
    }

    /// Whether the run so far went wrong (see [`Tess::tick`]).
    fn failed(&self) -> bool {
        self.fuel.get() == 0 || self.broken()
    }

    /// `tessAddContour` for one contour, on a fresh mesh.
    fn begin(&mut self, pts: impl Iterator<Item = [f32; 3]>) {
        self.new_mesh();
        self.r.reset(&[]);
        // A polygon that broke must not leave the flag set: every traversal
        // checks it, and the contour below would be built wrong.
        self.broken.set(false);
        // Building the contour takes a bounded number of steps.
        self.fuel.set(u64::MAX);
        let mut e = NIL;
        let mut n: u64 = 0;
        for (i, c) in pts.enumerate() {
            n += 1;
            if e == NIL {
                // A self-loop: one vertex, one edge.
                e = self.mesh_make_edge();
                self.mesh_splice(e, e ^ 1);
            } else {
                // A new vertex and edge right after e around the face.
                self.mesh_split_edge(e);
                e = self.lnext(e);
            }
            let o = self.org(e);
            self.v[o].coords = c;
            self.v[o].idx = i as i32;
            // A CCW contour adds +1 to the winding of what it encloses.
            self.e[e].winding = 1;
            self.e[e ^ 1].winding = -1;
        }
        self.fuel.set(1_000_000 + 1000 * n * n);
    }

    /// `tessTesselate(tess, TESS_WINDING_ODD,
    /// TESS_CONSTRAINED_DELAUNAY_TRIANGLES, 3, 3, NULL)`. Returns false
    /// where upstream returns 0 (no output).
    fn tesselate(&mut self) -> bool {
        self.elements.clear();
        self.vertex_indices.clear();
        if self.e.len() <= 2 {
            // No contour: upstream has no mesh.
            return false;
        }
        self.project_polygon();
        if self.compute_interior().is_err() || self.failed() {
            return false;
        }
        self.tessellate_interior();
        self.refine_delaunay();
        if self.failed() {
            return false;
        }
        self.output_polymesh();
        !self.failed()
    }

    /// `ComputeNormal` over the vertex list.
    fn compute_normal(&mut self) -> [f32; 3] {
        let mut pts = std::mem::take(&mut self.coords);
        pts.clear();
        let mut v = self.v[0].next;
        while v != 0 {
            if !self.tick() {
                break;
            }
            pts.push(self.v[v].coords);
            v = self.v[v].next;
        }
        let n = compute_normal(&pts, &[]);
        self.coords = pts;
        n
    }

    /// `CheckOrientation`: flip t if the contours' signed area is negative.
    fn check_orientation(&mut self) {
        let mut area = 0.0f32;
        let mut f = self.f[0].next;
        while f != 0 {
            if !self.tick() {
                break;
            }
            let start = self.f[f].an_edge;
            if self.e[start].winding > 0 {
                let mut e = start;
                loop {
                    if !self.tick() {
                        break;
                    }
                    let (os, ot) = self.st(self.org(e));
                    let (ds, dt) = self.st(self.dst(e));
                    area = fma(os - ds, ot + dt, area);
                    e = self.lnext(e);
                    if e == start {
                        break;
                    }
                }
            }
            f = self.f[f].next;
        }
        if area < 0.0 {
            let mut v = self.v[0].next;
            while v != 0 {
                if !self.tick() {
                    break;
                }
                let t = &mut self.v[v].t;
                *t = -*t;
                v = self.v[v].next;
            }
        }
    }

    /// `tessProjectPolygon`: project onto the plane perpendicular to the
    /// normal's longest axis (upstream's default, "better numerically"
    /// than a true projection), then orient and bound.
    fn project_polygon(&mut self) {
        let norm = self.compute_normal();
        let (s_unit, t_unit) = sweep_units(norm);
        let mut v = self.v[0].next;
        while v != 0 {
            if !self.tick() {
                break;
            }
            let vx = &mut self.v[v];
            vx.s = dot(vx.coords, s_unit);
            vx.t = dot(vx.coords, t_unit);
            v = vx.next;
        }
        // The normal is always computed here (OpenSCAD passes none).
        self.check_orientation();
        let mut first = true;
        let mut v = self.v[0].next;
        while v != 0 {
            if !self.tick() {
                break;
            }
            let (s, t) = self.st(v);
            if first {
                self.bmin = [s, t];
                self.bmax = [s, t];
                first = false;
            } else {
                if s < self.bmin[0] {
                    self.bmin[0] = s;
                }
                if s > self.bmax[0] {
                    self.bmax[0] = s;
                }
                if t < self.bmin[1] {
                    self.bmin[1] = t;
                }
                if t > self.bmax[1] {
                    self.bmax[1] = t;
                }
            }
            v = self.v[v].next;
        }
    }

    /// `tessMeshTessellateMonoRegion`: fan a monotone region into
    /// triangles, from right to left.
    fn tessellate_mono_region(&mut self, face: u32) {
        let mut up = self.f[face].an_edge;
        while self.vert_leq(self.dst(up), self.org(up)) {
            if !self.tick() {
                break;
            }
            up = self.lprev(up);
        }
        while self.vert_leq(self.org(up), self.dst(up)) {
            if !self.tick() {
                break;
            }
            up = self.lnext(up);
        }
        let mut lo = self.lprev(up);
        while self.lnext(up) != lo {
            if !self.tick() {
                break;
            }
            if self.vert_leq(self.dst(up), self.org(lo)) {
                // up->Dst is on the left: form triangles from lo->Org.
                while self.lnext(lo) != up
                    && (self.edge_goes_left(self.lnext(lo))
                        || self.edge_sign(self.org(lo), self.dst(lo), self.dst(self.lnext(lo)))
                            <= 0.0)
                {
                    if !self.tick() {
                        break;
                    }
                    lo = self.mesh_connect(self.lnext(lo), lo) ^ 1;
                }
                lo = self.lprev(lo);
            } else {
                // lo->Org is on the left: form CCW triangles from up->Dst.
                while self.lnext(lo) != up
                    && (self.edge_goes_right(self.lprev(up))
                        || self.edge_sign(self.dst(up), self.org(up), self.org(self.lprev(up)))
                            >= 0.0)
                {
                    if !self.tick() {
                        break;
                    }
                    up = self.mesh_connect(up, self.lprev(up)) ^ 1;
                }
                up = self.lnext(up);
            }
        }
        // Fan the rest from the leftmost vertex.
        while self.lnext(self.lnext(lo)) != up {
            if !self.tick() {
                break;
            }
            lo = self.mesh_connect(self.lnext(lo), lo) ^ 1;
        }
    }

    /// `tessMeshTessellateInterior`
    fn tessellate_interior(&mut self) {
        let mut f = self.f[0].next;
        while f != 0 {
            if !self.tick() {
                break;
            }
            // Faces this adds go before f, so they are not visited.
            let next = self.f[f].next;
            if self.f[f].inside {
                self.tessellate_mono_region(f);
            }
            f = next;
        }
    }

    /// `tessMeshRefineDelaunay`: flip interior edges until every one is
    /// locally Delaunay. Upstream re-queues only three of the four edges
    /// around a flipped one (`for (i=0;i<3;i++)`); kept.
    fn refine_delaunay(&mut self) {
        self.stack.clear();
        let mut f = self.f[0].next;
        while f != 0 {
            if !self.tick() {
                break;
            }
            if self.f[f].inside {
                let start = self.f[f].an_edge;
                let mut e = start;
                loop {
                    if !self.tick() {
                        break;
                    }
                    let internal = self.edge_is_internal(e);
                    self.e[e].mark = internal;
                    if internal && !self.e[e ^ 1].mark {
                        self.stack.push(e);
                    }
                    e = self.lnext(e);
                    if e == start {
                        break;
                    }
                }
            }
            f = self.f[f].next;
        }
        while let Some(e) = self.stack.pop() {
            if !self.tick() {
                break;
            }
            self.e[e].mark = false;
            self.e[e ^ 1].mark = false;
            if !self.edge_is_locally_delaunay(e) {
                self.mesh_flip_edge(e);
                let edges = [
                    self.lnext(e),
                    self.lprev(e),
                    self.lnext(e ^ 1),
                    self.lprev(e ^ 1),
                ];
                for &x in &edges[..3] {
                    if !self.e[x].mark && self.edge_is_internal(x) {
                        self.e[x].mark = true;
                        self.e[x ^ 1].mark = true;
                        self.stack.push(x);
                    }
                }
            }
        }
    }

    /// `OutputPolymesh` for triangles: number the vertices of inside faces
    /// in face order, then list each face's three.
    fn output_polymesh(&mut self) {
        let mut v = self.v[0].next;
        while v != 0 {
            if !self.tick() {
                break;
            }
            self.v[v].n = -1;
            v = self.v[v].next;
        }
        let mut max_vertex_count = 0;
        let mut f = self.f[0].next;
        while f != 0 {
            if !self.tick() {
                break;
            }
            if self.f[f].inside {
                let start = self.f[f].an_edge;
                let mut e = start;
                loop {
                    if !self.tick() {
                        break;
                    }
                    let o = self.org(e);
                    if self.v[o].n == -1 {
                        self.v[o].n = max_vertex_count;
                        self.vertex_indices.push(self.v[o].idx);
                        max_vertex_count += 1;
                    }
                    self.elements.push(self.v[o].n);
                    e = self.lnext(e);
                    if e == start {
                        break;
                    }
                }
                // Faces are triangles here; upstream pads shorter ones
                // with TESS_UNDEF and would overrun on longer ones.
                let len = self.elements.len() % 3;
                if len != 0 {
                    self.elements.resize(self.elements.len() + 3 - len, -1);
                }
            }
            f = self.f[f].next;
        }
    }
}

/// `ComputeNormal`: the normal of the largest triangle spanned by the two
/// vertices furthest apart along some axis and a third, over the vertices
/// in the mesh's list order (which decides ties): `head` then `tail`. The
/// two parts save the convex path from copying its contour into that
/// order.
fn compute_normal<'a>(head: &'a [[f32; 3]], tail: &'a [[f32; 3]]) -> [f32; 3] {
    let Some(first) = head.first().or(tail.first()) else {
        return [0.0, 0.0, 1.0];
    };
    let mut min_val = *first;
    let mut max_val = *first;
    let mut min_pt = [first; 3];
    let mut max_pt = [first; 3];
    // Written with selects rather than branches: on scattered coordinates
    // the branches mispredict, and this runs for every face.
    let mut extremes = |c: &'a [f32; 3]| {
        for i in 0..3 {
            let lt = c[i] < min_val[i];
            min_val[i] = if lt { c[i] } else { min_val[i] };
            min_pt[i] = if lt { c } else { min_pt[i] };
            let gt = c[i] > max_val[i];
            max_val[i] = if gt { c[i] } else { max_val[i] };
            max_pt[i] = if gt { c } else { max_pt[i] };
        }
    };
    head.iter().for_each(&mut extremes);
    tail.iter().for_each(&mut extremes);
    let mut i = 0;
    if max_val[1] - min_val[1] > max_val[0] - min_val[0] {
        i = 1;
    }
    if max_val[2] - min_val[2] > max_val[i] - min_val[i] {
        i = 2;
    }
    if min_val[i] >= max_val[i] {
        // All vertices are the same.
        return [0.0, 0.0, 1.0];
    }
    let mut norm = [0.0f32; 3];
    let mut max_len2 = 0.0f32;
    let c1 = *min_pt[i];
    let c2 = *max_pt[i];
    let d1 = [c1[0] - c2[0], c1[1] - c2[1], c1[2] - c2[2]];
    let mut largest = |c: &[f32; 3]| {
        let d2 = [c[0] - c2[0], c[1] - c2[1], c[2] - c2[2]];
        let tn = [
            fma(d1[1], d2[2], -(d1[2] * d2[1])),
            fma(d1[2], d2[0], -(d1[0] * d2[2])),
            fma(d1[0], d2[1], -(d1[1] * d2[0])),
        ];
        let len2 = fma(tn[2], tn[2], fma(tn[0], tn[0], tn[1] * tn[1]));
        let gt = len2 > max_len2;
        max_len2 = if gt { len2 } else { max_len2 };
        norm = if gt { tn } else { norm };
    };
    head.iter().for_each(&mut largest);
    tail.iter().for_each(&mut largest);
    if max_len2 <= 0.0 {
        // All points on one line: any decent normal will do.
        norm = [0.0; 3];
        norm[short_axis(d1)] = 1.0;
    }
    norm
}

/// [`compute_normal`] for four points (already in list order), written
/// out so that it compiles to straight-line code: most faces are quads.
#[inline]
fn compute_normal4(q: &[[f32; 3]; 4]) -> [f32; 3] {
    let mut lo = [0usize; 3];
    let mut hi = [0usize; 3];
    for k in 1..4 {
        for i in 0..3 {
            lo[i] = if q[k][i] < q[lo[i]][i] { k } else { lo[i] };
            hi[i] = if q[k][i] > q[hi[i]][i] { k } else { hi[i] };
        }
    }
    let ext = |i: usize| q[hi[i]][i] - q[lo[i]][i];
    let mut i = 0;
    if ext(1) > ext(0) {
        i = 1;
    }
    if ext(2) > ext(i) {
        i = 2;
    }
    if q[lo[i]][i] >= q[hi[i]][i] {
        return [0.0, 0.0, 1.0];
    }
    let c1 = q[lo[i]];
    let c2 = q[hi[i]];
    let d1 = [c1[0] - c2[0], c1[1] - c2[1], c1[2] - c2[2]];
    let mut norm = [0.0f32; 3];
    let mut max_len2 = 0.0f32;
    for c in q {
        let d2 = [c[0] - c2[0], c[1] - c2[1], c[2] - c2[2]];
        let tn = [
            fma(d1[1], d2[2], -(d1[2] * d2[1])),
            fma(d1[2], d2[0], -(d1[0] * d2[2])),
            fma(d1[0], d2[1], -(d1[1] * d2[0])),
        ];
        let len2 = fma(tn[2], tn[2], fma(tn[0], tn[0], tn[1] * tn[1]));
        let gt = len2 > max_len2;
        max_len2 = if gt { len2 } else { max_len2 };
        norm = if gt { tn } else { norm };
    }
    if max_len2 <= 0.0 {
        norm = [0.0; 3];
        norm[short_axis(d1)] = 1.0;
    }
    norm
}

/// `tessProjectPolygon`'s sweep-plane axes: project perpendicular to the
/// normal's longest axis (upstream's default, "better numerically" than a
/// true projection), with t chosen so that s, t, normal are right-handed.
fn sweep_units(norm: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    // Table lookups rather than `% 3` and branches: this runs for every
    // face, and on a curved surface the axis changes from face to face.
    // Row i is s_unit (1 at (i + 1) % 3), then t_unit for norm[i] > 0
    // (-0 at (i + 1) % 3, 1 at (i + 2) % 3) and otherwise (0 and -1).
    const S: [[f32; 3]; 3] = [[0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]];
    const T_POS: [[f32; 3]; 3] = [[0.0, -0.0, 1.0], [1.0, 0.0, -0.0], [-0.0, 1.0, 0.0]];
    const T_NEG: [[f32; 3]; 3] = [[0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]];
    let i = long_axis(norm);
    let t = if norm[i] > 0.0 { T_POS[i] } else { T_NEG[i] };
    (S[i], t)
}

/// `Dot`, fused as clang fuses it.
#[inline]
fn dot(c: [f32; 3], u: [f32; 3]) -> f32 {
    fma(c[2], u[2], fma(c[0], u[0], c[1] * u[1]))
}

/// `LongAxis`
#[inline]
fn long_axis(v: [f32; 3]) -> usize {
    let (a, b, c) = (v[0].abs(), v[1].abs(), v[2].abs());
    let i = usize::from(b > a);
    let m = if b > a { b } else { a };
    if c > m { 2 } else { i }
}

/// `ShortAxis`
fn short_axis(v: [f32; 3]) -> usize {
    let mut i = 0;
    if v[1].abs() < v[0].abs() {
        i = 1;
    }
    if v[2].abs() < v[i].abs() {
        i = 2;
    }
    i
}
