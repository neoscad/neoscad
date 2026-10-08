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
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use manifold_rust::cancel::CancelToken;
use manifold_rust::impl_mesh::ManifoldImpl;
use manifold_rust::linalg::{Mat3x4, Vec3};
use manifold_rust::manifold::Manifold;
/// The boolean operations, for [`ManifoldGeometry::boolean`].
pub use manifold_rust::types::OpType;
use manifold_rust::types::{BooleanEngine, Error, MeshGL64};

use crate::Matrix;
use crate::color::{Color, Scheme};
use crate::polygon2d::{Outline, Polygon2d};
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

/// The token a request's kernel operations run under, so one long
/// boolean stops on a cancel or a passed limit instead of running to its
/// end: on wasm32 a boolean that grows past the address space traps the
/// instance, which only a stop inside the operation prevents.
///
/// The token is over `interrupt` itself (a cancel sets it, and so does
/// the guard when a limit passes), and asks `guard` for the limits only
/// it can see, the clock and the measured memory, at every check. What a
/// boolean allocates between two looks is what a memory limit overshoots
/// by, so none is skipped: the kernel checks at its stage boundaries and
/// every few thousand items of its long loops, about 600,000 times in the
/// Menger sponge's depth-5 render, which is milliseconds of clock reads
/// (and why a host's memory probe must be cheap). `None` when nothing can
/// stop the request, so an unlimited render runs the kernel's
/// uncancellable path. A cancelled operation's result is empty with
/// [`Error::Cancelled`] ([`ManifoldGeometry::is_cancelled`]); callers turn
/// it into an interruption and never cache it.
pub fn kernel_token(
    interrupt: Option<&Arc<AtomicBool>>,
    guard: Option<&Arc<eval::limits::Guard>>,
) -> Option<CancelToken> {
    if interrupt.is_none() && guard.is_none() {
        return None;
    }
    let flag = interrupt.cloned().unwrap_or_default();
    let token = CancelToken::from_flag(flag);
    Some(match guard {
        None => token,
        Some(g) => {
            let g = g.clone();
            token.with_check(Arc::new(move || g.stopped()))
        }
    })
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
    /// Original ID -> the dotted name of the `part()` its faces came from
    /// (neoscad's `part()` extension; empty without parts). Original IDs
    /// survive booleans, so this says which part each output face belongs
    /// to; see [`ManifoldGeometry::tag_part`].
    parts: BTreeMap<u32, Arc<str>>,
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
            .field("parts", &self.parts)
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
        ManifoldGeometry {
            manifold,
            original_ids: BTreeSet::new(),
            id_to_color: BTreeMap::new(),
            subtracted: BTreeSet::new(),
            own_id: None,
            parts: BTreeMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.manifold.is_empty()
    }

    /// The result of a kernel operation its token cancelled: empty, and
    /// not the operation's answer, so it must not be used or cached.
    pub fn is_cancelled(&self) -> bool {
        self.manifold.status() == Error::Cancelled
    }

    /// A proper 2-manifold: Manifold reported no error and did not have to
    /// keep the mesh as a triangle soup (the repair path for closed but
    /// non-manifold input).
    pub fn is_valid(&self) -> bool {
        self.manifold.status() == Error::NoError && !self.manifold.as_impl().is_soup
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
    pub fn from_polyset(
        ps: &PolySet,
        ids: &dyn IdSource,
        warnings: &mut Warnings,
        errors: &mut Warnings,
    ) -> ManifoldGeometry {
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
        let mut mesh = MeshGL64 {
            num_prop: 3,
            ..Default::default()
        };
        mesh.vert_properties = ps.vertices.iter().flatten().copied().collect();
        // `std::map<std::optional<Color4f>, ...>`: uncoloured faces first,
        // then colours in order.
        type Group = (Option<Color>, Vec<usize>);
        let mut groups: BTreeMap<Option<[u32; 4]>, Group> = BTreeMap::new();
        for i in 0..ps.faces.len() {
            let ci = ps.color_indices.get(i).copied().unwrap_or(-1);
            let color = (ci >= 0).then(|| ps.colors[ci as usize]);
            groups
                .entry(color.map(|c| c.key()))
                .or_insert((color, Vec::new()))
                .1
                .push(i);
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
                mesh.tri_verts
                    .extend(ps.faces[f].iter().map(|&v| u64::from(v)));
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
            let repaired = orient_soup(&mesh, id)
                .map(|r| Manifold::from_mesh_gl64(&r))
                .filter(|r| r.status() == Error::NoError);
            m = match repaired {
                Some(r) => r,
                None => {
                    let r = Manifold::from_mesh_gl64_robust(&mesh);
                    if r.status() != Error::NoError {
                        // The repaired surface is converted anyway, fails
                        // again, and `createManifoldFromSurfaceMesh` returns
                        // null (`manifoldutils.cc:181-187, 275-279`): the
                        // result is empty, and callers that can fall back
                        // (`projection()`) do.
                        errors.push("[manifold] Input mesh is not closed!".into());
                        errors.push(format!(
                            "[manifold] Surface_mesh -> Manifold conversion failed: {}",
                            status_name(Error::NotManifold)
                        ));
                        return ManifoldGeometry::default();
                    }
                    r
                }
            };
            return ManifoldGeometry {
                manifold: m,
                original_ids: BTreeSet::from([id]),
                id_to_color: BTreeMap::new(),
                subtracted: BTreeSet::new(),
                own_id: None,
                parts: BTreeMap::new(),
            };
        }
        ManifoldGeometry {
            manifold: m,
            original_ids,
            id_to_color,
            subtracted: BTreeSet::new(),
            own_id: None,
            parts: BTreeMap::new(),
        }
    }

    /// `ManifoldGeometry::binOp` (`ManifoldGeometry.cc:263-289`).
    pub fn boolean(&self, rhs: &ManifoldGeometry, op: OpType) -> ManifoldGeometry {
        self.boolean_until(rhs, op, None)
    }

    /// [`ManifoldGeometry::boolean`] under `token` ([`kernel_token`]); a
    /// cancelled one is [`ManifoldGeometry::is_cancelled`].
    pub fn boolean_until(
        &self,
        rhs: &ManifoldGeometry,
        op: OpType,
        token: Option<&CancelToken>,
    ) -> ManifoldGeometry {
        // The exact engine needs manifold operands; a soup from the repair
        // path goes through the robust engine instead.
        let engine = if self.manifold.as_impl().is_soup || rhs.manifold.as_impl().is_soup {
            BooleanEngine::Robust
        } else {
            BooleanEngine::Exact
        };
        let manifold =
            self.manifold
                .boolean_with_engine_and_progress(&rhs.manifold, op, engine, token, None);
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
        Self::batch_until(op, parts, None)
    }

    /// [`ManifoldGeometry::batch`] under `token` ([`kernel_token`]): the
    /// kernel checks it between its rounds and inside each boolean, and a
    /// cancelled batch is [`ManifoldGeometry::is_cancelled`].
    pub fn batch_until(
        op: OpType,
        parts: Vec<ManifoldGeometry>,
        token: Option<&CancelToken>,
    ) -> Option<ManifoldGeometry> {
        use manifold_rust::csg_tree::CsgNode;
        let mut it = parts.into_iter();
        let first = it.next()?;
        let rest: Vec<ManifoldGeometry> = it.collect();
        if rest.is_empty() {
            return Some(first);
        }
        let soup =
            first.manifold.as_impl().is_soup || rest.iter().any(|p| p.manifold.as_impl().is_soup);
        if soup {
            let mut acc = first;
            for p in &rest {
                acc = acc.boolean_until(p, op, token);
                if acc.is_cancelled() {
                    break;
                }
            }
            return Some(acc);
        }
        let mut ids = first.clone();
        for p in &rest {
            ids = ids.combine_ids(p, op, Manifold::empty());
        }
        // Operands may share mesh IDs: two copies of one cached solid do
        // (whether they do depends on scheduling: siblings with the same key
        // computed at once both miss the cache and get separate IDs, computed
        // in turn the second is a cache hit). The kernel keeps each copy its
        // own run either way. Its `compose_meshes` (the disjoint-parts step
        // of a union) ranks each node's IDs node by node, as C++ `Compose`
        // does by offsetting them (`csg_tree.cpp:386-395`), and a boolean
        // offsets its right operand's IDs. Before manifold-rust 0.16.0,
        // `compose_meshes` merged the copies into one run, and this
        // renumbered colliding operands first; without that the exported
        // triangle order changed from run to run.
        let leaves: Vec<CsgNode> = std::iter::once(first)
            .chain(rest)
            .map(|p| CsgNode::leaf(p.manifold.into_impl()))
            .collect();
        ids.manifold = Manifold::from_impl(CsgNode::op_n(op, leaves).evaluate_with_token(token));

        Some(ids)
    }

    /// The colour bookkeeping of `binOp` for `self op rhs`, with the
    /// already computed solid.
    fn combine_ids(
        &self,
        rhs: &ManifoldGeometry,
        op: OpType,
        manifold: Manifold,
    ) -> ManifoldGeometry {
        let mut id_to_color = self.id_to_color.clone();
        let mut subtracted = self.subtracted.clone();
        let mut original_ids = self.original_ids.clone();
        original_ids.extend(rhs.original_ids.iter().copied());
        let mut parts = self.parts.clone();
        if op == OpType::Subtract {
            // Faces a subtrahend leaves behind are the minuend's new
            // surface, so they belong to the minuend's part when it is a
            // single part; a subtracted part's own name does not carry
            // over (it is not in the result).
            if let Some(owner) = self.single_part() {
                for id in rhs.ids() {
                    parts.insert(id, owner.clone());
                }
            }
        } else {
            for (id, name) in &rhs.parts {
                parts.entry(*id).or_insert_with(|| name.clone());
            }
        }
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
        ManifoldGeometry {
            manifold,
            original_ids,
            id_to_color,
            subtracted,
            own_id: None,
            parts,
        }
    }

    /// Every original ID the bookkeeping knows (the colour state's and the
    /// solid's own).
    fn ids(&self) -> BTreeSet<u32> {
        let mut ids = self.original_ids.clone();
        ids.extend(self.own_id);
        ids
    }

    /// The part every face belongs to, if it is one part.
    fn single_part(&self) -> Option<Arc<str>> {
        let mut owner: Option<&Arc<str>> = None;
        for id in self.ids() {
            let name = self.parts.get(&id)?;
            match owner {
                None => owner = Some(name),
                Some(o) if o == name => {}
                Some(_) => return None,
            }
        }
        owner.cloned()
    }

    /// Faces of more than one part (or of a part and no part): collapsing
    /// the solid into one original, as `set_color` and `to_original` do,
    /// would lose which is which.
    fn mixed_parts(&self) -> bool {
        !self.parts.is_empty() && self.single_part().is_none()
    }

    /// Whether any face belongs to a `part()`.
    pub fn has_parts(&self) -> bool {
        !self.parts.is_empty()
    }

    /// The part the faces with original ID `id` came from.
    pub fn part_of(&self, id: u32) -> Option<&Arc<str>> {
        self.parts.get(&id)
    }

    /// Attribute every face not already in a (nested) part to part `name`,
    /// giving each original ID a fresh one from `ids` first.
    ///
    /// The fresh IDs matter because a cached subtree is shared: in
    /// `part("a") x(); part("b") x();` both parts hold the same solid with
    /// the same IDs, and without renumbering the union could not tell
    /// their faces apart. The mesh is not rebuilt: only the IDs in its
    /// relation tables change, one for one, so the geometry, the triangle
    /// order within runs and the colours stay as they were.
    pub fn tag_part(&mut self, name: &Arc<str>, ids: &dyn IdSource) {
        let old = self.all_ids();
        if old.is_empty() {
            return;
        }
        let first = ids.reserve(old.len() as u32);
        let map: BTreeMap<u32, u32> = old.iter().copied().zip(first..).collect();
        // A part entry for an ID the solid no longer carries is dropped.
        self.parts.retain(|id, _| old.contains(id));
        self.relabel(&map);
        for &id in map.values() {
            self.parts.entry(id).or_insert_with(|| name.clone());
        }
    }

    /// Every original ID the solid carries anywhere: its bookkeeping
    /// (colours, subtracted faces, its own ID) and the kernel's relation
    /// tables (`parts` is keyed by these same IDs). A renumbering must
    /// cover all of them, or the solid would keep a stale ID in one table
    /// and lose a colour or a part.
    pub fn all_ids(&self) -> BTreeSet<u32> {
        let mut all = self.ids();
        all.extend(self.id_to_color.keys().copied());
        all.extend(self.subtracted.iter().copied());
        if !self.manifold.is_empty() {
            let imp = self.manifold.as_impl();
            all.extend(
                imp.mesh_relation
                    .mesh_id_transform
                    .values()
                    .filter(|r| r.original_id >= 0)
                    .map(|r| r.original_id as u32),
            );
        }
        all
    }

    /// Replace original IDs by `map` (IDs it does not name stay). The mesh
    /// is not rebuilt: only the IDs in its relation tables change, one for
    /// one, so the geometry, the triangle order within runs and the colours
    /// stay as they were. Every ID is mapped at once, so `map` may permute
    /// IDs, but no two IDs may end up the same.
    pub fn relabel(&mut self, map: &BTreeMap<u32, u32>) {
        let to = |id: u32| map.get(&id).copied().unwrap_or(id);
        let to_i = |id: i32| {
            if id < 0 { id } else { to(id as u32) as i32 }
        };
        if !self.manifold.is_empty() {
            let mut imp = std::mem::replace(&mut self.manifold, Manifold::empty()).into_impl();
            imp.mesh_relation.original_id = to_i(imp.mesh_relation.original_id);
            for r in imp.mesh_relation.mesh_id_transform.values_mut() {
                r.original_id = to_i(r.original_id);
            }
            for r in imp.mesh_relation.tri_ref.iter_mut() {
                r.original_id = to_i(r.original_id);
            }
            self.manifold = Manifold::from_impl(imp);
        }
        self.original_ids = self.original_ids.iter().map(|&i| to(i)).collect();
        self.id_to_color = self.id_to_color.iter().map(|(&i, c)| (to(i), *c)).collect();
        self.subtracted = self.subtracted.iter().map(|&i| to(i)).collect();
        self.own_id = self.own_id.map(to);
        self.parts = std::mem::take(&mut self.parts)
            .into_iter()
            .map(|(i, owner)| (to(i), owner))
            .collect();
    }

    /// Attribute every face not already in a nested part to part `name`,
    /// keeping the IDs: for a solid whose IDs were just drawn for it (a
    /// mesh converted at the part itself).
    pub fn claim_part(&mut self, name: &Arc<str>) {
        for id in self.ids() {
            self.parts.entry(id).or_insert_with(|| name.clone());
        }
    }

    /// A solid the kernel built from points (`Manifold::Hull`), wrapped as
    /// `ManifoldGeometry(manifold)` does: no original IDs, colours or cut
    /// faces, so a hull subtracted in a `difference()` is not drawn as a cut
    /// face. The kernel tagged it with an ID from its process-wide counter,
    /// whose value depends on which thread drew first; it is retagged with
    /// `id`, from a block reserved in tree order, so later booleans order
    /// its triangles the same way on every run.
    pub fn from_built(mut imp: ManifoldImpl, id: u32) -> ManifoldGeometry {
        set_original_id(&mut imp, id);
        ManifoldGeometry {
            manifold: Manifold::from_impl(imp),
            original_ids: BTreeSet::new(),
            id_to_color: BTreeMap::new(),
            subtracted: BTreeSet::new(),
            own_id: Some(id),
            parts: BTreeMap::new(),
        }
    }

    /// A closed, oriented mesh built elsewhere (a fillet's blend tool,
    /// `meshbrep::blend`) as one new original `id`, from a block reserved
    /// in tree order. Its surface tags are not kept: the normal render has
    /// no use for them. Empty if the mesh is not a 2-manifold.
    pub fn from_tagged(mesh: &meshbrep::TaggedMesh, id: u32) -> ManifoldGeometry {
        let n = mesh.triangles.len() as u64 * 3;
        let gl = MeshGL64 {
            num_prop: 3,
            vert_properties: mesh.positions.iter().flatten().copied().collect(),
            tri_verts: mesh
                .triangles
                .iter()
                .flat_map(|t| t.map(u64::from))
                .collect(),
            run_index: vec![0, n],
            run_original_id: vec![id],
            ..Default::default()
        };
        let m = Manifold::from_mesh_gl64(&gl);
        if m.status() != Error::NoError {
            return ManifoldGeometry::default();
        }
        ManifoldGeometry {
            manifold: m,
            original_ids: BTreeSet::from([id]),
            id_to_color: BTreeMap::new(),
            subtracted: BTreeSet::new(),
            own_id: None,
            parts: BTreeMap::new(),
        }
    }

    /// After a fillet's tools (originals `tools`) were added to and
    /// subtracted from `child_ids`: their faces are the part's own surface,
    /// not cut faces, so they are not drawn as a `difference()`'s cuts
    /// are; and when every face of the child had one colour, or belonged
    /// to one `part()`, the blends take it. (Blends between faces of
    /// different colours or parts get neither: `docs/followups.md`,
    /// "Fillets and chamfers".)
    pub fn adopt_tools(&mut self, child_ids: &BTreeSet<u32>, tools: &[u32]) {
        for id in tools {
            self.subtracted.remove(id);
        }
        // The same for the part (`--enable part`) the faces belong to.
        let mut parts = child_ids.iter().map(|id| self.parts.get(id));
        if let Some(Some(first)) = parts.next() {
            let first = first.clone();
            if parts.all(|p| p == Some(&first)) {
                for &id in tools {
                    self.parts.insert(id, first.clone());
                }
            }
        }
        let mut colours = child_ids.iter().map(|id| self.id_to_color.get(id));
        let Some(Some(first)) = colours.next() else {
            return;
        };
        let first = *first;
        if colours.all(|c| c.is_some_and(|c| c.key() == first.key())) {
            for &id in tools {
                self.id_to_color.insert(id, first);
            }
        }
    }

    /// The original IDs of the solid's own faces (not the cut faces of
    /// what a `difference()` in it subtracted).
    pub fn own_face_ids(&self) -> BTreeSet<u32> {
        let mut ids = self.ids();
        ids.retain(|id| !self.subtracted.contains(id));
        ids
    }

    /// [`Self::to_original`] for a solid that must not keep the ID it was
    /// built with: a solid from [`Self::from_built`] already is one
    /// original, and `to_original` would keep its ID, so it is retagged
    /// with a fresh one from `ids` instead (same mesh, no rebuild). Any
    /// other solid goes through `to_original`.
    pub fn to_fresh_original(&mut self, ids: &dyn IdSource) {
        let owner = self.single_part();
        if self.own_id.is_none() || self.manifold.is_empty() {
            self.own_id = None;
            self.to_original(ids);
            return;
        }
        let id = ids.reserve(1);
        let mut imp = std::mem::replace(&mut self.manifold, Manifold::empty()).into_impl();
        set_original_id(&mut imp, id);
        self.manifold = Manifold::from_impl(imp);
        self.own_id = Some(id);
        self.original_ids = BTreeSet::from([id]);
        self.id_to_color.clear();
        self.subtracted.clear();
        self.parts = owner.map(|o| BTreeMap::from([(id, o)])).unwrap_or_default();
    }

    /// `ManifoldGeometry::toOriginal` (`ManifoldGeometry.cc:383-392`): the
    /// solid becomes one original with no colour and no cut faces.
    ///
    /// With faces of several parts the IDs are kept instead (only the
    /// colours and cut faces are dropped), so the parts stay apart: the
    /// geometry is the same, only the triangle runs differ from a
    /// collapsed solid's. That case needs `part()`, which OpenSCAD lacks.
    pub fn to_original(&mut self, ids: &dyn IdSource) {
        if self.mixed_parts() {
            self.id_to_color.clear();
            self.subtracted.clear();
            return;
        }
        let owner = self.single_part();
        let id = self.make_original(ids);
        self.original_ids = BTreeSet::from([id]);
        self.id_to_color.clear();
        self.subtracted.clear();
        self.parts = owner.map(|o| BTreeMap::from([(id, o)])).unwrap_or_default();
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
    ///
    /// With faces of several parts (`color() { part("a") ...; part("b")
    /// ...; }`) the IDs are kept and each is mapped to the colour, which
    /// colours the same faces without merging the parts into one.
    pub fn set_color(&mut self, c: Color, ids: &dyn IdSource) {
        if self.mixed_parts() {
            self.id_to_color = self.ids().into_iter().map(|id| (id, c)).collect();
            self.subtracted.clear();
            return;
        }
        let owner = self.single_part();
        let id = self.make_original(ids);
        self.original_ids = BTreeSet::from([id]);
        self.id_to_color = BTreeMap::from([(id, c)]);
        self.subtracted.clear();
        self.parts = owner.map(|o| BTreeMap::from([(id, o)])).unwrap_or_default();
    }

    /// Every coloured face in `c`. For a copy of a solid converted from a
    /// mesh whose faces are all one colour: it is then what converting the
    /// same mesh in `c` gives, since a conversion gives each colour group
    /// one ID and nothing else of the colour reaches the kernel
    /// ([`Self::from_polyset`]).
    pub(crate) fn recolor_uniform(&mut self, c: Color) {
        for v in self.id_to_color.values_mut() {
            *v = c;
        }
    }

    /// C++ `AsOriginal()` with an ID from `ids`: rebuild the mesh as one run.
    fn make_original(&mut self, ids: &dyn IdSource) -> u32 {
        if let Some(id) = self.own_id {
            return id;
        }
        let id = ids.reserve(1);
        if !self.manifold.is_empty() {
            let mut mesh = canonical_mesh(&self.manifold);
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
        self.to_polyset_with_ids(scheme).0
    }

    /// [`Self::to_polyset`] with each face's original ID (see
    /// [`Self::part_of`]).
    pub fn to_polyset_with_ids(&self, scheme: &Scheme) -> (PolySet, Vec<u32>) {
        let mut face_ids = Vec::new();
        let mesh = canonical_mesh(&self.manifold);
        let np = mesh.num_prop as usize;
        let mut ps = PolySet {
            triangular: true,
            ..Default::default()
        };
        ps.vertices = mesh
            .vert_properties
            .chunks(np.max(3))
            .map(|v| [v[0], v[1], v[2]])
            .collect();
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
            return (ps, face_ids);
        }
        // Runs come in `canonical_mesh` order.
        let mut start = mesh.run_index[0] as usize;
        for run in 0..mesh.run_index.len() - 1 {
            let end = mesh.run_index[run + 1] as usize;
            if end == start {
                continue;
            }
            let ci = color_index(&mut ps, mesh.run_original_id[run]);
            for t in mesh.tri_verts[start..end].chunks(3) {
                ps.faces.push(vec![t[0] as u32, t[1] as u32, t[2] as u32]);
                ps.color_indices.push(ci);
                face_ids.push(mesh.run_original_id[run]);
            }
            start = end;
        }
        (ps, face_ids)
    }

    pub fn bounds(&self) -> Option<([f64; 3], [f64; 3])> {
        if self.is_empty() {
            return None;
        }
        let b = self.manifold.bounding_box();
        Some(([b.min.x, b.min.y, b.min.z], [b.max.x, b.max.y, b.max.z]))
    }

    /// `ManifoldGeometry::slice`: the cross-section at z = 0, as
    /// `CrossSection(manifold.Slice()).ToPolygons()`. The result is
    /// unsanitized; `projection(cut = true)` sanitizes it. The raw loops
    /// come from the implementation, as in `project`: since manifold-rust
    /// 0.15.0, `Manifold::slice` already runs the `CrossSection` union, and
    /// a second one here would union the loops twice where C++ does it once.
    pub fn slice(&self) -> Polygon2d {
        if self.is_empty() || self.manifold.as_impl().is_soup {
            return Polygon2d::default();
        }
        positive_union(self.manifold.as_impl().slice(0.0))
    }

    /// `ManifoldGeometry::project`: the outline seen from above, as
    /// `CrossSection(manifold.Project()).ToPolygons()`. manifold-rust's own
    /// `project()` unions with the non-zero rule at 6 decimals, where the
    /// C++ `CrossSection` constructor uses the positive rule at 8
    /// (`cross_section.cpp:35,273-279`), so the raw loops are taken from the
    /// implementation and unioned here.
    pub fn project(&self) -> Polygon2d {
        if self.is_empty() || self.manifold.as_impl().is_soup {
            return Polygon2d::default();
        }
        positive_union(self.manifold.as_impl().project())
    }
}

/// Manifold's `InitializeOriginal` with a given ID instead of a fresh one
/// from the kernel's counter: every triangle becomes part of one original
/// mesh `id`.
pub fn set_original_id(imp: &mut ManifoldImpl, id: u32) {
    use manifold_rust::types::Relation;
    let had_normals = imp.all_have_normals();
    let id = id as i32;
    imp.mesh_relation.original_id = id;
    for (tri, r) in imp.mesh_relation.tri_ref.iter_mut().enumerate() {
        r.mesh_id = id;
        r.original_id = id;
        r.face_id = -1;
        r.coplanar_id = tri as i32;
    }
    imp.mesh_relation.mesh_id_transform.clear();
    imp.mesh_relation.mesh_id_transform.insert(
        id,
        Relation {
            original_id: id,
            transform: Mat3x4::identity(),
            back_side: false,
            has_normals: had_normals,
        },
    );
}

/// The solid's mesh with its runs in an order that does not depend on
/// thread scheduling.
///
/// `GetMeshGL` groups triangles into runs sorted by original ID and then by
/// mesh ID. Mesh IDs come from Manifold's process-wide counter, drawn inside
/// the kernel (every boolean renumbers its right operand's IDs, every
/// transformed copy gets new ones), so when subtrees are built on several
/// threads the relative order of two runs sharing an original ID (copies of
/// one cached mesh, say) depends on which thread drew first. Everything else
/// in the kernel only compares mesh IDs for equality, so this ordering is
/// the one place the race leaks out: into exported files, and, through
/// [`ManifoldGeometry::set_color`]'s rebuild, into later solids.
///
/// Runs sharing an original ID are ordered here by their first triangle
/// instead. Vertex numbering and the triangle order inside a run follow the
/// kernel's own (stable, geometric) sorts, so that triangle is the same
/// every time, and no two runs share one, so the order is total. OpenSCAD's
/// serial order cannot be reproduced anyway: its IDs differ from ours.
fn canonical_mesh(m: &Manifold) -> MeshGL64 {
    let mut mesh = m.get_mesh_gl64(-1);
    let runs = mesh.run_original_id.len();
    if runs < 2 {
        return mesh;
    }
    let total = mesh.tri_verts.len();
    let span = |r: usize| -> (usize, usize) {
        let start = mesh.run_index[r] as usize;
        let end = mesh.run_index.get(r + 1).map_or(total, |&e| e as usize);
        (start, end)
    };
    let mut order: Vec<usize> = (0..runs).collect();
    order.sort_by_key(|&r| {
        let (start, end) = span(r);
        let first: [u64; 3] = if end >= start + 3 {
            [
                mesh.tri_verts[start],
                mesh.tri_verts[start + 1],
                mesh.tri_verts[start + 2],
            ]
        } else {
            [u64::MAX; 3]
        };
        (mesh.run_original_id[r], first)
    });
    if order.iter().enumerate().all(|(i, &r)| i == r) {
        return mesh;
    }
    let per_run_transform = mesh.run_transform.len() == 12 * runs;
    let per_run_flags = mesh.run_flags.len() == runs;
    let per_tri_face = mesh.face_id.len() * 3 == total;
    let mut tri_verts = Vec::with_capacity(total);
    let mut run_index = Vec::with_capacity(runs + 1);
    let mut run_original_id = Vec::with_capacity(runs);
    let mut run_transform = Vec::new();
    let mut run_flags = Vec::new();
    let mut face_id = Vec::new();
    for &r in &order {
        let (start, end) = span(r);
        run_index.push(tri_verts.len() as u64);
        tri_verts.extend_from_slice(&mesh.tri_verts[start..end]);
        run_original_id.push(mesh.run_original_id[r]);
        if per_run_transform {
            run_transform.extend_from_slice(&mesh.run_transform[12 * r..12 * r + 12]);
        }
        if per_run_flags {
            run_flags.push(mesh.run_flags[r]);
        }
        if per_tri_face {
            face_id.extend_from_slice(&mesh.face_id[start / 3..end / 3]);
        }
    }
    run_index.push(tri_verts.len() as u64);
    mesh.tri_verts = tri_verts;
    mesh.run_index = run_index;
    mesh.run_original_id = run_original_id;
    if per_run_transform {
        mesh.run_transform = run_transform;
    }
    if per_run_flags {
        mesh.run_flags = run_flags;
    }
    if per_tri_face {
        mesh.face_id = face_id;
    }
    // Per-halfedge tangents are not requested (`get_mesh_gl64(-1)`), so
    // there is nothing else to permute.
    debug_assert!(mesh.halfedge_tangent.is_empty());
    mesh
}

/// `CrossSection(Polygons)`: Clipper's union with the positive fill rule at
/// 8 decimal digits, read back as outlines.
fn positive_union(polys: Vec<Vec<manifold_rust::linalg::Vec2>>) -> Polygon2d {
    use clipper2_rust::{FillRule, PathD, PathsD, Point, union_d};
    if polys.is_empty() {
        return Polygon2d::default();
    }
    let paths: PathsD = polys
        .iter()
        .map(|p| p.iter().map(|v| Point::new(v.x, v.y)).collect::<PathD>())
        .collect();
    let res = union_d(&paths, &PathsD::new(), FillRule::Positive, 8);
    Polygon2d {
        outlines: res
            .iter()
            .map(|p| Outline::new(p.iter().map(|q| [q.x, q.y]).collect()))
            .collect(),
        sanitized: false,
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
        [
            mesh.vert_properties[i],
            mesh.vert_properties[i + 1],
            mesh.vert_properties[i + 2],
        ]
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
                p[0] * (q[1] * r[2] - q[2] * r[1]) - p[1] * (q[0] * r[2] - q[2] * r[0])
                    + p[2] * (q[0] * r[1] - q[1] * r[0])
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
    let open_verts: Vec<usize> = open
        .iter()
        .map(|&(_, b)| b)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let pos = |v: usize| {
        Vec3::new(
            mesh.vert_properties[3 * v],
            mesh.vert_properties[3 * v + 1],
            mesh.vert_properties[3 * v + 2],
        )
    };
    let mut bbox = BBox::default();
    for v in 0..num_vert {
        bbox.union_point(pos(v));
    }
    let tolerance = f64::max(mesh.tolerance, 1e-12 * bbox.scale());
    let half = tolerance / 2.0;
    let boxes: Vec<BBox> = open_verts
        .iter()
        .map(|&v| {
            BBox::from_points(
                pos(v) - Vec3::new(half, half, half),
                pos(v) + Vec3::new(half, half, half),
            )
        })
        .collect();
    let codes: Vec<u32> = open_verts
        .iter()
        .map(|&v| morton_code(pos(v), &bbox))
        .collect();
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
