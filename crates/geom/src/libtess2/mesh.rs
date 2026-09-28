//! The half-edge mesh (`mesh.c`): Guibas and Stolfi's quad-edge operations
//! as libtess2 implements them.
//!
//! Everything lives in index arenas. Half-edges come in pairs `2k`, `2k+1`,
//! so `Sym` is `e ^ 1`, and upstream's `eNext->Sym < eNext` test (which
//! relies on `EdgePair` putting `e` before `eSym` in memory) is "`e` is
//! odd". The dummy list heads are vertex 0, face 0 and the edge pair 0/1.
//! Killed elements are unlinked and left in the arena; upstream recycles
//! their memory, which only changes addresses, never the order of the
//! lists that the output is read from.

use super::{Face, HalfEdge, NIL, Tess, Vertex};

impl Tess {
    #[inline]
    pub(super) fn onext(&self, e: u32) -> u32 {
        self.e[e].onext
    }
    #[inline]
    pub(super) fn lnext(&self, e: u32) -> u32 {
        self.e[e].lnext
    }
    #[inline]
    pub(super) fn org(&self, e: u32) -> u32 {
        self.e[e].org
    }
    #[inline]
    pub(super) fn dst(&self, e: u32) -> u32 {
        self.e[e ^ 1].org
    }
    #[inline]
    pub(super) fn lface(&self, e: u32) -> u32 {
        self.e[e].lface
    }
    #[inline]
    pub(super) fn rface(&self, e: u32) -> u32 {
        self.e[e ^ 1].lface
    }
    /// `Oprev = Sym->Lnext`
    #[inline]
    pub(super) fn oprev(&self, e: u32) -> u32 {
        self.lnext(e ^ 1)
    }
    /// `Lprev = Onext->Sym`
    #[inline]
    pub(super) fn lprev(&self, e: u32) -> u32 {
        self.onext(e) ^ 1
    }
    /// `Rprev = Sym->Onext`
    #[inline]
    pub(super) fn rprev(&self, e: u32) -> u32 {
        self.onext(e ^ 1)
    }
    /// `Dnext = Rprev->Sym`
    #[inline]
    pub(super) fn dnext(&self, e: u32) -> u32 {
        self.rprev(e) ^ 1
    }

    /// `tessMeshNewMesh`: empty lists behind the three dummy heads, plus
    /// the scratch vertex (outside the lists) at index 1.
    pub(super) fn new_mesh(&mut self) {
        let head = Vertex {
            next: 0,
            prev: 0,
            ..Vertex::default()
        };
        self.v.reset(&[head, Vertex::default()]);
        self.scratch = 1;
        self.f.reset(&[Face {
            next: 0,
            prev: 0,
            ..Face::default()
        }]);
        // eHead and eHeadSym: each its own one-element `next` list.
        self.e.reset(&[
            HalfEdge {
                next: 0,
                ..HalfEdge::default()
            },
            HalfEdge {
                next: 1,
                ..HalfEdge::default()
            },
        ]);
    }

    /// `MakeEdge`: a new pair of half-edges forming their own loop, linked
    /// into the global edge list before `e_next`.
    fn make_edge(&mut self, e_next: u32) -> u32 {
        let e = self.e.alloc();
        let e_sym = e + 1;
        // MakeEdge sets every field of both halves.
        self.e[e] = HalfEdge::default();
        self.e[e_sym] = HalfEdge::default();
        // Make sure e_next points to the first edge of its pair.
        let e_next = e_next & !1;
        // Insert before e_next; the prev pointer is stored in Sym->next.
        let e_prev = self.e[e_next ^ 1].next;
        self.e[e_sym].next = e_prev;
        self.e[e_prev ^ 1].next = e;
        self.e[e].next = e_next;
        self.e[e_next ^ 1].next = e_sym;
        let h = &mut self.e[e];
        h.onext = e;
        h.lnext = e_sym;
        let h = &mut self.e[e_sym];
        h.onext = e_sym;
        h.lnext = e;
        e
    }

    /// `Splice(a, b)`: exchange `a->Onext` and `b->Onext`.
    fn splice(&mut self, a: u32, b: u32) {
        let a_onext = self.onext(a);
        let b_onext = self.onext(b);
        self.e[a_onext ^ 1].lnext = b;
        self.e[b_onext ^ 1].lnext = a;
        self.e[a].onext = b_onext;
        self.e[b].onext = a_onext;
    }

    /// `MakeVertex`: a new vertex, inserted before `v_next`, as the origin
    /// of every edge around `e_orig`. New vertices carry no input index
    /// until the caller sets one (upstream leaves the field unset; the only
    /// new vertices that survive are intersections, which get
    /// `TESS_UNDEF`).
    fn make_vertex(&mut self, e_orig: u32, v_next: u32) -> u32 {
        // Only the links are set: a reused vertex keeps its old
        // coordinates and index, as upstream's does.
        let v_new = self.v.alloc();
        let v_prev = self.v[v_next].prev;
        let v = &mut self.v[v_new];
        v.next = v_next;
        v.prev = v_prev;
        v.an_edge = e_orig;
        self.v[v_prev].next = v_new;
        self.v[v_next].prev = v_new;
        let mut e = e_orig;
        loop {
            if !self.tick() {
                break;
            }
            self.e[e].org = v_new;
            e = self.onext(e);
            if e == e_orig {
                break;
            }
        }
        v_new
    }

