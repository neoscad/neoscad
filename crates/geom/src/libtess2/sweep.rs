//! The sweep line (`sweep.c`) and its edge dictionary (`dict.c`): splits
//! the contours into monotone regions and marks each inside or outside by
//! the winding rule. Ported function for function; the comments on the
//! degenerate branches are upstream's, shortened.
//!
//! Upstream's only failure inside the sweep is an event queue that cannot
//! grow (see `pq.rs`); it `longjmp`s out and the polygon yields nothing.
//! Here that is [`Overflow`], returned through every caller.

use super::pq::INV_HANDLE;
use super::{NIL, Overflow, Region, Tess};

type R<T> = Result<T, Overflow>;

/// The winding rule OpenSCAD passes (`TESS_WINDING_ODD`).
#[inline]
fn is_winding_inside(n: i32) -> bool {
    n & 1 != 0
}

impl Tess {
    // --- dict.c: a sorted doubly linked list with a dummy head (node 0) ---

    /// `dictInsertBefore(node, region)`
    fn dict_insert_before(&mut self, mut node: u32, reg: u32) -> u32 {
        let e_new = self.r[reg].e_up;
        loop {
            if !self.tick() {
                break;
            }
            node = self.d[node].prev;
            let k = self.d[node].key;
            if k == NIL || self.edge_leq(self.r[k].e_up, e_new) {
                break;
            }
        }
        let new_node = self.d.alloc();
        let next = self.d[node].next;
        self.d[new_node] = super::DictNode {
            key: reg,
            next,
            prev: node,
        };
        self.d[next].prev = new_node;
        self.d[node].next = new_node;
        new_node
    }

    /// `dictDelete`
    fn dict_delete(&mut self, node: u32) {
        let super::DictNode { next, prev, .. } = self.d[node];
        self.d[next].prev = prev;
        self.d[prev].next = next;
        // Upstream's free-list link lands in `key`, where it would be read
        // as a region; a stale read gets null here instead.
        self.d.free(node);
        self.d[node].key = NIL;
    }

    /// `dictSearch` for a region whose upper edge would be `e_up`: the
    /// first region not below it. `ConnectLeftVertex` searches with a
    /// temporary region that only has `eUp` set.
    fn dict_search(&self, e_up: u32) -> u32 {
        let mut node = 0;
        loop {
            if !self.tick() {
                return NIL;
            }
            node = self.d[node].next;
            let k = self.d[node].key;
            if k == NIL || self.edge_leq(e_up, self.r[k].e_up) {
                return k;
            }
        }
    }

    /// `RegionBelow`, or `NIL` past the dummy head.
    #[inline]
    fn region_below(&self, r: u32) -> u32 {
        let n = self.r[r].node_up;
        self.d[self.d[n].prev].key
    }

    /// `RegionAbove`, or `NIL` past the dummy head.
    #[inline]
    fn region_above(&self, r: u32) -> u32 {
        let n = self.r[r].node_up;
        self.d[self.d[n].next].key
    }

    #[inline]
    fn e_up(&self, r: u32) -> u32 {
        self.r[r].e_up
    }

    // --- sweep.c ---

    /// `EdgeLeq`: whether `e1` is at or below `e2` where they cross the
    /// sweep line at the current event. Both are directed right to left.
    fn edge_leq(&self, e1: u32, e2: u32) -> bool {
        let event = self.event;
        if self.dst(e1) == event {
            if self.dst(e2) == event {
                // Both meet at the event: sort by slope.
                if self.vert_leq(self.org(e1), self.org(e2)) {
                    return self.edge_sign(self.dst(e2), self.org(e1), self.org(e2)) <= 0.0;
                }
                return self.edge_sign(self.dst(e1), self.org(e2), self.org(e1)) >= 0.0;
            }
            return self.edge_sign(self.dst(e2), event, self.org(e2)) <= 0.0;
        }
        if self.dst(e2) == event {
            return self.edge_sign(self.dst(e1), event, self.org(e1)) >= 0.0;
        }
        let t1 = self.edge_eval(self.dst(e1), event, self.org(e1));
        let t2 = self.edge_eval(self.dst(e2), event, self.org(e2));
        t1 >= t2
    }

