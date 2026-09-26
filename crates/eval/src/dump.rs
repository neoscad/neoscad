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
//!   transparent, so `group() { cube(); }` and `cube()` share a key. Unlike
//!   OpenSCAD it is exact: numbers print with Rust's shortest round-trip
//!   form instead of 6 digits (OpenSCAD's 6-digit key makes `cube(1)` and
//!   `cube(1.0000001)` share cached geometry), files are absolute with a
//!   nanosecond timestamp and the file size (a file edited twice in one
//!   second still misses), every string is quoted and escaped (OpenSCAD
//!   writes `text()` strings raw, so a `"` in the text can make two keys
//!   equal), and parameters the dump leaves out but geometry uses are
//!   included. All of a tree's keys are slices of one string built in a
//!   single pass, as OpenSCAD's `NodeCache` does, so computing them is
//!   linear in the dump size.

use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use lang::number::fmt_g;

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
    out: String,
    style: Style,
    /// The document directory `.csg` file names are made relative to.
    base: &'a Path,
}

/// The `.csg` export of `top`: the root's children, or the node the root
/// modifier (`!`) picked, with the trailing newline `openscad.cc` adds.
pub fn csg(top: &Node, doc_dir: &Path) -> String {
    let mut w = Writer { out: String::new(), style: Style::Csg, base: doc_dir };
    if top.kind == NodeKind::Root {
        for c in &top.children {
            w.csg_node(c, 0);
        }
    } else {
        w.csg_node(top, 0);
    }
    w.out.push('\n');
    w.out
}

