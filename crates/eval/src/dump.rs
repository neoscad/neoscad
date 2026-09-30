//! Text forms of the node tree: the `.csg` export, and the canonical
//! per-subtree keys the geometry cache is meant to use.
//!
//! Both come from one label writer ([`Style`]), so a parameter the dump
//! prints can't be forgotten by the key or the other way round.
//!
//! - [`csg`] is OpenSCAD's `Tree::getString(root, "\t")` (`NodeDumper`
//!   without `idString`): one line per node from its `toString()`, tab
//!   indentation, `%`/`#` written *before* the indentation, and the root
//!   printing only its children. Numbers are C++'s `ostream << double`
//!   (`%g`, 6 significant digits), not the `echo` formatter: the node
//!   classes stream raw doubles. Files print relative to the document
//!   directory and with their modification time, as `Filename`'s
//!   `operator<<` and `fs_timestamp` do; the dump stats the file but never
//!   reads it, and a missing file is timestamp 0 with no message.
//! - [`Keys`] follows OpenSCAD's `Tree::getIdString` (`NodeDumper` with
//!   `idString`), which is what OpenSCAD keys its geometry cache on: no
//!   whitespace, and a `group` with at most one child that has content is
//!   transparent, so `group() { cube(); }` and `cube()` share a key (even
//!   with empty groups beside the cube, which in 2D do change the result;
//!   the geometry cache splits those, see `geom`'s `cache_key`). Unlike
//!   OpenSCAD it is exact: numbers go in as their raw bits instead of 6
//!   digits (OpenSCAD's 6-digit key makes `cube(1)` and `cube(1.0000001)`
//!   share cached geometry; `-0` and `0` differ, all NaNs are one value,
//!   see `Writer::num`), files are absolute with a
//!   nanosecond timestamp and the file size (a file edited twice in one
//!   second still misses), every string is quoted and escaped (OpenSCAD
//!   writes `text()` strings raw, so a `"` in the text can make two keys
//!   equal), and parameters the dump leaves out but geometry uses are
//!   included. The key itself is a Merkle hash rather than the text: each
//!   node hashes its own label with its children's hashes (and their
//!   `%`/`#`), bottom-up in one pass, so computing every key is linear in
//!   the dump size. Two subtrees get the same key exactly when their
//!   exact texts would be equal, up to SHA-256 collisions and the empty
//!   groups the text also ignores.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lang::loader::{FileSystem, Metadata};
use lang::number::fmt_g;
use sha2::{Digest as _, Sha256};

use crate::node::{CsgOp, Discretizer, Node, NodeKind, OffsetJoin};
use crate::text_props;

/// Which text form a label is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    /// OpenSCAD's `.csg` export, byte for byte.
    Csg,
    /// The exact, whitespace-free cache key.
    Key,
}

struct Writer<'a> {
    /// Bytes rather than text: the key writes numbers as raw bits (see
    /// [`Writer::num`]). The `.csg` form only ever writes UTF-8.
    out: Vec<u8>,
    style: Style,
    /// The document directory `.csg` file names are made relative to.
    base: &'a Path,
    /// Where imported files are stat'ed for their time and size.
    fs: &'a dyn FileSystem,
}

