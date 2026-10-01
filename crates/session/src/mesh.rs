//! A triangle mesh for analysis (`check`, `measure`): the rendered solid's
//! triangles with the part each came from, plus a bounding volume
//! hierarchy for ray casts and distance queries.
//!
//! Everything here is serial and deterministic: the same solid gives the
//! same numbers at any thread count.

use std::sync::Arc;

use geom::manifold_geom::ManifoldGeometry;

pub type V3 = [f64; 3];

pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn scale(a: V3, s: f64) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
pub fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}
pub fn dist(a: V3, b: V3) -> f64 {
    norm(sub(a, b))
}

/// An axis-aligned box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub lo: V3,
    pub hi: V3,
}

impl Aabb {
    pub const EMPTY: Aabb = Aabb {
        lo: [f64::INFINITY; 3],
        hi: [f64::NEG_INFINITY; 3],
    };

    pub fn point(p: V3) -> Aabb {
        Aabb { lo: p, hi: p }
    }

    pub fn grow(&mut self, p: V3) {
        for (k, &x) in p.iter().enumerate() {
            self.lo[k] = self.lo[k].min(x);
            self.hi[k] = self.hi[k].max(x);
        }
    }

    pub fn union(&self, o: &Aabb) -> Aabb {
        let mut b = *self;
        b.grow(o.lo);
        b.grow(o.hi);
        b
    }

    pub fn is_empty(&self) -> bool {
        self.lo[0] > self.hi[0]
    }

    pub fn center(&self) -> V3 {
        scale(add(self.lo, self.hi), 0.5)
    }

    pub fn size(&self) -> V3 {
        sub(self.hi, self.lo)
    }

    /// Whether the boxes overlap or touch, with `pad` of slack.
    pub fn overlaps(&self, o: &Aabb, pad: f64) -> bool {
        (0..3).all(|k| self.lo[k] <= o.hi[k] + pad && o.lo[k] <= self.hi[k] + pad)
    }

    /// The smallest distance between two boxes (0 when they overlap).
    pub fn gap(&self, o: &Aabb) -> f64 {
        let mut s = 0.0;
        for k in 0..3 {
            let d = (o.lo[k] - self.hi[k]).max(self.lo[k] - o.hi[k]).max(0.0);
            s += d * d;
        }
        s.sqrt()
    }

    /// The distance from `p` to the box (0 inside).
    pub fn point_gap(&self, p: V3) -> f64 {
        let mut s = 0.0;
        for (k, &x) in p.iter().enumerate() {
            let d = (self.lo[k] - x).max(x - self.hi[k]).max(0.0);
            s += d * d;
        }
        s.sqrt()
    }

    /// The entry distance of a ray into the box, if it enters before
    /// `tmax`.
    fn ray_enter(&self, o: V3, inv: V3, tmax: f64) -> Option<f64> {
        let mut t0: f64 = 0.0;
        let mut t1 = tmax;
        for k in 0..3 {
            let (mut a, mut b) = ((self.lo[k] - o[k]) * inv[k], (self.hi[k] - o[k]) * inv[k]);
            if a > b {
                std::mem::swap(&mut a, &mut b);
            }
            // NaN (a zero direction on a face of the box) keeps the bound.
            if a > t0 {
                t0 = a;
            }
            if b < t1 {
                t1 = b;
            }
            if t0 > t1 {
                return None;
            }
        }
        Some(t0)
    }
}

/// The analysis mesh: triangles with outward normals and their parts.
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub verts: Vec<V3>,
    pub tris: Vec<[u32; 3]>,
    /// Index into [`Mesh::part_names`] per triangle, when it came from a
    /// part.
    pub part: Vec<Option<u32>>,
    pub part_names: Vec<Arc<str>>,
}

impl Mesh {
    /// The triangles of a solid, with each face's part.
    pub fn of_solid(m: &ManifoldGeometry) -> Mesh {
        let (ps, ids) = m.to_polyset_with_ids(&geom::color::CORNFIELD);
        let mut mesh = Mesh {
            verts: ps.vertices.clone(),
            ..Mesh::default()
        };
        for (f, id) in ps.faces.iter().zip(ids) {
            if f.len() != 3 {
                continue;
            }
            mesh.tris.push([f[0], f[1], f[2]]);
            let part =
                m.part_of(id).map(
                    |name| match mesh.part_names.iter().position(|n| n == name) {
                        Some(i) => i as u32,
                        None => {
                            mesh.part_names.push(name.clone());
                            (mesh.part_names.len() - 1) as u32
                        }
                    },
                );
            mesh.part.push(part);
        }
        mesh
    }

