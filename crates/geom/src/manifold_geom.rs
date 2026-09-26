//! 3D solids on the Manifold kernel, with OpenSCAD's colour bookkeeping
//! (`src/geometry/manifold/ManifoldGeometry.cc`, `manifoldutils.cc`).
//!
//! Manifold tags every triangle with the "original ID" of the mesh it came
//! from, through any number of booleans. OpenSCAD uses that to colour the
//! result: each input mesh gets one ID per colour, IDs that came from a
//! `color()` map to that colour, IDs on the right of a `difference()` are
//! "subtracted" (drawn and exported in the scheme's back colour, the green
//! cut faces of a render), and everything else gets the front colour.

use std::collections::{BTreeMap, BTreeSet};

use manifold_rust::linalg::{Mat3x4, Vec3};
use manifold_rust::manifold::Manifold;
use manifold_rust::types::{BooleanEngine, Error, MeshGL64, OpType};

use crate::Matrix;
use crate::color::{Color, Scheme};
use crate::polyset::{PolySet, Warnings};

/// Where original IDs come from. OpenSCAD calls `Manifold::ReserveIDs`
/// for every conversion as it goes. Here each conversion draws from a block
/// the evaluator reserved in tree order before anything ran in parallel
/// (one block per child slot of each subtree), so a parallel evaluation
/// assigns the same IDs as a serial one, and Manifold, which orders output
/// triangles by original ID, produces the same file every run.
pub trait IdSource {
    /// `count` fresh IDs, consecutive.
    fn reserve(&self, count: u32) -> u32;
}

/// IDs straight from Manifold's global counter.
#[derive(Debug, Default, Clone, Copy)]
pub struct GlobalIds;

impl IdSource for GlobalIds {
    fn reserve(&self, count: u32) -> u32 {
        Manifold::reserve_ids(count)
    }
}

/// A solid with the colour state OpenSCAD's `ManifoldGeometry` carries.
#[derive(Clone)]
pub struct ManifoldGeometry {
    pub manifold: Manifold,
    original_ids: BTreeSet<u32>,
    id_to_color: BTreeMap<u32, Color>,
    subtracted: BTreeSet<u32>,
    /// The single ID the whole solid carries after `set_color` (C++:
    /// `OriginalID() != -1` after `AsOriginal()`).
    own_id: Option<u32>,
}

/// Manifold's status names as OpenSCAD prints them
/// (`ManifoldUtils::statusToString`, `manifoldutils.cc:109-125`).
pub fn status_name(e: Error) -> &'static str {
    match e {
        Error::NoError => "NoError",
        Error::NonFiniteVertex => "NonFiniteVertex",
        Error::NotManifold => "NotManifold",
        Error::VertexOutOfBounds => "VertexOutOfBounds",
        Error::PropertiesWrongLength => "PropertiesWrongLength",
        Error::MissingPositionProperties => "MissingPositionProperties",
        Error::MergeVectorsDifferentLengths => "MergeVectorsDifferentLengths",
        Error::MergeIndexOutOfBounds => "MergeIndexOutOfBounds",
        Error::TransformWrongLength => "TransformWrongLength",
        Error::RunIndexWrongLength => "RunIndexWrongLength",
        Error::FaceIdWrongLength => "FaceIDWrongLength",
        _ => "unknown",
    }
}

impl std::fmt::Debug for ManifoldGeometry {
    // `Manifold` has no `Debug`; its size says enough in a dump.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManifoldGeometry")
            .field("vertices", &self.manifold.num_vert())
            .field("triangles", &self.manifold.num_tri())
            .field("original_ids", &self.original_ids)
            .field("id_to_color", &self.id_to_color)
            .field("subtracted", &self.subtracted)
            .finish()
    }
}

impl Default for ManifoldGeometry {
    fn default() -> Self {
        ManifoldGeometry::new(Manifold::empty())
    }
}

