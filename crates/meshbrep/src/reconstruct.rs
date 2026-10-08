//! Mesh-guided B-rep reconstruction.
//!
//! Faces are connected regions of triangles on one exact surface; edges
//! are the boundary chains between two faces, given exact curves;
//! vertices are where three or more faces meet (or where two tangent
//! surfaces' intersection crosses itself), solved on the exact surfaces.
//! Every choice is made in an order fixed by the input's indices or by
//! coordinates, never by hashing, so equal input gives equal output.

use std::collections::BTreeMap;

use crate::curve;
use crate::edges::{self, make_edge};
use crate::math::*;
use crate::model::{Surface, TaggedMesh};
use crate::nurbs::Spline;
use crate::solve::{solve, tangent_point};
use crate::surf::Surf;
use crate::tangency::{Cont, Side, contact};
use crate::topo::{TEdge, TFace, Topo};
use crate::{Error, Failure};

/// Tolerances. Relative ones are multiplied by the model's size (the
/// largest edge of its bounding box).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tolerances {
    /// Surfaces closer than this (relative) are the same surface, and
    /// surfaces this close to touching are tangent. It must absorb the
    /// rounding of the transforms that placed the surfaces. Default 1e-9.
    pub merge: f64,
    /// How far (absolute, model units) a fitted B-spline edge or
    /// parameter-space curve may stray from the surfaces it lies on.
    /// Default 1e-7, the precision written to the STEP file.
    pub fit: f64,
    /// How far (absolute, model units) a [`crate::Surface::BSpline`] may
    /// be from the surfaces it was made to meet: a blend whose spine and
    /// contact curves were fitted touches a cylinder only to within that
    /// fit. A side of a patch that lies on a neighbouring surface within
    /// `merge` (scaled) plus this, with normals parallel, is their
    /// contact: the edge between them is that side, and vertices on it are
    /// solved along it. On top of the tessellation's sagitta, this is the
    /// error a fitted surface adds; the residuals and validation of such a
    /// model are only as good as it. Default 1e-7, as `fit`. It matters
    /// only where a B-spline surface is.
    pub surface_fit: f64,
}

impl Default for Tolerances {
    fn default() -> Self {
        Tolerances {
            merge: 1e-9,
            fit: 1e-7,
            surface_fit: 1e-7,
        }
    }
}

/// Options for [`crate::reconstruct`].
#[derive(Clone, Default)]
pub struct Options {
    /// Tolerances.
    pub tolerances: Tolerances,
    /// Polled between the stages of reconstruction and once per face
    /// while faces are checked: when it returns true, reconstruction
    /// stops with [`Error::Stopped`]. A caller that reconstructs while a
    /// user waits (an editor's preview) can then abandon a large model at
    /// once instead of finishing work nobody will look at. `None` never
    /// stops.
    pub should_stop: Option<StopFn>,
}

/// A stop signal for [`Options::should_stop`].
pub type StopFn = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

impl Options {
    /// Whether [`Options::should_stop`] asks to stop.
    pub(crate) fn stopped(&self) -> bool {
        self.should_stop.as_ref().is_some_and(|f| f())
    }

    /// `Err(Stopped)` when [`Options::should_stop`] asks to stop.
    pub(crate) fn poll(&self) -> Result<(), Failure> {
        if self.stopped() {
            return Err(Error::Stopped.into());
        }
        Ok(())
    }
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("tolerances", &self.tolerances)
            .field("should_stop", &self.should_stop.is_some())
            .finish()
    }
}

impl PartialEq for Options {
    /// Equal tolerances and the same stop signal (the same allocation, or
    /// none on both sides): two closures cannot be compared otherwise.
    fn eq(&self, o: &Options) -> bool {
        self.tolerances == o.tolerances
            && match (&self.should_stop, &o.should_stop) {
                (None, None) => true,
                (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
                _ => false,
            }
    }
}

/// The surface classes of the triangles, and per class its surface,
/// whether it is a faceted plane, and the input index it came from.
struct Classes {
    of_tri: Vec<usize>,
    surf: Vec<Surf>,
    faceted: Vec<bool>,
    input: Vec<Option<u32>>,
}

/// A float that sorts, for the index below.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Key(f64);
impl Eq for Key {}
impl PartialOrd for Key {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Key {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&o.0)
    }
}

/// The surface classes found so far, sorted by one number per kind, so
/// that finding the class an exact record belongs to looks at a window of
/// candidates instead of every class. A model with tens of thousands of
/// planes (`$fn` spheres keep a plane per facet) spent seconds in the
/// pairwise scan.
///
/// The window is wide enough to hold every class [`Surf::same`] accepts,
/// so the answer (the lowest-numbered equal class) is the scan's:
///
/// - planes by `|n · o|`: for equal planes `n₁ · (o₂ - o₁) < tol` and the
///   normals differ by at most `√(2·1e-12)`, so the keys differ by at most
///   `tol + 1.5e-6 · reach`, `reach` the largest `|o|`;
/// - cylinders by radius, cones by apex `x`, spheres by centre `x`, each
///   within `tol` for equal surfaces.
struct SameIndex {
    maps: [BTreeMap<Key, Vec<usize>>; 6],
    plane_window: f64,
    tol: f64,
}

impl SameIndex {
    fn new(tol: f64, reach: f64) -> SameIndex {
        SameIndex {
            maps: Default::default(),
            plane_window: tol + 2e-6 * reach,
            tol,
        }
    }

    fn key(&self, s: &Surf) -> (usize, f64, f64) {
        match *s {
            Surf::Plane { o, n } => (0, n.dot(o).abs(), self.plane_window),
            Surf::Cyl { r, .. } => (1, r, self.tol),
            Surf::Cone { apex, .. } => (2, apex.x, self.tol),
            Surf::Sphere { c, .. } => (3, c.x, self.tol),
            Surf::Torus { c, .. } => (4, c.x, self.tol),
            // Equal patches have equal control points, the first within
            // the tolerance.
            Surf::Spline(ref s) => (5, s.public.control[0][0][0], self.tol),
        }
    }

    fn insert(&mut self, s: &Surf, class: usize) {
        let (m, k, _) = self.key(s);
        self.maps[m].entry(Key(k)).or_default().push(class);
    }

    /// The lowest-numbered class in `surf` that is the same surface as `e`.
    fn first_same(&self, surf: &[Surf], e: &Surf, tol: f64) -> Option<usize> {
        let (m, k, w) = self.key(e);
        // The window, widened by a few ulps so that rounding in the key
        // itself cannot drop a class at its edge.
        let w = w * (1.0 + 1e-9) + 4.0 * f64::EPSILON * k.abs();
        self.maps[m]
            .range(Key(k - w)..=Key(k + w))
            .flat_map(|(_, cs)| cs.iter().copied())
            .filter(|&c| surf[c].same(e, tol))
            .min()
    }
}

/// One boundary chain between two faces.
#[derive(Clone, Debug)]
struct Chain {
    /// The face whose half-edges the chain follows, and the face across.
    f: usize,
    g: usize,
    /// Mesh vertices along the chain.
    verts: Vec<u32>,
    /// Half-edges along the chain on `f`'s side.
    hes: Vec<usize>,
    closed: bool,
}

pub(crate) struct Built {
    pub topo: Topo,
    pub scale: f64,
    pub mesh_genus: i64,
    pub mesh_components: usize,
    pub max_vertex_residual: f64,
    pub max_chain_deviation: f64,
    pub tangencies: Vec<(u32, u32, Cont)>,
    /// Per face, its triangles' share of the mesh's signed volume.
    pub face_mesh_volume: Vec<f64>,
}