    fn delete_region(&mut self, reg: u32) {
        let e = self.e_up(reg);
        self.e[e].active_region = NIL;
        self.dict_delete(self.r[reg].node_up);
        // As for dictionary nodes, the link would land in `eUp`.
        self.r.free(reg);
        self.r[reg].e_up = NIL;
    }

    /// `FixUpperEdge`: replace a temporary upper edge.
    fn fix_upper_edge(&mut self, reg: u32, new_edge: u32) {
        self.mesh_delete(self.e_up(reg));
        let r = &mut self.r[reg];
        r.fix_upper_edge = false;
        r.e_up = new_edge;
        self.e[new_edge].active_region = reg;
    }

    fn top_left_region(&mut self, mut reg: u32) -> u32 {
        let org = self.org(self.e_up(reg));
        // The region above the uppermost edge with the same origin.
        loop {
            if !self.tick() {
                break;
            }
            reg = self.region_above(reg);
            if self.org(self.e_up(reg)) != org {
                break;
            }
        }
        // A temporary edge from ConnectRightVertex: fix it now.
        if self.r[reg].fix_upper_edge {
            let below = self.region_below(reg);
            let e = self.mesh_connect(self.e_up(below) ^ 1, self.lnext(self.e_up(reg)));
            self.fix_upper_edge(reg, e);
            reg = self.region_above(reg);
        }
        reg
    }

    fn top_right_region(&self, mut reg: u32) -> u32 {
        let dst = self.dst(self.e_up(reg));
        loop {
            if !self.tick() {
                return reg;
            }
            reg = self.region_above(reg);
            if self.dst(self.e_up(reg)) != dst {
                return reg;
            }
        }
    }

    /// `AddRegionBelow`: a new active region with upper edge `e_new_up`,
    /// somewhere below `reg_above`. Winding and inside are not set.
    fn add_region_below(&mut self, reg_above: u32, e_new_up: u32) -> u32 {
        // The winding number and inside flag are left as they were, as
        // upstream leaves them in a reused region; callers set both.
        let reg_new = self.r.alloc();
        let r = &mut self.r[reg_new];
        r.e_up = e_new_up;
        r.node_up = NIL;
        r.dirty = false;
        r.fix_upper_edge = false;
        let node = self.dict_insert_before(self.r[reg_above].node_up, reg_new);
        self.r[reg_new].node_up = node;
        self.e[e_new_up].active_region = reg_new;
        reg_new
    }

    fn compute_winding(&mut self, reg: u32) {
        let above = self.region_above(reg);
        let w = self.r[above].winding_number + self.e[self.e_up(reg)].winding;
        let r = &mut self.r[reg];
        r.winding_number = w;
        r.inside = is_winding_inside(w);
    }

    /// `FinishRegion`: copy the region's inside flag to its mesh face and
    /// drop it from the sweep line.
    fn finish_region(&mut self, reg: u32) {
        let e = self.e_up(reg);
        let f = self.lface(e);
        self.f[f].inside = self.r[reg].inside;
        self.f[f].an_edge = e;
        self.delete_region(reg);
    }

    /// `FinishLeftRegions`: finish the regions between the left-going
    /// edges of a vertex, from `reg_first` down to (not including)
    /// `reg_last` (`NIL`: as far as possible), relinking the mesh to match
    /// the dictionary order. Returns the last left-going edge.
    fn finish_left_regions(&mut self, reg_first: u32, reg_last: u32) -> u32 {
        let mut reg_prev = reg_first;
        let mut e_prev = self.e_up(reg_first);
        while reg_prev != reg_last {
            if !self.tick() {
                break;
            }
            self.r[reg_prev].fix_upper_edge = false;
            let reg = self.region_below(reg_prev);
            let mut e = self.e_up(reg);
            if self.org(e) != self.org(e_prev) {
                if !self.r[reg].fix_upper_edge {
                    self.finish_region(reg_prev);
                    break;
                }
                // A temporary edge from ConnectRightVertex: fix it now.
                e = self.mesh_connect(self.lprev(e_prev), e ^ 1);
                self.fix_upper_edge(reg, e);
            }
            // Relink so that e_prev->Onext == e.
            if self.onext(e_prev) != e {
                self.mesh_splice(self.oprev(e), e);
                self.mesh_splice(e_prev, e);
            }
            self.finish_region(reg_prev);
            e_prev = self.e_up(reg);
            reg_prev = reg;
        }
        e_prev
    }

