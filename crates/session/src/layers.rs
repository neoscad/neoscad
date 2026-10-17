//! 2D exports that keep the model's parts apart (`--enable part`).
//!
//! An SVG or DXF export normally writes the 2D result as one union, as
//! OpenSCAD does, so a drawing of many touching members loses every
//! boundary between them. When the part extension is on and the model has
//! `part()` nodes, the export instead writes one layer per part name
//! (`<g id="NAME">` in SVG, a layer named after it in DXF) and one for the
//! geometry outside every part, each from its own render ([`split`]).
//!
//! A layer's geometry is the model's tree pruned to what the layer owns:
//! every leaf belongs to its innermost enclosing part (or to no part), and
//! the pruned tree keeps a leaf only when its owner is the layer, along
//! with every operation above the leaves it keeps. The operations keep
//! their meaning where a subset of their children would change it:
//!
//! - a `difference()` keeps all its subtracted children whole (so a part
//!   cut by a hole stays cut), and owns nothing when its first child is
//!   pruned away (so a subtracted part has no outline of its own);
//! - an `intersection()` keeps whole the children it would otherwise lose;
//! - every other node keeps its pruned children, `projection()` and the
//!   extrusions included, so the parts of a 3D model drawn through
//!   `projection()` are kept apart too.
//!
//! Each layer is then clipped to the model's own 2D result, so no layer
//! draws anything the model does not have (a `hull()` or `minkowski()` of
//! several parts is drawn per part, within the model's outline). Layers
//! may overlap where parts do; that is the point of a framing drawing.

use eval::node::{CsgOp, Node, NodeKind};
use geom::Geometry;
use geom::polygon2d::Polygon2d;

/// One layer of a part-split 2D export.
#[derive(Debug, Clone)]
pub struct Layer {
    /// The part's full dotted name; `None` for the geometry outside every
    /// part.
    pub name: Option<String>,
    /// The innermost `color()` that colours all of the part (around the
    /// part call, or around everything inside it).
    pub color: Option<[f32; 4]>,
    pub outline: Polygon2d,
}

impl Layer {
    /// The file writers' view.
    pub fn io(&self) -> io::Layer<'_> {
        io::Layer {
            name: self.name.as_deref(),
            color: self.color,
            outlines: &self.outline.outlines,
        }
    }
}

/// Every part name under `top` in order of first appearance, with its
/// colour (see [`Layer::color`]). Background (`%`) subtrees are skipped,
/// as the render skips them.
pub fn part_names(top: &Node) -> Vec<(String, Option<[f32; 4]>)> {
    let mut out: Vec<(String, Option<[f32; 4]>)> = Vec::new();
    // Pre-order from an explicit stack, as `parts::find`: a recursive
    // module can make a tree too deep to walk recursively.
    let mut stack: Vec<(&Node, Option<[f32; 4]>)> = vec![(top, None)];
    while let Some((n, above)) = stack.pop() {
        if n.origin.as_ref().is_some_and(|o| o.tag_background) {
            continue;
        }
        let mut colour = above;
        match &n.kind {
            NodeKind::Color { rgba } => colour = Some(*rgba),
            NodeKind::Part { name } if !out.iter().any(|(m, _)| m == name) => {
                out.push((name.clone(), inner_colour(n).or(above)));
            }
            _ => {}
        }
        for c in n.children.iter().rev() {
            stack.push((c, colour));
        }
    }
    out
}

/// The `color()` that everything inside part node `n` is under: the first
/// one down its chain of only children (`part("x") color("red") ...`).
fn inner_colour(n: &Node) -> Option<[f32; 4]> {
    let mut n = n;
    while let [only] = n.children.as_slice() {
        match &only.kind {
            NodeKind::Color { rgba } => return Some(*rgba),
            NodeKind::Part { .. } => return None,
            _ => n = only,
        }
    }
    None
}

/// Whether `top` has a part to split by.
pub fn has_parts(top: &Node) -> bool {
    !crate::parts::find(top).is_empty()
}