/// The `.csg` export of `top`: the root's children, or the node the root
/// modifier (`!`) picked, with the trailing newline `openscad.cc` adds.
pub fn csg(top: &Node, doc_dir: &Path, fs: &dyn FileSystem) -> String {
    let mut w = Writer {
        out: Vec::new(),
        style: Style::Csg,
        base: doc_dir,
        fs,
    };
    if top.kind == NodeKind::Root {
        for c in &top.children {
            w.csg_node(c, 0);
        }
    } else {
        w.csg_node(top, 0);
    }
    w.out.push(b'\n');
    // Everything written in this style came from `str`s, so this is
    // always valid; the lossy fallback only avoids a panic path.
    String::from_utf8(w.out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

impl Writer<'_> {
    fn csg_node(&mut self, n: &Node, depth: usize) {
        self.modifiers(n);
        (0..depth).for_each(|_| self.out.push(b'\t'));
        self.label(n);
        if n.children.is_empty() {
            self.out.extend_from_slice(b";\n");
        } else {
            self.out.extend_from_slice(b" {\n");
            for c in &n.children {
                self.csg_node(c, depth + 1);
            }
            (0..depth).for_each(|_| self.out.push(b'\t'));
            self.out.extend_from_slice(b"}\n");
        }
    }

    /// `%` then `#`, from the node's own instantiation (list nodes, which
    /// would pass them down, exist only with the lazy-union experiment).
    fn modifiers(&mut self, n: &Node) {
        if let Some(o) = &n.origin {
            if o.tag_background {
                self.out.push(b'%');
            }
            if o.tag_highlight {
                self.out.push(b'#');
            }
        }
    }

    fn key(&self) -> bool {
        self.style == Style::Key
    }

    /// Literal label text; the key drops its spaces.
    fn lit(&mut self, s: &str) {
        if self.key() {
            self.out.extend(s.bytes().filter(|&c| c != b' '));
        } else {
            self.out.extend_from_slice(s.as_bytes());
        }
    }

    /// A number. The `.csg` form prints it as C++ streams do; the key
    /// takes its raw bits, which used to be its `{:?}` text: formatting
    /// every point of a big polyhedron was a quarter of a render's time.
    ///
    /// The bits are exactly as fine as that text was. Every finite value
    /// is its own key, including `-0` apart from `0`, which must stay
    /// apart because the geometry can differ (a `-0` coordinate exports
    /// as `-0` in OFF). All NaNs are one key, as `{:?}`'s `NaN` was: a
    /// NaN's sign and payload are accidents of the arithmetic (fused or
    /// not), and splitting on them would only make the cache miss.
    ///
    /// The record stays unambiguous: [`NUM`] can't occur in the UTF-8
    /// text around it, and exactly eight bytes follow it.
    fn num(&mut self, v: f64) {
        if self.key() {
            let v = if v.is_nan() { f64::NAN } else { v };
            self.out.push(NUM);
            self.out.extend_from_slice(&v.to_bits().to_le_bytes());
        } else if v.is_nan() {
            // macOS's printf, under the nightly's `ostream`, writes `nan`
            // whatever the sign bit (`multmatrix([[n, -n]])` with `n =
            // asin(1.1)` prints `nan, nan`), where `fmt_g` follows glibc's
            // `-nan`. Which sign a NaN ends up with is an accident of the
            // arithmetic (fused or not, see `fma`), so the dump drops it.
            self.out.extend_from_slice(b"nan");
        } else {
            self.out.extend_from_slice(fmt_g(v).as_bytes());
        }
    }

    /// An integer, as text in both forms (formatted in place: polyhedron
    /// face lists are most of a big key).
    fn int(&mut self, v: impl std::fmt::Display) {
        let _ = write!(self.out, "{v}");
    }

    fn boolean(&mut self, b: bool) {
        self.out
            .extend_from_slice(if b { b"true" } else { b"false" });
    }

    /// `QuotedString`'s `operator<<`.
    fn quoted(&mut self, s: &str) {
        lang::dump::quoted(&mut self.out, s.as_bytes());
    }

    /// A string OpenSCAD writes between quotes without escaping (the
    /// `text()` parameters). The key escapes it so it stays unambiguous.
    fn raw_quoted(&mut self, s: &str) {
        if self.key() {
            self.quoted(s);
        } else {
            self.out.push(b'"');
            self.out.extend_from_slice(s.as_bytes());
            self.out.push(b'"');
        }
    }

    fn vec(&mut self, v: &[f64]) {
        self.out.push(b'[');
        for (i, &x) in v.iter().enumerate() {
            if i > 0 {
                self.lit(", ");
            }
            self.num(x);
        }
        self.out.push(b']');
    }

    /// `operator<<(CurveDiscretizer)` (without the experimental `$fe`).
    fn disc(&mut self, d: &Discretizer) {
        self.lit("$fn = ");
        self.num(d.fn_);
        self.lit(", $fa = ");
        self.num(d.fa);
        self.lit(", $fs = ");
        self.num(d.fs);
    }

    /// `Filename`'s `operator<<` and `fs_timestamp`: in the `.csg`, the
    /// path relative to the document directory and whole seconds; in the
    /// key, the absolute path and nanoseconds.
    fn file(&mut self, file: &str) {
        if self.key() {
            self.quoted(file);
        } else {
            let rel = fs_relative(self.fs, Path::new(file), self.base);
            self.quoted(&rel.to_string_lossy());
        }
    }

    fn timestamp(&mut self, file: &str) {
        self.lit(", timestamp = ");
        let t = mtime_nanos(self.fs, file);
        if self.key() {
            self.int(t);
            // The key also holds the size, so a file rewritten within the
            // file system's timestamp resolution still misses the cache
            // (imported geometry is cached under this key).
            self.lit(", size = ");
            self.int(file_size(self.fs, file));
        } else {
            // std::chrono::duration_cast truncates toward zero.
            self.int(t / 1_000_000_000);
        }
    }

    /// The node's `toString()`.
    fn label(&mut self, n: &Node) {
        match &n.kind {
            NodeKind::Root => self.lit("root()"),
            NodeKind::Group { .. } => self.lit("group()"),
            NodeKind::IntersectionFor => self.lit("intersection()"),
            NodeKind::Csg(op) => self.lit(match op {
                CsgOp::Union => "union()",
                CsgOp::Difference => "difference()",
                CsgOp::Intersection => "intersection()",
            }),
            NodeKind::Transform { matrix, .. } => {
                self.lit("multmatrix([");
                for (j, row) in matrix.iter().enumerate() {
                    if j > 0 {
                        self.lit(", ");
                    }
                    self.vec(row);
                }
                self.lit("])");
            }
            NodeKind::Color { rgba } => {
                self.lit("color(");
                self.vec(&rgba.map(f64::from));
                self.lit(")");
            }
            NodeKind::Render { convexity } => {
                self.lit("render(convexity = ");
                self.int(convexity);
                self.lit(")");
            }
            NodeKind::Projection { cut, convexity } => {
                self.lit("projection(cut = ");
                self.boolean(*cut);
                self.lit(", convexity = ");
                self.int(convexity);
                self.lit(")");
            }
            NodeKind::Minkowski { convexity } => {
                self.lit("minkowski(convexity = ");
                self.int(convexity);
                self.lit(")");
            }
            NodeKind::Hull => self.lit("hull()"),
            NodeKind::Part { name } => {
                self.lit("part(name = ");
                self.quoted(name);
                self.lit(")");
            }
            NodeKind::Fill => self.lit("fill()"),
            NodeKind::Resize {
                newsize,
                autosize,
                convexity,
            } => {
                // CgalAdvNode::toString: no spaces inside these vectors, and
                // the flags stream as C++ bools (0/1).
                self.lit("resize(newsize = [");
                for (i, &x) in newsize.iter().enumerate() {
                    if i > 0 {
                        self.out.push(b',');
                    }
                    self.num(x);
                }
                self.lit("], auto = [");
                for (i, &b) in autosize.iter().enumerate() {
                    if i > 0 {
                        self.out.push(b',');
                    }
                    self.int(u8::from(b));
                }
                self.lit("], convexity = ");
                self.int(convexity);
                self.lit(")");
            }
            NodeKind::Offset {
                delta,
                chamfer,
                join,
                disc,
            } => {
                let round = *join == OffsetJoin::Round;
                self.lit(if round {
                    "offset(r = "
                } else {
                    "offset(delta = "
                });
                self.num(*delta);
                if !round {
                    self.lit(", chamfer = ");
                    self.boolean(*chamfer);
                }
                self.lit(", ");
                self.disc(disc);
                self.lit(")");
            }
            NodeKind::LinearExtrude(e) => {
                self.lit("linear_extrude(height = ");
                let [x, y, z] = e.height;
                let height = (x * x + y * y + z * z).sqrt();
                self.num(height);
                if height > 0.0 {
                    let v = [x / height, y / height, z / height];
                    if v[2] < 1.0 {
                        self.lit(", v = [ ");
                        self.num(v[0]);
                        self.lit(", ");
                        self.num(v[1]);
                        self.lit(", ");
                        self.num(v[2]);
                        self.lit("]");
                    }
                }
                if self.key() {
                    // The key keeps the exact direction the norm above rounds.
                    self.lit(", height_vector = ");
                    self.vec(&e.height);
                }
                if e.center {
                    self.lit(", center = true");
                }
                if e.has_twist {
                    self.lit(", twist = ");
                    self.num(e.twist);
                }
                if e.has_slices {
                    self.lit(", slices = ");
                    self.int(e.slices);
                }
                if e.has_segments {
                    self.lit(", segments = ");
                    self.int(e.segments);
                }
                let [sx, sy] = e.scale;
                if sx != sy {
                    self.lit(", scale = ");
                    self.vec(&e.scale);
                } else if sx != 1.0 {
                    self.lit(", scale = ");
                    self.num(sx);
                }
                if !(e.has_slices && e.has_segments) || self.key() {
                    self.lit(", ");
                    self.disc(&e.disc);
                }
                if e.convexity > 1 || self.key() {
                    self.lit(", convexity = ");
                    self.int(e.convexity);
                }
                self.lit(")");
            }
            NodeKind::RotateExtrude {
                angle,
                start,
                convexity,
                disc,
            } => {
                self.lit("rotate_extrude(angle = ");
                self.num(*angle);
                self.lit(", start = ");
                self.num(*start);
                self.lit(", convexity = ");
                self.int(convexity);
                self.lit(", ");
                self.disc(disc);
                self.lit(")");
            }
            NodeKind::Cube { size, center } => {
                self.lit("cube(size = ");
                self.vec(size);
                self.lit(", center = ");
                self.boolean(*center);
                self.lit(")");
            }
            NodeKind::Sphere { r, disc } => {
                self.lit("sphere(");
                self.disc(disc);
                self.lit(", r = ");
                self.num(*r);
                self.lit(")");
            }
            NodeKind::Cylinder {
                h,
                r1,
                r2,
                center,
                disc,
            } => {
                self.lit("cylinder(");
                self.disc(disc);
                self.lit(", h = ");
                self.num(*h);
                self.lit(", r1 = ");
                self.num(*r1);
                self.lit(", r2 = ");
                self.num(*r2);
                self.lit(", center = ");
                self.boolean(*center);
                self.lit(")");
            }
            NodeKind::Polyhedron {
                points,
                faces,
                convexity,
            } => {
                self.lit("polyhedron(points = [");
                for (i, p) in points.iter().enumerate() {
                    if i > 0 {
                        self.lit(", ");
                    }
                    self.vec(p);
                }
                self.lit("], faces = ");
                self.indices(faces);
                self.lit(", convexity = ");
                self.int(convexity);
                self.lit(")");
            }
            NodeKind::Square { size, center } => {
                self.lit("square(size = ");
                self.vec(size);
                self.lit(", center = ");
                self.boolean(*center);
                self.lit(")");
            }
            NodeKind::Circle { r, disc } => {
                self.lit("circle(");
                self.disc(disc);
                self.lit(", r = ");
                self.num(*r);
                self.lit(")");
            }
            NodeKind::Polygon {
                points,
                paths,
                convexity,
            } => {
                self.lit("polygon(points = [");
                for (i, p) in points.iter().enumerate() {
                    if i > 0 {
                        self.lit(", ");
                    }
                    self.vec(p);
                }
                self.lit("], paths = ");
                if paths.is_empty() {
                    self.lit("undef");
                } else {
                    self.indices(paths);
                }
                self.lit(", convexity = ");
                self.int(convexity);
                self.lit(")");
            }
            NodeKind::Surface {
                file,
                center,
                invert,
                convexity,
            } => {
                self.lit("surface(file = ");
                self.file(file);
                self.lit(", center = ");
                self.boolean(*center);
                self.lit(", invert = ");
                self.boolean(*invert);
                if self.key() {
                    self.lit(", convexity = ");
                    self.int(convexity);
                }
                self.timestamp(file);
                self.lit(")");
            }
            NodeKind::Import(i) => {
                self.lit("import(file = ");
                self.file(&i.file);
                if let Some(id) = &i.id {
                    self.lit(", id = ");
                    self.quoted(id);
                }
                if let Some(layer) = &i.layer {
                    self.lit(", layer = ");
                    self.quoted(layer);
                }
                self.lit(", origin = ");
                self.vec(&i.origin);
                if i.kind == "svg" || self.key() {
                    self.lit(", dpi = ");
                    self.num(i.dpi);
                }
                if self.key() {
                    self.lit(", width = ");
                    self.num(i.width);
                    self.lit(", height = ");
                    self.num(i.height);
                }
                self.lit(", scale = ");
                self.num(i.scale);
                self.lit(", center = ");
                self.boolean(i.center);
                self.lit(", convexity = ");
                self.int(i.convexity);
                self.lit(", ");
                self.disc(&i.disc);
                self.timestamp(&i.file);
                self.lit(")");
            }
            NodeKind::Text(t) => {
                // operator<<(FreetypeRenderer::Params): the strings go out
                // unescaped, and `script` only when it is non-empty.
                let (script, direction) = text_props::resolve(t);
                self.lit("text(text = ");
                self.raw_quoted(&t.text);
                self.lit(", size = ");
                self.num(t.size);
                self.lit(", spacing = ");
                self.num(t.spacing);
                self.lit(", font = ");
                self.raw_quoted(&t.font);
                self.lit(", direction = ");
                self.raw_quoted(direction);
                self.lit(", language = ");
                self.raw_quoted(&t.language);
                if !script.is_empty() {
                    self.lit(", script = ");
                    self.raw_quoted(&script);
                }
                self.lit(", halign = ");
                self.raw_quoted(&t.halign);
                self.lit(", valign = ");
                self.raw_quoted(&t.valign);
                self.lit(", ");
                self.disc(&t.disc);
                self.lit(")");
            }
        }
    }

    fn indices(&mut self, lists: &[Vec<usize>]) {
        self.out.push(b'[');
        for (i, l) in lists.iter().enumerate() {
            if i > 0 {
                self.lit(", ");
            }
            self.out.push(b'[');
            for (k, x) in l.iter().enumerate() {
                if k > 0 {
                    self.lit(", ");
                }
                self.int(x);
            }
            self.out.push(b']');
        }
        self.out.push(b']');
    }
}

/// The modification time of `file` in nanoseconds since the Unix epoch, or
/// 0 if it does not exist (`fs_timestamp`).
/// 0 as well when the file system cannot tell (`FileSystem::metadata`).
fn mtime_nanos(fs: &dyn FileSystem, file: &str) -> i128 {
    if file.is_empty() {
        return 0;
    }
    fs.metadata(Path::new(file))
        .and_then(|m| m.modified)
        .unwrap_or(0)
}

/// The size of `file` in bytes, or -1 if it does not exist.
fn file_size(fs: &dyn FileSystem, file: &str) -> i128 {
    if file.is_empty() {
        return -1;
    }
    fs.metadata(Path::new(file))
        .map_or(-1, |m| i128::from(m.len))
}

/// `std::filesystem::relative(p, base)` as `fs_uncomplete` calls it:
/// both sides made weakly canonical (symlinks resolved as far as the path
/// exists, then `.` and `..` folded), then `lexically_relative`. An empty
/// path stays empty.
fn fs_relative(fs: &dyn FileSystem, p: &Path, base: &Path) -> PathBuf {
    if p.as_os_str().is_empty() {
        return PathBuf::new();
    }
    lexically_relative(&weakly_canonical(fs, p), &weakly_canonical(fs, base))
}

fn weakly_canonical(fs: &dyn FileSystem, p: &Path) -> PathBuf {
    let comps: Vec<Component<'_>> = p.components().collect();
    for i in (1..=comps.len()).rev() {
        let head: PathBuf = comps[..i].iter().collect();
        if let Some(mut c) = fs.canonicalize(&head) {
            c.extend(&comps[i..]);
            return lexically_normal(&c);
        }
    }
    lexically_normal(p)
}

fn lexically_normal(p: &Path) -> PathBuf {
    let mut out: Vec<Component<'_>> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(c),
            },
            c => out.push(c),
        }
    }
    out.iter().collect()
}