    /// `AddRightEdges`: put the right-going edges of one vertex, from
    /// `e_first` CCW up to (not including) `e_last`, into the dictionary,
    /// and update windings and mesh order.
    fn add_right_edges(
        &mut self,
        reg_up: u32,
        e_first: u32,
        e_last: u32,
        mut e_top_left: u32,
        clean_up: bool,
    ) -> R<()> {
        let mut e = e_first;
        loop {
            if !self.tick() {
                break;
            }
            self.add_region_below(reg_up, e ^ 1);
            e = self.onext(e);
            if e == e_last {
                break;
            }
        }
        if e_top_left == NIL {
            e_top_left = self.rprev(self.e_up(self.region_below(reg_up)));
        }
        let mut reg_prev = reg_up;
        let mut e_prev = e_top_left;
        let mut first_time = true;
        loop {
            if !self.tick() {
                break;
            }
            let reg = self.region_below(reg_prev);
            e = self.e_up(reg) ^ 1;
            if self.org(e) != self.org(e_prev) {
                break;
            }
            if self.onext(e) != e_prev {
                // Unlink e and relink it below e_prev.
                self.mesh_splice(self.oprev(e), e);
                self.mesh_splice(self.oprev(e_prev), e);
            }
            let w = self.r[reg_prev].winding_number - self.e[e].winding;
            self.r[reg].winding_number = w;
            self.r[reg].inside = is_winding_inside(w);
            // Two outgoing edges with the same slope: merge them before
            // any intersection tests.
            self.r[reg_prev].dirty = true;
            if !first_time && self.check_for_right_splice(reg_prev) {
                self.add_winding(e, e_prev);
                self.delete_region(reg_prev);
                self.mesh_delete(e_prev);
            }
            first_time = false;
            reg_prev = reg;
            e_prev = e;
        }
        self.r[reg_prev].dirty = true;
        if clean_up {
            self.walk_dirty_regions(reg_prev)?;
        }
        Ok(())
    }

    /// `AddWinding(eDst, eSrc)`
    fn add_winding(&mut self, e_dst: u32, e_src: u32) {
        let w = self.e[e_src].winding;
        let ws = self.e[e_src ^ 1].winding;
        self.e[e_dst].winding += w;
        self.e[e_dst ^ 1].winding += ws;
    }

    /// `CheckForRightSplice`: make sure the upper edge's origin is above
    /// the lower edge or the other way round, splicing the offending
    /// vertex into the other edge if not. Returns whether it changed
    /// anything.
    fn check_for_right_splice(&mut self, reg_up: u32) -> bool {
        let reg_lo = self.region_below(reg_up);
        let e_up = self.e_up(reg_up);
        let e_lo = self.e_up(reg_lo);
        if self.vert_leq(self.org(e_up), self.org(e_lo)) {
            if self.edge_sign(self.dst(e_lo), self.org(e_up), self.org(e_lo)) > 0.0 {
                return false;
            }
            // e_up->Org appears to be below e_lo.
            if !self.vert_eq(self.org(e_up), self.org(e_lo)) {
                // Splice e_up->Org into e_lo.
                self.mesh_split_edge(e_lo ^ 1);
                self.mesh_splice(e_up, self.oprev(e_lo));
                self.r[reg_up].dirty = true;
                self.r[reg_lo].dirty = true;
            } else if self.org(e_up) != self.org(e_lo) {
                // Merge the two vertices, discarding e_up->Org.
                let h = self.v[self.org(e_up)].pq_handle;
                self.pq.delete(&self.v, h);
                self.mesh_splice(self.oprev(e_lo), e_up);
            }
        } else {
            if self.edge_sign(self.dst(e_up), self.org(e_lo), self.org(e_up)) < 0.0 {
                return false;
            }
            // e_lo->Org appears to be above e_up: splice it into e_up.
            let above = self.region_above(reg_up);
            self.r[above].dirty = true;
            self.r[reg_up].dirty = true;
            self.mesh_split_edge(e_up ^ 1);
            self.mesh_splice(self.oprev(e_lo), e_up);
        }
        true
    }