    pub fn corners(&self, t: usize) -> [V3; 3] {
        let [a, b, c] = self.tris[t];
        [
            self.verts[a as usize],
            self.verts[b as usize],
            self.verts[c as usize],
        ]
    }

    /// The (unnormalised) normal: twice the area, pointing out.
    pub fn cross(&self, t: usize) -> V3 {
        let [a, b, c] = self.corners(t);
        cross(sub(b, a), sub(c, a))
    }

    pub fn area(&self, t: usize) -> f64 {
        norm(self.cross(t)) / 2.0
    }

    /// The unit outward normal (zero for a degenerate triangle).
    pub fn normal(&self, t: usize) -> V3 {
        let n = self.cross(t);
        let l = norm(n);
        if l > 0.0 { scale(n, 1.0 / l) } else { [0.0; 3] }
    }

    pub fn centroid(&self, t: usize) -> V3 {
        let [a, b, c] = self.corners(t);
        scale(add(add(a, b), c), 1.0 / 3.0)
    }

    pub fn bbox(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for t in &self.tris {
            for &v in t {
                b.grow(self.verts[v as usize]);
            }
        }
        b
    }

    pub fn tri_box(&self, t: usize) -> Aabb {
        let [a, b, c] = self.corners(t);
        let mut x = Aabb::point(a);
        x.grow(b);
        x.grow(c);
        x
    }

    /// Edges not shared by exactly two faces once corners are merged by
    /// position ([`bad_edges`]).
    pub fn bad_edges(&self) -> Option<BadEdges> {
        bad_edges(&self.verts, self.tris.iter().copied())
    }

    /// Both welds of [`weld`]: by exact and by `f32` position.
    pub fn weld(&self) -> Weld {
        weld(&self.verts, self.tris.iter().copied())
    }

    pub fn part_name(&self, t: usize) -> Option<&Arc<str>> {
        self.part[t].map(|i| &self.part_names[i as usize])
    }

    /// Volume, surface area and centre of mass (the centroid of the
    /// enclosed volume, by the divergence theorem over signed tetrahedra
    /// from the origin).
    /// Triangle `t`'s share of a closed shell's signed volume (the
    /// tetrahedron it spans with the origin): positive summed over a
    /// shell wound outward, negative over one wound inward (a void).
    pub fn signed_volume(&self, t: usize) -> f64 {
        let [a, b, d] = self.corners(t);
        dot(a, cross(b, d)) / 6.0
    }

    pub fn mass(&self) -> (f64, f64, V3) {
        let (mut vol, mut area) = (0.0, 0.0);
        let mut c = [0.0; 3];
        for t in 0..self.tris.len() {
            let [a, b, d] = self.corners(t);
            let v = dot(a, cross(b, d)) / 6.0;
            vol += v;
            c = add(c, scale(add(add(a, b), d), v / 4.0));
            area += self.area(t);
        }
        let centroid = if vol.abs() > 0.0 {
            scale(c, 1.0 / vol)
        } else {
            self.bbox().center()
        };
        (vol, area, centroid)
    }

    /// Connected pieces: triangles sharing a vertex are one piece. Returns
    /// each triangle's piece and the number of pieces, numbered in order of
    /// their first triangle.
    pub fn components(&self) -> (Vec<u32>, usize) {
        let mut parent: Vec<u32> = (0..self.verts.len() as u32).collect();
        fn find(p: &mut [u32], mut x: u32) -> u32 {
            while p[x as usize] != x {
                p[x as usize] = p[p[x as usize] as usize];
                x = p[x as usize];
            }
            x
        }
        for t in &self.tris {
            let r = find(&mut parent, t[0]);
            for &v in &t[1..] {
                let s = find(&mut parent, v);
                if s != r {
                    parent[s as usize] = r;
                }
            }
        }
        let mut label = vec![u32::MAX; self.verts.len()];
        let mut n = 0;
        let mut out = Vec::with_capacity(self.tris.len());
        for t in &self.tris {
            let r = find(&mut parent, t[0]) as usize;
            if label[r] == u32::MAX {
                label[r] = n;
                n += 1;
            }
            out.push(label[r]);
        }
        (out, n as usize)
    }
}