/// With `use_contacts` false, tangent pairs are not detected analytically (only
/// the tests turn it off, to reach the arc-merging path).
pub(crate) fn build(
    mesh: &TaggedMesh,
    opts: &Options,
    use_contacts: bool,
) -> Result<Built, Failure> {
    opts.poll()?;
    let nt = mesh.triangles.len();
    if nt == 0 {
        return Err(Error::InvalidInput("the mesh has no triangles".into()).into());
    }
    if mesh.triangle_surface.len() != nt {
        return Err(Error::InvalidInput(format!(
            "{} triangles but {} surface ids",
            nt,
            mesh.triangle_surface.len()
        ))
        .into());
    }
    let np = mesh.positions.len();
    let pos: Vec<V> = mesh.positions.iter().map(|&p| V::from(p)).collect();
    if let Some(i) = pos.iter().position(|p| !p.is_finite()) {
        return Err(Error::InvalidInput(format!("position {i} is not finite")).into());
    }
    let tris: Vec<[usize; 3]> = mesh
        .triangles
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    for (t, tri) in tris.iter().enumerate() {
        if tri.iter().any(|&i| i >= np) {
            return Err(Error::InvalidInput(format!(
                "triangle {t} indexes a position that does not exist"
            ))
            .into());
        }
        if tri[0] == tri[1] || tri[1] == tri[2] || tri[0] == tri[2] {
            return Err(Error::InvalidInput(format!("triangle {t} repeats a vertex")).into());
        }
    }
    let mut lo = pos[tris[0][0]];
    let mut hi = lo;
    for tri in &tris {
        for &i in tri {
            let p = pos[i];
            lo = v(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
            hi = v(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
        }
    }
    let scale = (hi.x - lo.x).max(hi.y - lo.y).max(hi.z - lo.z);
    if scale <= 0.0 || !scale.is_finite() {
        return Err(Error::InvalidInput("the mesh is flat".into()).into());
    }
    let tol = opts.tolerances.merge * scale;
    let fit_tol = opts.tolerances.fit;

    // Half-edge h = 3 t + k runs from tris[t][k] to tris[t][(k + 1) % 3].
    let twin = twins(&tris)?;
    // Features below the tolerance a boundary is checked to (corners
    // closer than it touch) cannot be written: clean them up first.
    let touch_tol = fit_tol.max(tol);
    if let Some(clean) = clean_mesh(mesh, touch_tol)? {
        // The rebuilt mesh numbers its triangles afresh; what it reports
        // goes back to this mesh's numbers.
        let back = |ts: &mut Vec<u32>| {
            for t in ts.iter_mut() {
                *t = clean.kept[*t as usize];
            }
        };
        let mut b = build(&clean.mesh, opts, use_contacts).map_err(|mut f| {
            back(&mut f.triangles);
            f
        })?;
        for f in &mut b.topo.faces {
            back(&mut f.source);
        }
        if clean.collapsed > 0 {
            b.topo.notes.push(format!(
                "{} mesh edge{} shorter than {touch_tol:.1e} collapsed",
                clean.collapsed,
                if clean.collapsed == 1 { "" } else { "s" }
            ));
        }
        if clean.flipped > 0 {
            b.topo.notes.push(format!(
                "{} needle triangle{} (narrower than {touch_tol:.1e}) flipped into {}",
                clean.flipped,
                if clean.flipped == 1 { "" } else { "s" },
                if clean.flipped == 1 {
                    "its neighbour"
                } else {
                    "their neighbours"
                }
            ));
        }
        return Ok(b);
    }
    let ends = |h: usize| (tris[h / 3][h % 3], tris[h / 3][(h % 3 + 1) % 3]);
    let mesh_genus_components = mesh_topology(&tris, np);

    // 1. Surface classes.
    let cls = classes(mesh, &pos, &tris, &twin, tol)?;
    let tcls = &cls.of_tri;
    opts.poll()?;

    // 2. Faces: connected triangles of one class.
    let mut uf = UnionFind::new(nt);
    for h in 0..3 * nt {
        if tcls[h / 3] == tcls[twin[h] / 3] {
            uf.join(h / 3, twin[h] / 3);
        }
    }
    let mut face_of_root: BTreeMap<usize, usize> = BTreeMap::new();
    let mut tface = vec![0usize; nt];
    for (t, tf) in tface.iter_mut().enumerate() {
        let r = uf.find(t);
        let n = face_of_root.len();
        *tf = *face_of_root.entry(r).or_insert(n);
    }
    let nf = face_of_root.len();
    let mut fcls = vec![0usize; nf];
    let mut fvote = vec![0.0f64; nf];
    // Each face's share of the mesh's signed volume (about the centre of
    // the bounding box, to keep the terms small): a shell whose faces sum
    // to a negative volume is a void.
    let mid = (lo + hi) * 0.5;
    let mut face_mesh_volume = vec![0.0f64; nf];
    for (t, &[a, b, c]) in tris.iter().enumerate() {
        let nrm = (pos[b] - pos[a]).cross(pos[c] - pos[a]);
        let cen = (pos[a] + pos[b] + pos[c]) * (1.0 / 3.0);
        fcls[tface[t]] = tcls[t];
        fvote[tface[t]] += nrm.dot(cls.surf[tcls[t]].grad(cen));
        face_mesh_volume[tface[t]] += (pos[a] - mid).dot((pos[b] - mid).cross(pos[c] - mid)) / 6.0;
    }

    // A face with no boundary is a whole mesh component on one surface.
    // On a sphere or a torus that is a lone one; on anything else it is flat: a
    // closed bubble of zero volume lying in one plane, which Manifold can
    // leave behind where coplanar cuts meet after a rotation (four
    // triangles on one plane in a rotated block cut by touching bars).
    // It has no exact counterpart, so it is dropped, and the mesh is
    // rebuilt without it so that its genus and components stay honest.
    {
        let mut bounded = vec![false; nf];
        for h in 0..3 * nt {
            if tface[twin[h] / 3] != tface[h / 3] {
                bounded[tface[h / 3]] = true;
            }
        }
        let flat: Vec<usize> = (0..nf)
            .filter(|&f| {
                !bounded[f]
                    && !matches!(cls.surf[fcls[f]], Surf::Sphere { .. } | Surf::Torus { .. })
            })
            .collect();
        let vol_tol = tol * scale * scale;
        if let Some(&f) = flat.iter().find(|&&f| face_mesh_volume[f].abs() > vol_tol) {
            return Err(Failure {
                error: Error::Reconstruction(format!(
                    "face {f} has no boundary but encloses volume {:.3e}",
                    face_mesh_volume[f]
                )),
                triangles: (0..nt)
                    .filter(|&t| tface[t] == f)
                    .map(|t| t as u32)
                    .collect(),
            });
        }
        // A closed component of two planar faces with no volume between
        // them is a bubble too: two nearly parallel planes facing apart
        // around one closed chain (Manifold's sliver where a BOSL2 `skin()`
        // meets a flush face, `skin__094`). Its faces have no width, which
        // the validator refuses.
        let mut comp = UnionFind::new(nf);
        for h in 0..3 * nt {
            comp.join(tface[h / 3], tface[twin[h] / 3]);
        }
        let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for f in 0..nf {
            members.entry(comp.find(f)).or_default().push(f);
        }
        let bubbles: Vec<usize> = members
            .values()
            .filter(|fs| {
                fs.len() == 2
                    && fs.iter().all(|&f| cls.surf[fcls[f]].is_plane())
                    && fs.iter().map(|&f| face_mesh_volume[f]).sum::<f64>().abs() <= vol_tol
            })
            .flatten()
            .copied()
            .collect();
        if !flat.is_empty() || !bubbles.is_empty() {
            let gone: Vec<bool> = (0..nf)
                .map(|f| flat.contains(&f) || bubbles.contains(&f))
                .collect();
            let mut keep = mesh.clone();
            let kept: Vec<usize> = (0..nt).filter(|&t| !gone[tface[t]]).collect();
            keep.triangles = kept.iter().map(|&t| mesh.triangles[t]).collect();
            keep.triangle_surface = kept.iter().map(|&t| mesh.triangle_surface[t]).collect();
            // The rebuilt mesh numbers its triangles afresh; what it
            // reports goes back to this mesh's numbers.
            let back = |ts: &mut Vec<u32>| {
                for t in ts.iter_mut() {
                    *t = kept[*t as usize] as u32;
                }
            };
            let mut b = build(&keep, opts, use_contacts).map_err(|mut f| {
                back(&mut f.triangles);
                f
            })?;
            for f in &mut b.topo.faces {
                back(&mut f.source);
            }
            if !flat.is_empty() {
                b.topo.notes.push(format!(
                    "dropped {} flat closed component{} (zero volume, one plane)",
                    flat.len(),
                    if flat.len() == 1 { "" } else { "s" }
                ));
            }
            if !bubbles.is_empty() {
                let n = bubbles.len() / 2;
                b.topo.notes.push(format!(
                    "dropped {n} closed component{} of two planar faces with no volume between them",
                    if n == 1 { "" } else { "s" }
                ));
            }
            return Ok(b);
        }
    }

    // 3. Faces around each mesh vertex.
    let mut vfaces: Vec<Vec<usize>> = vec![Vec::new(); np];
    for (t, tri) in tris.iter().enumerate() {
        for &p in tri {
            if !vfaces[p].contains(&tface[t]) {
                vfaces[p].push(tface[t]);
            }
        }
    }
    let classes_at = |p: usize| -> Vec<usize> {
        let mut c: Vec<usize> = vfaces[p].iter().map(|&f| fcls[f]).collect();
        c.sort_unstable();
        c.dedup();
        c
    };

    // 4. Analytic contacts between adjacent classes.
    let mut contacts: BTreeMap<(usize, usize), Option<Cont>> = BTreeMap::new();
    for h in 0..3 * nt {
        let (a, b) = (tcls[h / 3], tcls[twin[h] / 3]);
        if a < b {
            contacts.entry((a, b)).or_insert_with(|| {
                use_contacts
                    .then(|| contact(&cls.surf[a], &cls.surf[b], tol, opts.tolerances.surface_fit))
                    .flatten()
            });
        }
    }
    let contact_of = |a: usize, b: usize| -> Option<Cont> {
        let k = if a < b { (a, b) } else { (b, a) };
        contacts.get(&k).cloned().flatten()
    };
    let tangencies: Vec<(u32, u32, Cont)> = contacts
        .iter()
        .filter_map(|(&(a, b), c)| {
            let c = c.clone()?;
            Some((cls.input[a]?, cls.input[b]?, c))
        })
        .collect();

    // 5. Where two curved surfaces touch at a point, their intersection
    // curve crosses itself: a vertex although only two surfaces meet
    // there. Candidates are vertices on exactly two curved surfaces with
    // nearly parallel normals; the exact touching point is solved for, and
    // the nearest mesh vertex keeps it.
    //
    // Where the mesh has the crossing itself (four faces, two surfaces,
    // as two cones of equal angle crossing near their base circles give),
    // that vertex keeps it before any two-face one. Near a tangency the
    // mesh's crossing can be far off the exact one (0.9 mm on cones of
    // radius 5), and solving it onto the two surfaces only slides it
    // along their intersection: the curves on either side then fold back
    // on themselves (BOSL2 `distributors` examples).
    let mut tangent_at: Vec<Option<V>> = vec![None; np];
    {
        let mut best: Vec<(V, usize, (bool, f64))> = Vec::new();
        for p in 0..np {
            let fl = &vfaces[p];
            let (ca, cb) = match fl.len() {
                0 | 1 => continue,
                2 => (fcls[fl[0]], fcls[fl[1]]),
                _ => match classes_at(p)[..] {
                    [a, b] => (a, b),
                    _ => continue,
                },
            };
            if ca == cb {
                continue;
            }
            let (a, b) = (&cls.surf[ca], &cls.surf[cb]);
            if a.is_plane() && b.is_plane() {
                continue;
            }
            // Surfaces tangent along a whole curve have no crossing.
            if contact_of(ca, cb).is_some_and(|c| c.is_curve()) {
                continue;
            }
            let q = solve(&[a.clone(), b.clone()], pos[p]).0;
            if a.grad(q).cross(b.grad(q)).len() > 0.35 {
                continue;
            }
            if let Some(tp) = tangent_point(a, b, q, scale) {
                let dist = (tp - pos[p]).len();
                if dist > 0.05 * scale {
                    continue;
                }
                // Crossings in the mesh first, then the nearest.
                let d = (fl.len() == 2, dist);
                match best
                    .iter_mut()
                    .find(|(x, _, _)| (*x - tp).len() < 1e-7 * scale)
                {
                    Some(e) => {
                        if d < e.2 {
                            *e = (tp, p, d);
                        }
                    }
                    None => best.push((tp, p, d)),
                }
            }
        }
        for (tp, p, _) in best {
            tangent_at[p] = Some(tp);
        }
    }
    let is_corner = |p: usize| vfaces[p].len() >= 3 || tangent_at[p].is_some();
    let boundary = |h: usize| tface[twin[h] / 3] != tface[h / 3];
    let next_in_tri = |h: usize| 3 * (h / 3) + (h % 3 + 1) % 3;
    // The next boundary half-edge of the same face, leaving the end of h.
    let next_b = |h: usize| -> Result<usize, Failure> {
        let mut cur = next_in_tri(h);
        for _ in 0..3 * nt {
            if boundary(cur) {
                return Ok(cur);
            }
            cur = next_in_tri(twin[cur]);
        }
        Err(Failure {
            error: Error::Reconstruction("a vertex walk did not end".into()),
            triangles: vec![(h / 3) as u32],
        })
    };

    opts.poll()?;
    // 6. Chains.
    const NONE: usize = usize::MAX;
    let mut he_chain: Vec<(usize, bool)> = vec![(NONE, false); 3 * nt];
    let mut chains: Vec<Chain> = Vec::new();
    let bhe: Vec<usize> = (0..3 * nt).filter(|&h| boundary(h)).collect();
    let take = |start: usize,
                closed: bool,
                he_chain: &mut Vec<(usize, bool)>,
                chains: &mut Vec<Chain>|
     -> Result<(), Failure> {
        let f = tface[start / 3];
        let g = tface[twin[start] / 3];
        let id = chains.len();
        let mut verts = vec![ends(start).0 as u32];
        let mut hes = Vec::new();
        let mut cur = start;
        loop {
            he_chain[cur] = (id, true);
            he_chain[twin[cur]] = (id, false);
            hes.push(cur);
            let b = ends(cur).1;
            verts.push(b as u32);
            let nx = next_b(cur)?;
            if closed {
                if nx == start {
                    break;
                }
            } else if is_corner(b) {
                break;
            }
            if tface[twin[nx] / 3] != g {
                // The face across changed at a vertex that is not a
                // corner: a pinch. The chain ends there.
                break;
            }
            if he_chain[nx].0 != NONE {
                return Err(Failure {
                    error: Error::Reconstruction("a boundary chain ran into another".into()),
                    triangles: vec![(nx / 3) as u32, (twin[nx] / 3) as u32],
                });
            }
            cur = nx;
        }
        chains.push(Chain {
            f,
            g,
            verts,
            hes,
            closed,
        });
        Ok(())
    };
    for &h in &bhe {
        if he_chain[h].0 == NONE && is_corner(ends(h).0) {
            take(h, false, &mut he_chain, &mut chains)?;
        }
    }
    // Closed chains start at their lexicographically smallest vertex.
    let mut rest: Vec<usize> = bhe
        .iter()
        .copied()
        .filter(|&h| he_chain[h].0 == NONE)
        .collect();
    rest.sort_by(|&a, &b| pos[ends(a).0].lex_cmp(pos[ends(b).0]).then(a.cmp(&b)));
    for h in rest {
        if he_chain[h].0 == NONE {
            take(h, true, &mut he_chain, &mut chains)?;
        }
    }

    // 7. Exact vertices, one per corner mesh vertex (or per closed chain).
    let mut topo = Topo::default();
    let mut vmap: BTreeMap<usize, usize> = BTreeMap::new();
    let mut max_res = 0.0f64;
    let mut vertex_for = |p: usize, cl: &[usize], topo: &mut Topo| -> usize {
        if let Some(&i) = vmap.get(&p) {
            return i;
        }
        let all;
        let cl = if cl.is_empty() {
            all = classes_at(p);
            &all[..]
        } else {
            cl
        };
        let (q, res) = place_vertex(p, cl, &cls, &pos, tangent_at[p], &contact_of);
        max_res = max_res.max(res);
        let i = topo.add_vertex(q);
        vmap.insert(p, i);
        i
    };
    let mut built: Vec<Option<TEdge>> = Vec::with_capacity(chains.len());
    let mut max_chain_dev = 0.0f64;
    for ch in &chains {
        let e = chain_edge(
            ch,
            &cls,
            &fcls,
            &pos,
            &contact_of,
            &mut topo,
            &mut vertex_for,
            fit_tol,
        );
        built.push(Some(e));
    }

    // 8. Arc merging: a two-face vertex that splits one curve between the
    // same two faces (a tangent-crossing candidate that was not needed)
    // joins the two edges again.
    let mut alive: Vec<bool> = vec![true; chains.len()];
    let mut n_merged = 0usize;
    loop {
        let mut merged = false;
        // Ends per mesh vertex.
        let mut at: BTreeMap<u32, Vec<(usize, bool)>> = BTreeMap::new();
        for (i, ch) in chains.iter().enumerate() {
            if !alive[i] || ch.closed {
                continue;
            }
            at.entry(ch.verts[0]).or_default().push((i, true));
            at.entry(*ch.verts.last().expect("chain"))
                .or_default()
                .push((i, false));
        }
        for (&p, uses) in &at {
            if uses.len() != 2 || vfaces[p as usize].len() != 2 {
                continue;
            }
            let ((i, i_start), (j, j_start)) = (uses[0], uses[1]);
            if i == j {
                continue;
            }
            let (ei, ej) = (
                built[i].as_ref().expect("edge"),
                built[j].as_ref().expect("edge"),
            );
            if !curve::same_carrier(&ei.curve, &ej.curve, tol) {
                continue;
            }
            // Orient both to run through p: first ends at p, second starts.
            let (first, second) = if !i_start {
                (i, j)
            } else if !j_start {
                (j, i)
            } else {
                (i, j)
            };
            let mut a = chains[first].clone();
            let mut b = chains[second].clone();
            if *a.verts.last().expect("chain") != p {
                reverse_chain(&mut a, &twin);
            }
            if b.verts[0] != p {
                reverse_chain(&mut b, &twin);
            }
            if a.f != b.f {
                continue;
            }
            a.verts.extend_from_slice(&b.verts[1..]);
            a.hes.extend_from_slice(&b.hes);
            a.closed = a.verts[0] == *a.verts.last().expect("chain");
            alive[second] = false;
            built[second] = None;
            for &h in &a.hes {
                he_chain[h] = (first, true);
                he_chain[twin[h]] = (first, false);
            }
            chains[first] = a;
            let e = chain_edge(
                &chains[first],
                &cls,
                &fcls,
                &pos,
                &contact_of,
                &mut topo,
                &mut vertex_for,
                fit_tol,
            );
            built[first] = Some(e);
            merged = true;
            n_merged += 1;
            break;
        }
        if !merged {
            break;
        }
    }
    let mut chain_devs = vec![0.0f64; chains.len()];
    for (i, ch) in chains.iter().enumerate() {
        let (ca, cb) = (fcls[ch.f], fcls[ch.g]);
        // Only chains with a curved side count, and not those between
        // surfaces that touch. Between two planes the mesh's vertices are
        // on both already, except where the planes are nearly parallel
        // (flush faces a rotation's rounding left apart), whose line is
        // ill-defined; along a contact the mesh may cross, touch or miss
        // wherever its polygon vertices fall (a capsule's chain strays 5
        // sagittas from its circle). Neither says anything about the
        // topology.
        let curved = !cls.surf[ca].is_plane() || !cls.surf[cb].is_plane();
        // Nor those ending at a point where two surfaces touch (two
        // cones crossing there): the mesh's crossing near it is as
        // ill-conditioned as a contact curve.
        let at_touch = [ch.verts[0], *ch.verts.last().expect("chain")]
            .iter()
            .any(|&p| tangent_at[p as usize].is_some());
        if alive[i] && curved && contact_of(ca, cb).is_none() && !at_touch {
            let pts: Vec<V> = ch.verts.iter().map(|&p| pos[p as usize]).collect();
            let (sa, sb) = (&cls.surf[ca], &cls.surf[cb]);
            let d = edges::chain_deviation(sa, sb, &pts);
            // Measured across the surfaces, not along them: where they
            // meet at a grazing angle θ the mesh's crossing slides along
            // them by its sagitta / sin θ (a cylinder poking 0.01 mm
            // through a face meets it at 2.6°), which is the tessellation
            // and not the topology.
            let q = solve(&[sa.clone(), sb.clone()], pts[pts.len() / 2]).0;
            let sin = sa.grad(q).cross(sb.grad(q)).len();
            chain_devs[i] = d * sin.clamp(0.02, 1.0);
            max_chain_dev = max_chain_dev.max(chain_devs[i]);
        }
    }

    // Number the surviving chains as edges.
    let mut edge_of_chain = vec![NONE; chains.len()];
    for (i, b) in built.into_iter().enumerate() {
        if let Some(mut e) = b {
            e.chain_dev = chain_devs[i];
            edge_of_chain[i] = topo.edges.len();
            topo.edges.push(e);
        }
    }

    opts.poll()?;
    // 9. Loops: walk each face's boundary half-edges.
    topo.faces = (0..nf)
        .map(|f| TFace {
            surf: cls.surf[fcls[f]].clone(),
            same_sense: fvote[f] > 0.0,
            faceted: cls.faceted[fcls[f]],
            loops: Vec::new(),
            param: None,
            pcurves: Vec::new(),
            outer: Vec::new(),
            tris: Vec::new(),
            source: Vec::new(),
        })
        .collect();
    for (t, &[a, b, c]) in tris.iter().enumerate() {
        let f = tface[t];
        topo.faces[f].source.push(t as u32);
        if matches!(topo.faces[f].surf, Surf::Torus { .. }) {
            topo.faces[f].tris.push([pos[a], pos[b], pos[c]]);
        }
    }
    // A half-edge starts its chain (in its own direction) when it is the
    // chain's first half-edge (forward) or the twin of its last (reverse).
    let starts_chain = |h: usize, he_chain: &[(usize, bool)]| {
        let (c, fwd) = he_chain[h];
        let ch = &chains[c];
        if fwd {
            ch.hes[0] == h
        } else {
            twin[*ch.hes.last().expect("chain")] == h
        }
    };
    let mut seen = vec![false; 3 * nt];
    for &h in &bhe {
        if seen[h] || !starts_chain(h, &he_chain) {
            continue;
        }
        let f = tface[h / 3];
        let mut lp = Vec::new();
        let mut cur = h;
        loop {
            seen[cur] = true;
            if starts_chain(cur, &he_chain) {
                let (c, fwd) = he_chain[cur];
                lp.push((edge_of_chain[c], fwd));
            }
            cur = next_b(cur)?;
            if cur == h {
                break;
            }
            if seen[cur] {
                return Err(Failure {
                    error: Error::Reconstruction("a loop walk revisited a half-edge".into()),
                    triangles: vec![(cur / 3) as u32, (twin[cur] / 3) as u32],
                });
            }
        }
        topo.faces[f].loops.push(lp);
    }
    for (f, face) in topo.faces.iter().enumerate() {
        // A face with no boundary covers a whole closed component by
        // itself (a lone sphere or torus): it gets seams later, from
        // nothing.
        if face.loops.is_empty() && !matches!(face.surf, Surf::Sphere { .. } | Surf::Torus { .. }) {
            return Err(Failure {
                error: Error::Reconstruction(format!("face {f} has no boundary")),
                triangles: face.source.clone(),
            });
        }
    }
    for e in &mut topo.edges {
        // Record which faces each edge separates (forward user first).
        e.faces = [NONE, NONE];
    }
    for (fi, f) in topo.faces.iter().enumerate() {
        for lp in &f.loops {
            for &(e, fwd) in lp {
                topo.edges[e].faces[if fwd { 0 } else { 1 }] = fi;
            }
        }
    }
    if let Some(e) = topo.edges.iter().find(|e| e.faces.contains(&NONE)) {
        let f = e.faces.iter().copied().find(|&f| f != NONE);
        return Err(Failure {
            error: Error::Reconstruction("an edge is not used by two faces".into()),
            triangles: f.map(|f| topo.faces[f].source.clone()).unwrap_or_default(),
        });
    }
    if n_merged > 0 {
        topo.notes.push(format!(
            "{n_merged} edges split at unneeded vertices merged again"
        ));
    }
    let short = topo.collapse_short_edges(tol);
    if short > 0 {
        topo.notes.push(format!(
            "{short} edges shorter than the merge tolerance collapsed (corners the mesh split)"
        ));
    }
    let empty = topo.drop_empty_faces();
    if !empty.is_empty() {
        let mut k = 0;
        face_mesh_volume.retain(|_| {
            let keep = empty.binary_search(&k).is_err();
            k += 1;
            keep
        });
        topo.notes.push(format!(
            "{} face{} of no area (every edge shorter than the merge tolerance) removed",
            empty.len(),
            if empty.len() == 1 { "" } else { "s" }
        ));
    }
    let digons = topo.drop_digons(tol);
    if !digons.is_empty() {
        let mut k = 0;
        face_mesh_volume.retain(|_| {
            let keep = digons.binary_search(&k).is_err();
            k += 1;
            keep
        });
        topo.notes.push(format!(
            "{} face{} of no area (two edges along one line or curve) removed",
            digons.len(),
            if digons.len() == 1 { "" } else { "s" }
        ));
    }
    let n_tangent = tangent_at.iter().filter(|t| t.is_some()).count();
    if n_tangent > 0 {
        topo.notes.push(format!(
            "{n_tangent} tangent points split intersection curves"
        ));
    }
    Ok(Built {
        topo,
        scale,
        mesh_genus: mesh_genus_components.0,
        mesh_components: mesh_genus_components.1,
        max_vertex_residual: max_res,
        max_chain_deviation: max_chain_dev,
        tangencies,
        face_mesh_volume,
    })
}

/// The edge of one chain: its vertices (solved, or reused) and curve.
#[allow(clippy::too_many_arguments)]
fn chain_edge(
    ch: &Chain,
    cls: &Classes,
    fcls: &[usize],
    pos: &[V],
    contact_of: &dyn Fn(usize, usize) -> Option<Cont>,
    topo: &mut Topo,
    vertex_for: &mut dyn FnMut(usize, &[usize], &mut Topo) -> usize,
    fit_tol: f64,
) -> TEdge {
    let (ca, cb) = (fcls[ch.f], fcls[ch.g]);
    let (sa, sb) = (&cls.surf[ca], &cls.surf[cb]);
    let first = ch.verts[0] as usize;
    let last = *ch.verts.last().expect("chain") as usize;
    let (v0, v1) = if ch.closed {
        let mut pair = [ca, cb];
        pair.sort_unstable();
        let i = vertex_for(first, &pair, topo);
        (i, i)
    } else {
        // Corners are solved on every class around them.
        (vertex_for(first, &[], topo), vertex_for(last, &[], topo))
    };
    let pts: Vec<V> = ch.verts.iter().map(|&p| pos[p as usize]).collect();
    let (p0, p1) = (topo.verts[v0], topo.verts[v1]);
    let both_faceted = cls.faceted[ca] && cls.faceted[cb];
    let b = if both_faceted && !ch.closed {
        // Two faceted planes: the straight segment between the vertices,
        // which lie on both by construction.
        let l = (p1 - p0).len();
        edges::Built {
            curve: crate::model::Curve::Line {
                origin: p0.arr(),
                direction: (p1 - p0).norm().arr(),
            },
            range: [0.0, l],
            dev: 0.0,
        }
    } else {
        make_edge(sa, sb, contact_of(ca, cb), p0, p1, &pts, ch.closed, fit_tol)
    };
    TEdge {
        v0,
        v1,
        curve: b.curve,
        range: b.range,
        faces: [ch.f, ch.g],
        chain: pts,
        seam: false,
        dev: b.dev,
        chain_dev: 0.0,
    }
}

/// The exact position of a corner: on every class around the mesh vertex
/// (`cl` empty means "all of them"), with analytically tangent pairs
/// replaced by their contact set, and kept at the mesh position when every
/// class there is a faceted plane (those planes came from the mesh).
fn place_vertex(
    p: usize,
    cl: &[usize],
    cls: &Classes,
    pos: &[V],
    tangent: Option<V>,
    contact_of: &dyn Fn(usize, usize) -> Option<Cont>,
) -> (V, f64) {
    let residual = |q: V| {
        cl.iter()
            .fold(0.0f64, |m, &c| m.max(cls.surf[c].f(q).abs()))
    };
    if let Some(tp) = tangent {
        return (tp, residual(tp));
    }
    if cl.iter().all(|&c| cls.faceted[c]) {
        return (pos[p], residual(pos[p]));
    }
    let mut taken = vec![false; cl.len()];
    let mut cons: Vec<Surf> = Vec::new();
    let mut sides: Vec<(std::sync::Arc<Spline>, Side)> = Vec::new();
    for i in 0..cl.len() {
        for j in i + 1..cl.len() {
            if taken[i] || taken[j] {
                continue;
            }
            if let Some(c) = contact_of(cl[i], cl[j]) {
                match c.nearest_side(pos[p]) {
                    Some((s, sd)) => sides.push((s.clone(), sd)),
                    None => cons.extend(c.constraints()),
                }
                taken[i] = true;
                taken[j] = true;
            }
        }
    }
    for (i, &c) in cl.iter().enumerate() {
        if !taken[i] {
            cons.push(cls.surf[c].clone());
        }
    }
    if let Some(q) = on_sides(&sides, &mut cons, pos[p]) {
        return (q, residual(q));
    }
    let q = solve(&cons, pos[p]).0;
    (q, residual(q))
}

/// A vertex on a side of a B-spline patch that touches another surface
/// (a blend's contact): solved along that side, not on the two surfaces,
/// which touch at a grazing angle (and, where the patch was fitted, only
/// to within the fit). Two sides of one patch meet at its corner; a side
/// of another patch adds that patch as a surface to meet. Along the side
/// the point is the one nearest `p0` that meets the other surfaces:
/// Gauss–Newton in the side's parameter. `None` with no side.
fn on_sides(sides: &[(std::sync::Arc<Spline>, Side)], cons: &mut Vec<Surf>, p0: V) -> Option<V> {
    let (s, sd) = sides.first()?.clone();
    for (t, td) in &sides[1..] {
        if (std::sync::Arc::ptr_eq(t, &s) || t.same(&s, 0.0)) && td.fixed_u != sd.fixed_u {
            let (u, v) = if sd.fixed_u {
                (sd.at, td.at)
            } else {
                (td.at, sd.at)
            };
            return Some(s.eval(u, v));
        }
        cons.push(Surf::Spline(t.clone()));
    }
    let (u, v) = s.project(p0, false);
    let mut t = if sd.fixed_u { v } else { u };
    let [lo, hi] = s.iso_range(sd.fixed_u);
    let (lo, hi) = (lo - (hi - lo), hi + (hi - lo));
    for _ in 0..60 {
        let (q, dq) = s.iso(sd.fixed_u, sd.at, t);
        let (mut num, mut den) = (0.0, 0.0);
        for c in cons.iter() {
            let g = c.grad(q).dot(dq);
            num += c.f(q) * g;
            den += g * g;
        }
        if den <= 0.0 || den.is_nan() {
            break;
        }
        let step = num / den;
        let nt = (t - step).clamp(lo, hi);
        let moved = ((nt - t) * dq.len()).abs();
        t = nt;
        if moved <= 1e-15 * (1.0 + q.len()) || moved.is_nan() {
            break;
        }
    }
    Some(s.iso(sd.fixed_u, sd.at, t).0)
}

fn reverse_chain(c: &mut Chain, twin: &[usize]) {
    std::mem::swap(&mut c.f, &mut c.g);
    c.verts.reverse();
    c.hes = c.hes.iter().rev().map(|&h| twin[h]).collect();
}

/// Half-edge twins, or an error if the mesh is not a 2-manifold.
fn twins(tris: &[[usize; 3]]) -> Result<Vec<usize>, Error> {
    let n = 3 * tris.len();
    let mut keys: Vec<(usize, usize, usize)> = Vec::with_capacity(n);
    for (t, tri) in tris.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            keys.push((a.min(b), a.max(b), 3 * t + k));
        }
    }
    keys.sort_unstable();
    let mut twin = vec![usize::MAX; n];
    let mut i = 0;
    while i < n {
        let j = i + 1;
        if j >= n || keys[j].0 != keys[i].0 || keys[j].1 != keys[i].1 {
            return Err(Error::NotManifold(
                "an edge belongs to only one triangle (the mesh is not closed)".into(),
            ));
        }
        if j + 1 < n && keys[j + 1].0 == keys[i].0 && keys[j + 1].1 == keys[i].1 {
            return Err(Error::NotManifold(
                "an edge belongs to more than two triangles".into(),
            ));
        }
        let (h1, h2) = (keys[i].2, keys[j].2);
        let start = |h: usize| tris[h / 3][h % 3];
        if start(h1) == start(h2) {
            return Err(Error::NotManifold(
                "two triangles sharing an edge are oriented inconsistently".into(),
            ));
        }
        twin[h1] = h2;
        twin[h2] = h1;
        i += 2;
    }
    Ok(twin)
}