    /// `CheckForLeftSplice`: the same for the destinations.
    fn check_for_left_splice(&mut self, reg_up: u32) -> bool {
        let reg_lo = self.region_below(reg_up);
        let e_up = self.e_up(reg_up);
        let e_lo = self.e_up(reg_lo);
        if self.vert_leq(self.dst(e_up), self.dst(e_lo)) {
            if self.edge_sign(self.dst(e_up), self.dst(e_lo), self.org(e_up)) < 0.0 {
                return false;
            }
            // e_lo->Dst is above e_up: splice it into e_up.
            let above = self.region_above(reg_up);
            self.r[above].dirty = true;
            self.r[reg_up].dirty = true;
            let e = self.mesh_split_edge(e_up);
            self.mesh_splice(e_lo ^ 1, e);
            let lf = self.lface(e);
            self.f[lf].inside = self.r[reg_up].inside;
        } else {
            if self.edge_sign(self.dst(e_lo), self.dst(e_up), self.org(e_lo)) > 0.0 {
                return false;
            }
            // e_up->Dst is below e_lo: splice it into e_lo.
            self.r[reg_up].dirty = true;
            self.r[reg_lo].dirty = true;
            let e = self.mesh_split_edge(e_lo);
            self.mesh_splice(self.lnext(e_up), e_lo ^ 1);
            let rf = self.rface(e);
            self.f[rf].inside = self.r[reg_up].inside;
        }
        true
    }

    /// `CheckForIntersect`: if the upper and lower edges of `reg_up`
    /// cross, add the intersection vertex. Returns true if that recursed
    /// into `AddRightEdges` (all dirty regions are then already walked).
    fn check_for_intersect(&mut self, mut reg_up: u32) -> R<bool> {
        let mut reg_lo = self.region_below(reg_up);
        let mut e_up = self.e_up(reg_up);
        let mut e_lo = self.e_up(reg_lo);
        let org_up = self.org(e_up);
        let org_lo = self.org(e_lo);
        let dst_up = self.dst(e_up);
        let dst_lo = self.dst(e_lo);
        let event = self.event;

        if org_up == org_lo {
            return Ok(false); // right endpoints are the same
        }
        let (_, ou_t) = self.st(org_up);
        let (_, du_t) = self.st(dst_up);
        let (_, ol_t) = self.st(org_lo);
        let (_, dl_t) = self.st(dst_lo);
        // Upstream's MIN and MAX macros, including which operand wins ties.
        let t_min_up = if ou_t <= du_t { ou_t } else { du_t };
        let t_max_lo = if ol_t >= dl_t { ol_t } else { dl_t };
        if t_min_up > t_max_lo {
            return Ok(false); // t ranges do not overlap
        }
        if self.vert_leq(org_up, org_lo) {
            if self.edge_sign(dst_lo, org_up, org_lo) > 0.0 {
                return Ok(false);
            }
        } else if self.edge_sign(dst_up, org_lo, org_up) < 0.0 {
            return Ok(false);
        }

        // The edges intersect, at least marginally. Compute the point in a
        // scratch vertex, as upstream does with a stack `TESSvertex`.
        let (mut is, mut it) = self.edge_intersect(dst_up, org_up, dst_lo, org_lo);
        let scratch = self.scratch;
        let (es, et) = self.st(event);
        self.set_st(scratch, is, it);
        if self.vert_leq(scratch, event) {
            // Slightly left of the sweep line: use the event instead.
            is = es;
            it = et;
            self.set_st(scratch, is, it);
        }
        // Right of the rightmost origin: clamp to it.
        let org_min = if self.vert_leq(org_up, org_lo) {
            org_up
        } else {
            org_lo
        };
        if self.vert_leq(org_min, scratch) {
            (is, it) = self.st(org_min);
            self.set_st(scratch, is, it);
        }

        if self.vert_eq(scratch, org_up) || self.vert_eq(scratch, org_lo) {
            // Easy case: intersection at one of the right endpoints.
            self.check_for_right_splice(reg_up);
            return Ok(false);
        }

        if (!self.vert_eq(dst_up, event) && self.edge_sign(dst_up, event, scratch) >= 0.0)
            || (!self.vert_eq(dst_lo, event) && self.edge_sign(dst_lo, event, scratch) <= 0.0)
        {
            // Very unusual: the new upper or lower edge would pass on the
            // wrong side of the sweep event, or through it.
            if dst_lo == event {
                // Splice dst_lo into e_up, and process the new region(s).
                self.mesh_split_edge(e_up ^ 1);
                self.mesh_splice(e_lo ^ 1, e_up);
                reg_up = self.top_left_region(reg_up);
                e_up = self.e_up(self.region_below(reg_up));
                self.finish_left_regions(self.region_below(reg_up), reg_lo);
                self.add_right_edges(reg_up, self.oprev(e_up), e_up, e_up, true)?;
                return Ok(true);
            }
            if dst_up == event {
                // Splice dst_up into e_lo, and process the new region(s).
                self.mesh_split_edge(e_lo ^ 1);
                self.mesh_splice(self.lnext(e_up), self.oprev(e_lo));
                reg_lo = reg_up;
                reg_up = self.top_right_region(reg_up);
                let e = self.rprev(self.e_up(self.region_below(reg_up)));
                self.r[reg_lo].e_up = self.oprev(e_lo);
                e_lo = self.finish_left_regions(reg_lo, NIL);
                self.add_right_edges(reg_up, self.onext(e_lo), self.rprev(e_up), e, true)?;
                return Ok(true);
            }
            // Called from ConnectRightVertex: split an edge passing on the
            // wrong side of the event and leave the rest to it.
            if self.edge_sign(dst_up, event, scratch) >= 0.0 {
                let above = self.region_above(reg_up);
                self.r[above].dirty = true;
                self.r[reg_up].dirty = true;
                self.mesh_split_edge(e_up ^ 1);
                let o = self.org(e_up);
                self.set_st(o, es, et);
            }
            if self.edge_sign(dst_lo, event, scratch) <= 0.0 {
                self.r[reg_up].dirty = true;
                self.r[reg_lo].dirty = true;
                self.mesh_split_edge(e_lo ^ 1);
                let o = self.org(e_lo);
                self.set_st(o, es, et);
            }
            return Ok(false);
        }

        // General case: split both edges and splice them into a new
        // vertex at the intersection.
        self.mesh_split_edge(e_up ^ 1);
        self.mesh_split_edge(e_lo ^ 1);
        self.mesh_splice(self.oprev(e_lo), e_up);
        let o = self.org(e_up);
        self.set_st(o, is, it);
        let h = self.pq.insert(&self.v, o);
        if h == INV_HANDLE {
            return Err(Overflow);
        }
        let v = &mut self.v[o];
        v.pq_handle = h;
        // GetIntersectData: the new vertex is no input vertex. Its
        // interpolated coordinates are not computed, since OpenSCAD drops
        // every triangle that uses it.
        v.idx = -1;
        let above = self.region_above(reg_up);
        self.r[above].dirty = true;
        self.r[reg_up].dirty = true;
        self.r[reg_lo].dirty = true;
        Ok(false)
    }