    /// `MakeFace`: a new face, inserted before `f_next`, as the left face
    /// of every edge around `e_orig`. It is inside if `f_next` is.
    fn make_face(&mut self, e_orig: u32, f_next: u32) -> u32 {
        let f_new = self.f.alloc();
        let f_prev = self.f[f_next].prev;
        let inside = self.f[f_next].inside;
        self.f[f_new] = Face {
            next: f_next,
            prev: f_prev,
            an_edge: e_orig,
            inside,
        };
        self.f[f_prev].next = f_new;
        self.f[f_next].prev = f_new;
        let mut e = e_orig;
        loop {
            if !self.tick() {
                break;
            }
            self.e[e].lface = f_new;
            e = self.lnext(e);
            if e == e_orig {
                break;
            }
        }
        f_new
    }

    /// `KillEdge`: unlink the pair from the global edge list.
    fn kill_edge(&mut self, e_del: u32) {
        let e_del = e_del & !1;
        let e_next = self.e[e_del].next;
        let e_prev = self.e[e_del ^ 1].next;
        self.e[e_next ^ 1].next = e_prev;
        self.e[e_prev ^ 1].next = e_next;
        let head = self.e.free(e_del);
        self.e[e_del].next = head;
    }

    /// `KillVertex`: unlink `v_del`, giving its edges the origin `new_org`.
    fn kill_vertex(&mut self, v_del: u32, new_org: u32) {
        let e_start = self.v[v_del].an_edge;
        let mut e = e_start;
        loop {
            if !self.tick() {
                break;
            }
            self.e[e].org = new_org;
            e = self.onext(e);
            if e == e_start {
                break;
            }
        }
        let Vertex { prev, next, .. } = self.v[v_del];
        self.v[next].prev = prev;
        self.v[prev].next = next;
        let head = self.v.free(v_del);
        self.v[v_del].next = head;
    }

    /// `KillFace`: unlink `f_del`, giving its edges the left face
    /// `new_lface`.
    fn kill_face(&mut self, f_del: u32, new_lface: u32) {
        let e_start = self.f[f_del].an_edge;
        let mut e = e_start;
        loop {
            if !self.tick() {
                break;
            }
            self.e[e].lface = new_lface;
            e = self.lnext(e);
            if e == e_start {
                break;
            }
        }
        let Face { prev, next, .. } = self.f[f_del];
        self.f[next].prev = prev;
        self.f[prev].next = next;
        let head = self.f.free(f_del);
        self.f[f_del].next = head;
    }

    /// `tessMeshMakeEdge`: one edge, two vertices and a loop.
    pub(super) fn mesh_make_edge(&mut self) -> u32 {
        let e = self.make_edge(0);
        self.make_vertex(e, 0);
        self.make_vertex(e ^ 1, 0);
        self.make_face(e, 0);
        e
    }

    /// `tessMeshSplice`: see `mesh.h`. Merges or splits the origins, and
    /// joins or splits the left faces, of `e_org` and `e_dst`.
    pub(super) fn mesh_splice(&mut self, e_org: u32, e_dst: u32) {
        if e_org == e_dst {
            return;
        }
        let mut joining_vertices = false;
        let mut joining_loops = false;
        if self.org(e_dst) != self.org(e_org) {
            joining_vertices = true;
            self.kill_vertex(self.org(e_dst), self.org(e_org));
        }
        if self.lface(e_dst) != self.lface(e_org) {
            joining_loops = true;
            self.kill_face(self.lface(e_dst), self.lface(e_org));
        }
        self.splice(e_dst, e_org);
        if !joining_vertices {
            // Split one vertex into two; the new one is e_dst->Org.
            self.make_vertex(e_dst, self.org(e_org));
            let o = self.org(e_org);
            self.v[o].an_edge = e_org;
        }
        if !joining_loops {
            // Split one loop into two; the new one is e_dst->Lface.
            self.make_face(e_dst, self.lface(e_org));
            let l = self.lface(e_org);
            self.f[l].an_edge = e_org;
        }
    }