fn lexically_relative(p: &Path, base: &Path) -> PathBuf {
    let a: Vec<Component<'_>> = p.components().collect();
    let b: Vec<Component<'_>> = base.components().collect();
    let root = |v: &[Component<'_>]| {
        v.iter()
            .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
            .count()
    };
    if a[..root(&a)] != b[..root(&b)] {
        return PathBuf::new();
    }
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut n: i64 = 0;
    for c in &b[common..] {
        match c {
            Component::ParentDir => n -= 1,
            Component::Normal(s) if !s.is_empty() => n += 1,
            _ => {}
        }
    }
    if n < 0 {
        return PathBuf::new();
    }
    if n == 0 && common == a.len() {
        return PathBuf::from(".");
    }
    let mut r = PathBuf::new();
    (0..n).for_each(|_| r.push(".."));
    r.extend(&a[common..]);
    r
}

/// Canonical keys for every subtree of a node tree (see the module docs).
#[derive(Debug)]
pub struct Keys {
    /// Each node's Merkle hash, by `Node::index`.
    hashes: Vec<Digest>,
}

type Digest = [u8; 32];

/// First byte of every hashed record, so records of different shapes can
/// never be the same bytes.
const TAG_NODE: u8 = b'N';
const TAG_MODS: u8 = b'M';
const TAG_EMPTY: u8 = b'E';
/// Marks a number's eight raw bytes inside a key label. It is never part
/// of UTF-8, so no literal, integer or quoted string can contain it.
const NUM: u8 = 0xFF;