    fn set_st(&mut self, v: u32, s: f32, t: f32) {
        let v = &mut self.v[v];
        v.s = s;
        v.t = t;
    }

    /// `WalkDirtyRegions`: restore the dictionary invariants for every
    /// region whose upper or lower edge changed, bottom up.
    fn walk_dirty_regions(&mut self, mut reg_up: u32) -> R<()> {
        let mut reg_lo = self.region_below(reg_up);
        loop {
            if !self.tick() {
                return Err(Overflow);
            }
            // Find the lowest dirty region.
            while self.r[reg_lo].dirty {
                if !self.tick() {
                    break;
                }
                reg_up = reg_lo;
                reg_lo = self.region_below(reg_lo);
            }
            if !self.r[reg_up].dirty {
                reg_lo = reg_up;
                reg_up = self.region_above(reg_up);
                if reg_up == NIL || !self.r[reg_up].dirty {
                    return Ok(());
                }
            }
            self.r[reg_up].dirty = false;
            let mut e_up = self.e_up(reg_up);
            let mut e_lo = self.e_up(reg_lo);

            if self.dst(e_up) != self.dst(e_lo) {
                // Check the edge order at the Dst vertices.
                if self.check_for_left_splice(reg_up) {
                    // A fixable edge is no longer needed.
                    if self.r[reg_lo].fix_upper_edge {
                        self.delete_region(reg_lo);
                        self.mesh_delete(e_lo);
                        reg_lo = self.region_below(reg_up);
                        e_lo = self.e_up(reg_lo);
                    } else if self.r[reg_up].fix_upper_edge {
                        self.delete_region(reg_up);
                        self.mesh_delete(e_up);
                        reg_up = self.region_above(reg_lo);
                        e_up = self.e_up(reg_up);
                    }
                }
            }
            if self.org(e_up) != self.org(e_lo) {
                if self.dst(e_up) != self.dst(e_lo)
                    && !self.r[reg_up].fix_upper_edge
                    && !self.r[reg_lo].fix_upper_edge
                    && (self.dst(e_up) == self.event || self.dst(e_lo) == self.event)
                {
                    if self.check_for_intersect(reg_up)? {
                        // Walked recursively; done.
                        return Ok(());
                    }
                } else {
                    self.check_for_right_splice(reg_up);
                }
            }
            if self.org(e_up) == self.org(e_lo) && self.dst(e_up) == self.dst(e_lo) {
                // A degenerate loop of two edges: delete it.
                self.add_winding(e_lo, e_up);
                self.delete_region(reg_up);
                self.mesh_delete(e_up);
                reg_up = self.region_above(reg_lo);
            }
        }
    }

