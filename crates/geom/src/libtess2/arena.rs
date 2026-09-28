//! Storage for one kind of mesh element, indexed by `u32`, allocating the
//! way upstream's `bucketalloc.c` does.
//!
//! On inputs that break its preconditions (collinear faces with repeated
//! points, for instance) upstream follows links to elements it has already
//! freed. That is deterministic as long as the memory was reused rather
//! than returned: `bucketAlloc` hands out the most recently freed element
//! first, a reused element keeps the fields its constructor does not set,
//! and `bucketFree` overwrites the element's first field with the
//! free-list link. [`Arena::alloc`] and [`Arena::free`] reproduce that, so
//! stale links reach the same elements as in OpenSCAD.
//!
//! Where upstream follows a null link it crashes (so does OpenSCAD). Here
//! the read yields an inert element (all links null), a write goes to a
//! scratch one, and the arena sets the flag all of a tessellator's arenas
//! share; [`super::Tess::tick`] then ends every traversal and the polygon
//! fails like one upstream cannot tessellate. Freeing a null link, where
//! upstream would corrupt its allocator, sets the flag too. A `Vec` index
//! is bounds-checked anyway, so the normal path pays nothing for this.

use std::cell::Cell;
use std::rc::Rc;

pub(super) struct Arena<T> {
    items: Vec<T>,
    inert: T,
    scratch: T,
    /// Set when a link was null; shared by all of a tessellator's arenas,
    /// so that [`super::Tess::tick`] checks one flag, and cleared only when
    /// a polygon starts ([`super::Tess::begin`]).
    broken: Rc<Cell<bool>>,
    /// Elements before the pool: the list heads, which upstream keeps in
    /// its mesh and dictionary structs rather than in a bucket.
    base: u32,
    /// Items per slot: 2 for half-edges, which come in pairs.
    per: u32,
    /// `bucketSize` for this kind of element.
    bucket: u32,
    // The free list: freed slots (most recent last), then the untouched
    // rest of the newest bucket, then the first bucket's last slot, which
    // stays at the tail for good (`bucketAlloc` makes a new bucket rather
    // than hand out an item whose link is null).
    freed: Vec<u32>,
    fresh_next: u32,
    fresh_end: u32,
    buckets: u32,
}

impl<T: Default + Copy> Arena<T> {
    pub(super) fn new(base: u32, per: u32, bucket: u32, broken: Rc<Cell<bool>>) -> Self {
        Arena {
            items: Vec::new(),
            inert: T::default(),
            scratch: T::default(),
            broken,
            base,
            per,
            bucket,
            freed: Vec::new(),
            fresh_next: 0,
            fresh_end: 0,
            buckets: 0,
        }
    }

    /// A fresh allocator (`createBucketAlloc`), holding only the heads.
    pub(super) fn reset(&mut self, heads: &[T]) {
        self.items.clear();
        self.items.extend_from_slice(heads);
        self.freed.clear();
        self.fresh_next = 0;
        self.fresh_end = 0;
        self.buckets = 0;
    }

    /// Elements ever allocated, heads included.
    pub(super) fn len(&self) -> usize {
        self.items.len()
    }

    #[inline]
    fn item(&self, slot: u32) -> u32 {
        self.base + self.per * slot
    }

    /// `bucketAlloc`: the index of the element (the first of the pair for
    /// half-edges). A reused element keeps its old fields; a new one starts
    /// from `T::default()` (upstream's is uninitialized memory).
    pub(super) fn alloc(&mut self) -> u32 {
        let slot = if let Some(s) = self.freed.pop() {
            s
        } else {
            if self.fresh_next == self.fresh_end {
                // `CreateBucket`. The first bucket's last slot becomes the
                // permanent tail, so only bucket - 1 of its slots are used.
                let start = self.buckets * self.bucket;
                self.buckets += 1;
                self.fresh_next = start;
                self.fresh_end = start + self.bucket - u32::from(self.buckets == 1);
            }
            self.fresh_next += 1;
            self.fresh_next - 1
        };
        let idx = self.item(slot);
        let need = (idx + self.per) as usize;
        if self.items.len() < need {
            self.items.resize(need, T::default());
        }
        idx
    }

    /// `bucketFree`: returns what upstream writes into the element's first
    /// field, the old head of the free list (as an element index).
    pub(super) fn free(&mut self, idx: u32) -> u32 {
        // Freeing something that is not an element (a null link read from
        // a broken mesh) is where upstream corrupts its allocator. Here it
        // only marks the mesh broken: queued, the bogus slot would come
        // back from `alloc` as an index near `u32::MAX`, which overflows
        // and could ask `resize` for billions of elements.
        if idx < self.base || idx as usize >= self.items.len() {
            self.broken.set(true);
            return u32::MAX;
        }
        let head = if let Some(&s) = self.freed.last() {
            self.item(s)
        } else if self.fresh_next < self.fresh_end {
            self.item(self.fresh_next)
        } else {
            // The tail slot (bucket - 1 of the first bucket).
            self.item(self.bucket - 1)
        };
        self.freed.push((idx - self.base) / self.per);
        head
    }
}

impl<T> std::ops::Index<u32> for Arena<T> {
    type Output = T;
    #[inline]
    fn index(&self, i: u32) -> &T {
        match self.items.get(i as usize) {
            Some(x) => x,
            None => {
                self.broken.set(true);
                &self.inert
            }
        }
    }
}

impl<T> std::ops::IndexMut<u32> for Arena<T> {
    #[inline]
    fn index_mut(&mut self, i: u32) -> &mut T {
        match self.items.get_mut(i as usize) {
            Some(x) => x,
            None => {
                self.broken.set(true);
                &mut self.scratch
            }
        }
    }
}
