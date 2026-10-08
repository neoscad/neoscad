//! Whether the meshes a model is built from bound solids: each
//! `polyhedron()` and imported mesh is looked at on its own, before any
//! boolean, and a problem is reported against the call that made it.
//!
//! OpenSCAD says nothing specific here. A mesh that is open or has
//! edges shared by more than two faces fails Manifold's conversion, and
//! OpenSCAD prints `PolySet -> Manifold conversion failed: NotManifold`
//! without saying where or why. A mesh that is closed and consistently
//! wound but inside out converts without a word, as a solid of negative
//! volume, and every boolean with it goes wrong: pieces vanish, others
//! float, and the result's edges are pinched. A pinched-edge hint alone
//! sends an agent after the wrong cause: on such a thread sweep, the
//! signed volume is what shows the fault.
//!
//! These findings are NeoSCAD's own: they go into the structured
//! diagnostics (the JSON, the server, the editor) and `check`, never into
//! the console text OpenSCAD prints, which the conformance suite compares
//! word for word.
//!
//! Faces are compared by vertex position (an exact weld, as
//! [`crate::mesh::bad_edges`] does), so a polyhedron that repeats a point
//! under two indices is still closed. Each face is kept whole: its edges
//! are the polygon's own, whether or not it is planar.

use std::sync::Arc;

use eval::Node;
use eval::node::NodeKind;
use geom::polyset::PolySet;

use lang::ast::{ExprKind, InstKind, Instantiation, Scope};
use lang::source::Span;

use crate::mesh::{Aabb, V3, add, scale};

/// What is wrong with one mesh, in its own coordinates.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeshIssues {
    /// Faces with at least three distinct corners.
    pub faces: usize,
    /// Edges used by one face: how many, and the first one's midpoint.
    pub open: Option<(usize, V3)>,
    /// Edges used by more than two faces, and surfaces that cannot be
    /// oriented at all (a Möbius strip): how many edges, and where.
    pub not_manifold: Option<(usize, V3)>,
    /// Faces wound against the rest, by index into the mesh's faces
    /// (PolySet order), ascending.
    pub flipped: Vec<u32>,
    /// The centroid of the first flipped face.
    pub flipped_at: V3,
    /// Whether "flipped" means inward: the faces' shells are closed, so
    /// which way is out is known from the signed volume. For an open
    /// surface the flipped faces are the minority against their
    /// neighbours.
    pub inward: bool,
    /// The signed volume of the mesh as wound (mm³); negative when the
    /// whole mesh is inside out.
    pub volume: f64,
}

impl MeshIssues {
    /// Every face points inward: the mesh is inside out.
    pub fn inside_out(&self) -> bool {
        self.inward && self.faces > 0 && self.flipped.len() == self.faces
    }

    fn is_clean(&self) -> bool {
        self.open.is_none() && self.not_manifold.is_none() && self.flipped.is_empty()
    }
}

/// A face index type: polyhedron nodes hold `usize`, PolySets `u32`.
pub trait Index: Copy {
    fn get(self) -> u32;
}

impl Index for u32 {
    fn get(self) -> u32 {
        self
    }
}

impl Index for usize {
    fn get(self) -> u32 {
        self as u32
    }
}

