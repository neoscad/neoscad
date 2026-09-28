//! OpenSCAD's use of libtess2: `GeometryUtils::tessellatePolygonWithHoles`
//! (`GeometryUtils.cc`), for the one-contour faces `tessellate_faces`
//! passes it and for the facets with holes that
//! `createPolySetFromNefPolyhedron3` (`cgalutils.cc`) passes it.
//!
//! Around the tessellator it cleans the face by index (repeated vertices,
//! null ears, non-finite points), keeps triangles as they are, and after
//! tessellating repairs what libtess2 can break for a mesh: it reverses a
//! triangle whose edges run against the face's, drops triangles that use a
//! vertex libtess2 created at a self-intersection, and closes whatever the
//! triangles leave uncovered with `EdgeDict::triangulateLoops`. That last
//! step walks `std::unordered_map`s, so the dictionary is kept in a
//! [`CxxMap`] that iterates in libc++'s order.

use super::Tess;
use super::convex::Convex;
use super::cxxmap::CxxMap;

/// Triangulates polygon faces exactly as OpenSCAD does. Holds the
/// tessellator's storage, so one value can tessellate many faces without
/// allocating for each.
#[derive(Default)]
pub struct Tessellator {
    tess: Tess,
    convex: Convex,
    counts: FastCounts,
    dict: EdgeDict,
    face: Vec<u32>,
    /// A cycle being cleaned, and the contour lengths within `face`, for
    /// [`Tessellator::tessellate_polygon_with_holes`].
    cycle: Vec<u32>,
    lens: Vec<usize>,
    pts: Vec<[f32; 3]>,
    /// Always run the full sweep (see [`Tessellator::set_fast_paths`]).
    full_sweep_only: bool,
}

impl std::fmt::Debug for Tessellator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tessellator")
    }
}