/// A mesh cleaned of features below the touching tolerance
/// ([`clean_mesh`]).
struct Clean {
    mesh: TaggedMesh,
    /// Per triangle of `mesh`, its number in the mesh cleaned.
    kept: Vec<u32>,
    collapsed: usize,
    flipped: usize,
}

/// Removes what the mesh has below `tol` (the distance at which the
/// validator calls two parts of a boundary touching), keeping it a closed
/// 2-manifold: edges shorter than `tol` are collapsed, and needle
/// triangles (a corner within `tol` of the opposite edge) have that edge
/// flipped. `None` if there was nothing to do.
///
/// Manifold leaves both where faces meet at a tangency or are flush from
/// different chains of transforms. A short edge (two corners 3e-8 apart
/// at a BOSL2 `stroke()` joint, `rounding__035`) sits between the merge
/// tolerance, below which reconstruction collapses edges itself, and the
/// touching tolerance, so its two corners became two exact vertices that
/// the boundary check then found touching. A needle (a wall's corner
/// 2.1e-9 inside the edge of the face it meets, `hinges__015`) is in the
/// face whose edge it lies along, so that face's boundary runs along the
/// edge and also through the corner on it, which no exact face can do.
/// Flipping changes nothing in space; a collapse moves one corner by less
/// than `tol`, below the precision written to the file.
///
/// A collapse keeps the lower-numbered vertex, and is made only where it
/// keeps the mesh a manifold (the two vertices share exactly the two
/// neighbours across the edge) and turns no triangle over. A needle's
/// flip is made only when neither new triangle is a needle and the edge
/// it makes is new. Changes are made in rounds of independent ones until
/// none is left; collapses lower the vertex count and flips the needle
/// count, so the rounds end.
fn clean_mesh(mesh: &TaggedMesh, tol: f64) -> Result<Option<Clean>, Failure> {
    let pos: Vec<V> = mesh.positions.iter().map(|&p| V::from(p)).collect();
    let mut tris: Vec<[usize; 3]> = mesh
        .triangles
        .iter()
        .map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    let mut surf = mesh.triangle_surface.clone();
    // Per current triangle, its number in the input.
    let mut kept: Vec<u32> = (0..tris.len() as u32).collect();
    let (mut collapsed, mut flipped) = (0usize, 0usize);
    let thin = |p: V, q: V, r: V| {
        let longest = (q - p).len().max((r - q).len()).max((p - r).len());
        (q - p).cross(r - p).len() / longest.max(1e-300) < tol
    };
    for _ in 0..1000 {
        let mut changed = false;
        // Collapses.
        let mut short: Vec<(f64, usize, usize)> = Vec::new();
        for t in &tris {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                if a < b {
                    let l = (pos[b] - pos[a]).len();
                    if l < tol {
                        short.push((l, a, b));
                    }
                }
            }
        }
        if !short.is_empty() {
            short.sort_by(|x, y| x.0.total_cmp(&y.0).then((x.1, x.2).cmp(&(y.1, y.2))));
            let mut around: Vec<Vec<usize>> = vec![Vec::new(); pos.len()];
            for (i, t) in tris.iter().enumerate() {
                for &p in t {
                    around[p].push(i);
                }
            }
            let mut busy = vec![false; pos.len()];
            let mut gone_tri = vec![false; tris.len()];
            let mut to = vec![usize::MAX; pos.len()];
            for &(_, a, b) in &short {
                if busy[a] || busy[b] {
                    continue;
                }
                let ring = |p: usize| -> Vec<usize> {
                    let mut r: Vec<usize> = around[p]
                        .iter()
                        .flat_map(|&i| tris[i])
                        .filter(|&q| q != p)
                        .collect();
                    r.sort_unstable();
                    r.dedup();
                    r
                };
                let (ra, rb) = (ring(a), ring(b));
                let common: Vec<usize> = ra.iter().copied().filter(|q| rb.contains(q)).collect();
                let shared: Vec<usize> = around[b]
                    .iter()
                    .copied()
                    .filter(|&i| tris[i].contains(&a))
                    .collect();
                if shared.len() != 2 || common.len() != 2 {
                    continue;
                }
                // No triangle that keeps its area may turn over.
                let turns = around[b].iter().any(|&i| {
                    if shared.contains(&i) {
                        return false;
                    }
                    let [p, q, r] = tris[i].map(|x| pos[x]);
                    let before = (q - p).cross(r - p);
                    let moved = tris[i].map(|x| if x == b { pos[a] } else { pos[x] });
                    let after = (moved[1] - moved[0]).cross(moved[2] - moved[0]);
                    before.len() > tol * tol && before.dot(after) <= 0.0
                });
                if turns {
                    continue;
                }
                for &p in ra.iter().chain(&rb) {
                    busy[p] = true;
                }
                busy[a] = true;
                busy[b] = true;
                for &i in &shared {
                    gone_tri[i] = true;
                }
                to[b] = a;
                collapsed += 1;
                changed = true;
            }
            if changed {
                let mut nt = Vec::with_capacity(tris.len());
                let mut ns = Vec::with_capacity(tris.len());
                let mut nk = Vec::with_capacity(tris.len());
                for (i, t) in tris.iter().enumerate() {
                    if gone_tri[i] {
                        continue;
                    }
                    nt.push(t.map(|p| if to[p] != usize::MAX { to[p] } else { p }));
                    ns.push(surf[i]);
                    nk.push(kept[i]);
                }
                tris = nt;
                surf = ns;
                kept = nk;
                continue;
            }
        }
        // Needle flips. Finding the needles is a scan; the half-edge
        // structure is built only for a mesh that has some.
        let needles: Vec<(usize, usize)> = (0..tris.len())
            .filter_map(|t| {
                let tri = tris[t];
                let len = |k: usize| (pos[tri[(k + 1) % 3]] - pos[tri[k]]).len();
                let k = (0..3).max_by(|&i, &j| len(i).total_cmp(&len(j)).then(j.cmp(&i)))?;
                let (a, b, v) = (tri[k], tri[(k + 1) % 3], tri[(k + 2) % 3]);
                thin(pos[a], pos[b], pos[v]).then_some((t, k))
            })
            .collect();
        if needles.is_empty() {
            break;
        }
        let twin = twins(&tris)?;
        let mut edges: std::collections::BTreeSet<(usize, usize)> = tris
            .iter()
            .flat_map(|t| (0..3).map(move |k| (t[k].min(t[(k + 1) % 3]), t[k].max(t[(k + 1) % 3]))))
            .collect();
        let mut touched = vec![false; tris.len()];
        for (t, k) in needles {
            let tri = tris[t];
            let (a, b, v) = (tri[k], tri[(k + 1) % 3], tri[(k + 2) % 3]);
            let h = 3 * t + k;
            let u = twin[h] / 3;
            if touched[t] || touched[u] || u == t {
                continue;
            }
            // The neighbour's half-edge runs b -> a; its third corner
            // follows.
            let w = tris[u][(twin[h] % 3 + 2) % 3];
            if w == v || edges.contains(&(v.min(w), v.max(w))) {
                continue;
            }
            if thin(pos[b], pos[a], pos[w])
                || thin(pos[a], pos[w], pos[v])
                || thin(pos[w], pos[b], pos[v])
            {
                continue;
            }
            tris[t] = [a, w, v];
            tris[u] = [w, b, v];
            // The pair is the neighbour's: the needle had no area.
            surf[t] = surf[u];
            edges.remove(&(a.min(b), a.max(b)));
            edges.insert((v.min(w), v.max(w)));
            touched[t] = true;
            touched[u] = true;
            flipped += 1;
            changed = true;
        }
        if !changed {
            break;
        }
    }
    if collapsed == 0 && flipped == 0 {
        return Ok(None);
    }
    let mut out = mesh.clone();
    out.triangles = tris.iter().map(|t| t.map(|p| p as u32)).collect();
    out.triangle_surface = surf;
    Ok(Some(Clean {
        mesh: out,
        kept,
        collapsed,
        flipped,
    }))
}