/// Edges of a mesh that are not shared by exactly two faces once vertices
/// are merged by position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BadEdges {
    /// How many such edges.
    pub edges: usize,
    /// The midpoint of the first one (in a fixed order).
    pub at: V3,
    /// The box around all of them.
    pub bbox: Aabb,
}

/// What rounding the corners to `f32`, as a slicer reads an STL, does to
/// a mesh beyond what merging exact positions does ([`weld`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StlPrecision {
    /// Triangles two or more of whose corners become one point.
    pub collapsed_faces: usize,
    /// Edges shared by other than two faces at `f32`, beyond those of the
    /// exact weld.
    pub nonmanifold_edges: usize,
    /// The midpoint of the first such edge, or the centroid of the first
    /// collapsed face when there is none (model coordinates).
    pub at: V3,
    /// The box around the bad edges, or around the collapsed faces when
    /// there are no bad edges.
    pub bbox: Aabb,
}

/// The two ways a file reader can merge a mesh's corners ([`weld`]).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Weld {
    /// By exact position, as an ASCII STL keeps it ([`bad_edges`]).
    pub exact: Option<BadEdges>,
    /// By `f32`-rounded position, as a binary STL stores it and as slicers
    /// parse either kind: only what that adds to `exact`.
    pub f32: Option<StlPrecision>,
}

/// The edges an STL reader finds used by other than two faces.
///
/// Manifold's own status cannot see these: where two pieces touch along
/// an edge (a rib ending exactly on the rim of a cylinder, two cubes
/// sharing an edge), its result keeps a separate vertex for each piece at
/// the same position, so every edge it knows of has two faces. An STL has
/// no vertex identities; a reader merges corners by position, and then
/// that edge has four faces and the file is not manifold. Without this
/// weld, a part like that passes `check` as manifold while its exported
/// STL fails any watertightness test.
///
/// Vertices merge by exact position, which is what an ASCII STL keeps
/// (each coordinate is written in its shortest exact form). A triangle
/// that collapses when its corners merge is skipped, as a reader dropping
/// degenerate facets would. [`weld`] also merges by `f32` position.
///
/// The mesh must be a valid Manifold solid's (every edge paired by vertex
/// index): then only merged positions can unpair edges, and a mesh with
/// no two vertices at one position is answered after the first sort.
pub fn bad_edges(verts: &[V3], tris: impl Iterator<Item = [u32; 3]> + Clone) -> Option<BadEdges> {
    weld(verts, tris).exact
}