/// Look at one mesh. `faces` are counter-clockwise seen from outside, as
/// a PolySet holds them, unless `clockwise` (a polyhedron's own face
/// lists, which OpenSCAD reverses when it builds the PolySet:
/// `PolyhedronNode::createGeometry`, `primitives.cc:399-414`). Face
/// indices in the result are the input's. `None` when nothing is wrong.
///
/// Cost: two sorts (vertices, then edges) and a walk over the faces, no
/// hashing; about what [`crate::mesh::bad_edges`] costs on a mesh with
/// shared positions.
pub fn analyze<I: Index>(verts: &[V3], faces: &[Vec<I>], clockwise: bool) -> Option<MeshIssues> {
    // Canonical vertex per position (the lowest index there); -0 and 0
    // are one position.
    let mut keys: Vec<([u64; 3], u32)> = verts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            (
                p.map(|c| if c == 0.0 { 0u64 } else { c.to_bits() }),
                i as u32,
            )
        })
        .collect();
    keys.sort_unstable();
    let mut canon: Vec<u32> = (0..verts.len() as u32).collect();
    let mut welded = false;
    for w in keys.windows(2) {
        if w[0].0 == w[1].0 {
            canon[w[1].1 as usize] = canon[w[0].1 as usize];
            welded = true;
        }
    }
    drop(keys);

    // Each face's corners after the weld, a corner repeated in a row
    // counted once: a sweep's triangle with two corners on the axis has no
    // area, and its other two edges would pair with each other.
    let mut ring: Vec<u32> = Vec::new();
    // Each edge as (lower vertex, higher vertex << 32 | face << 1 |
    // forward), forward when the face runs it from its lower vertex to its
    // higher. Bucketed by the lower vertex below rather than sorted: a
    // comparison sort of every edge was most of the cost on a
    // 360,000-face polyhedron.
    let mut edges: Vec<(u32, u64)> = Vec::with_capacity(faces.len() * 4);
    let mut live = vec![false; faces.len()];
    let mut six_vol = vec![0.0f64; faces.len()];
    let mut n_live = 0usize;
    for (fi, f) in faces.iter().enumerate() {
        ring.clear();
        for &i in f {
            let c = canon[i.get() as usize];
            if ring.last() != Some(&c) {
                ring.push(c);
            }
        }
        while ring.len() > 1 && ring.first() == ring.last() {
            ring.pop();
        }
        if ring.len() < 3 {
            continue;
        }
        if clockwise {
            ring.reverse();
        }
        live[fi] = true;
        n_live += 1;
        for j in 0..ring.len() {
            let (a, b) = (ring[j], ring[(j + 1) % ring.len()]);
            edges.push((
                a.min(b),
                u64::from(a.max(b)) << 32 | (fi as u64) << 1 | u64::from(a < b),
            ));
        }
        // Six times the signed volume of the face fanned from its first
        // corner with the origin (the divergence theorem).
        let p0 = verts[ring[0] as usize];
        let mut v = 0.0;
        for j in 1..ring.len() - 1 {
            let (p1, p2) = (verts[ring[j] as usize], verts[ring[j + 1] as usize]);
            v += p0[0] * (p1[1] * p2[2] - p1[2] * p2[1])
                + p0[1] * (p1[2] * p2[0] - p1[0] * p2[2])
                + p0[2] * (p1[0] * p2[1] - p1[1] * p2[0]);
        }
        six_vol[fi] = v;
    }
    if n_live == 0 {
        return None;
    }
    // Counting sort by the lower vertex; each vertex's few edges are then
    // sorted by the higher one (and the face, for a fixed order).
    let mut bucket = vec![0u32; verts.len() + 1];
    for &(lo, _) in &edges {
        bucket[lo as usize + 1] += 1;
    }
    for k in 0..verts.len() {
        bucket[k + 1] += bucket[k];
    }
    let mut sorted = vec![0u64; edges.len()];
    let mut fill = bucket.clone();
    for &(lo, rest) in &edges {
        sorted[fill[lo as usize] as usize] = rest;
        fill[lo as usize] += 1;
    }
    drop((edges, fill));

    let mid = |e: u64| -> V3 {
        let (u, v) = ((e >> 32) as usize, (e & 0xffff_ffff) as usize);
        scale(add(verts[u], verts[v]), 0.5)
    };
    let mut out = MeshIssues {
        faces: n_live,
        ..Default::default()
    };
    // Faces joined across an edge used by exactly two faces, and whether
    // their windings agree there (they do when they run it in opposite
    // directions).
    let mut pairs: Vec<(u32, u32, bool)> = Vec::with_capacity(sorted.len() / 2);
    let mut touches_open = vec![false; faces.len()];
    let (mut open, mut open_at) = (0usize, None);
    let (mut shared, mut shared_at) = (0usize, None);
    // The faces around each edge used by more than two faces.
    let mut crowded: Vec<Vec<u32>> = Vec::new();
    let face_of = |x: u64| ((x & 0xffff_ffff) >> 1) as u32;
    for lo in 0..verts.len() {
        let run = &mut sorted[bucket[lo] as usize..bucket[lo + 1] as usize];
        run.sort_unstable();
        let mut i = 0;
        while i < run.len() {
            let hi = run[i] >> 32;
            let mut j = i + 1;
            while j < run.len() && run[j] >> 32 == hi {
                j += 1;
            }
            let key = (lo as u64) << 32 | hi;
            match j - i {
                1 => {
                    open += 1;
                    open_at.get_or_insert(key);
                    touches_open[face_of(run[i]) as usize] = true;
                }
                2 => {
                    let (a, b) = (run[i], run[i + 1]);
                    if face_of(a) != face_of(b) {
                        pairs.push((face_of(a), face_of(b), (a & 1) != (b & 1)));
                    }
                }
                _ => {
                    shared += 1;
                    shared_at.get_or_insert(key);
                    crowded.push(run[i..j].iter().map(|&x| face_of(x)).collect());
                }
            }
            i = j;
        }
    }
    drop((sorted, bucket));
    if let Some(e) = open_at {
        out.open = Some((open, mid(e)));
    }
    // Each face's neighbours, in the order found.
    let mut start = vec![0u32; faces.len() + 1];
    for &(a, b, _) in &pairs {
        start[a as usize + 1] += 1;
        start[b as usize + 1] += 1;
    }
    for k in 0..faces.len() {
        start[k + 1] += start[k];
    }
    let mut adj = vec![(0u32, false); start[faces.len()] as usize];
    let mut fill = start.clone();
    for &(a, b, agree) in &pairs {
        adj[fill[a as usize] as usize] = (b, agree);
        fill[a as usize] += 1;
        adj[fill[b as usize] as usize] = (a, agree);
        fill[b as usize] += 1;
    }
    drop((pairs, fill));

    // Components by breadth-first search; `parity` is whether a face is
    // wound against the component's first face.
    let mut comp = vec![u32::MAX; faces.len()];
    let mut parity = vec![false; faces.len()];
    let mut comps: Vec<Component> = Vec::new();
    let mut queue: Vec<u32> = Vec::new();
    let mut conflict_at: Option<u32> = None;
    let mut conflicts = 0usize;
    for seed in 0..faces.len() {
        if !live[seed] || comp[seed] != u32::MAX {
            continue;
        }
        let id = comps.len() as u32;
        let mut c = Component::default();
        comp[seed] = id;
        queue.clear();
        queue.push(seed as u32);
        let mut q = 0;
        while q < queue.len() {
            let f = queue[q] as usize;
            q += 1;
            c.faces += 1;
            c.closed &= !touches_open[f];
            let s = if parity[f] { -1.0 } else { 1.0 };
            c.aligned += s * six_vol[f];
            c.as_wound += six_vol[f];
            c.count[usize::from(parity[f])] += 1;
            for &ring_v in &faces[f] {
                c.bbox.grow(verts[ring_v.get() as usize]);
            }
            for &(g, agree) in &adj[start[f] as usize..start[f + 1] as usize] {
                let g = g as usize;
                let want = parity[f] ^ !agree;
                if comp[g] == u32::MAX {
                    comp[g] = id;
                    parity[g] = want;
                    queue.push(g as u32);
                } else if parity[g] != want {
                    c.orientable = false;
                    conflicts += 1;
                    conflict_at.get_or_insert(f as u32);
                }
            }
        }
        comps.push(c);
    }
    // A shell is closed when each edge its faces use is used by an even
    // number of them: two cubes of one polyhedron sharing an edge are two
    // closed shells, but a triangle whose every edge is shared with three
    // others (two cubes sharing a face) is no shell and has no inside.
    let mut per: Vec<(u32, usize)> = Vec::new();
    for group in &crowded {
        per.clear();
        for &f in group {
            let k = comp[f as usize];
            match per.iter_mut().find(|(c, _)| *c == k) {
                Some(e) => e.1 += 1,
                None => per.push((k, 1)),
            }
        }
        for &(k, n) in &per {
            if n % 2 == 1 && k != u32::MAX {
                comps[k as usize].closed = false;
            }
        }
    }
    let centroid = |f: usize| -> V3 {
        let n = faces[f].len() as f64;
        let s = faces[f]
            .iter()
            .fold([0.0; 3], |s, &i| add(s, verts[i.get() as usize]));
        scale(s, 1.0 / n)
    };
    if let Some(f) = conflict_at {
        // Each conflicting edge is met from both faces.
        let n = conflicts.div_ceil(2);
        let at = shared_at.map_or_else(|| centroid(f as usize), mid);
        out.not_manifold = Some((shared + n, at));
    } else if let Some(e) = shared_at {
        out.not_manifold = Some((shared, mid(e)));
    }

    // Which parity of each component is outward. A closed shell should
    // enclose positive volume, unless it is a cavity: a shell inside a
    // larger one is taken to be a hole in it (and a shell inside that
    // hole a solid again). Containment is judged by bounding boxes, the
    // largest shells first; a separate inside-out shell next to a larger
    // correct one is therefore found, one inside it is not.
    let mut order: Vec<usize> = (0..comps.len()).collect();
    order.sort_by(|&a, &b| {
        comps[b]
            .aligned
            .abs()
            .total_cmp(&comps[a].aligned.abs())
            .then(a.cmp(&b))
    });
    let mut sign = vec![1.0f64; comps.len()];
    let mut placed: Vec<usize> = Vec::new();
    for &k in &order {
        let c = &comps[k];
        if c.closed {
            let host = placed
                .iter()
                .rev()
                .copied()
                .find(|&h| comps[h].closed && strictly_inside(&c.bbox, &comps[h].bbox));
            sign[k] = host.map_or(1.0, |h| -sign[h]);
            placed.push(k);
        }
    }
    // For each component, the parity whose faces are wrong (None: none).
    let mut wrong: Vec<Option<bool>> = vec![None; comps.len()];
    for (k, c) in comps.iter().enumerate() {
        if !c.orientable {
            continue;
        }
        if c.closed {
            // A shell with no volume (a doubled sheet) has no outside.
            let size = c.bbox.size();
            let scale3 = size[0].max(size[1]).max(size[2]).powi(3);
            if c.aligned.abs() <= 1e-9 * scale3 {
                continue;
            }
            // Parity-0 faces are right when the aligned volume has the
            // shell's sign.
            let zero_right = (c.aligned > 0.0) == (sign[k] > 0.0);
            wrong[k] = Some(zero_right);
        } else if c.count[1] > 0 {
            // Open: the minority is wound against its neighbours; on a
            // tie, the faces not wound like the first one.
            wrong[k] = Some(c.count[1] <= c.count[0]);
        }
    }
    out.inward = comps
        .iter()
        .zip(&wrong)
        .all(|(c, w)| c.closed && c.orientable && w.is_some());
    for f in 0..faces.len() {
        if live[f]
            && let Some(p) = wrong[comp[f] as usize]
            && parity[f] == p
        {
            out.flipped.push(f as u32);
        }
    }
    if let Some(&f) = out.flipped.first() {
        out.flipped_at = centroid(f as usize);
    }
    out.volume = comps.iter().map(|c| c.as_wound).sum::<f64>() / 6.0;
    // Pieces that touch only through points at the same position, each
    // with its own copies (two cubes sharing an edge, with distinct
    // vertices): Manifold takes the mesh by index and accepts it, as
    // OpenSCAD's tests intend, so it is neither open nor non-manifold
    // here. What a file of the result would show is the pinched-edge
    // check's business (`crate::mesh::bad_edges`).
    if welded
        && (out.open.is_some() || out.not_manifold.is_some())
        && conflict_at.is_none()
        && paired_by_index(faces, clockwise)
    {
        out.open = None;
        out.not_manifold = None;
    }
    (!out.is_clean()).then_some(out)
}