    /// `ConnectRightVertex`: connect a vertex whose edges all go left to
    /// the unprocessed part of the mesh, with a temporary fixable edge
    /// unless it lies on a neighbouring edge.
    fn connect_right_vertex(&mut self, mut reg_up: u32, mut e_bottom_left: u32) -> R<()> {
        let mut e_top_left = self.onext(e_bottom_left);
        let reg_lo = self.region_below(reg_up);
        let e_up = self.e_up(reg_up);
        let e_lo = self.e_up(reg_lo);
        let mut degenerate = false;

        if self.dst(e_up) != self.dst(e_lo) {
            self.check_for_intersect(reg_up)?;
        }
        // The upper or lower edge may now pass through the event, or meet
        // a new intersection vertex.
        if self.vert_eq(self.org(e_up), self.event) {
            self.mesh_splice(self.oprev(e_top_left), e_up);
            reg_up = self.top_left_region(reg_up);
            e_top_left = self.e_up(self.region_below(reg_up));
            self.finish_left_regions(self.region_below(reg_up), reg_lo);
            degenerate = true;
        }
        if self.vert_eq(self.org(e_lo), self.event) {
            self.mesh_splice(e_bottom_left, self.oprev(e_lo));
            e_bottom_left = self.finish_left_regions(reg_lo, NIL);
            degenerate = true;
        }
        if degenerate {
            return self.add_right_edges(
                reg_up,
                self.onext(e_bottom_left),
                e_top_left,
                e_top_left,
                true,
            );
        }
        // Connect to the closer of e_lo->Org and e_up->Org.
        let e_new = if self.vert_leq(self.org(e_lo), self.org(e_up)) {
            self.oprev(e_lo)
        } else {
            e_up
        };
        let e_new = self.mesh_connect(self.lprev(e_bottom_left), e_new);
        // No clean-up yet, or e_new might go before it is marked.
        self.add_right_edges(reg_up, e_new, self.onext(e_new), self.onext(e_new), false)?;
        let ar = self.e[e_new ^ 1].active_region;
        self.r[ar].fix_upper_edge = true;
        self.walk_dirty_regions(reg_up)
    }

    /// `ConnectLeftDegenerate`: the event lies exactly on a processed edge
    /// or vertex; splice it in.
    fn connect_left_degenerate(&mut self, mut reg_up: u32, v_event: u32) -> R<()> {
        let e = self.e_up(reg_up);
        if self.vert_eq(self.org(e), v_event) {
            // e->Org is unprocessed: combine them and wait for it to come
            // out of the queue. (Upstream asserts this cannot happen; its
            // release build runs it.)
            self.mesh_splice(e, self.v[v_event].an_edge);
            return Ok(());
        }
        if !self.vert_eq(self.dst(e), v_event) {
            // General case: splice v_event into e, which passes through it.
            self.mesh_split_edge(e ^ 1);
            if self.r[reg_up].fix_upper_edge {
                // Delete the unused part of the fixable edge.
                self.mesh_delete(self.onext(e));
                self.r[reg_up].fix_upper_edge = false;
            }
            self.mesh_splice(self.v[v_event].an_edge, e);
            return self.sweep_event(v_event);
        }
        // v_event coincides with e->Dst, already processed (upstream
        // asserts this away too). Splice in the extra right-going edges.
        reg_up = self.top_right_region(reg_up);
        let reg = self.region_below(reg_up);
        let mut e_top_right = self.e_up(reg) ^ 1;
        let mut e_top_left = self.onext(e_top_right);
        let e_last = e_top_left;
        if self.r[reg].fix_upper_edge {
            self.delete_region(reg);
            self.mesh_delete(e_top_right);
            e_top_right = self.oprev(e_top_left);
        }
        self.mesh_splice(self.v[v_event].an_edge, e_top_right);
        if !self.edge_goes_left(e_top_left) {
            e_top_left = NIL;
        }
        self.add_right_edges(reg_up, self.onext(e_top_right), e_last, e_top_left, true)
    }