/// Merge a valid solid's corners by exact position ([`bad_edges`]) and by
/// `f32`-rounded position.
///
/// The second is what a slicer sees. Binary STL stores `f32`, and slicers
/// parse ASCII STL into `f32` too, so two vertices that are distinct in
/// `f64` but round to one `f32` become one point: the triangles between
/// them collapse, and where a collapse is not a clean edge contraction the
/// edges around it end up with one face or three. A finely tessellated
/// twisted thread in the CAD pilot (205,294 triangles) checked manifold
/// by exact position and had 738 non-manifold edges in its STL.
///
/// One sort does both: positions equal in `f64` are equal in `f32`, so
/// the vertices are sorted by their `f32` key and each group of one `f32`
/// position (almost always a single vertex) is split by exact position.
/// With no group of two, which is almost every model, neither weld can
/// change an edge and the answer is known after that one sort of 16-byte
/// keys (the exact-only weld it replaced sorted 32-byte keys). Serial and
/// deterministic.
pub fn weld(verts: &[V3], tris: impl Iterator<Item = [u32; 3]> + Clone) -> Weld {
    // -0 and 0 are one position in either precision.
    let mut keys: Vec<([u32; 3], u32)> = verts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            (
                p.map(|c| {
                    let f = c as f32;
                    if f == 0.0 { 0u32 } else { f.to_bits() }
                }),
                i as u32,
            )
        })
        .collect();
    keys.sort_unstable();
    if !keys.windows(2).any(|w| w[0].0 == w[1].0) {
        return Weld::default();
    }
    // Canonical vertex per position (the lowest index there, as the sort
    // puts it first in its group), for each precision, and which vertices
    // share a position with another.
    let n = verts.len();
    let mut canon32: Vec<u32> = (0..n as u32).collect();
    let mut canon64 = canon32.clone();
    let mut shared32 = vec![false; n];
    let mut shared64 = vec![false; n];
    let (mut merged64, mut only32) = (false, false);
    let exact = |i: u32| verts[i as usize].map(|c| if c == 0.0 { 0u64 } else { c.to_bits() });
    let mut group: Vec<([u64; 3], u32)> = Vec::new();
    let mut i = 0;
    while i < keys.len() {
        let mut j = i + 1;
        while j < keys.len() && keys[j].0 == keys[i].0 {
            j += 1;
        }
        if j - i > 1 {
            let first = keys[i].1;
            for k in &keys[i..j] {
                canon32[k.1 as usize] = first;
                shared32[k.1 as usize] = true;
            }
            group.clear();
            group.extend(keys[i..j].iter().map(|k| (exact(k.1), k.1)));
            group.sort_unstable();
            for w in group.windows(2) {
                if w[0].0 == w[1].0 {
                    canon64[w[1].1 as usize] = canon64[w[0].1 as usize];
                    shared64[w[0].1 as usize] = true;
                    shared64[w[1].1 as usize] = true;
                    merged64 = true;
                } else {
                    only32 = true;
                }
            }
        }
        i = j;
    }
    let ex = merged64.then(|| count_welded(verts, tris.clone(), &canon64, &shared64));
    let mut out = Weld {
        exact: ex.as_ref().and_then(|w| w.bad),
        f32: None,
    };
    if only32 {
        let w = count_welded(verts, tris, &canon32, &shared32);
        let (base_edges, base_collapsed) = ex
            .as_ref()
            .map_or((0, 0), |e| (e.bad.map_or(0, |b| b.edges), e.collapsed));
        let edges = w.bad.map_or(0, |b| b.edges).saturating_sub(base_edges);
        let collapsed = w.collapsed.saturating_sub(base_collapsed);
        if edges > 0 {
            let b = w.bad.expect("edges counted");
            out.f32 = Some(StlPrecision {
                collapsed_faces: collapsed,
                nonmanifold_edges: edges,
                at: b.at,
                bbox: b.bbox,
            });
        } else if collapsed > 0 {
            out.f32 = Some(StlPrecision {
                collapsed_faces: collapsed,
                nonmanifold_edges: 0,
                at: w.collapsed_at,
                bbox: w.collapsed_box,
            });
        }
    }
    out
}

/// What one weld of [`weld`] does to the triangles.
struct Welded {
    bad: Option<BadEdges>,
    collapsed: usize,
    collapsed_at: V3,
    collapsed_box: Aabb,
}

fn count_welded(
    verts: &[V3],
    tris: impl Iterator<Item = [u32; 3]>,
    canon: &[u32],
    shared: &[bool],
) -> Welded {
    // An edge between two vertices that merged with nothing has the
    // solid's own faces, which a valid Manifold result pairs: only edges
    // at a shared position are counted, so even a pinched
    // million-triangle mesh sorts a handful.
    let mut edges: Vec<u64> = Vec::new();
    let mut collapsed = 0usize;
    let mut collapsed_at = None;
    let mut collapsed_box = Aabb::EMPTY;
    for t in tris {
        if !t.iter().any(|&v| shared[v as usize]) {
            continue;
        }
        let [a, b, c] = t.map(|v| canon[v as usize]);
        if a == b || b == c || c == a {
            // Skipped, as a reader dropping degenerate facets would.
            collapsed += 1;
            let p = t.map(|v| verts[v as usize]);
            collapsed_at.get_or_insert(scale(add(add(p[0], p[1]), p[2]), 1.0 / 3.0));
            for q in p {
                collapsed_box.grow(q);
            }
            continue;
        }
        for (k, (u, v)) in [(a, b), (b, c), (c, a)].into_iter().enumerate() {
            if shared[t[k] as usize] || shared[t[(k + 1) % 3] as usize] {
                edges.push(u64::from(u.min(v)) << 32 | u64::from(u.max(v)));
            }
        }
    }
    edges.sort_unstable();
    let mut count = 0usize;
    let mut first: Option<u64> = None;
    let mut bbox = Aabb::EMPTY;
    let mut i = 0;
    while i < edges.len() {
        let mut j = i + 1;
        while j < edges.len() && edges[j] == edges[i] {
            j += 1;
        }
        if j - i != 2 {
            count += 1;
            first.get_or_insert(edges[i]);
            let (u, v) = ((edges[i] >> 32) as usize, (edges[i] & 0xffff_ffff) as usize);
            bbox.grow(verts[u]);
            bbox.grow(verts[v]);
        }
        i = j;
    }
    let bad = first.map(|e| {
        let (u, v) = ((e >> 32) as usize, (e & 0xffff_ffff) as usize);
        BadEdges {
            edges: count,
            at: scale(add(verts[u], verts[v]), 0.5),
            bbox,
        }
    });
    Welded {
        bad,
        collapsed,
        collapsed_at: collapsed_at.unwrap_or([0.0; 3]),
        collapsed_box,
    }
}