/// Every directed edge (by vertex index, a repeated corner counted once)
/// used once, and its reverse used once: what Manifold's conversion
/// requires before any merging.
fn paired_by_index<I: Index>(faces: &[Vec<I>], clockwise: bool) -> bool {
    let mut edges: Vec<(u64, bool)> = Vec::with_capacity(faces.len() * 4);
    let mut ring: Vec<u32> = Vec::new();
    for f in faces {
        ring.clear();
        for &i in f {
            if ring.last() != Some(&i.get()) {
                ring.push(i.get());
            }
        }
        while ring.len() > 1 && ring.first() == ring.last() {
            ring.pop();
        }
        if ring.len() < 3 {
            continue;
        }
        if clockwise {
            ring.reverse();
        }
        for j in 0..ring.len() {
            let (a, b) = (ring[j], ring[(j + 1) % ring.len()]);
            edges.push((u64::from(a.min(b)) << 32 | u64::from(a.max(b)), a < b));
        }
    }
    edges.sort_unstable();
    let (pairs, rest) = edges.as_chunks::<2>();
    rest.is_empty() && pairs.iter().all(|[a, b]| a.0 == b.0 && !a.1 && b.1)
}

#[derive(Debug, Clone)]
struct Component {
    faces: usize,
    /// No face has an edge used by that face alone.
    closed: bool,
    orientable: bool,
    /// Six times the signed volume with every face turned to agree with
    /// the first one, and as wound.
    aligned: f64,
    as_wound: f64,
    /// Faces agreeing with the first one, and against it.
    count: [usize; 2],
    bbox: Aabb,
}