    /// `ConnectLeftVertex`: connect a vertex whose edges all go right to
    /// the processed part of the mesh.
    fn connect_left_vertex(&mut self, v_event: u32) -> R<()> {
        let an = self.v[v_event].an_edge;
        let reg_up = self.dict_search(an ^ 1);
        let reg_lo = self.region_below(reg_up);
        if reg_lo == NIL {
            // Upstream: "This may happen if the input polygon is coplanar."
            return Ok(());
        }
        let e_up = self.e_up(reg_up);
        let e_lo = self.e_up(reg_lo);

        // Try merging with the upper or lower chain first.
        if self.edge_sign(self.dst(e_up), v_event, self.org(e_up)) == 0.0 {
            return self.connect_left_degenerate(reg_up, v_event);
        }
        // Connect to the rightmost processed vertex of either chain.
        let reg = if self.vert_leq(self.dst(e_lo), self.dst(e_up)) {
            reg_up
        } else {
            reg_lo
        };
        if self.r[reg_up].inside || self.r[reg].fix_upper_edge {
            let e_new = if reg == reg_up {
                self.mesh_connect(an ^ 1, self.lnext(e_up))
            } else {
                self.mesh_connect(self.dnext(e_lo), an) ^ 1
            };
            if self.r[reg].fix_upper_edge {
                self.fix_upper_edge(reg, e_new);
            } else {
                let r = self.add_region_below(reg_up, e_new);
                self.compute_winding(r);
            }
            self.sweep_event(v_event)
        } else {
            // Outside the polygon: no need to connect it.
            self.add_right_edges(reg_up, an, an, NIL, true)
        }
    }

    /// `SweepEvent`: everything the sweep line does at a vertex.
    fn sweep_event(&mut self, v_event: u32) -> R<()> {
        self.event = v_event;
        // Is the vertex the right end of an edge already in the dictionary?
        let an = self.v[v_event].an_edge;
        let mut e = an;
        while self.e[e].active_region == NIL {
            if !self.tick() {
                break;
            }
            e = self.onext(e);
            if e == an {
                // All edges go right.
                return self.connect_left_vertex(v_event);
            }
        }
        // Finish the regions the left-going edges close.
        let reg_up = self.top_left_region(self.e[e].active_region);
        let reg = self.region_below(reg_up);
        let e_top_left = self.e_up(reg);
        let e_bottom_left = self.finish_left_regions(reg, NIL);
        // Then add the right-going edges.
        if self.onext(e_bottom_left) == e_top_left {
            self.connect_right_vertex(reg_up, e_bottom_left)
        } else {
            self.add_right_edges(
                reg_up,
                self.onext(e_bottom_left),
                e_top_left,
                e_top_left,
                true,
            )
        }
    }

    /// `AddSentinel`: a horizontal edge at `t` beyond every input feature.
    fn add_sentinel(&mut self, smin: f32, smax: f32, t: f32) {
        let e = self.mesh_make_edge();
        let o = self.org(e);
        self.set_st(o, smax, t);
        let d = self.dst(e);
        self.set_st(d, smin, t);
        self.event = d;
        let reg = self.r.alloc();
        self.r[reg] = Region {
            e_up: e,
            node_up: NIL,
            winding_number: 0,
            inside: false,
            dirty: false,
            fix_upper_edge: false,
        };
        // dictInsert: insert before the head.
        let node = self.dict_insert_before(0, reg);
        self.r[reg].node_up = node;
    }