impl ManifoldGeometry {
    fn new(manifold: Manifold) -> Self {
        ManifoldGeometry { manifold, original_ids: BTreeSet::new(), id_to_color: BTreeMap::new(), subtracted: BTreeSet::new(), own_id: None }
    }

    pub fn is_empty(&self) -> bool {
        self.manifold.is_empty()
    }

    /// `createManifoldFromPolySet` (`manifoldutils.cc:127-195`): triangulate,
    /// give each colour its own original ID, and build the solid; if that
    /// fails, merge coincident vertices and try again.
    ///
    /// OpenSCAD then repairs the mesh with CGAL (a convex hull for convex
    /// input, `repairPolySet` otherwise). Without CGAL, the repair here is
    /// Manifold's robust import, which keeps a closed, orientable but
    /// non-manifold mesh as a triangle soup that the robust boolean engine
    /// accepts. A mesh that is not even closed becomes empty, with the
    /// error OpenSCAD gives when its repair finds the same.
    pub fn from_polyset(ps: &PolySet, ids: &dyn IdSource, warnings: &mut Warnings, errors: &mut Warnings) -> ManifoldGeometry {
        let tri;
        let ps = if ps.triangular && ps.faces.iter().all(|f| f.len() == 3) {
            ps
        } else {
            let mut t = ps.tessellate(warnings);
            // A "triangular" polyhedron may still hold faces with fewer
            // than three indices, which the tessellator drops.
            t.faces.retain(|f| f.len() == 3);
            tri = t;
            &tri
        };
        let mut mesh = MeshGL64 { num_prop: 3, ..Default::default() };
        mesh.vert_properties = ps.vertices.iter().flatten().copied().collect();
        // `std::map<std::optional<Color4f>, ...>`: uncoloured faces first,
        // then colours in order.
        type Group = (Option<Color>, Vec<usize>);
        let mut groups: BTreeMap<Option<[u32; 4]>, Group> = BTreeMap::new();
        for i in 0..ps.faces.len() {
            let ci = ps.color_indices.get(i).copied().unwrap_or(-1);
            let color = (ci >= 0).then(|| ps.colors[ci as usize]);
            groups.entry(color.map(|c| c.key())).or_insert((color, Vec::new())).1.push(i);
        }
        let first = ids.reserve(groups.len() as u32);
        let mut original_ids = BTreeSet::new();
        let mut id_to_color = BTreeMap::new();
        for (id, (color, faces)) in (first..).zip(groups.values()) {
            if let Some(c) = color {
                id_to_color.insert(id, *c);
            }
            mesh.run_index.push(mesh.tri_verts.len() as u64);
            mesh.run_original_id.push(id);
            original_ids.insert(id);
            for &f in faces {
                mesh.tri_verts.extend(ps.faces[f].iter().map(|&v| u64::from(v)));
            }
        }
        mesh.run_index.push(mesh.tri_verts.len() as u64);

        let mut m = Manifold::from_mesh_gl64(&mesh);
        if m.status() != Error::NoError && merge_coincident(&mut mesh) {
            m = Manifold::from_mesh_gl64(&mesh);
        }
        if m.status() != Error::NoError {
            warnings.push(format!(
                "PolySet -> Manifold conversion failed: {}\nTrying to repair and reconstruct mesh..",
                status_name(m.status())
            ));
            // OpenSCAD's repair rebuilds the mesh without its colours
            // ("TODO: preserve color", `manifoldutils.cc:188`) as one new
            // original.
            let id = ids.reserve(1);
            let repaired = orient_soup(&mesh, id).map(|r| Manifold::from_mesh_gl64(&r)).filter(|r| r.status() == Error::NoError);
            m = match repaired {
                Some(r) => r,
                None => {
                    let r = Manifold::from_mesh_gl64_robust(&mesh);
                    if r.status() != Error::NoError {
                        errors.push("[manifold] Input mesh is not closed!".into());
                        return ManifoldGeometry::default();
                    }
                    r
                }
            };
            return ManifoldGeometry { manifold: m, original_ids: BTreeSet::from([id]), id_to_color: BTreeMap::new(), subtracted: BTreeSet::new(), own_id: None };
        }
        ManifoldGeometry { manifold: m, original_ids, id_to_color, subtracted: BTreeSet::new(), own_id: None }
    }