impl Default for Component {
    fn default() -> Self {
        Component {
            faces: 0,
            closed: true,
            orientable: true,
            aligned: 0.0,
            as_wound: 0.0,
            count: [0, 0],
            bbox: Aabb::EMPTY,
        }
    }
}

fn strictly_inside(a: &Aabb, b: &Aabb) -> bool {
    (0..3).all(|k| a.lo[k] > b.lo[k] && a.hi[k] < b.hi[k])
}

/// Which mesh a problem is in.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// A `polyhedron()` call.
    Polyhedron,
    /// An `import()` of a mesh file, by its file name.
    Import(String),
}

/// A problem with one input mesh, placed in the model.
#[derive(Debug, Clone, PartialEq)]
pub struct InputIssue {
    /// `polyhedron-inside-out`, `polyhedron-flipped-faces`,
    /// `polyhedron-open` or `polyhedron-not-manifold`.
    pub code: lang::diag::DiagCode,
    pub message: String,
    pub fix: String,
    /// Where, in the model's coordinates: the first flipped face's
    /// centroid, or the first bad edge's midpoint.
    pub point: V3,
    /// The call's program unit, span and line (for the diagnostic).
    pub unit: u32,
    pub span: Span,
    pub line: u32,
    /// For a mechanical fix: the faces to reverse, in the call's own face
    /// list (polyhedron only), and how many faces the call made.
    pub reverse: Vec<u32>,
    pub faces_len: usize,
    pub source: Source,
    /// The call as `file:line` (the file's name only), once reported.
    pub call: Option<String>,
}