/// The layers of `top`, whose 2D result is `root`: the geometry outside
/// every part first, then each part name in order of first appearance,
/// each rendered by `render` and clipped to `root`. Empty layers are left
/// out. `render` returns `None` for a tree that renders nothing (or could
/// not be rendered).
pub fn split(
    top: &Node,
    root: &Polygon2d,
    render: &mut dyn FnMut(&Node) -> Option<Geometry>,
) -> Vec<Layer> {
    let mut wanted: Vec<(Option<String>, Option<[f32; 4]>)> = vec![(None, None)];
    wanted.extend(part_names(top).into_iter().map(|(n, c)| (Some(n), c)));
    let mut out = Vec::new();
    for (name, color) in wanted {
        let Some(tree) = prune(top, name.as_deref()) else {
            continue;
        };
        let Some(Geometry::Polygon2d(p)) = render(&tree) else {
            continue;
        };
        let outline =
            geom::clipper::apply(&[Some(&p), Some(root)], geom::clipper::Op2::Intersection);
        if outline.is_empty() {
            continue;
        }
        out.push(Layer {
            name,
            color,
            outline,
        });
    }
    out
}

/// A copy of `n`'s own fields with `children`.
fn with_children(n: &Node, children: Vec<Node>) -> Node {
    let mut c = Node::new(n.kind.clone(), n.origin.clone(), n.index);
    c.anchors = n.anchors.clone();
    c.children = children;
    c
}

/// `top` pruned to the leaves `owner` owns (see the module docs); `None`
/// when it owns nothing there.
pub fn prune(top: &Node, owner: Option<&str>) -> Option<Node> {
    struct Frame<'a> {
        node: &'a Node,
        /// The innermost part this node is in (itself included).
        within: Option<&'a str>,
        kids: Vec<Option<Node>>,
    }
    fn open<'a>(node: &'a Node, above: Option<&'a str>) -> Frame<'a> {
        let within = match &node.kind {
            NodeKind::Part { name } => Some(name.as_str()),
            _ => above,
        };
        Frame {
            node,
            within,
            kids: Vec::with_capacity(node.children.len()),
        }
    }
    // Post-order from an explicit stack: a tree as deep as the evaluator
    // allows must not overflow the native stack here.
    let mut stack = vec![open(top, None)];
    loop {
        let f = stack.last_mut().expect("the top is popped last");
        let next = f.kids.len();
        if let Some(c) = f.node.children.get(next) {
            if c.origin.as_ref().is_some_and(|o| o.tag_background) {
                // Not rendered either way: kept as it is.
                f.kids.push(Some(c.clone()));
            } else {
                let within = f.within;
                stack.push(open(c, within));
            }
            continue;
        }
        let f = stack.pop().expect("just looked at it");
        let n = f.node;
        let done = if n.children.is_empty() {
            (f.within == owner).then(|| n.clone())
        } else {
            match &n.kind {
                NodeKind::Csg(CsgOp::Difference) => {
                    let mut kids = f.kids.into_iter();
                    kids.next().flatten().map(|first| {
                        let mut children = vec![first];
                        children.extend(n.children[1..].iter().cloned());
                        with_children(n, children)
                    })
                }
                NodeKind::Csg(CsgOp::Intersection) | NodeKind::IntersectionFor => {
                    if f.kids.iter().all(Option::is_none) {
                        None
                    } else {
                        let children = f
                            .kids
                            .into_iter()
                            .zip(&n.children)
                            .map(|(k, c)| k.unwrap_or_else(|| c.clone()))
                            .collect();
                        Some(with_children(n, children))
                    }
                }
                _ => {
                    let children: Vec<Node> = f.kids.into_iter().flatten().collect();
                    (!children.is_empty()).then(|| with_children(n, children))
                }
            }
        };
        match stack.last_mut() {
            Some(parent) => parent.kids.push(done),
            None => return done,
        }
    }
}