    /// `ManifoldGeometry::binOp` (`ManifoldGeometry.cc:263-289`).
    pub fn boolean(&self, rhs: &ManifoldGeometry, op: OpType) -> ManifoldGeometry {
        // The exact engine needs manifold operands; a soup from the repair
        // path goes through the robust engine instead.
        let engine = if self.manifold.as_impl().is_soup || rhs.manifold.as_impl().is_soup { BooleanEngine::Robust } else { BooleanEngine::Exact };
        let manifold = self.manifold.boolean_with_engine(&rhs.manifold, op, engine);
        self.combine_ids(rhs, op, manifold)
    }

    /// Fold `op` over `parts` left to right, as `applyOperator3DManifold`
    /// does with `geom = geom op child`.
    ///
    /// OpenSCAD's Manifold operators are lazy: each `+`, `-` or `*` only
    /// adds a node to Manifold's CSG tree, and the tree is evaluated when
    /// the result is first used, which flattens a chain of the same
    /// operation into one batch (Manifold `csg_tree.cpp`: unions of
    /// disjoint parts are composed without any boolean, the rest are
    /// merged smallest first, and a difference subtracts the union of its
    /// subtrahends). manifold-rust's booleans are eager, so a pairwise fold
    /// is quadratic in the number of children; building the same tree with
    /// its port of `csg_tree` keeps OpenSCAD's cost and its evaluation
    /// order. Soup operands (from the repair path) need the robust engine,
    /// which the tree does not use, so they fold pairwise.
    pub fn batch(op: OpType, parts: Vec<ManifoldGeometry>) -> Option<ManifoldGeometry> {
        use manifold_rust::csg_tree::CsgNode;
        let mut it = parts.into_iter();
        let first = it.next()?;
        let rest: Vec<ManifoldGeometry> = it.collect();
        if rest.is_empty() {
            return Some(first);
        }
        let soup = first.manifold.as_impl().is_soup || rest.iter().any(|p| p.manifold.as_impl().is_soup);
        if soup {
            return Some(rest.iter().fold(first, |acc, p| acc.boolean(p, op)));
        }
        let mut ids = first.clone();
        for p in &rest {
            ids = ids.combine_ids(p, op, Manifold::empty());
        }
        let leaves: Vec<CsgNode> =
            std::iter::once(first).chain(rest).map(|p| CsgNode::leaf(p.manifold.into_impl())).collect();
        ids.manifold = Manifold::from_impl(CsgNode::op_n(op, leaves).evaluate());
        Some(ids)
    }

    /// The colour bookkeeping of `binOp` for `self op rhs`, with the
    /// already computed solid.
    fn combine_ids(&self, rhs: &ManifoldGeometry, op: OpType, manifold: Manifold) -> ManifoldGeometry {
        let mut id_to_color = self.id_to_color.clone();
        let mut subtracted = self.subtracted.clone();
        let mut original_ids = self.original_ids.clone();
        original_ids.extend(rhs.original_ids.iter().copied());
        if op == OpType::Subtract {
            // Faces from the subtrahend are cut faces unless they had a
            // colour of their own.
            for id in &rhs.original_ids {
                match rhs.id_to_color.get(id) {
                    Some(c) => {
                        id_to_color.insert(*id, *c);
                    }
                    None => {
                        subtracted.insert(*id);
                    }
                }
            }
        } else {
            for (id, c) in &rhs.id_to_color {
                id_to_color.entry(*id).or_insert(*c);
            }
            subtracted.extend(rhs.subtracted.iter().copied());
        }
        ManifoldGeometry { manifold, original_ids, id_to_color, subtracted, own_id: None }
    }