/// How far apart `f32` values are at the largest coordinate of a box: the
/// finest detail an STL of it can hold there.
pub fn f32_spacing(b: &Aabb) -> f64 {
    let m = b.lo.iter().chain(&b.hi).fold(0f64, |m, c| m.max(c.abs()));
    let x = (m as f32).max(f32::MIN_POSITIVE);
    if !x.is_finite() {
        return f64::INFINITY;
    }
    f64::from(f32::from_bits(x.to_bits() + 1)) - f64::from(x)
}

/// A bounding volume hierarchy over a mesh's triangles.
#[derive(Debug, Clone)]
pub struct Bvh {
    nodes: Vec<BvhNode>,
    /// Triangle indices, leaves' ranges into this.
    order: Vec<u32>,
}

#[derive(Debug, Clone)]
struct BvhNode {
    b: Aabb,
    /// Leaf: `count > 0`, triangles `order[first..first + count]`.
    /// Inner: children at `first` and `first + 1`.
    first: u32,
    count: u32,
}

const LEAF: usize = 4;

impl Bvh {
    pub fn new(mesh: &Mesh) -> Bvh {
        let n = mesh.tris.len();
        let boxes: Vec<Aabb> = (0..n).map(|t| mesh.tri_box(t)).collect();
        let centers: Vec<V3> = boxes.iter().map(Aabb::center).collect();
        let mut bvh = Bvh {
            nodes: Vec::with_capacity(2 * n / LEAF + 1),
            order: (0..n as u32).collect(),
        };
        bvh.nodes.push(BvhNode {
            b: Aabb::EMPTY,
            first: 0,
            count: 0,
        });
        // (node, start, end) still to split.
        let mut stack = vec![(0usize, 0usize, n)];
        while let Some((node, start, end)) = stack.pop() {
            let mut b = Aabb::EMPTY;
            let mut cb = Aabb::EMPTY;
            for &t in &bvh.order[start..end] {
                b = b.union(&boxes[t as usize]);
                cb.grow(centers[t as usize]);
            }
            bvh.nodes[node].b = b;
            if end - start <= LEAF {
                bvh.nodes[node].first = start as u32;
                bvh.nodes[node].count = (end - start) as u32;
                continue;
            }
            // Split at the median along the widest axis of the centres;
            // ties are broken by index, so the tree is deterministic.
            let s = cb.size();
            let axis = if s[0] >= s[1] && s[0] >= s[2] {
                0
            } else if s[1] >= s[2] {
                1
            } else {
                2
            };
            let mid = (start + end) / 2;
            bvh.order[start..end].select_nth_unstable_by(mid - start, |&a, &c| {
                centers[a as usize][axis]
                    .total_cmp(&centers[c as usize][axis])
                    .then(a.cmp(&c))
            });
            let left = bvh.nodes.len();
            bvh.nodes.push(BvhNode {
                b: Aabb::EMPTY,
                first: 0,
                count: 0,
            });
            bvh.nodes.push(BvhNode {
                b: Aabb::EMPTY,
                first: 0,
                count: 0,
            });
            bvh.nodes[node].first = left as u32;
            stack.push((left + 1, mid, end));
            stack.push((left, start, mid));
        }
        bvh
    }

    pub fn bbox(&self) -> Aabb {
        self.nodes.first().map_or(Aabb::EMPTY, |n| n.b)
    }