impl Tessellator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Turn the convex fast path (`convex.rs`) on or off. It is on by
    /// default; turning it off runs every polygon through the full sweep,
    /// which gives the same triangles more slowly. This exists so that the
    /// two paths can be compared against each other.
    #[doc(hidden)]
    pub fn set_fast_paths(&mut self, on: bool) {
        self.full_sweep_only = !on;
    }

    /// `tessellatePolygonWithHoles(vertices, {face}, triangles, nullptr)`:
    /// appends the triangles of `face` (indices into `verts`) to `out`.
    /// `verts` are the mesh's vertices in `float`, as OpenSCAD casts them;
    /// distinct indices must name distinct vertices.
    pub fn tessellate_polygon(
        &mut self,
        verts: &[[f32; 3]],
        face: &[u32],
        out: &mut Vec<[u32; 3]>,
    ) {
        if !self.full_sweep_only {
            // Quads, the commonest face, straight to their fast path when
            // the clean-up has nothing to remove (no index repeats next to
            // itself or one apart, every point finite).
            if let &[a, b, c, d] = face {
                let pts = [a, b, c, d].map(|i| verts[i as usize]);
                let clean = (a != b)
                    & (b != c)
                    & (c != d)
                    & (d != a)
                    & (a != c)
                    & (b != d)
                    & pts.iter().flatten().all(|x| x.is_finite());
                if clean && self.convex.tessellate_quad(pts, face, out) {
                    return;
                }
            }
            // Caps and faces of unrotated models, in one pass (see there).
            if self.convex.tessellate_axis_aligned(verts, face, out) {
                return;
            }
        }
        // Remove consecutive equal indices and null ears (a, b, a), and
        // vertices with a NaN or infinite coordinate, which crash libtess2.
        // Most faces have none: one pass gathers the points and checks,
        // and they skip the copy.
        self.pts.clear();
        if !gather(verts, face, &mut self.pts) {
            self.tessellate_clean(verts, face, out);
            return;
        }
        let mut f = std::mem::take(&mut self.face);
        f.clear();
        f.extend_from_slice(face);
        clean(verts, &mut f);
        self.pts.clear();
        self.pts.extend(f.iter().map(|&i| verts[i as usize]));
        self.tessellate_clean(verts, &f, out);
        self.face = f;
    }

    /// `tessellatePolygonWithHoles(vertices, faces, triangles, nullptr)`:
    /// appends the triangles of the polygon whose outline is `faces[0]`
    /// and whose other contours are holes to `out`. Each contour is
    /// cleaned as a lone face is; nothing comes out if the outline has
    /// fewer than three points left, and holes that collapse are dropped.
    /// libtess2 fills by the odd rule, but the repair after it flips a
    /// triangle whose edge runs against a contour's, so holes should wind
    /// against the outline, as a Nef facet's do; one winding with it gets
    /// refilled by `triangulateLoops`, as upstream does.
    pub fn tessellate_polygon_with_holes(
        &mut self,
        verts: &[[f32; 3]],
        faces: &[Vec<u32>],
        out: &mut Vec<[u32; 3]>,
    ) {
        match faces {
            [] => return,
            // The same steps as for a face of a mesh, fast paths included.
            [f] => return self.tessellate_polygon(verts, f, out),
            _ => {}
        }
        let mut flat = std::mem::take(&mut self.face);
        let mut cycle = std::mem::take(&mut self.cycle);
        flat.clear();
        self.lens.clear();
        for (k, face) in faces.iter().enumerate() {
            cycle.clear();
            cycle.extend_from_slice(face);
            clean(verts, &mut cycle);
            if cycle.len() < 3 {
                if k == 0 {
                    // Upstream returns before looking at the holes.
                    break;
                }
                continue;
            }
            flat.extend_from_slice(&cycle);
            self.lens.push(cycle.len());
        }
        if !self.lens.is_empty() {
            self.pts.clear();
            self.pts.extend(flat.iter().map(|&i| verts[i as usize]));
            if self.lens.len() == 1 {
                // Every hole collapsed: upstream hands libtess2 the outline
                // alone, which is the lone-face case (a triangle passes
                // straight through there, as it does upstream).
                self.tessellate_clean(verts, &flat, out);
            } else {
                let lens = std::mem::take(&mut self.lens);
                self.sweep(verts, &flat, &lens, out);
                self.lens = lens;
            }
        }
        self.face = flat;
        self.cycle = cycle;
    }

    /// The rest of `tessellatePolygonWithHoles`, for a cleaned face whose
    /// points are in `self.pts`.
    fn tessellate_clean(&mut self, verts: &[[f32; 3]], f: &[u32], out: &mut Vec<[u32; 3]>) {
        if f.len() < 3 {
            return;
        }
        if f.len() == 3 {
            out.push([f[0], f[1], f[2]]);
            return;
        }

        // A convex polygon skips the sweep (see `convex.rs`). Its triangles
        // all run the polygon's way round, so the flip test below never
        // fires and every edge count returns to zero: the bookkeeping can
        // be skipped too.
        if !self.full_sweep_only {
            let done = if let [a, b, c, d] = self.pts[..] {
                self.convex.tessellate_quad([a, b, c, d], f, out)
            } else {
                self.convex.tessellate(&self.pts, f, out)
            };
            if done {
                return;
            }
        }

        self.sweep(verts, f, &[f.len()], out);
    }

    /// libtess2 and the repairs after it, for cleaned contours whose
    /// points are in `self.pts`: `f` holds the contours' indices back to
    /// back (upstream's `allindices`) and `lens` their lengths.
    fn sweep(&mut self, verts: &[[f32; 3]], f: &[u32], lens: &[usize], out: &mut Vec<[u32; 3]>) {
        self.tess.begin(&self.pts, lens);
        if !self.tess.tesselate() {
            return;
        }
        // Every edge of the polygon, which the triangles must reproduce.
        // Only the counts matter unless loops are left over (below).
        self.counts.reset(f.len());
        for c in contours(f, lens) {
            add_face(&mut self.counts, c);
        }
        let tris_before = out.len();
        let elements = &self.tess.elements;
        let vindices = &self.tess.vertex_indices;
        for t in elements.as_chunks::<3>().0 {
            let mut mapped = [0i32; 3];
            let mut err = false;
            for k in 0..3 {
                // A vertex libtess2 created (TESS_UNDEF): skip the
                // triangle. Upstream calls this a FIXME.
                match t[k] {
                    n if n >= 0 && vindices[n as usize] >= 0 => {
                        mapped[k] = f[vindices[n as usize] as usize] as i32;
                    }
                    _ => err = true,
                }
            }
            if err {
                continue;
            }
            // A triangle with an edge running against the polygon's is
            // flipped, unless another edge agrees with it.
            let mut reverse = false;
            for k in 0..3 {
                let e = (mapped[k], mapped[(k + 1) % 3]);
                if self.counts.count(e) > 0 {
                    reverse = false;
                    break;
                } else if self.counts.count((e.1, e.0)) > 0 {
                    reverse = true;
                }
            }
            if reverse {
                mapped.reverse();
            }
            remove_triangle(&mut self.counts, mapped);
            out.push(mapped.map(|x| x as u32));
        }
        if !self.counts.is_empty() {
            // Close the loops the triangles left open. Which triangles that
            // makes depends on the order of upstream's hash map, which
            // depends on its whole history: replay it in the libc++ map.
            self.dict.edges.reset();
            for c in contours(f, lens) {
                add_face(&mut self.dict, c);
            }
            for t in &out[tris_before..] {
                remove_triangle(&mut self.dict, t.map(|x| x as i32));
            }
            self.dict.triangulate_loops(out);
        }
        debug_assert!(
            out[tris_before..]
                .iter()
                .flatten()
                .all(|&x| (x as usize) < verts.len())
        );
    }
}

