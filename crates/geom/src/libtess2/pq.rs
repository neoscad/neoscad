//! The event queue (`priorityq.c`): the input vertices sorted once by a
//! randomized quicksort with a fixed seed, plus a binary heap for the
//! intersection vertices the sweep adds. Ties between equal vertices are
//! broken by the sort's exact comparison sequence, so it is ported step for
//! step rather than replaced with `sort_by`.
//!
//! OpenSCAD gives libtess2 no `realloc` (`GeometryUtils.cc`,
//! `ma.extraVertices = 256`), so neither part can grow: a heap insert past
//! half the initial capacity fails, and the whole polygon then produces no
//! triangles. [`Pq::heap_insert`] keeps that limit.

use super::{Arena, NIL, Vertex};

/// `INV_HANDLE`: an insert failed.
pub(super) const INV_HANDLE: i32 = 0x0fff_ffff;

#[derive(Default)]
pub(super) struct Pq {
    // The heap (`PriorityQHeap`); nodes and handles are 1-based.
    nodes: Vec<i32>,
    handles: Vec<(u32, i32)>,
    heap_size: i32,
    heap_max: i32,
    free_list: i32,
    // The sorted part (`PriorityQ`).
    keys: Vec<u32>,
    order: Vec<u32>,
    size: i32,
    max: i32,
    initialized: bool,
}

#[inline]
fn leq(v: &Arena<Vertex>, x: u32, y: u32) -> bool {
    let (x, y) = (&v[x], &v[y]);
    x.s < y.s || (x.s == y.s && x.t <= y.t)
}

impl Pq {
    /// `pqNewPriorityQ(size)`, reusing this queue's storage.
    pub(super) fn reset(&mut self, size: i32) {
        // Upstream allocates `size + 1` entries; they are only reached as
        // the heap grows, so they are added on demand (`heap_insert`).
        self.nodes.clear();
        self.nodes.resize(2, 0);
        self.handles.clear();
        self.handles.resize(2, (NIL, 0));
        self.heap_size = 0;
        self.heap_max = size;
        self.free_list = 0;
        // So that the heap minimum of an empty heap is NULL.
        self.nodes[1] = 1;
        self.handles[1].0 = NIL;
        self.keys.clear();
        self.order.clear();
        self.size = 0;
        self.max = size;
        self.initialized = false;
    }

    fn float_down(&mut self, v: &Arena<Vertex>, mut curr: i32) {
        let h_curr = self.nodes[curr as usize];
        loop {
            let mut child = curr << 1;
            if child < self.heap_size
                && leq(
                    v,
                    self.handles[self.nodes[(child + 1) as usize] as usize].0,
                    self.handles[self.nodes[child as usize] as usize].0,
                )
            {
                child += 1;
            }
            if child > self.heap_size
                || leq(
                    v,
                    self.handles[h_curr as usize].0,
                    self.handles[self.nodes[child as usize] as usize].0,
                )
            {
                self.nodes[curr as usize] = h_curr;
                self.handles[h_curr as usize].1 = curr;
                break;
            }
            let h_child = self.nodes[child as usize];
            self.nodes[curr as usize] = h_child;
            self.handles[h_child as usize].1 = curr;
            curr = child;
        }
    }

    fn float_up(&mut self, v: &Arena<Vertex>, mut curr: i32) {
        let h_curr = self.nodes[curr as usize];
        loop {
            let parent = curr >> 1;
            let h_parent = self.nodes[parent as usize];
            if parent == 0
                || leq(
                    v,
                    self.handles[h_parent as usize].0,
                    self.handles[h_curr as usize].0,
                )
            {
                self.nodes[curr as usize] = h_curr;
                self.handles[h_curr as usize].1 = curr;
                break;
            }
            self.nodes[curr as usize] = h_parent;
            self.handles[h_parent as usize].1 = curr;
            curr = parent;
        }
    }

    /// `pqHeapInsert`, with no `realloc`: fails once the heap would pass
    /// half its capacity.
    fn heap_insert(&mut self, v: &Arena<Vertex>, key: u32) -> i32 {
        self.heap_size += 1;
        let curr = self.heap_size;
        if curr * 2 > self.heap_max {
            return INV_HANDLE;
        }
        // FloatDown reads children up to 2 * size, within upstream's
        // capacity; keep them allocated.
        let need = 2 * curr as usize + 2;
        if self.nodes.len() < need {
            self.nodes.resize(need, 0);
            self.handles.resize(need, (NIL, 0));
        }
        let free = if self.free_list == 0 {
            curr
        } else {
            let f = self.free_list;
            self.free_list = self.handles[f as usize].1;
            f
        };
        self.nodes[curr as usize] = free;
        self.handles[free as usize] = (key, curr);
        // The heap is always initialized here: pqInit runs before the
        // sweep adds anything.
        self.float_up(v, curr);
        free
    }

    fn heap_minimum(&self) -> u32 {
        self.handles[self.nodes[1] as usize].0
    }

    fn heap_extract_min(&mut self, v: &Arena<Vertex>) -> u32 {
        let h_min = self.nodes[1];
        let min = self.handles[h_min as usize].0;
        if self.heap_size > 0 {
            self.nodes[1] = self.nodes[self.heap_size as usize];
            let n1 = self.nodes[1];
            self.handles[n1 as usize].1 = 1;
            self.handles[h_min as usize] = (NIL, self.free_list);
            self.free_list = h_min;
            self.heap_size -= 1;
            if self.heap_size > 0 {
                self.float_down(v, 1);
            }
        }
        min
    }