/// The mesh's genus (summed over components) and component count, from
/// its Euler characteristic: χ = V − E + F = 2 (C − g).
fn mesh_topology(tris: &[[usize; 3]], np: usize) -> (i64, usize) {
    let mut uf = UnionFind::new(np);
    let mut used = vec![false; np];
    for tri in tris {
        for &i in tri {
            used[i] = true;
        }
        uf.join(tri[0], tri[1]);
        uf.join(tri[1], tri[2]);
    }
    let nv = used.iter().filter(|&&u| u).count() as i64;
    let mut comps = 0usize;
    for (i, &u) in used.iter().enumerate() {
        if u && uf.find(i) == i {
            comps += 1;
        }
    }
    let ne = (3 * tris.len() / 2) as i64;
    let chi = nv - ne + tris.len() as i64;
    (comps as i64 - chi / 2, comps)
}

/// Assigns each triangle a surface class.
///
/// Exact surfaces that are geometrically equal share a class (two cubes
/// on one plate give one top face). Faceted triangles get a plane each,
/// and adjacent faceted triangles that are coplanar share a class, as do
/// faceted triangles lying in an adjacent exact plane.
fn classes(
    mesh: &TaggedMesh,
    pos: &[V],
    tris: &[[usize; 3]],
    twin: &[usize],
    tol: f64,
) -> Result<Classes, Error> {
    let nt = tris.len();
    let ns = mesh.surfaces.len();
    let mut exact: Vec<Option<Surf>> = Vec::with_capacity(ns);
    for (i, s) in mesh.surfaces.iter().enumerate() {
        let e = Surf::from_public(s);
        if let Some(e) = &e
            && !e.well_formed()
        {
            return Err(Error::InvalidInput(format!(
                "surface {i} ({}) is malformed",
                s.kind()
            )));
        }
        exact.push(e);
    }
    let mut used = vec![false; ns];
    for (t, &s) in mesh.triangle_surface.iter().enumerate() {
        let s = s as usize;
        if s >= ns {
            return Err(Error::InvalidInput(format!(
                "triangle {t} names surface {s}, but there are {ns}"
            )));
        }
        if exact[s].is_none() && !matches!(mesh.surfaces[s], Surface::Faceted) {
            return Err(Error::Unsupported(mesh.surfaces[s].kind()));
        }
        used[s] = true;
    }
    // Exact classes: the first used record of each geometric surface.
    let mut class_of_rec = vec![usize::MAX; ns];
    let mut surf: Vec<Surf> = Vec::new();
    let mut input: Vec<Option<u32>> = Vec::new();
    let reach = exact
        .iter()
        .flatten()
        .map(|s| s.key_point().len())
        .fold(0.0, f64::max);
    let mut index = SameIndex::new(tol, reach);
    for s in 0..ns {
        let Some(e) = exact[s].clone() else { continue };
        if !used[s] {
            continue;
        }
        let c = index.first_same(&surf, &e, tol);
        if c.is_none() {
            index.insert(&e, surf.len());
        }
        class_of_rec[s] = c.unwrap_or_else(|| {
            surf.push(e);
            input.push(Some(s as u32));
            surf.len() - 1
        });
    }
    let n_exact = surf.len();
    let mut faceted = vec![false; n_exact];
    let mut of_tri = vec![usize::MAX; nt];
    let is_faceted = |t: usize| exact[mesh.triangle_surface[t] as usize].is_none();
    for t in 0..nt {
        if !is_faceted(t) {
            of_tri[t] = class_of_rec[mesh.triangle_surface[t] as usize];
        }
    }
    // Faceted triangles: a plane each (or none if degenerate), joined with
    // coplanar faceted neighbours.
    let tri_plane = |t: usize| -> Option<(Surf, f64)> {
        let [a, b, c] = tris[t];
        let n = (pos[b] - pos[a]).cross(pos[c] - pos[a]);
        let area2 = n.len();
        // Twice the area; a triangle thinner than the tolerance has no
        // trustworthy normal.
        (area2 > tol * tol).then(|| {
            (
                Surf::Plane {
                    o: pos[a],
                    n: n.norm(),
                },
                area2,
            )
        })
    };
    let planes: Vec<Option<(Surf, f64)>> = (0..nt)
        .map(|t| if is_faceted(t) { tri_plane(t) } else { None })
        .collect();
    let on_plane = |s: &Surf, t: usize| -> bool {
        let Surf::Plane { o, n } = *s else {
            return false;
        };
        tris[t].iter().all(|&i| (pos[i] - o).dot(n).abs() < tol)
    };
    let coplanar = |t: usize, u: usize| -> bool {
        match (&planes[t], &planes[u]) {
            (Some((pt, _)), Some((pu, _))) => {
                let (Surf::Plane { n: nt_, .. }, Surf::Plane { n: nu, .. }) = (pt, pu) else {
                    return false;
                };
                nt_.dot(*nu) > 0.0 && on_plane(pt, u) && on_plane(pu, t)
            }
            _ => false,
        }
    };
    let mut uf = UnionFind::new(nt);
    for h in 0..3 * nt {
        let (t, u) = (h / 3, twin[h] / 3);
        if t < u && is_faceted(t) && is_faceted(u) && coplanar(t, u) {
            uf.join(t, u);
        }
    }
    // Degenerate faceted triangles join a faceted neighbour; a run of
    // them joins through each other, so this repeats until nothing joins
    // (each pass joins at least one more, or stops).
    let mut has_plane: Vec<bool> = planes.iter().map(Option::is_some).collect();
    loop {
        // Each pass joins only to triangles that had a plane (or had
        // joined one) before it, so the first pass is the single pass
        // this always was, and the groups of any mesh it settled are
        // unchanged.
        let before = has_plane.clone();
        let mut joined = false;
        for t in 0..nt {
            if is_faceted(t)
                && !before[t]
                && let Some(u) = (0..3)
                    .map(|k| twin[3 * t + k] / 3)
                    .find(|&u| is_faceted(u) && before[u])
            {
                uf.join(t, u);
                has_plane[t] = true;
                joined = true;
            }
        }
        if !joined {
            break;
        }
    }
    // Each group's plane: its largest triangle's.
    let mut group_best: BTreeMap<usize, (f64, usize)> = BTreeMap::new();
    for t in 0..nt {
        if let Some((_, a)) = planes[t] {
            let r = uf.find(t);
            let e = group_best.entry(r).or_insert((a, t));
            if a > e.0 {
                *e = (a, t);
            }
        }
    }
    // A faceted group lying in an adjacent exact plane joins it: the
    // first such neighbour in triangle order.
    let mut joins: BTreeMap<usize, usize> = BTreeMap::new();
    for t in 0..nt {
        if !is_faceted(t) {
            continue;
        }
        let r = uf.find(t);
        let Some(&(_, bt)) = group_best.get(&r) else {
            continue;
        };
        if joins.contains_key(&r) {
            continue;
        }
        let plane = planes[bt].as_ref().expect("plane").0.clone();
        for k in 0..3 {
            let w = twin[3 * t + k] / 3;
            if !is_faceted(w) && surf[of_tri[w]].same(&plane, tol) {
                joins.insert(r, of_tri[w]);
                break;
            }
        }
    }
    let mut class_of_group: BTreeMap<usize, usize> = BTreeMap::new();
    for t in 0..nt {
        if !is_faceted(t) {
            continue;
        }
        let r = uf.find(t);
        if let Some(&c) = class_of_group.get(&r) {
            of_tri[t] = c;
            continue;
        }
        let Some(&(_, bt)) = group_best.get(&r) else {
            // Slivers of no area between exact faces (a faceted mesh's
            // edge snapped onto an exact one): they take an exact
            // neighbour's surface; with no area they add nothing to it.
            let Some(w) = (0..3)
                .map(|k| twin[3 * t + k] / 3)
                .find(|&w| !is_faceted(w))
            else {
                return Err(Error::InvalidInput(
                    "a faceted region has only degenerate triangles".into(),
                ));
            };
            of_tri[t] = of_tri[w];
            continue;
        };
        let c = match joins.get(&r) {
            Some(&c) => c,
            None => {
                surf.push(planes[bt].as_ref().expect("plane").0.clone());
                faceted.push(true);
                input.push(None);
                surf.len() - 1
            }
        };
        class_of_group.insert(r, c);
        of_tri[t] = c;
    }
    Ok(Classes {
        of_tri,
        surf,
        faceted,
        input,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The index answers exactly as the pairwise scan it replaced: the
    /// lowest-numbered equal class, over surfaces with near-duplicates
    /// (rotated copies rounded differently, the same plane from far away).
    #[test]
    fn same_index_matches_the_scan() {
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let tol = 1e-9 * 200.0;
        let mut recs = Vec::new();
        for i in 0..3000 {
            let pick = |r: f64, k: usize| (r * k as f64).floor();
            let n = v(rnd() - 0.5, rnd() - 0.5, rnd() - 0.5).norm();
            let n = if i % 3 == 0 { v(0.0, 0.0, 1.0) } else { n };
            let o = v(pick(rnd(), 5) * 10.0, pick(rnd(), 5), 100.0 * rnd());
            let jitter = v(rnd(), rnd(), rnd()) * (tol * 0.3);
            recs.push(match i % 4 {
                0 => Surf::Plane { o: o + jitter, n },
                1 => Surf::Cyl {
                    o: o + jitter,
                    a: n,
                    r: 1.0 + pick(rnd(), 3),
                },
                2 => Surf::Sphere {
                    c: v(pick(rnd(), 3), 0.0, 0.0) + jitter,
                    r: 2.0 + pick(rnd(), 2),
                },
                _ => Surf::Cone {
                    apex: v(pick(rnd(), 3), 1.0, 0.0) + jitter,
                    a: v(0.0, 0.0, 1.0),
                    k: 0.5,
                },
            });
        }
        let reach = recs.iter().map(|s| s.key_point().len()).fold(0.0, f64::max);
        let mut index = SameIndex::new(tol, reach);
        let mut classes: Vec<Surf> = Vec::new();
        for e in &recs {
            let scan = classes.iter().position(|r| r.same(e, tol));
            assert_eq!(index.first_same(&classes, e, tol), scan);
            if scan.is_none() {
                index.insert(e, classes.len());
                classes.push(e.clone());
            }
        }
        // Enough merging to mean something.
        assert!(classes.len() < recs.len() * 3 / 4, "{}", classes.len());
    }
}