impl Keys {
    /// Keys for `root`'s tree; imported files are stat'ed through `fs`.
    ///
    /// Sibling subtrees are hashed on rayon's pool (natively; wasm32 has
    /// one thread). Each node's hash depends only on its own subtree and
    /// goes in its node's slot, so the keys are the same at any thread
    /// count and in any schedule.
    pub fn new(root: &Node, fs: &dyn FileSystem) -> Keys {
        fn max_index(n: &Node) -> usize {
            n.children.iter().map(max_index).fold(n.index, usize::max)
        }
        let len = max_index(root) + 1;
        let mut survey = Survey {
            counts: vec![0; len],
            sizes: vec![0; len],
            stats: Stats::default(),
            fs,
        };
        survey.walk(root);
        let hashes: Vec<Slot> = (0..len).map(|_| Slot::default()).collect();
        let shared = Shared {
            counts: &survey.counts,
            sizes: &survey.sizes,
            stats: &survey.stats,
            hashes: &hashes,
            #[cfg(not(target_arch = "wasm32"))]
            threads: rayon::current_num_threads(),
        };
        shared.builder().hash(root);
        Keys {
            // An index no node has (the tree need not use every one)
            // keeps the zero hash; `get` is never asked for it.
            hashes: hashes.into_iter().map(|h| h.get()).collect(),
        }
    }