impl Writer<'_> {
    fn csg_node(&mut self, n: &Node, depth: usize) {
        self.modifiers(n);
        (0..depth).for_each(|_| self.out.push('\t'));
        self.label(n);
        if n.children.is_empty() {
            self.out.push_str(";\n");
        } else {
            self.out.push_str(" {\n");
            for c in &n.children {
                self.csg_node(c, depth + 1);
            }
            (0..depth).for_each(|_| self.out.push('\t'));
            self.out.push_str("}\n");
        }
    }

    /// `%` then `#`, from the node's own instantiation (list nodes, which
    /// would pass them down, exist only with the lazy-union experiment).
    fn modifiers(&mut self, n: &Node) {
        if let Some(o) = &n.origin {
            if o.tag_background {
                self.out.push('%');
            }
            if o.tag_highlight {
                self.out.push('#');
            }
        }
    }

    fn key(&self) -> bool {
        self.style == Style::Key
    }

    /// Literal label text; the key drops its spaces.
    fn lit(&mut self, s: &str) {
        if self.key() {
            self.out.extend(s.chars().filter(|&c| c != ' '));
        } else {
            self.out.push_str(s);
        }
    }

    fn num(&mut self, v: f64) {
        if self.key() {
            self.out.push_str(&format!("{v:?}"));
        } else {
            self.out.push_str(&fmt_g(v));
        }
    }

    fn int(&mut self, v: impl std::fmt::Display) {
        self.out.push_str(&v.to_string());
    }

    fn boolean(&mut self, b: bool) {
        self.out.push_str(if b { "true" } else { "false" });
    }

    /// `QuotedString`'s `operator<<`.
    fn quoted(&mut self, s: &str) {
        self.out.push('"');
        for c in s.chars() {
            match c {
                '\t' => self.out.push_str("\\t"),
                '\n' => self.out.push_str("\\n"),
                '\r' => self.out.push_str("\\r"),
                '"' => self.out.push_str("\\\""),
                '\\' => self.out.push_str("\\\\"),
                c => self.out.push(c),
            }
        }
        self.out.push('"');
    }

    /// A string OpenSCAD writes between quotes without escaping (the
    /// `text()` parameters). The key escapes it so it stays unambiguous.
    fn raw_quoted(&mut self, s: &str) {
        if self.key() {
            self.quoted(s);
        } else {
            self.out.push('"');
            self.out.push_str(s);
            self.out.push('"');
        }
    }

    fn vec(&mut self, v: &[f64]) {
        self.out.push('[');
        for (i, &x) in v.iter().enumerate() {
            if i > 0 {
                self.lit(", ");
            }
            self.num(x);
        }
        self.out.push(']');
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
            let rel = fs_relative(Path::new(file), self.base);
            self.quoted(&rel.to_string_lossy());
        }
    }

    fn timestamp(&mut self, file: &str) {
        self.lit(", timestamp = ");
        let t = mtime_nanos(file);
        if self.key() {
            self.int(t);
            // The key also holds the size, so a file rewritten within the
            // file system's timestamp resolution still misses the cache
            // (imported geometry is cached under this key).
            self.lit(", size = ");
            self.int(file_size(file));
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
            NodeKind::Fill => self.lit("fill()"),
            NodeKind::Resize { newsize, autosize, convexity } => {
                // CgalAdvNode::toString: no spaces inside these vectors, and
                // the flags stream as C++ bools (0/1).
                self.lit("resize(newsize = [");
                for (i, &x) in newsize.iter().enumerate() {
                    if i > 0 {
                        self.out.push(',');
                    }
                    self.num(x);
                }
                self.lit("], auto = [");
                for (i, &b) in autosize.iter().enumerate() {
                    if i > 0 {
                        self.out.push(',');
                    }
                    self.int(u8::from(b));
                }
                self.lit("], convexity = ");
                self.int(convexity);
                self.lit(")");
            }
            NodeKind::Offset { delta, chamfer, join, disc } => {
                let round = *join == OffsetJoin::Round;
                self.lit(if round { "offset(r = " } else { "offset(delta = " });
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
            NodeKind::RotateExtrude { angle, start, convexity, disc } => {
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
            NodeKind::Cylinder { h, r1, r2, center, disc } => {
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
            NodeKind::Polyhedron { points, faces, convexity } => {
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
            NodeKind::Polygon { points, paths, convexity } => {
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
            NodeKind::Surface { file, center, invert, convexity } => {
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
        self.out.push('[');
        for (i, l) in lists.iter().enumerate() {
            if i > 0 {
                self.lit(", ");
            }
            self.out.push('[');
            for (k, x) in l.iter().enumerate() {
                if k > 0 {
                    self.lit(", ");
                }
                self.int(x);
            }
            self.out.push(']');
        }
        self.out.push(']');
    }
}

/// The modification time of `file` in nanoseconds since the Unix epoch, or
/// 0 if it does not exist (`fs_timestamp`).
fn mtime_nanos(file: &str) -> i128 {
    if file.is_empty() {
        return 0;
    }
    let Ok(t) = std::fs::metadata(file).and_then(|m| m.modified()) else { return 0 };
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

/// The size of `file` in bytes, or -1 if it does not exist.
fn file_size(file: &str) -> i128 {
    if file.is_empty() {
        return -1;
    }
    std::fs::metadata(file).map_or(-1, |m| i128::from(m.len()))
}

/// `std::filesystem::relative(p, base)` as `fs_uncomplete` calls it:
/// both sides made weakly canonical (symlinks resolved as far as the path
/// exists, then `.` and `..` folded), then `lexically_relative`. An empty
/// path stays empty.
fn fs_relative(p: &Path, base: &Path) -> PathBuf {
    if p.as_os_str().is_empty() {
        return PathBuf::new();
    }
    lexically_relative(&weakly_canonical(p), &weakly_canonical(base))
}

fn weakly_canonical(p: &Path) -> PathBuf {
    let comps: Vec<Component<'_>> = p.components().collect();
    for i in (1..=comps.len()).rev() {
        let head: PathBuf = comps[..i].iter().collect();
        if let Ok(mut c) = head.canonicalize() {
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
    let root = |v: &[Component<'_>]| v.iter().take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir)).count();
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
    text: String,
    /// Byte range of each node's key in `text`, by `Node::index`.
    spans: Vec<(usize, usize)>,
}

impl Keys {
    pub fn new(root: &Node) -> Keys {
        fn max_index(n: &Node) -> usize {
            n.children.iter().map(max_index).fold(n.index, usize::max)
        }
        let len = max_index(root) + 1;
        let mut counts = vec![0u32; len];
        content_counts(root, &mut counts);
        let mut w = Writer { out: String::new(), style: Style::Key, base: Path::new("") };
        let mut spans = vec![(0, 0); len];
        w.key_node(root, &counts, &mut spans);
        Keys { text: w.out, spans }
    }

    /// The key of the subtree rooted at `node`, which must belong to the
    /// tree these keys were built from. A node's own `%`/`#` are not part of
    /// its key (they change how its parent uses it, not what it is), but
    /// they are part of the parent's.
    pub fn get(&self, node: &Node) -> &str {
        let (a, b) = self.spans[node.index];
        &self.text[a..b]
    }
}

fn is_group(n: &Node) -> bool {
    matches!(n.kind, NodeKind::Root | NodeKind::Group { .. })
}

/// `GroupNodeChecker`: for each group, how many children have content (a
/// non-group node, or a group with such a child). Returns whether `n` has
/// content.
fn content_counts(n: &Node, counts: &mut [u32]) -> bool {
    let mut c = 0;
    for ch in &n.children {
        if content_counts(ch, counts) {
            c += 1;
        }
    }
    counts[n.index] = c;
    !is_group(n) || c > 0
}

impl Writer<'_> {
    fn key_node(&mut self, n: &Node, counts: &[u32], spans: &mut [(usize, usize)]) {
        self.modifiers(n);
        let start = self.out.len();
        if is_group(n) {
            let wrap = counts[n.index] > 1;
            if wrap {
                self.label(n);
                self.out.push('{');
            }
            for c in &n.children {
                self.key_node(c, counts, spans);
            }
            if wrap {
                self.out.push('}');
            }
        } else {
            self.label(n);
            if n.children.is_empty() {
                self.out.push(';');
            } else {
                self.out.push('{');
                for c in &n.children {
                    self.key_node(c, counts, spans);
                }
                self.out.push('}');
            }
        }
        spans[n.index] = (start, self.out.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Origin;
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
        Node { kind, children, origin: Some(Box::new(origin)), index }
    }

    fn cube(s: f64, index: usize) -> Node {
        node(NodeKind::Cube { size: [s; 3], center: false }, index, vec![])
    }

    #[test]
    fn csg_layout() {
        let mut hl = cube(2.0, 3);
        hl.origin.as_mut().unwrap().tag_highlight = true;
        hl.origin.as_mut().unwrap().tag_background = true;
        let g = node(NodeKind::Group { name: None }, 1, vec![cube(1.0, 2), hl]);
        let root = Node { kind: NodeKind::Root, children: vec![g], origin: None, index: 0 };
        assert_eq!(
            csg(&root, Path::new("/")),
            "group() {\n\tcube(size = [1, 1, 1], center = false);\n%#\tcube(size = [2, 2, 2], center = false);\n}\n\n"
        );
    }

    #[test]
    fn keys_are_exact_and_skip_single_child_groups() {
        let a = node(NodeKind::Group { name: None }, 1, vec![cube(1.0, 2)]);
        let b = cube(1.0000001, 3);
        let root = Node { kind: NodeKind::Root, children: vec![a, b], origin: None, index: 0 };
        let k = Keys::new(&root);
        assert_eq!(k.get(&root.children[0]), "cube(size=[1.0,1.0,1.0],center=false);");
        assert_eq!(k.get(&root.children[0]), k.get(&root.children[0].children[0]));
        assert_ne!(k.get(&root.children[0]), k.get(&root.children[1]));
        assert!(k.get(&root).starts_with("root(){cube("));
    }

    #[test]
    fn relative_paths() {
        let r = |p: &str, b: &str| lexically_relative(Path::new(p), Path::new(b)).display().to_string();
        assert_eq!(r("/a/b/c.stl", "/a/b"), "c.stl");
        assert_eq!(r("/a/x/c.stl", "/a/b"), "../x/c.stl");
        assert_eq!(r("/a/b", "/a/b"), ".");
        assert_eq!(lexically_normal(Path::new("/a/b/../c/./d")), Path::new("/a/c/d"));
    }
}