    /// The nearest triangle a ray from `o` along unit `d` hits beyond
    /// `tmin` (and before `tmax`), ignoring triangles `skip` says to: its
    /// distance and index.
    pub fn ray(
        &self,
        mesh: &Mesh,
        o: V3,
        d: V3,
        tmin: f64,
        tmax: f64,
        skip: impl Fn(u32) -> bool,
    ) -> Option<(f64, u32)> {
        if self.order.is_empty() {
            return None;
        }
        let inv = [1.0 / d[0], 1.0 / d[1], 1.0 / d[2]];
        let mut best: Option<(f64, u32)> = None;
        let mut limit = tmax;
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            let node = &self.nodes[i];
            if node.b.ray_enter(o, inv, limit).is_none() {
                continue;
            }
            if node.count > 0 {
                for &t in &self.order[node.first as usize..(node.first + node.count) as usize] {
                    if skip(t) {
                        continue;
                    }
                    if let Some(h) = ray_triangle(o, d, mesh.corners(t as usize))
                        && h > tmin
                        && h < limit
                    {
                        limit = h;
                        best = Some((h, t));
                    }
                }
            } else {
                let (a, b) = (node.first as usize, node.first as usize + 1);
                let ta = self.nodes[a].b.ray_enter(o, inv, limit);
                let tb = self.nodes[b].b.ray_enter(o, inv, limit);
                // Nearer child last, so it is searched first.
                match (ta, tb) {
                    (Some(x), Some(y)) if x <= y => {
                        stack.push(b);
                        stack.push(a);
                    }
                    (Some(_), Some(_)) => {
                        stack.push(a);
                        stack.push(b);
                    }
                    (Some(_), None) => stack.push(a),
                    (None, Some(_)) => stack.push(b),
                    (None, None) => {}
                }
            }
        }
        best
    }

    /// The smallest distance between the surfaces of two meshes, with the
    /// closest points (on `a`, then on `b`). Branch and bound over both
    /// hierarchies with exact triangle-triangle distances.
    pub fn closest(&self, a: &Mesh, other: &Bvh, b: &Mesh) -> Option<(f64, V3, V3)> {
        if self.order.is_empty() || other.order.is_empty() {
            return None;
        }
        let mut best = (f64::INFINITY, [0.0; 3], [0.0; 3]);
        let mut stack = vec![(0usize, 0usize)];
        while let Some((i, j)) = stack.pop() {
            let (ni, nj) = (&self.nodes[i], &other.nodes[j]);
            if ni.b.gap(&nj.b) >= best.0 {
                continue;
            }
            match (ni.count > 0, nj.count > 0) {
                (true, true) => {
                    for &s in &self.order[ni.first as usize..(ni.first + ni.count) as usize] {
                        for &t in &other.order[nj.first as usize..(nj.first + nj.count) as usize] {
                            let (d, p, q) =
                                tri_tri_distance(a.corners(s as usize), b.corners(t as usize));
                            if d < best.0 {
                                best = (d, p, q);
                            }
                        }
                    }
                }
                // Descend into the bigger box (or the only inner node).
                (false, true) => {
                    stack.push((ni.first as usize, j));
                    stack.push((ni.first as usize + 1, j));
                }
                (true, false) => {
                    stack.push((i, nj.first as usize));
                    stack.push((i, nj.first as usize + 1));
                }
                (false, false) => {
                    let (si, sj) = (norm(ni.b.size()), norm(nj.b.size()));
                    if si >= sj {
                        stack.push((ni.first as usize, j));
                        stack.push((ni.first as usize + 1, j));
                    } else {
                        stack.push((i, nj.first as usize));
                        stack.push((i, nj.first as usize + 1));
                    }
                }
            }
        }
        best.0.is_finite().then_some(best)
    }
}