    /// The key of the subtree rooted at `node`, which must belong to the
    /// tree these keys were built from: 128 bits of its Merkle hash. A
    /// node's own `%`/`#` are not part of its key (they change how its
    /// parent uses it, not what it is), but they are part of the parent's.
    pub fn get(&self, node: &Node) -> u128 {
        let d = &self.hashes[node.index];
        u128::from_le_bytes(d[..16].try_into().expect("16 bytes"))
    }
}

fn is_group(n: &Node) -> bool {
    matches!(n.kind, NodeKind::Root | NodeKind::Group { .. })
}

/// One serial pass over the tree before hashing, collecting what the
/// hashing threads need to read.
struct Survey<'a> {
    /// `GroupNodeChecker`: for each group, how many children have content
    /// (a non-group node, or a group with such a child).
    counts: Vec<u32>,
    /// Each subtree's node count, which decides where hashing is worth
    /// splitting across threads.
    sizes: Vec<usize>,
    /// `FileSystem` is not `Sync`, so imported files are stat'ed here, on
    /// the caller's thread.
    stats: Stats,
    fs: &'a dyn FileSystem,
}

impl Survey<'_> {
    /// Returns whether `n` has content.
    fn walk(&mut self, n: &Node) -> bool {
        let file = match &n.kind {
            NodeKind::Surface { file, .. } => Some(file),
            NodeKind::Import(i) => Some(&i.file),
            _ => None,
        };
        if let Some(f) = file
            && !f.is_empty()
            && !self.stats.0.contains_key(f)
        {
            let m = self.fs.metadata(Path::new(f));
            self.stats.0.insert(f.clone(), m);
        }
        let mut c = 0;
        let mut size = 1;
        for ch in &n.children {
            if self.walk(ch) {
                c += 1;
            }
            size += self.sizes[ch.index];
        }
        self.counts[n.index] = c;
        self.sizes[n.index] = size;
        !is_group(n) || c > 0
    }
}

