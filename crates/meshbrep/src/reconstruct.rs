//! Mesh-guided B-rep reconstruction.
//!
//! Faces are connected regions of triangles on one exact surface; edges
//! are the boundary chains between two faces, given exact curves;
//! vertices are where three or more faces meet (or where two tangent
//! surfaces' intersection crosses itself), solved on the exact surfaces.
//! Every choice is made in an order fixed by the input's indices or by
//! coordinates, never by hashing, so equal input gives equal output.

use std::collections::BTreeMap;

use crate::Error;
use crate::curve;
use crate::edges::{self, make_edge};
use crate::math::*;
use crate::model::{Surface, TaggedMesh};
use crate::solve::{solve, tangent_point};
use crate::surf::Surf;
use crate::tangency::{Cont, contact};
use crate::topo::{TEdge, TFace, Topo};

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
}

impl Default for Tolerances {
    fn default() -> Self {
        Tolerances {
            merge: 1e-9,
            fit: 1e-7,
        }
    }
}

/// Options for [`crate::reconstruct`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Options {
    /// Tolerances.
    pub tolerances: Tolerances,
}

/// The surface classes of the triangles, and per class its surface,
/// whether it is a faceted plane, and the input index it came from.
struct Classes {
    of_tri: Vec<usize>,
    surf: Vec<Surf>,
    faceted: Vec<bool>,
    input: Vec<Option<u32>>,
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
pub(crate) fn build(mesh: &TaggedMesh, opts: &Options, use_contacts: bool) -> Result<Built, Error> {
    let nt = mesh.triangles.len();
    if nt == 0 {
        return Err(Error::InvalidInput("the mesh has no triangles".into()));
    }
    if mesh.triangle_surface.len() != nt {
        return Err(Error::InvalidInput(format!(
            "{} triangles but {} surface ids",
            nt,
            mesh.triangle_surface.len()
        )));
    }
    let np = mesh.positions.len();
    let pos: Vec<V> = mesh.positions.iter().map(|&p| V::from(p)).collect();
    if let Some(i) = pos.iter().position(|p| !p.is_finite()) {
        return Err(Error::InvalidInput(format!("position {i} is not finite")));
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
            )));
        }
        if tri[0] == tri[1] || tri[1] == tri[2] || tri[0] == tri[2] {
            return Err(Error::InvalidInput(format!(
                "triangle {t} repeats a vertex"
            )));
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
        return Err(Error::InvalidInput("the mesh is flat".into()));
    }
    let tol = opts.tolerances.merge * scale;
    let fit_tol = opts.tolerances.fit;

    // Half-edge h = 3 t + k runs from tris[t][k] to tris[t][(k + 1) % 3].
    let twin = twins(&tris)?;
    let ends = |h: usize| (tris[h / 3][h % 3], tris[h / 3][(h % 3 + 1) % 3]);
    let mesh_genus_components = mesh_topology(&tris, np);

    // 1. Surface classes.
    let cls = classes(mesh, &pos, &tris, &twin, tol)?;
    let tcls = &cls.of_tri;

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
                    .then(|| contact(&cls.surf[a], &cls.surf[b], tol))
                    .flatten()
            });
        }
    }
    let contact_of = |a: usize, b: usize| -> Option<Cont> {
        let k = if a < b { (a, b) } else { (b, a) };
        contacts.get(&k).copied().flatten()
    };
    let tangencies: Vec<(u32, u32, Cont)> = contacts
        .iter()
        .filter_map(|(&(a, b), c)| {
            let c = (*c)?;
            Some((cls.input[a]?, cls.input[b]?, c))
        })
        .collect();

    // 5. Where two curved surfaces touch at a point, their intersection
    // curve crosses itself: a vertex although only two faces meet there.
    // Candidates are two-face vertices with nearly parallel normals; the
    // exact touching point is solved for, and the nearest mesh vertex
    // keeps it.
    let mut tangent_at: Vec<Option<V>> = vec![None; np];
    {
        let mut best: Vec<(V, usize, f64)> = Vec::new();
        for p in 0..np {
            let fl = &vfaces[p];
            if fl.len() != 2 || fcls[fl[0]] == fcls[fl[1]] {
                continue;
            }
            let (ca, cb) = (fcls[fl[0]], fcls[fl[1]]);
            let (a, b) = (cls.surf[ca], cls.surf[cb]);
            if a.is_plane() && b.is_plane() {
                continue;
            }
            // Surfaces tangent along a whole curve have no crossing.
            if contact_of(ca, cb).is_some_and(|c| c.is_curve()) {
                continue;
            }
            let q = solve(&[a, b], pos[p]).0;
            if a.grad(q).cross(b.grad(q)).len() > 0.35 {
                continue;
            }
            if let Some(tp) = tangent_point(&a, &b, q, scale) {
                let d = (tp - pos[p]).len();
                if d > 0.05 * scale {
                    continue;
                }
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
    let next_b = |h: usize| -> Result<usize, Error> {
        let mut cur = next_in_tri(h);
        for _ in 0..3 * nt {
            if boundary(cur) {
                return Ok(cur);
            }
            cur = next_in_tri(twin[cur]);
        }
        Err(Error::Reconstruction("a vertex walk did not end".into()))
    };

    // 6. Chains.
    const NONE: usize = usize::MAX;
    let mut he_chain: Vec<(usize, bool)> = vec![(NONE, false); 3 * nt];
    let mut chains: Vec<Chain> = Vec::new();
    let bhe: Vec<usize> = (0..3 * nt).filter(|&h| boundary(h)).collect();
    let take = |start: usize,
                closed: bool,
                he_chain: &mut Vec<(usize, bool)>,
                chains: &mut Vec<Chain>|
     -> Result<(), Error> {
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
                return Err(Error::Reconstruction(
                    "a boundary chain ran into another".into(),
                ));
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
    for (i, ch) in chains.iter().enumerate() {
        let (ca, cb) = (fcls[ch.f], fcls[ch.g]);
        if alive[i] && !(cls.faceted[ca] && cls.faceted[cb]) {
            let pts: Vec<V> = ch.verts.iter().map(|&p| pos[p as usize]).collect();
            let d = edges::chain_deviation(&cls.surf[ca], &cls.surf[cb], &pts);
            max_chain_dev = max_chain_dev.max(d);
        }
    }

    // Number the surviving chains as edges.
    let mut edge_of_chain = vec![NONE; chains.len()];
    for (i, b) in built.into_iter().enumerate() {
        if let Some(e) = b {
            edge_of_chain[i] = topo.edges.len();
            topo.edges.push(e);
        }
    }

    // 9. Loops: walk each face's boundary half-edges.
    topo.faces = (0..nf)
        .map(|f| TFace {
            surf: cls.surf[fcls[f]],
            same_sense: fvote[f] > 0.0,
            faceted: cls.faceted[fcls[f]],
            loops: Vec::new(),
            param: None,
            pcurves: Vec::new(),
            outer: Vec::new(),
        })
        .collect();
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
                return Err(Error::Reconstruction(
                    "a loop walk revisited a half-edge".into(),
                ));
            }
        }
        topo.faces[f].loops.push(lp);
    }
    for (f, face) in topo.faces.iter().enumerate() {
        // A face with no boundary covers a whole closed component by
        // itself (a lone sphere): it gets a seam later, from nothing.
        if face.loops.is_empty() && !matches!(face.surf, Surf::Sphere { .. }) {
            return Err(Error::Reconstruction(format!("face {f} has no boundary")));
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
    if topo.edges.iter().any(|e| e.faces.contains(&NONE)) {
        return Err(Error::Reconstruction(
            "an edge is not used by two faces".into(),
        ));
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
    let (sa, sb) = (cls.surf[ca], cls.surf[cb]);
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
        make_edge(
            &sa,
            &sb,
            contact_of(ca, cb),
            p0,
            p1,
            &pts,
            ch.closed,
            fit_tol,
        )
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
    for i in 0..cl.len() {
        for j in i + 1..cl.len() {
            if taken[i] || taken[j] {
                continue;
            }
            if let Some(c) = contact_of(cl[i], cl[j]) {
                cons.extend(c.constraints());
                taken[i] = true;
                taken[j] = true;
            }
        }
    }
    for (i, &c) in cl.iter().enumerate() {
        if !taken[i] {
            cons.push(cls.surf[c]);
        }
    }
    let q = solve(&cons, pos[p]).0;
    (q, residual(q))
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
        if let Some(e) = &e {
            if !e.well_formed() {
                return Err(Error::InvalidInput(format!(
                    "surface {i} ({}) is malformed",
                    s.kind()
                )));
            }
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
    for s in 0..ns {
        let Some(e) = exact[s] else { continue };
        if !used[s] {
            continue;
        }
        let c = surf.iter().position(|r| r.same(&e, tol));
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
    // Degenerate faceted triangles join a faceted neighbour.
    for t in 0..nt {
        if is_faceted(t) && planes[t].is_none() {
            if let Some(u) = (0..3)
                .map(|k| twin[3 * t + k] / 3)
                .find(|&u| is_faceted(u) && planes[u].is_some())
            {
                uf.join(t, u);
            }
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
        let plane = planes[bt].expect("plane").0;
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
            return Err(Error::InvalidInput(
                "a faceted region has only degenerate triangles".into(),
            ));
        };
        let c = match joins.get(&r) {
            Some(&c) => c,
            None => {
                surf.push(planes[bt].expect("plane").0);
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