    fn heap_delete(&mut self, v: &Arena<Vertex>, h_curr: i32) {
        let curr = self.handles[h_curr as usize].1;
        self.nodes[curr as usize] = self.nodes[self.heap_size as usize];
        let nc = self.nodes[curr as usize];
        self.handles[nc as usize].1 = curr;
        self.heap_size -= 1;
        if curr <= self.heap_size {
            if curr <= 1
                || leq(
                    v,
                    self.handles[self.nodes[(curr >> 1) as usize] as usize].0,
                    self.handles[self.nodes[curr as usize] as usize].0,
                )
            {
                self.float_down(v, curr);
            } else {
                self.float_up(v, curr);
            }
        }
        self.handles[h_curr as usize] = (NIL, self.free_list);
        self.free_list = h_curr;
    }

    /// `pqInit`: sort the keys inserted so far, largest first, with the
    /// randomized quicksort and insertion sort upstream uses.
    pub(super) fn init(&mut self, v: &Arena<Vertex>) {
        let key = |pq: &Pq, i: usize| pq.keys[pq.order[i] as usize];
        let n = self.size as usize;
        self.order.clear();
        self.order.extend(0..n as u32);
        let mut seed: u32 = 2016473283;
        let mut stack: Vec<(isize, isize)> = vec![(0, n as isize - 1)];
        while let Some((mut p, mut r)) = stack.pop() {
            while r > p + 10 {
                seed = seed.wrapping_mul(1539415821).wrapping_add(1);
                let i = p + (u64::from(seed) % (r - p + 1) as u64) as isize;
                self.order.swap(i as usize, p as usize);
                let piv = self.order[p as usize];
                let pk = self.keys[piv as usize];
                let mut i = p - 1;
                let mut j = r + 1;
                loop {
                    // GT(**i, *piv) is !LEQ(**i, *piv); LT(**j, *piv) is
                    // !LEQ(*piv, **j).
                    loop {
                        i += 1;
                        if leq(v, key(self, i as usize), pk) {
                            break;
                        }
                    }
                    loop {
                        j -= 1;
                        if leq(v, pk, key(self, j as usize)) {
                            break;
                        }
                    }
                    self.order.swap(i as usize, j as usize);
                    if i >= j {
                        break;
                    }
                }
                // Undo the last swap.
                self.order.swap(i as usize, j as usize);
                if i - p < r - j {
                    stack.push((j + 1, r));
                    r = i - 1;
                } else {
                    stack.push((p, i - 1));
                    p = j + 1;
                }
            }
            // Insertion sort small lists.
            let mut i = p + 1;
            while i <= r {
                let piv = self.order[i as usize];
                let pk = self.keys[piv as usize];
                let mut j = i;
                while j > p && !leq(v, pk, key(self, (j - 1) as usize)) {
                    self.order[j as usize] = self.order[(j - 1) as usize];
                    j -= 1;
                }
                self.order[j as usize] = piv;
                i += 1;
            }
        }
        self.max = self.size;
        self.initialized = true;
        // pqHeapInit on an empty heap does nothing.
    }

    /// `pqInsert`: before `init`, append to the keys to sort (a negative
    /// handle); after, insert into the heap.
    pub(super) fn insert(&mut self, v: &Arena<Vertex>, key: u32) -> i32 {
        if self.initialized {
            return self.heap_insert(v, key);
        }
        let curr = self.size;
        self.size += 1;
        if self.size >= self.max {
            return INV_HANDLE;
        }
        self.keys.push(key);
        -(curr + 1)
    }

    /// `pqExtractMin`, or `NIL` when empty.
    pub(super) fn extract_min(&mut self, v: &Arena<Vertex>) -> u32 {
        if self.size == 0 {
            return self.heap_extract_min(v);
        }
        let sort_min = self.keys[self.order[self.size as usize - 1] as usize];
        if self.heap_size != 0 {
            let heap_min = self.heap_minimum();
            if leq(v, heap_min, sort_min) {
                return self.heap_extract_min(v);
            }
        }
        loop {
            self.size -= 1;
            if !(self.size > 0 && self.keys[self.order[self.size as usize - 1] as usize] == NIL) {
                break;
            }
        }
        sort_min
    }

    /// `pqMinimum`, or `NIL` when empty.
    pub(super) fn minimum(&self, v: &Arena<Vertex>) -> u32 {
        if self.size == 0 {
            return self.heap_minimum();
        }
        let sort_min = self.keys[self.order[self.size as usize - 1] as usize];
        if self.heap_size != 0 {
            let heap_min = self.heap_minimum();
            if leq(v, heap_min, sort_min) {
                return heap_min;
            }
        }
        sort_min
    }

    /// `pqDelete`
    pub(super) fn delete(&mut self, v: &Arena<Vertex>, curr: i32) {
        if curr >= 0 {
            self.heap_delete(v, curr);
            return;
        }
        let curr = -(curr + 1);
        self.keys[curr as usize] = NIL;
        while self.size > 0 && self.keys[self.order[self.size as usize - 1] as usize] == NIL {
            self.size -= 1;
        }
    }
}