/// The time and size of every file the tree imports, taken before any
/// hashing starts. A key label asks nothing else of the file system
/// (`.csg` paths, the only other use, are not written in keys), so this
/// stands in for it.
#[derive(Default)]
struct Stats(HashMap<String, Option<Metadata>>);

impl FileSystem for Stats {
    fn read(&self, _: &Path) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
    fn exists(&self, _: &Path) -> bool {
        false
    }
    fn is_dir(&self, _: &Path) -> bool {
        false
    }
    fn canonicalize(&self, _: &Path) -> Option<PathBuf> {
        None
    }
    fn metadata(&self, path: &Path) -> Option<Metadata> {
        // A label that stats a file `Survey::walk` did not look for would get
        // "no such file", and its key would silently lose the file's
        // time and size (edits to it would hit the stale cache entry).
        // A new node kind that names a file must be added there.
        debug_assert!(
            path.to_str().is_some_and(|p| self.0.contains_key(p)),
            "a key label stat'ed a file Survey::walk missed: {path:?}"
        );
        path.to_str().and_then(|p| self.0.get(p).copied()).flatten()
    }
}

/// `%` and `#` of a node as one byte, as its parent sees them.
fn modifier_bits(n: &Node) -> u8 {
    n.origin.as_ref().map_or(0, |o| {
        u8::from(o.tag_background) | (u8::from(o.tag_highlight) << 1)
    })
}

/// Builds the keys bottom-up. A key used to be the whole subtree's text,
/// hashed separately at every node, which costs the tree size times its
/// depth: BOSL2's `attach()` recursion in `examples/fractal_tree.scad`
/// makes a 475 MB dump and spent about 9 s in SHA-256. Hashing each node's
/// own label once, together with its children's fixed-size hashes, is
/// linear in the dump size and just as exact, because every record is
/// self-delimiting (a tag byte, the label's length, the child count, then
/// 32 bytes per child).
struct KeyBuilder<'a> {
    w: Writer<'a>,
    s: Shared<'a>,
}

/// What every hashing thread reads, and the slots they fill: each node's
/// exactly once, by the one thread that hashes its subtree.
#[derive(Clone, Copy)]
struct Shared<'a> {
    counts: &'a [u32],
    sizes: &'a [usize],
    stats: &'a Stats,
    hashes: &'a [Slot],
    /// The pool's size: with one thread, splitting only adds overhead.
    #[cfg(not(target_arch = "wasm32"))]
    threads: usize,
}

/// A node whose subtree has at least this many nodes hashes its children
/// on the pool. Below it a task costs more than the hashing it would
/// spread (a node's label and SHA-256 take about a microsecond).
#[cfg(not(target_arch = "wasm32"))]
const PARALLEL_MIN_NODES: usize = 512;

impl<'a> Shared<'a> {
    fn builder(self) -> KeyBuilder<'a> {
        KeyBuilder {
            w: Writer {
                out: Vec::new(),
                style: Style::Key,
                base: Path::new(""),
                fs: self.stats,
            },
            s: self,
        }
    }

    fn hash_of(&self, n: &Node) -> Digest {
        self.hashes[n.index].get()
    }
}

/// One node's hash, writable from whichever thread hashes that node.
/// Relaxed atomics compile to plain loads and stores; the ordering comes
/// from the tree: a slot is written once, by the thread that hashes its
/// subtree, and read afterwards by its parent, either on the same thread
/// or after rayon's `for_each` returns, which synchronises with every
/// task it ran. (A `OnceLock` per node costs a compare-and-swap per node,
/// which showed on one thread.)
#[derive(Default)]
struct Slot([AtomicU64; 4]);

impl Slot {
    fn set(&self, d: Digest) {
        for (a, w) in self.0.iter().zip(d.as_chunks::<8>().0) {
            a.store(u64::from_le_bytes(*w), Ordering::Relaxed);
        }
    }

    fn get(&self) -> Digest {
        let mut d = [0; 32];
        for (a, w) in self.0.iter().zip(d.as_chunks_mut::<8>().0) {
            *w = a.load(Ordering::Relaxed).to_le_bytes();
        }
        d
    }
}