    /// `ManifoldGeometry::transform`.
    pub fn transform(&mut self, m: &Matrix) {
        let col = |c: usize| Vec3::new(m[0][c], m[1][c], m[2][c]);
        let mat = Mat3x4::from_cols(col(0), col(1), col(2), col(3));
        self.manifold = self.manifold.transform(&mat);
    }

    /// `ManifoldGeometry::setColor` (`ManifoldGeometry.cc:371-381`): make
    /// the whole solid one original (if it is not already) and map that ID
    /// to the colour, forgetting earlier colours and cut faces.
    pub fn set_color(&mut self, c: Color, ids: &dyn IdSource) {
        let id = self.make_original(ids);
        self.original_ids = BTreeSet::from([id]);
        self.id_to_color = BTreeMap::from([(id, c)]);
        self.subtracted.clear();
    }

    /// C++ `AsOriginal()` with an ID from `ids`: rebuild the mesh as one run.
    fn make_original(&mut self, ids: &dyn IdSource) -> u32 {
        if let Some(id) = self.own_id {
            return id;
        }
        let id = ids.reserve(1);
        if !self.manifold.is_empty() {
            let mut mesh = self.manifold.get_mesh_gl64(-1);
            mesh.run_index = vec![0, mesh.tri_verts.len() as u64];
            mesh.run_original_id = vec![id];
            mesh.run_transform.clear();
            mesh.run_flags.clear();
            mesh.face_id.clear();
            let rebuilt = Manifold::from_mesh_gl64(&mesh);
            if rebuilt.status() == Error::NoError {
                self.manifold = rebuilt;
            }
        }
        self.own_id = Some(id);
        id
    }

    /// `ManifoldGeometry::toPolySet` (`ManifoldGeometry.cc:129-210`): the
    /// triangles run by run, each run coloured by its original ID.
    pub fn to_polyset(&self, scheme: &Scheme) -> PolySet {
        let mesh = self.manifold.get_mesh_gl64(-1);
        let np = mesh.num_prop as usize;
        let mut ps = PolySet { triangular: true, ..Default::default() };
        ps.vertices = mesh.vert_properties.chunks(np.max(3)).map(|v| [v[0], v[1], v[2]]).collect();
        let mut front: Option<i32> = None;
        let mut back: Option<i32> = None;
        let mut by_color: BTreeMap<[u32; 4], i32> = BTreeMap::new();
        let mut by_id: BTreeMap<u32, i32> = BTreeMap::new();
        let mut color_index = |ps: &mut PolySet, id: u32| -> i32 {
            let push = |ps: &mut PolySet, c: Color| {
                ps.colors.push(c);
                ps.colors.len() as i32 - 1
            };
            if self.subtracted.contains(&id) {
                return *back.get_or_insert_with(|| push(ps, scheme.face_back));
            }
            if let Some(&i) = by_id.get(&id) {
                return i;
            }
            let Some(c) = self.id_to_color.get(&id) else {
                return *front.get_or_insert_with(|| push(ps, scheme.face_front));
            };
            let i = *by_color.entry(c.key()).or_insert_with(|| push(ps, *c));
            by_id.insert(id, i);
            i
        };
        if mesh.run_index.is_empty() {
            return ps;
        }
        // Manifold orders runs by original ID, then by its internal mesh
        // ID. Mesh IDs come from a global counter, so when copies of one
        // mesh were built on different threads their order is a race.
        // Ordering runs that share an original ID by their lowest vertex
        // instead keeps exports byte-identical from run to run; OpenSCAD's
        // serial order is not reproducible anyway, as its IDs differ too.
        let mut runs: Vec<(u32, u64, usize, usize)> = Vec::with_capacity(mesh.run_index.len());
        let mut start = mesh.run_index[0] as usize;
        for run in 0..mesh.run_index.len() - 1 {
            let end = mesh.run_index[run + 1] as usize;
            if end > start {
                let low = mesh.tri_verts[start..end].iter().copied().min().unwrap_or(0);
                runs.push((mesh.run_original_id[run], low, start, end));
                start = end;
            }
        }
        runs.sort_by_key(|&(id, low, _, _)| (id, low));
        for (id, _, start, end) in runs {
            let ci = color_index(&mut ps, id);
            for t in mesh.tri_verts[start..end].chunks(3) {
                ps.faces.push(vec![t[0] as u32, t[1] as u32, t[2] as u32]);
                ps.color_indices.push(ci);
            }
        }
        ps
    }