impl InputIssue {
    /// The problem is the winding (inside out or flipped faces), which is
    /// what makes Manifold's booleans leave pinched edges.
    pub fn is_winding(&self) -> bool {
        use lang::diag::DiagCode as C;
        matches!(
            self.code,
            C::PolyhedronInsideOut | C::PolyhedronFlippedFaces
        )
    }
}

/// A cache of [`analyze`] by node: given the node and the analysis to
/// run, the (possibly remembered) result.
pub type Memo<'a> =
    dyn FnMut(&Node, &dyn Fn() -> Option<MeshIssues>) -> Arc<Option<MeshIssues>> + 'a;

/// How many calls with problems are reported; more would bury the rest of
/// the log, and the first ones say what to fix.
const MAX_CALLS: usize = 10;

/// Every polyhedron (and, through `import_mesh`, imported mesh) under
/// `top` that does not bound a solid, one entry per call and problem, in
/// tree order. `import_mesh` gives an import node's mesh when it is at
/// hand (the geometry cache), or `None`; `memo` caches each mesh's
/// analysis by its node's key (`None`: no memo).
///
/// Subtrees that do not use the mesh as a solid are skipped: `hull()`
/// reads only the points, and a `%` background is not rendered.
pub fn find(
    top: &Node,
    import_mesh: &dyn Fn(&Node) -> Option<Arc<PolySet>>,
    memo: &mut Memo<'_>,
) -> Vec<InputIssue> {
    let mut out: Vec<InputIssue> = Vec::new();
    // (origin, code) already reported.
    let mut seen: Vec<(u32, Span, lang::diag::DiagCode)> = Vec::new();
    let mut calls: Vec<(u32, Span)> = Vec::new();
    let mut stack: Vec<(&Node, eval::node::Matrix)> = vec![(top, eval::node::IDENTITY)];
    while let Some((n, m)) = stack.pop() {
        if n.origin.as_ref().is_some_and(|o| o.tag_background) {
            continue;
        }
        let mut faces_len = 0;
        let issues = match &n.kind {
            NodeKind::Hull => continue,
            NodeKind::Transform { matrix, .. } => {
                let m2 = mul(&m, matrix);
                for c in n.children.iter().rev() {
                    stack.push((c, m2));
                }
                continue;
            }
            NodeKind::Polyhedron { points, faces, .. } => {
                faces_len = faces.len();
                let r = memo(n, &|| analyze(points, faces, true));
                r.as_ref().clone().map(|r| (r, Source::Polyhedron))
            }
            NodeKind::Import(i) => {
                let Some(ps) = import_mesh(n) else {
                    continue;
                };
                let r = memo(n, &|| analyze(&ps.vertices, &ps.faces, false));
                // The name alone: the node holds the resolved path, which
                // is long and says nothing the call does not.
                let name = std::path::Path::new(&i.file)
                    .file_name()
                    .map_or(i.file.clone(), |n| n.to_string_lossy().into_owned());
                r.as_ref().clone().map(|r| (r, Source::Import(name)))
            }
            _ => {
                for c in n.children.iter().rev() {
                    stack.push((c, m));
                }
                continue;
            }
        };
        let (Some((r, source)), Some(o)) = (issues, n.origin.as_ref()) else {
            continue;
        };
        let call = (o.unit, o.span);
        if !calls.contains(&call) {
            if calls.len() == MAX_CALLS {
                continue;
            }
            calls.push(call);
        }
        for mut issue in describe(&r, &source, &m) {
            if seen.contains(&(o.unit, o.span, issue.code)) {
                continue;
            }
            seen.push((o.unit, o.span, issue.code));
            issue.unit = o.unit;
            issue.span = o.span;
            issue.line = o.line;
            issue.faces_len = faces_len;
            out.push(issue);
        }
    }
    out
}