    /// `InitEdgeDict`: the two sentinels. The bounding box is widened by
    /// its size or 0.01, in `double` since the constant is one, then
    /// stored back in `float`.
    fn init_edge_dict(&mut self) {
        self.d.reset(&[super::DictNode {
            key: NIL,
            next: 0,
            prev: 0,
        }]);
        let w = self.bmax[0] - self.bmin[0];
        let h = self.bmax[1] - self.bmin[1];
        let pad = |x: f32| {
            if f64::from(x) > 0.01 {
                f64::from(x)
            } else {
                0.01
            }
        };
        let smin = (f64::from(self.bmin[0]) - pad(w)) as f32;
        let smax = (f64::from(self.bmax[0]) + pad(w)) as f32;
        let tmin = (f64::from(self.bmin[1]) - pad(h)) as f32;
        let tmax = (f64::from(self.bmax[1]) + pad(h)) as f32;
        self.add_sentinel(smin, smax, tmin);
        self.add_sentinel(smin, smax, tmax);
    }

    /// `RemoveDegenerateEdges`: zero-length edges, and contours of fewer
    /// than three vertices.
    fn remove_degenerate_edges(&mut self) {
        let mut e = self.e[0].next;
        while e != 0 {
            if !self.tick() {
                break;
            }
            let mut e_next = self.e[e].next;
            let mut e_lnext = self.lnext(e);
            if self.vert_eq(self.org(e), self.dst(e)) && self.lnext(self.lnext(e)) != e {
                // Zero-length edge in a contour of at least 3 edges.
                self.mesh_splice(e_lnext, e);
                self.mesh_delete(e);
                e = e_lnext;
                e_lnext = self.lnext(e);
            }
            if self.lnext(e_lnext) == e {
                // A contour of one or two edges.
                if e_lnext != e {
                    if e_lnext == e_next || e_lnext == e_next ^ 1 {
                        e_next = self.e[e_next].next;
                    }
                    self.mesh_delete(e_lnext);
                }
                if e == e_next || e == e_next ^ 1 {
                    e_next = self.e[e_next].next;
                }
                self.mesh_delete(e);
            }
            e = e_next;
        }
    }

    /// `InitPriorityQ`: every vertex into the queue, with room for
    /// `max(8, extraVertices)` more (OpenSCAD sets `extraVertices` to 256).
    fn init_priority_q(&mut self) -> R<()> {
        let mut count = 0;
        let mut v = self.v[0].next;
        while v != 0 {
            if !self.tick() {
                break;
            }
            count += 1;
            v = self.v[v].next;
        }
        self.pq.reset(count + 256);
        let mut v = self.v[0].next;
        while v != 0 {
            if !self.tick() {
                break;
            }
            let h = self.pq.insert(&self.v, v);
            if h == INV_HANDLE {
                return Err(Overflow);
            }
            self.v[v].pq_handle = h;
            v = self.v[v].next;
        }
        self.pq.init(&self.v);
        Ok(())
    }

    /// `RemoveDegenerateFaces`: faces of two edges that splices on
    /// processed edges can leave behind. Upstream saves `f->next` before
    /// deleting, but the deletion can free that face too; it then walks
    /// the free list, as this does through the arena (see `arena.rs`).
    fn remove_degenerate_faces(&mut self) {
        let mut f = self.f[0].next;
        while f != 0 {
            if !self.tick() {
                break;
            }
            let f_next = self.f[f].next;
            let e = self.f[f].an_edge;
            if self.lnext(self.lnext(e)) == e {
                self.add_winding(self.onext(e), e);
                self.mesh_delete(e);
            }
            f = f_next;
        }
    }

    /// `tessComputeInterior`: the planar arrangement of the contours, split
    /// into monotone regions marked inside or outside.
    pub(super) fn compute_interior(&mut self) -> R<()> {
        self.remove_degenerate_edges();
        self.init_priority_q()?;
        self.init_edge_dict();
        loop {
            if !self.tick() {
                break;
            }
            let v = self.pq.extract_min(&self.v);
            if v == NIL {
                break;
            }
            loop {
                if !self.tick() {
                    break;
                }
                let v_next = self.pq.minimum(&self.v);
                if v_next == NIL || !self.vert_eq(v_next, v) {
                    break;
                }
                // Merge all vertices at exactly the same place.
                let v_next = self.pq.extract_min(&self.v);
                self.mesh_splice(self.v[v].an_edge, self.v[v_next].an_edge);
            }
            self.sweep_event(v)?;
        }
        // DoneEdgeDict and DonePriorityQ only free memory.
        self.remove_degenerate_faces();
        Ok(())
    }
}