    pub fn bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        if self.is_empty() {
            return None;
        }
        let b = self.manifold.bounding_box();
        Some(([b.min.x, b.min.y, b.min.z], [b.max.x, b.max.y, b.max.z]))
    }
}

/// The part of OpenSCAD's CGAL repair (`CGALUtils::repairPolySet`, then
/// `orientToBoundAVolume`) that matters for real input: triangles that are
/// individually flipped. With the merge vectors applied and degenerate
/// triangles dropped, each connected patch is re-oriented so that every
/// shared edge is traversed in opposite directions, then flipped as a whole
/// if it encloses negative volume. Returns `None` when some edge is shared
/// by other than two triangles, which orientation cannot fix.
fn orient_soup(mesh: &MeshGL64, id: u32) -> Option<MeshGL64> {
    use std::collections::HashMap;
    let mut to = (0..(mesh.vert_properties.len() / 3) as u64).collect::<Vec<u64>>();
    for (f, t) in mesh.merge_from_vert.iter().zip(&mesh.merge_to_vert) {
        to[*f as usize] = *t;
    }
    let mut tris: Vec<[u64; 3]> = mesh
        .tri_verts
        .chunks(3)
        .map(|t| [to[t[0] as usize], to[t[1] as usize], to[t[2] as usize]])
        .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
        .collect();
    let mut edges: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
    for (i, t) in tris.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            edges.entry((a.min(b), a.max(b))).or_default().push(i);
        }
    }
    if edges.values().any(|v| v.len() != 2) {
        return None;
    }
    let has_edge = |t: &[u64; 3], a: u64, b: u64| (0..3).any(|k| t[k] == a && t[(k + 1) % 3] == b);
    let mut done = vec![false; tris.len()];
    let pos = |v: u64| {
        let i = 3 * v as usize;
        [mesh.vert_properties[i], mesh.vert_properties[i + 1], mesh.vert_properties[i + 2]]
    };
    for seed in 0..tris.len() {
        if done[seed] {
            continue;
        }
        done[seed] = true;
        let mut component = vec![seed];
        let mut queue = vec![seed];
        while let Some(i) = queue.pop() {
            let t = tris[i];
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                for &j in &edges[&(a.min(b), a.max(b))] {
                    if j == i || done[j] {
                        continue;
                    }
                    // The neighbour must run the edge b -> a.
                    if has_edge(&tris[j], a, b) {
                        tris[j].swap(1, 2);
                    }
                    done[j] = true;
                    component.push(j);
                    queue.push(j);
                }
            }
        }
        let volume: f64 = component
            .iter()
            .map(|&i| {
                let [p, q, r] = tris[i].map(pos);
                p[0] * (q[1] * r[2] - q[2] * r[1]) - p[1] * (q[0] * r[2] - q[2] * r[0]) + p[2] * (q[0] * r[1] - q[1] * r[0])
            })
            .sum();
        if volume < 0.0 {
            for &i in &component {
                tris[i].swap(1, 2);
            }
        }
    }
    let tri_verts: Vec<u64> = tris.iter().flatten().copied().collect();
    let run_end = tri_verts.len() as u64;
    Some(MeshGL64 {
        num_prop: 3,
        vert_properties: mesh.vert_properties.clone(),
        tri_verts,
        run_index: vec![0, run_end],
        run_original_id: vec![id],
        ..Default::default()
    })
}