    /// `tessMeshDelete`: remove the edge `e_del`, joining or splitting
    /// faces and dropping vertices left isolated.
    pub(super) fn mesh_delete(&mut self, e_del: u32) {
        let e_del_sym = e_del ^ 1;
        let mut joining_loops = false;
        if self.lface(e_del) != self.rface(e_del) {
            joining_loops = true;
            self.kill_face(self.lface(e_del), self.rface(e_del));
        }
        if self.onext(e_del) == e_del {
            self.kill_vertex(self.org(e_del), NIL);
        } else {
            let rf = self.rface(e_del);
            self.f[rf].an_edge = self.oprev(e_del);
            let o = self.org(e_del);
            self.v[o].an_edge = self.onext(e_del);
            self.splice(e_del, self.oprev(e_del));
            if !joining_loops {
                self.make_face(e_del, self.lface(e_del));
            }
        }
        if self.onext(e_del_sym) == e_del_sym {
            self.kill_vertex(self.org(e_del_sym), NIL);
            self.kill_face(self.lface(e_del_sym), NIL);
        } else {
            let lf = self.lface(e_del);
            self.f[lf].an_edge = self.oprev(e_del_sym);
            let o = self.org(e_del_sym);
            self.v[o].an_edge = self.onext(e_del_sym);
            self.splice(e_del_sym, self.oprev(e_del_sym));
        }
        self.kill_edge(e_del);
    }

    /// `tessMeshAddEdgeVertex`: a new edge `eNew == eOrg->Lnext` whose
    /// destination is a new vertex.
    fn mesh_add_edge_vertex(&mut self, e_org: u32) -> u32 {
        let e_new = self.make_edge(e_org);
        let e_new_sym = e_new ^ 1;
        self.splice(e_new, self.lnext(e_org));
        self.e[e_new].org = self.dst(e_org);
        self.make_vertex(e_new_sym, self.org(e_new));
        let lf = self.lface(e_org);
        self.e[e_new].lface = lf;
        self.e[e_new_sym].lface = lf;
        e_new
    }

    /// `tessMeshSplitEdge`: split `e_org` in two at a new vertex
    /// `eOrg->Dst == eNew->Org`, returning `eNew == eOrg->Lnext`.
    pub(super) fn mesh_split_edge(&mut self, e_org: u32) -> u32 {
        let temp = self.mesh_add_edge_vertex(e_org);
        let e_new = temp ^ 1;
        self.splice(e_org ^ 1, self.oprev(e_org ^ 1));
        self.splice(e_org ^ 1, e_new);
        let no = self.org(e_new);
        self.e[e_org ^ 1].org = no;
        let nd = self.dst(e_new);
        self.v[nd].an_edge = e_new ^ 1;
        let rf = self.rface(e_org);
        self.e[e_new ^ 1].lface = rf;
        self.e[e_new].winding = self.e[e_org].winding;
        self.e[e_new ^ 1].winding = self.e[e_org ^ 1].winding;
        e_new
    }

    /// `tessMeshConnect`: a new edge from `eOrg->Dst` to `eDst->Org`.
    pub(super) fn mesh_connect(&mut self, e_org: u32, e_dst: u32) -> u32 {
        let e_new = self.make_edge(e_org);
        let e_new_sym = e_new ^ 1;
        let mut joining_loops = false;
        if self.lface(e_dst) != self.lface(e_org) {
            joining_loops = true;
            self.kill_face(self.lface(e_dst), self.lface(e_org));
        }
        self.splice(e_new, self.lnext(e_org));
        self.splice(e_new_sym, e_dst);
        self.e[e_new].org = self.dst(e_org);
        self.e[e_new_sym].org = self.org(e_dst);
        let lf = self.lface(e_org);
        self.e[e_new].lface = lf;
        self.e[e_new_sym].lface = lf;
        self.f[lf].an_edge = e_new_sym;
        if !joining_loops {
            self.make_face(e_new, lf);
        }
        e_new
    }

    /// `tessMeshFlipEdge`: replace the diagonal of the quad formed by the
    /// two triangles beside `edge` with the other diagonal.
    pub(super) fn mesh_flip_edge(&mut self, edge: u32) {
        let a0 = edge;
        let a1 = self.lnext(a0);
        let a2 = self.lnext(a1);
        let b0 = edge ^ 1;
        let b1 = self.lnext(b0);
        let b2 = self.lnext(b1);
        let a_org = self.org(a0);
        let a_opp = self.org(a2);
        let b_org = self.org(b0);
        let b_opp = self.org(b2);
        let fa = self.lface(a0);
        let fb = self.lface(b0);

        self.e[a0].org = b_opp;
        self.e[a0].onext = b1 ^ 1;
        self.e[b0].org = a_opp;
        self.e[b0].onext = a1 ^ 1;
        self.e[a2].onext = b0;
        self.e[b2].onext = a0;
        self.e[b1].onext = a2 ^ 1;
        self.e[a1].onext = b2 ^ 1;

        self.e[a0].lnext = a2;
        self.e[a2].lnext = b1;
        self.e[b1].lnext = a0;

        self.e[b0].lnext = b2;
        self.e[b2].lnext = a1;
        self.e[a1].lnext = b0;

        self.e[a1].lface = fb;
        self.e[b1].lface = fa;

        self.f[fa].an_edge = a0;
        self.f[fb].an_edge = b0;

        if self.v[a_org].an_edge == a0 {
            self.v[a_org].an_edge = b1;
        }
        if self.v[b_org].an_edge == b0 {
            self.v[b_org].an_edge = a1;
        }
    }
}