/// The contours of `f`, split at the lengths in `lens`.
fn contours<'a>(f: &'a [u32], lens: &'a [usize]) -> impl Iterator<Item = &'a [u32]> + 'a {
    lens.iter().scan(0usize, move |start, &n| {
        let c = &f[*start..*start + n];
        *start += n;
        Some(c)
    })
}

/// Push the points of `face` onto `pts`, and return whether [`clean`]
/// would change the face: a repeated index next to itself or one apart (a
/// null ear), or a non-finite vertex.
fn gather(verts: &[[f32; 3]], face: &[u32], pts: &mut Vec<[f32; 3]>) -> bool {
    let n = face.len();
    if n < 3 {
        return true;
    }
    let mut bad = false;
    let (mut a, mut b) = (face[n - 2], face[n - 1]);
    for &c in face {
        let p = verts[c as usize];
        bad |= (a == b) | (a == c) | !(p[0].is_finite() & p[1].is_finite() & p[2].is_finite());
        pts.push(p);
        (a, b) = (b, c);
    }
    bad
}

/// The clean-up loop at the start of `tessellatePolygonWithHoles`, in
/// place.
fn clean(verts: &[[f32; 3]], f: &mut Vec<u32>) {
    let mut i: usize = 0;
    while f.len() >= 3 && i < f.len() {
        let n = f.len();
        if f[i] == f[(i + 1) % n] {
            f.remove(i);
        } else if f[(i + n - 1) % n] == f[(i + 1) % n] {
            if i == 0 {
                f.drain(0..2);
            } else {
                f.drain(i - 1..i + 1);
            }
            // Upstream's `size_t` wraps at 0, which ends the loop.
            i = i.wrapping_sub(1);
        } else if verts[f[i] as usize].iter().any(|c| !c.is_finite()) {
            f.remove(i);
        } else {
            i += 1;
        }
    }
}

/// `EdgeDict`: directed edges with multiplicities, where an edge and its
/// reverse cancel.
#[derive(Default)]
struct EdgeDict {
    edges: CxxMap<(i32, i32), i32>,
    v2e: CxxMap<i32, Vec<i32>>,
    v2e_reverse: CxxMap<i32, Vec<i32>>,
}

/// The edge multiset operations `EdgeDict` is used through.
trait EdgeCounts {
    fn count(&self, e: (i32, i32)) -> i32;
    fn add(&mut self, e: (i32, i32));
    /// Decrement an edge whose count is positive, erasing it at zero.
    fn remove(&mut self, e: (i32, i32));
}