/// Möller-Trumbore: the distance along unit `d` from `o` to the triangle,
/// either side.
pub fn ray_triangle(o: V3, d: V3, [a, b, c]: [V3; 3]) -> Option<f64> {
    let e1 = sub(b, a);
    let e2 = sub(c, a);
    let p = cross(d, e2);
    let det = dot(e1, p);
    if det.abs() < 1e-300 {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub(o, a);
    let u = dot(s, p) * inv;
    // A little slack so a ray through a shared edge hits one of the two
    // triangles rather than slipping between them.
    const E: f64 = 1e-9;
    if !(-E..=1.0 + E).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(d, q) * inv;
    if v < -E || u + v > 1.0 + E {
        return None;
    }
    Some(dot(e2, q) * inv)
}

/// The closest point to `p` on triangle `abc` (Ericson, "Real-Time
/// Collision Detection", 5.1.5).
pub fn closest_on_triangle(p: V3, [a, b, c]: [V3; 3]) -> V3 {
    let ab = sub(b, a);
    let ac = sub(c, a);
    let ap = sub(p, a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = sub(p, b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return add(a, scale(ab, d1 / (d1 - d3)));
    }
    let cp = sub(p, c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return add(a, scale(ac, d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return add(b, scale(sub(c, b), (d4 - d3) / ((d4 - d3) + (d5 - d6))));
    }
    let denom = 1.0 / (va + vb + vc);
    add(a, add(scale(ab, vb * denom), scale(ac, vc * denom)))
}

/// The closest points of segments `p1q1` and `p2q2` (Ericson 5.1.9).
pub fn closest_segments(p1: V3, q1: V3, p2: V3, q2: V3) -> (V3, V3) {
    let d1 = sub(q1, p1);
    let d2 = sub(q2, p2);
    let r = sub(p1, p2);
    let a = dot(d1, d1);
    let e = dot(d2, d2);
    let f = dot(d2, r);
    let (s, t);
    if a <= 1e-300 && e <= 1e-300 {
        return (p1, p2);
    }
    if a <= 1e-300 {
        s = 0.0;
        t = (f / e).clamp(0.0, 1.0);
    } else {
        let c = dot(d1, r);
        if e <= 1e-300 {
            t = 0.0;
            s = (-c / a).clamp(0.0, 1.0);
        } else {
            let b = dot(d1, d2);
            let denom = a * e - b * b;
            let mut s0 = if denom > 0.0 {
                ((b * f - c * e) / denom).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut t0 = (b * s0 + f) / e;
            if t0 < 0.0 {
                t0 = 0.0;
                s0 = (-c / a).clamp(0.0, 1.0);
            } else if t0 > 1.0 {
                t0 = 1.0;
                s0 = ((b - c) / a).clamp(0.0, 1.0);
            }
            s = s0;
            t = t0;
        }
    }
    (add(p1, scale(d1, s)), add(p2, scale(d2, t)))
}

/// The distance between two triangles and the closest points. Checks the
/// six vertex-face and nine edge-edge pairs, which covers every case
/// except crossing triangles; those are reported by the caller's overlap
/// test (the booleans), not here.
pub fn tri_tri_distance(s: [V3; 3], t: [V3; 3]) -> (f64, V3, V3) {
    let mut best = (f64::INFINITY, s[0], t[0]);
    let mut try_pair = |p: V3, q: V3| {
        let d = dist(p, q);
        if d < best.0 {
            best = (d, p, q);
        }
    };
    for &p in &s {
        try_pair(p, closest_on_triangle(p, t));
    }
    for &q in &t {
        try_pair(closest_on_triangle(q, s), q);
    }
    for i in 0..3 {
        for j in 0..3 {
            let (p, q) = closest_segments(s[i], s[(i + 1) % 3], t[j], t[(j + 1) % 3]);
            try_pair(p, q);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube(o: V3, s: f64) -> Mesh {
        let ps = geom::primitives::cube([s; 3], false);
        let mut m = Mesh {
            verts: ps.vertices.iter().map(|v| add(*v, o)).collect(),
            ..Mesh::default()
        };
        let t = ps.tessellate(&mut Vec::new());
        for f in &t.faces {
            m.tris.push([f[0], f[1], f[2]]);
            m.part.push(None);
        }
        m
    }

    #[test]
    fn a_cube_has_its_mass() {
        let m = cube([1.0, 2.0, 3.0], 2.0);
        let (v, a, c) = m.mass();
        assert!((v - 8.0).abs() < 1e-9, "{v}");
        assert!((a - 24.0).abs() < 1e-9);
        assert!(dist(c, [2.0, 3.0, 4.0]) < 1e-9);
    }

    /// Two meshes as one, the second's vertices after the first's.
    fn join(mut a: Mesh, b: &Mesh) -> Mesh {
        let n = a.verts.len() as u32;
        a.verts.extend(&b.verts);
        a.tris.extend(b.tris.iter().map(|t| t.map(|v| v + n)));
        a.part.extend(&b.part);
        a
    }

    #[test]
    fn pinched_edges_are_found_by_position() {
        // Apart: nothing merges.
        assert_eq!(
            join(cube([0.0; 3], 1.0), &cube([2.0, 0.0, 0.0], 1.0)).bad_edges(),
            None
        );
        // Sharing an edge along z at x = y = 1: four faces there.
        let m = join(cube([0.0; 3], 1.0), &cube([1.0, 1.0, 0.0], 1.0));
        let b = m.bad_edges().unwrap();
        assert_eq!(b.edges, 1);
        assert_eq!(b.at, [1.0, 1.0, 0.5]);
        // A vertex split in two at one position (as a property seam
        // leaves it) is merged back and pairs.
        let mut c = cube([0.0; 3], 1.0);
        let v = c.tris[0][0];
        c.verts.push(c.verts[v as usize]);
        let dup = (c.verts.len() - 1) as u32;
        c.tris[0][0] = dup;
        // The other faces at that corner keep the original index, so by
        // index the split breaks edges; by position it is whole.
        assert_eq!(c.bad_edges(), None);
    }

    #[test]
    fn f32_rounding_merges_what_exact_positions_keep_apart() {
        // Two 100 mm cubes 1e-7 apart: distinct in f64, one face at f32
        // (whose spacing at 200 is about 1.5e-5), so that face's edges
        // have four faces in an STL.
        let m = join(
            cube([0.0; 3], 100.0),
            &cube([100.0 + 1e-7, 0.0, 0.0], 100.0),
        );
        let w = m.weld();
        assert_eq!(w.exact, None);
        let p = w.f32.unwrap();
        assert_eq!(p.collapsed_faces, 0);
        // The square's four sides and its diagonal (each cube splits it
        // along the same diagonal, so the two diagonals merge).
        assert!(p.nonmanifold_edges >= 4, "{p:?}");
        assert!((p.at[0] - 100.0).abs() < 1e-6, "{p:?}");
        // A millimetre apart, nothing merges at either precision.
        let m = join(cube([0.0; 3], 100.0), &cube([101.0, 0.0, 0.0], 100.0));
        assert_eq!(m.weld(), Weld::default());
        // A cube whose corner is split in two 1e-9 apart: exact keeps the
        // halves apart, f32 merges them back, and nothing breaks.
        let mut c = cube([0.0; 3], 100.0);
        let v = c.tris[0][0];
        let mut q = c.verts[v as usize];
        q[0] += 1e-9;
        c.verts.push(q);
        c.tris[0][0] = (c.verts.len() - 1) as u32;
        assert_eq!(c.weld().f32, None);
    }

    #[test]
    fn f32_spacing_at_a_parts_size() {
        let b = Aabb {
            lo: [-17.0, -15.0, 0.0],
            hi: [17.0, 15.0, 48.0],
        };
        // 48 is in [32, 64): 2^5 * 2^-23.
        assert_eq!(f32_spacing(&b), 2f64.powi(-18));
    }

    #[test]
    fn rays_find_the_far_side() {
        let m = cube([0.0; 3], 2.0);
        let b = Bvh::new(&m);
        let (t, _) = b
            .ray(&m, [1.0, 1.0, 1.9], [0.0, 0.0, -1.0], 0.0, 100.0, |_| false)
            .unwrap();
        assert!((t - 1.9).abs() < 1e-9);
    }

    #[test]
    fn distance_between_cubes() {
        let a = cube([0.0; 3], 1.0);
        let b = cube([3.0, 0.5, 0.0], 1.0);
        let (d, _, _) = Bvh::new(&a).closest(&a, &Bvh::new(&b), &b).unwrap();
        assert!((d - 2.0).abs() < 1e-9, "{d}");
        // Edge to edge: a cube offset diagonally.
        let c = cube([2.0, 2.0, 0.5], 1.0);
        let (d, _, _) = Bvh::new(&a).closest(&a, &Bvh::new(&c), &c).unwrap();
        assert!((d - 2f64.sqrt()).abs() < 1e-9, "{d}");
    }
}