/// [`find`], each problem then logged on `con` as a NeoSCAD-only warning
/// ([`eval::Console::note`]) at the call that made the mesh, with its fix
/// and, when the faces are written out, the exact edit. `program` gives
/// each evaluation unit's program (0 the main one, `1 + i` the i-th
/// library). The issues come back with [`InputIssue::call`] set.
pub fn report<'a, W: std::io::Write>(
    con: &mut eval::Console<W>,
    top: &Node,
    import_mesh: &dyn Fn(&Node) -> Option<Arc<PolySet>>,
    memo: &mut Memo<'_>,
    program: &dyn Fn(u32) -> Option<&'a lang::Program>,
    cwd: &std::path::Path,
) -> Vec<InputIssue> {
    use lang::diag::{Diagnostic, Hint, PathBase, Severity};
    let mut issues = find(top, import_mesh, memo);
    for issue in &mut issues {
        let Some(p) = program(issue.unit) else {
            continue;
        };
        let mut d = Diagnostic::new(issue.code, Severity::Warning, issue.message.clone())
            .at(issue.span, issue.line)
            .with_base(PathBase::MainFileDir);
        d.hints.push(Hint {
            message: issue.fix.clone(),
            replacement: literal_fix(p, issue),
        });
        con.note(&d, &p.sources, cwd);
        let file = p.sources.path(issue.span.file);
        issue.call = Some(format!(
            "{}:{}",
            file.file_name()
                .map_or(file.to_string_lossy(), |n| n.to_string_lossy()),
            issue.line
        ));
    }
    issues
}

/// The edit that fixes a winding problem mechanically, when the call's
/// `faces` argument is written out as a list of lists of numbers: the
/// same text with the listed faces' indices in reverse order. `None` for
/// faces computed by an expression (the fix is then the hint's text), or
/// when the literal does not match the faces the call made (a face that
/// evaluation dropped would shift the indices).
pub fn literal_fix(program: &lang::Program, issue: &InputIssue) -> Option<(Span, String)> {
    if !issue.is_winding() || issue.source != Source::Polyhedron || issue.reverse.is_empty() {
        return None;
    }
    let ast = &program.ast;
    let inst = find_inst(&ast.root, issue.span)?;
    if ast.name(inst.name) != "polyhedron" {
        return None;
    }
    let faces_arg = inst
        .args
        .iter()
        .find(|a| a.name.is_some_and(|n| ast.name(n) == "faces"))
        .or_else(|| inst.args.iter().filter(|a| a.name.is_none()).nth(1))?;
    let faces = ast.expr(faces_arg.expr);
    let ExprKind::Vector(list) = &faces.kind else {
        return None;
    };
    if list.len() != issue.faces_len {
        return None;
    }
    let text = &program.sources.get(faces.span.file).text;
    let src = |sp: Span| std::str::from_utf8(&text[sp.start as usize..sp.end as usize]).ok();
    let mut out = String::new();
    let mut pos = faces.span.start;
    let mut k = 0;
    for (i, &f) in list.iter().enumerate() {
        let e = ast.expr(f);
        let ExprKind::Vector(ix) = &e.kind else {
            return None;
        };
        if ix.len() < 3
            || !ix
                .iter()
                .all(|&x| matches!(ast.expr(x).kind, ExprKind::Number(_)))
            || e.span.file != faces.span.file
        {
            return None;
        }
        if issue.reverse.get(k) != Some(&(i as u32)) {
            continue;
        }
        k += 1;
        // Each index's text moves to its mirror position; the separators,
        // spaces and comments between them stay where they are.
        for (j, &x) in ix.iter().enumerate() {
            let here = ast.expr(x).span;
            let there = ast.expr(ix[ix.len() - 1 - j]).span;
            out.push_str(src(Span::new(faces.span.file, pos, here.start))?);
            out.push_str(src(there)?);
            pos = here.end;
        }
    }
    if k != issue.reverse.len() {
        return None;
    }
    out.push_str(src(Span::new(faces.span.file, pos, faces.span.end))?);
    Some((faces.span, out))
}