/// `EdgeDict::add(const IndexedFace&)`
fn add_face(d: &mut impl EdgeCounts, face: &[u32]) {
    let n = face.len();
    for i in 0..n {
        let e = (face[(i + 1) % n] as i32, face[i] as i32);
        if d.count(e) > 0 {
            d.remove(e);
        } else {
            d.add((e.1, e.0));
        }
    }
}

/// `EdgeDict::remove(const IndexedTriangle&)`
fn remove_triangle(d: &mut impl EdgeCounts, t: [i32; 3]) {
    for i in 0..3 {
        let e = (t[i], t[(i + 1) % 3]);
        if d.count(e) > 0 {
            d.remove(e);
        } else {
            d.add((e.1, e.0));
        }
    }
}

/// Edge counts without an iteration order, for the common case where the
/// triangles cover the polygon and nothing is iterated: an open-addressing
/// table (linear probing, deletion by backward shift) with a multiplicative
/// hash, sized for the polygon and reused. `std`'s map hashes with SipHash,
/// which was most of the cost of this bookkeeping.
#[derive(Default)]
struct FastCounts {
    keys: Vec<u64>,
    vals: Vec<i32>,
    mask: usize,
    len: usize,
}

/// No edge: both indices `u32::MAX`, which no vertex has.
const EMPTY: u64 = u64::MAX;

fn key(e: (i32, i32)) -> u64 {
    (u64::from(e.0 as u32) << 32) | u64::from(e.1 as u32)
}