impl KeyBuilder<'_> {
    fn hash(&mut self, n: &Node) -> Digest {
        #[cfg(not(target_arch = "wasm32"))]
        if self.s.threads > 1 && n.children.len() > 1 && self.s.sizes[n.index] >= PARALLEL_MIN_NODES
        {
            use rayon::prelude::*;
            let s = self.s;
            n.children.par_iter().for_each_init(
                || s.builder(),
                |b, c| {
                    b.hash(c);
                },
            );
        } else {
            for c in &n.children {
                self.hash(c);
            }
        }
        #[cfg(target_arch = "wasm32")]
        for c in &n.children {
            self.hash(c);
        }
        let d = if is_group(n) && self.s.counts[n.index] <= 1 {
            self.transparent(n)
        } else {
            self.w.out.clear();
            self.w.label(n);
            let mut h = Sha256::new();
            h.update([TAG_NODE]);
            h.update((self.w.out.len() as u64).to_le_bytes());
            h.update(&self.w.out);
            h.update((n.children.len() as u64).to_le_bytes());
            for c in &n.children {
                h.update([modifier_bits(c)]);
                h.update(self.s.hash_of(c));
            }
            h.finalize().into()
        };
        self.s.hashes[n.index].set(d);
        d
    }

    /// A group with at most one child that has content takes that child's
    /// key, as `Tree::getIdString` leaves such groups out: `group() {
    /// cube(); }` and `cube()` share a key. Children without content are
    /// empty groups and are left out whatever their modifiers, but they
    /// are not always inert. In 3D the union drops them, so the group
    /// computes exactly what its child does. In 2D it does not:
    /// `group() { group(); square(1); }` unions `[nothing, square]`
    /// through Clipper, which snaps the square to Clipper's grid, while
    /// the square alone passes through (OpenSCAD's `applyToChildren2D`
    /// does the same). So equal keys here do not promise equal geometry.
    /// `geom`'s evaluator gives such groups keys of their own
    /// (`cache_key`); anything else caching on these keys must do the
    /// same, or its result depends on which of the two it computed first.
    /// A `%`/`#` on the content child changes the group's result (a
    /// background child is skipped), so it is hashed in rather than lost.
    fn transparent(&self, n: &Node) -> Digest {
        let content = n
            .children
            .iter()
            .find(|c| !is_group(c) || self.s.counts[c.index] > 0);
        match content {
            None => Sha256::digest([TAG_EMPTY]).into(),
            Some(c) => match modifier_bits(c) {
                0 => self.s.hash_of(c),
                m => {
                    let mut h = Sha256::new();
                    h.update([TAG_MODS, m]);
                    h.update(self.s.hash_of(c));
                    h.finalize().into()
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Origin;
    use lang::loader::StdFs;
    use lang::source::Span;

    fn node(kind: NodeKind, index: usize, children: Vec<Node>) -> Node {
        let origin = Origin {
            name: String::new(),
            unit: 0,
            span: Span::default(),
            line: 1,
            tag_root: false,
            tag_highlight: false,
            tag_background: false,
        };
        Node {
            kind,
            children,
            origin: Some(Box::new(origin)),
            index,
        }
    }

    fn cube(s: f64, index: usize) -> Node {
        node(
            NodeKind::Cube {
                size: [s; 3],
                center: false,
            },
            index,
            vec![],
        )
    }

    #[test]
    fn csg_layout() {
        let mut hl = cube(2.0, 3);
        hl.origin.as_mut().unwrap().tag_highlight = true;
        hl.origin.as_mut().unwrap().tag_background = true;
        let g = node(NodeKind::Group { name: None }, 1, vec![cube(1.0, 2), hl]);
        let root = Node {
            kind: NodeKind::Root,
            children: vec![g],
            origin: None,
            index: 0,
        };
        assert_eq!(
            csg(&root, Path::new("/"), &StdFs),
            "group() {\n\tcube(size = [1, 1, 1], center = false);\n%#\tcube(size = [2, 2, 2], center = false);\n}\n\n"
        );
    }

    #[test]
    fn keys_are_exact_and_skip_single_child_groups() {
        let a = node(NodeKind::Group { name: None }, 1, vec![cube(1.0, 2)]);
        let b = cube(1.0000001, 3);
        let c = cube(1.0, 4);
        let root = Node {
            kind: NodeKind::Root,
            children: vec![a, b, c],
            origin: None,
            index: 0,
        };
        let k = Keys::new(&root, &StdFs);
        // The label hashed for a node keeps every bit of its numbers.
        let mut w = Writer {
            out: Vec::new(),
            style: Style::Key,
            base: Path::new(""),
            fs: &StdFs,
        };
        w.label(&root.children[1]);
        let mut want = b"cube(size=[".to_vec();
        for i in 0..3 {
            if i > 0 {
                want.push(b',');
            }
            want.push(NUM);
            want.extend_from_slice(&1.0000001f64.to_bits().to_le_bytes());
        }
        want.extend_from_slice(b"],center=false)");
        assert_eq!(w.out, want);
        let [a, b, c] = [0, 1, 2].map(|i| k.get(&root.children[i]));
        assert_eq!(a, k.get(&root.children[0].children[0]));
        assert_eq!(a, c);
        assert_ne!(a, b);
        assert_ne!(k.get(&root), a);
    }

    #[test]
    fn keys_see_child_modifiers_and_order() {
        let group = |index, children| node(NodeKind::Group { name: None }, index, children);
        let mut bg = cube(1.0, 2);
        bg.origin.as_mut().unwrap().tag_background = true;
        // `group() { %cube(1); }` makes nothing; `cube(1)` makes a cube.
        let tree = group(0, vec![group(1, vec![bg]), cube(1.0, 3)]);
        let k = Keys::new(&tree, &StdFs);
        assert_ne!(k.get(&tree.children[0]), k.get(&tree.children[1]));
        // The same children in another order are another union key, and
        // empty groups beside a single child don't hide it.
        let ab = group(0, vec![cube(1.0, 1), cube(2.0, 2)]);
        let ba = group(0, vec![cube(2.0, 1), cube(1.0, 2)]);
        assert_ne!(
            Keys::new(&ab, &StdFs).get(&ab),
            Keys::new(&ba, &StdFs).get(&ba)
        );
        let padded = group(0, vec![group(1, vec![]), cube(1.0, 2), group(3, vec![])]);
        let bare = cube(1.0, 0);
        assert_eq!(
            Keys::new(&padded, &StdFs).get(&padded),
            Keys::new(&bare, &StdFs).get(&bare)
        );
    }

    #[test]
    fn number_keys_split_zeros_and_merge_nans() {
        let key = |s: f64| Keys::new(&cube(s, 0), &StdFs).get(&cube(s, 0));
        // `-0` and `0` can make different output (`-0` coordinates), so
        // they stay apart; neighbouring doubles do too.
        assert_ne!(key(0.0), key(-0.0));
        assert_ne!(key(1.0), key(f64::from_bits(1.0f64.to_bits() + 1)));
        // Every NaN is one key, whatever its sign or payload.
        let odd_nan = f64::from_bits(f64::NAN.to_bits() | 0x8000_0000_0000_0001);
        assert!(odd_nan.is_nan());
        assert_eq!(key(f64::NAN), key(odd_nan));
        assert_eq!(key(f64::NAN), key(-f64::NAN));
        assert_ne!(key(f64::NAN), key(f64::INFINITY));
    }

    #[test]
    fn number_bytes_cannot_pose_as_text() {
        // Text is UTF-8, which never holds the byte that opens a number,
        // even for the highest code points; a number is that byte plus 8.
        let part = |name: &str| {
            node(
                NodeKind::Part {
                    name: name.to_string(),
                },
                0,
                vec![],
            )
        };
        let label = |n: &Node| {
            let mut w = Writer {
                out: Vec::new(),
                style: Style::Key,
                base: Path::new(""),
                fs: &StdFs,
            };
            w.label(n);
            w.out
        };
        let text = label(&part("\u{ff}\u{fffe}\u{10ffff}\"\\"));
        assert!(!text.contains(&NUM));
        let c = label(&cube(1.0, 0));
        assert_eq!(c.iter().filter(|&&b| b == NUM).count(), 3);
    }

    #[test]
    fn relative_paths() {
        let r = |p: &str, b: &str| {
            lexically_relative(Path::new(p), Path::new(b))
                .display()
                .to_string()
        };
        assert_eq!(r("/a/b/c.stl", "/a/b"), "c.stl");
        assert_eq!(r("/a/x/c.stl", "/a/b"), "../x/c.stl");
        assert_eq!(r("/a/b", "/a/b"), ".");
        assert_eq!(
            lexically_normal(Path::new("/a/b/../c/./d")),
            Path::new("/a/c/d")
        );
    }

    /// `Keys::new` hashes sibling subtrees on rayon's pool. Every key
    /// (not only the root's) must be the same on one thread, where it
    /// never splits, and on several, where it splits at every level of
    /// this tree: wide levels, a deep chain, repeated subtrees (equal
    /// keys under different indices), single-child groups (whose key is
    /// their child's) and `%`/`#` on children.
    #[test]
    fn keys_are_the_same_at_any_thread_count() {
        fn tree(next: &mut usize, depth: u32) -> Node {
            let index = *next;
            *next += 2;
            if depth == 0 {
                return cube((index % 7) as f64, index);
            }
            let width = if depth.is_multiple_of(2) { 2 } else { 9 };
            let mut children: Vec<Node> = (0..width).map(|_| tree(next, depth - 1)).collect();
            if depth == 3 {
                children[0].origin.as_mut().unwrap().tag_background = true;
                children[1].origin.as_mut().unwrap().tag_highlight = true;
            }
            let kind = match depth % 3 {
                0 => NodeKind::Group { name: None },
                1 => NodeKind::Csg(CsgOp::Union),
                _ => NodeKind::Csg(CsgOp::Difference),
            };
            let n = node(kind, index + 1, children);
            // A single-child group above each subtree: transparent keys.
            node(NodeKind::Group { name: None }, index, vec![n])
        }
        let mut next = 1;
        let root = Node {
            kind: NodeKind::Root,
            children: (0..3).map(|_| tree(&mut next, 6)).collect(),
            origin: None,
            index: 0,
        };
        let keys = |threads: usize| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap()
                .install(|| Keys::new(&root, &StdFs))
                .hashes
        };
        let one = keys(1);
        assert!(root.children.iter().all(|c| one[c.index] != [0; 32]));
        assert_eq!(one, keys(4));
        assert_eq!(one, keys(13));
    }
}