/// `MeshGL64::Merge` (Manifold `src/sort.cpp`, `MergeMeshGLP`): join the
/// open edges of a mesh by merging vertices that lie within the tolerance
/// of each other, recorded in the mesh's merge vectors. manifold-rust only
/// ports the single-precision version, so this is the same algorithm (open
/// halfedges, a BVH over their start vertices, union-find) with the
/// double-precision tolerance, `max(tolerance, kPrecision * bbox.Scale())`
/// with `kPrecision = 1e-12`. Returns whether there were open edges.
fn merge_coincident(mesh: &mut MeshGL64) -> bool {
    use manifold_rust::collider::Collider;
    use manifold_rust::disjoint_sets::DisjointSets;
    use manifold_rust::sort::morton_code;
    use manifold_rust::types::Box as BBox;

    let num_vert = mesh.vert_properties.len() / 3;
    let num_tri = mesh.tri_verts.len() / 3;
    let mut merge_map: Vec<usize> = (0..num_vert).collect();
    for (f, t) in mesh.merge_from_vert.iter().zip(&mesh.merge_to_vert) {
        merge_map[*f as usize] = *t as usize;
    }
    let next = [1usize, 2, 0];
    let mut open: BTreeSet<(usize, usize)> = BTreeSet::new();
    for tri in 0..num_tri {
        for i in 0..3 {
            let a = merge_map[mesh.tri_verts[3 * tri + next[i]] as usize];
            let b = merge_map[mesh.tri_verts[3 * tri + i] as usize];
            if !open.remove(&(b, a)) {
                open.insert((a, b));
            }
        }
    }
    if open.is_empty() {
        return false;
    }
    let open_verts: Vec<usize> = open.iter().map(|&(_, b)| b).collect::<BTreeSet<_>>().into_iter().collect();
    let pos = |v: usize| Vec3::new(mesh.vert_properties[3 * v], mesh.vert_properties[3 * v + 1], mesh.vert_properties[3 * v + 2]);
    let mut bbox = BBox::default();
    for v in 0..num_vert {
        bbox.union_point(pos(v));
    }
    let tolerance = f64::max(mesh.tolerance, 1e-12 * bbox.scale());
    let half = tolerance / 2.0;
    let boxes: Vec<BBox> = open_verts
        .iter()
        .map(|&v| BBox::from_points(pos(v) - Vec3::new(half, half, half), pos(v) + Vec3::new(half, half, half)))
        .collect();
    let codes: Vec<u32> = open_verts.iter().map(|&v| morton_code(pos(v), &bbox)).collect();
    let mut order: Vec<usize> = (0..open_verts.len()).collect();
    order.sort_by_key(|&i| codes[i]);
    let sorted_box: Vec<BBox> = order.iter().map(|&i| boxes[i]).collect();
    let sorted_code: Vec<u32> = order.iter().map(|&i| codes[i]).collect();
    let sorted_vert: Vec<usize> = order.iter().map(|&i| open_verts[i]).collect();
    let collider = Collider::new(sorted_box.clone(), sorted_code);
    let uf = DisjointSets::new(num_vert as u32);
    collider.collisions_with_boxes(&sorted_box, false, |a, b| {
        uf.unite(sorted_vert[a] as u32, sorted_vert[b] as u32);
    });
    for (f, t) in mesh.merge_from_vert.iter().zip(&mesh.merge_to_vert) {
        uf.unite(*f as u32, *t as u32);
    }
    mesh.merge_from_vert.clear();
    mesh.merge_to_vert.clear();
    for v in 0..num_vert {
        let to = uf.find(v as u32) as usize;
        if to != v {
            mesh.merge_from_vert.push(v as u64);
            mesh.merge_to_vert.push(to as u64);
        }
    }
    true
}