impl FastCounts {
    /// Empty, with room for the edges of an `n`-gon and its triangles.
    fn reset(&mut self, n: usize) {
        let cap = (4 * n).next_power_of_two().max(16);
        self.keys.clear();
        self.keys.resize(cap, EMPTY);
        self.vals.clear();
        self.vals.resize(cap, 0);
        self.mask = cap - 1;
        self.len = 0;
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    fn home(&self, k: u64) -> usize {
        (k.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize & self.mask
    }

    /// The slot holding `k`, or the empty slot where it would go.
    #[inline]
    fn find(&self, k: u64) -> usize {
        let mut i = self.home(k);
        while self.keys[i] != k && self.keys[i] != EMPTY {
            i = (i + 1) & self.mask;
        }
        i
    }

    fn grow(&mut self) {
        let keys = std::mem::take(&mut self.keys);
        let vals = std::mem::take(&mut self.vals);
        let cap = 2 * keys.len();
        self.keys = vec![EMPTY; cap];
        self.vals = vec![0; cap];
        self.mask = cap - 1;
        for (k, v) in keys.into_iter().zip(vals) {
            if k != EMPTY {
                let i = self.find(k);
                self.keys[i] = k;
                self.vals[i] = v;
            }
        }
    }
}

impl EdgeCounts for FastCounts {
    fn count(&self, e: (i32, i32)) -> i32 {
        let i = self.find(key(e));
        if self.keys[i] == EMPTY {
            0
        } else {
            self.vals[i]
        }
    }

    fn add(&mut self, e: (i32, i32)) {
        let k = key(e);
        let i = self.find(k);
        if self.keys[i] == k {
            self.vals[i] += 1;
            return;
        }
        self.keys[i] = k;
        self.vals[i] = 1;
        self.len += 1;
        if 2 * self.len > self.keys.len() {
            self.grow();
        }
    }

    fn remove(&mut self, e: (i32, i32)) {
        let mut i = self.find(key(e));
        if self.keys[i] == EMPTY {
            return;
        }
        self.vals[i] -= 1;
        if self.vals[i] != 0 {
            return;
        }
        // Backward-shift deletion: pull later entries of the probe run
        // into the hole unless their home lies in (hole, entry].
        self.len -= 1;
        self.keys[i] = EMPTY;
        let mut j = i;
        loop {
            j = (j + 1) & self.mask;
            let k = self.keys[j];
            if k == EMPTY {
                return;
            }
            let h = self.home(k);
            let stays = if i <= j {
                i < h && h <= j
            } else {
                i < h || h <= j
            };
            if !stays {
                self.keys[i] = k;
                self.vals[i] = self.vals[j];
                self.keys[j] = EMPTY;
                i = j;
            }
        }
    }
}

impl EdgeCounts for EdgeDict {
    fn count(&self, e: (i32, i32)) -> i32 {
        self.edges.find(&e).map_or(0, |n| *self.edges.val(n))
    }

    fn add(&mut self, e: (i32, i32)) {
        let n = self.edges.entry(e);
        *self.edges.val_mut(n) += 1;
    }

    fn remove(&mut self, e: (i32, i32)) {
        let n = self.edges.entry(e);
        *self.edges.val_mut(n) -= 1;
        if *self.edges.val(n) == 0 {
            self.edges.erase(n);
        }
    }
}

impl EdgeDict {
    fn remove_from_v2e(&mut self, vidx: i32, next: i32, prev: i32) {
        let n = self.v2e.entry(vidx);
        let l = self.v2e.val_mut(n);
        if let Some(p) = l.iter().position(|&x| x == next) {
            l.remove(p);
        }
        if l.is_empty() {
            self.v2e.erase(n);
        }
        let n = self.v2e_reverse.entry(vidx);
        let l = self.v2e_reverse.val_mut(n);
        if let Some(p) = l.iter().position(|&x| x == prev) {
            l.remove(p);
        }
        if l.is_empty() {
            self.v2e_reverse.erase(n);
        }
    }

    /// `extractTriangle`: clip the ear at `vidx` whose outgoing edge goes
    /// to `next`. Returns false where upstream would read an empty list.
    fn extract_triangle(&mut self, vidx: i32, next: i32, out: &mut Vec<[u32; 3]>) -> bool {
        let n = self.v2e_reverse.entry(vidx);
        let Some(&prev) = self.v2e_reverse.val(n).first() else {
            return false;
        };
        let t = [prev, vidx, next];
        out.push(t.map(|x| x as u32));
        remove_triangle(self, t);
        let n = self.v2e.entry(next);
        if !self.v2e.val(n).contains(&prev) {
            let p = self.v2e.entry(prev);
            self.v2e.val_mut(p).push(next);
            let q = self.v2e_reverse.entry(next);
            self.v2e_reverse.val_mut(q).push(prev);
        }
        self.remove_from_v2e(vidx, next, prev);
        self.remove_from_v2e(prev, vidx, next);
        self.remove_from_v2e(next, prev, vidx);
        true
    }

    /// `triangulateLoops`: ear-clip the remaining edge loops, preferring
    /// vertices with a single outgoing edge, in hash-map order.
    fn triangulate_loops(&mut self, out: &mut Vec<[u32; 3]>) {
        self.v2e.reset();
        self.v2e_reverse.reset();
        let mut it = self.edges.first();
        let mut total = 0usize;
        while let Some(n) = it {
            let (a, b) = self.edges.key(n);
            for _ in 0..*self.edges.val(n) {
                let x = self.v2e.entry(a);
                self.v2e.val_mut(x).push(b);
                let y = self.v2e_reverse.entry(b);
                self.v2e_reverse.val_mut(y).push(a);
                total += 1;
            }
            it = self.edges.next(n);
        }
        // Each ear removes at least one edge; the cap stops a loop that
        // upstream would spin in forever on inconsistent input.
        let mut budget = 2 * total + 8;
        while !self.v2e.is_empty() && budget > 0 {
            budget -= 1;
            let mut pick = None;
            let mut it = self.v2e.first();
            while let Some(n) = it {
                if self.v2e.val(n).len() == 1 {
                    pick = Some(n);
                    break;
                }
                it = self.v2e.next(n);
            }
            // Only vertices with several edges left: take the first.
            let Some(n) = pick.or_else(|| self.v2e.first()) else {
                break;
            };
            let vidx = self.v2e.key(n);
            let Some(&next) = self.v2e.val(n).first() else {
                break;
            };
            if !self.extract_triangle(vidx, next, out) {
                break;
            }
        }
    }
}