/// The instantiation at `span`, anywhere in the program.
pub(crate) fn find_inst(scope: &Scope, span: Span) -> Option<&Instantiation> {
    for m in &scope.modules {
        if let Some(i) = find_inst(&m.body, span) {
            return Some(i);
        }
    }
    for i in &scope.instantiations {
        if i.span == span {
            return Some(i);
        }
        if i.span.file == span.file && (i.span.start > span.start || i.span.end < span.end) {
            // Not inside this one.
            continue;
        }
        if let Some(found) = find_inst(&i.children, span) {
            return Some(found);
        }
        if let InstKind::If {
            else_children: Some(e),
        } = &i.kind
            && let Some(found) = find_inst(e, span)
        {
            return Some(found);
        }
    }
    None
}

fn mul(a: &eval::node::Matrix, b: &eval::node::Matrix) -> eval::node::Matrix {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..4).map(|k| a[r][k] * b[k][c]).sum()))
}

/// A number as the messages print it: at most 6 significant digits.
fn num(x: f64) -> String {
    render::snapshot::number(crate::stats::round6(x))
}

fn at(p: V3) -> String {
    format!("[{}, {}, {}]", num(p[0]), num(p[1]), num(p[2]))
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The issues of one mesh as messages and fixes, the winding first: it is
/// what an agent must fix before anything else makes sense.
fn describe(r: &MeshIssues, source: &Source, m: &eval::node::Matrix) -> Vec<InputIssue> {
    use lang::diag::DiagCode as C;
    // Points in the model's coordinates, where a snapshot or `measure`
    // would find them.
    let mut r = r.clone();
    let place = |p: V3| geom::polyset::apply(m, p);
    r.flipped_at = place(r.flipped_at);
    r.open = r.open.map(|(n, p)| (n, place(p)));
    r.not_manifold = r.not_manifold.map(|(n, p)| (n, place(p)));
    let r = &r;
    let poly = matches!(source, Source::Polyhedron);
    let what = match source {
        Source::Polyhedron => "this polyhedron".to_string(),
        Source::Import(f) => format!("the mesh imported from '{f}'"),
    };
    let mut out = Vec::new();
    let issue = |code, message: String, fix: String, point, reverse: Vec<u32>| InputIssue {
        code,
        message,
        fix,
        point,
        unit: 0,
        span: Span::default(),
        line: 0,
        reverse,
        faces_len: 0,
        source: source.clone(),
        call: None,
    };
    if r.inside_out() {
        let fix = if poly {
            "OpenSCAD wants each face's points in clockwise order seen from outside the solid; \
             these are counter-clockwise. Reverse every face's point list, e.g. \
             `faces = [for (f = faces) [for (i = [len(f) - 1:-1:0]) f[i]]]`"
        } else {
            "re-export the mesh with its normals pointing outward (in a mesh editor: flip, or \
             recalculate normals outside)"
        };
        out.push(issue(
            C::PolyhedronInsideOut,
            format!(
                "{what} is inside out: all {} face{} point inward (its signed volume is {} mm³); \
                 booleans with it give wrong results",
                r.faces,
                if r.faces == 1 { "" } else { "s" },
                num(r.volume)
            ),
            fix.into(),
            r.flipped_at,
            (0..r.faces as u32).collect(),
        ));
    } else if !r.flipped.is_empty() {
        let n = r.flipped.len();
        let message = if r.inward {
            format!(
                "{} of {what}'s {} faces {} inward, wound opposite to the other {}; the first \
                 is at {}",
                n,
                r.faces,
                if n == 1 { "points" } else { "point" },
                r.faces - n,
                at(r.flipped_at)
            )
        } else {
            format!(
                "{} of {what}'s {} faces {} wound opposite to {} neighbours; the first is at {}",
                n,
                r.faces,
                if n == 1 { "is" } else { "are" },
                if n == 1 { "its" } else { "their" },
                at(r.flipped_at)
            )
        };
        let fix = if poly {
            format!(
                "reverse the point order of {}: OpenSCAD wants each face clockwise seen from \
                 outside{}",
                if n == 1 { "that face" } else { "those faces" },
                if r.inward && 2 * n > r.faces {
                    "; they are most of the faces, so the few others are the ones already right"
                } else {
                    ""
                }
            )
        } else {
            "flip those faces in a mesh editor (recalculate normals outside), then re-export".into()
        };
        out.push(issue(
            C::PolyhedronFlippedFaces,
            message,
            fix,
            r.flipped_at,
            r.flipped.clone(),
        ));
    }
    if let Some((n, p)) = r.open {
        out.push(issue(
            C::PolyhedronOpen,
            format!(
                "{what} is not closed: {} used by only one face, the first at {}",
                plural(n, "edge is", "edges are"),
                at(p)
            ),
            "every edge must be shared by exactly two faces: add the missing faces, and make \
             faces that meet use the same points; an edge that ends part-way along another is \
             not joined to it"
                .into(),
            p,
            Vec::new(),
        ));
    }
    if let Some((n, p)) = r.not_manifold {
        out.push(issue(
            C::PolyhedronNotManifold,
            format!(
                "{what} is not manifold: {} shared by more than two faces or join faces that \
                 cannot be wound consistently, the first at {}",
                plural(n, "edge is", "edges are"),
                at(p)
            ),
            "each edge must belong to exactly two faces that run it in opposite directions: \
             remove duplicate faces, or split the shape into separate solids"
                .into(),
            p,
            Vec::new(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit cube's faces, clockwise seen from outside (OpenSCAD's
    /// polyhedron order, the manual's example).
    fn cube() -> (Vec<V3>, Vec<Vec<usize>>) {
        let p = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let f = vec![
            vec![0, 1, 2, 3],
            vec![4, 5, 1, 0],
            vec![7, 6, 5, 4],
            vec![5, 6, 2, 1],
            vec![6, 7, 3, 2],
            vec![7, 4, 0, 3],
        ];
        (p, f)
    }

    #[test]
    fn a_correct_cube_is_clean() {
        let (p, f) = cube();
        assert_eq!(analyze(&p, &f, true), None);
    }

    #[test]
    fn an_inside_out_cube() {
        let (p, mut f) = cube();
        f.iter_mut().for_each(|f| f.reverse());
        let r = analyze(&p, &f, true).unwrap();
        assert!(r.inside_out(), "{r:?}");
        assert_eq!(r.flipped.len(), 6);
        assert!((r.volume + 1.0).abs() < 1e-12);
    }

    #[test]
    fn one_flipped_face() {
        let (p, mut f) = cube();
        f[3].reverse();
        let r = analyze(&p, &f, true).unwrap();
        assert!(!r.inside_out());
        assert!(r.inward);
        assert_eq!(r.flipped, vec![3]);
        assert_eq!(r.flipped_at, [1.0, 0.5, 0.5]);
    }

    #[test]
    fn five_flipped_faces_are_the_five_not_the_one() {
        // The pilot's thread: the caps were right and the sides wrong, and
        // flipping the minority would have made it worse.
        let (p, mut f) = cube();
        for (i, face) in f.iter_mut().enumerate() {
            if i != 2 {
                face.reverse();
            }
        }
        let r = analyze(&p, &f, true).unwrap();
        assert_eq!(r.flipped, vec![0, 1, 3, 4, 5]);
        assert!(r.inward);
    }

    #[test]
    fn an_open_cube() {
        let (p, mut f) = cube();
        f.pop();
        let r = analyze(&p, &f, true).unwrap();
        assert_eq!(r.open.map(|o| o.0), Some(4));
        assert!(r.flipped.is_empty());
        assert!(!r.inward);
    }

    #[test]
    fn repeated_points_are_welded() {
        // The top face uses copies of the top corners.
        let (mut p, mut f) = cube();
        p.extend([
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ]);
        f[2] = vec![11, 10, 9, 8];
        assert_eq!(analyze(&p, &f, true), None);
    }

    #[test]
    fn a_cavity_is_not_inside_out() {
        // A 3 mm cube with a 1 mm cube cavity: the inner shell faces into
        // the hole, which is right.
        let (p, f) = cube();
        let mut pts: Vec<V3> = p.iter().map(|v| v.map(|c| c * 3.0)).collect();
        let mut faces = f.clone();
        let n = pts.len();
        pts.extend(p.iter().map(|v| v.map(|c| c + 1.0)));
        faces.extend(
            f.iter()
                .map(|f| f.iter().rev().map(|&i| i + n).collect::<Vec<_>>()),
        );
        assert_eq!(analyze(&pts, &faces, true), None);
    }

    #[test]
    fn a_separate_inside_out_cube_is_found() {
        let (p, f) = cube();
        let mut pts = p.clone();
        let mut faces = f.clone();
        let n = pts.len();
        pts.extend(p.iter().map(|v| [v[0] + 3.0, v[1], v[2]]));
        faces.extend(
            f.iter()
                .map(|f| f.iter().rev().map(|&i| i + n).collect::<Vec<_>>()),
        );
        let r = analyze(&pts, &faces, true).unwrap();
        assert_eq!(r.flipped, vec![6, 7, 8, 9, 10, 11]);
        assert!(!r.inside_out());
    }

    #[test]
    fn edges_of_three_faces_are_not_manifold() {
        let (p, mut f) = cube();
        f.push(vec![0, 1, 6]);
        let r = analyze(&p, &f, true).unwrap();
        assert!(r.not_manifold.is_some());
    }
}
