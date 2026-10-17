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
//! - a `minkowski()`, `hull()` or `resize()` with parts in it gives the
//!   geometry in it outside those parts to each of them, kept whole in
//!   each part's layer and with no layer of its own, so `minkowski() {
//!   part("p") square(10); circle(1); }` draws p rounded rather than p
//!   square plus a stray circle;
//! - every other node keeps its pruned children, `projection()` and the
//!   extrusions included, so the parts of a 3D model drawn through
//!   `projection()` are kept apart too.
//!
//! Background (`%`) children are kept as they are, since the render skips
//! them, but never count as something a layer owns: otherwise a `%` first
//! child would pull a `difference()` into every layer.
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

/// `rgba` as a colour, or `None` for the "no colour" `color()` stores for
/// an undefined or unknown colour (negative components). Taking it as a
/// colour would paint the part with alpha `-1`, an invisible SVG path.
fn real_colour(rgba: [f32; 4]) -> Option<[f32; 4]> {
    rgba.iter().all(|c| *c >= 0.0).then_some(rgba)
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
            NodeKind::Color { rgba } => colour = real_colour(*rgba).or(above),
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
            NodeKind::Color { rgba } if real_colour(*rgba).is_some() => return Some(*rgba),
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
    let boxes: Vec<Option<Bounds>> = root.outlines.iter().map(outline_bounds).collect();
    let mut out = Vec::new();
    for (name, color) in wanted {
        let Some(tree) = prune(top, name.as_deref()) else {
            continue;
        };
        let Some(Geometry::Polygon2d(p)) = render(&tree) else {
            continue;
        };
        let near = near(root, &boxes, &p);
        let outline =
            geom::clipper::apply(&[Some(&p), Some(&near)], geom::clipper::Op2::Intersection);
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

type Bounds = ([f64; 2], [f64; 2]);

fn outline_bounds(o: &geom::polygon2d::Outline) -> Option<Bounds> {
    let mut it = o.vertices.iter();
    let first = *it.next()?;
    Some(it.fold((first, first), |(lo, hi), v| {
        (
            [lo[0].min(v[0]), lo[1].min(v[1])],
            [hi[0].max(v[0]), hi[1].max(v[1])],
        )
    }))
}

/// The outlines of `root` (with bounds `boxes`) that can meet `layer`.
///
/// Clipping every layer against the whole model made the split quadratic:
/// a drawing of 1600 separate parts took 24 s against 0.07 s for its
/// union, each of its 1600 clips sweeping all 1600 outlines. An outline
/// whose bounds miss the layer's adds no winding anywhere the layer is, so
/// leaving it out does not change the intersection. The bounds are padded
/// well past Clipper's grid (2^-27), so snapping cannot carry a vertex
/// across the gap.
fn near(root: &Polygon2d, boxes: &[Option<Bounds>], layer: &Polygon2d) -> Polygon2d {
    const PAD: f64 = 1e-6;
    let Some((lo, hi)) = layer.bounds() else {
        return Polygon2d {
            outlines: Vec::new(),
            sanitized: root.sanitized,
        };
    };
    let meets = |b: &Option<Bounds>| {
        b.is_some_and(|(blo, bhi)| (0..2).all(|i| blo[i] <= hi[i] + PAD && lo[i] <= bhi[i] + PAD))
    };
    Polygon2d {
        outlines: root
            .outlines
            .iter()
            .zip(boxes)
            .filter(|(_, b)| meets(b))
            .map(|(o, _)| o.clone())
            .collect(),
        sanitized: root.sanitized,
    }
}

/// A copy of `n`'s own fields with `children`.
fn with_children(n: &Node, children: Vec<Node>) -> Node {
    let mut c = Node::new(n.kind.clone(), n.origin.clone(), n.index);
    c.anchors = n.anchors.clone();
    c.children = children;
    c
}

fn is_background(n: &Node) -> bool {
    n.origin.as_ref().is_some_and(|o| o.tag_background)
}

/// `top` pruned to the leaves `owner` owns (see the module docs); `None`
/// when it owns nothing there.
pub fn prune(top: &Node, owner: Option<&str>) -> Option<Node> {
    /// What a child left of itself.
    struct Kid {
        node: Option<Node>,
        /// A `%` child, kept whole and never owned.
        background: bool,
        /// It keeps a leaf of `owner` itself, not only geometry it was
        /// given by an enclosing `hull()` and the like.
        hit: bool,
        /// It has a leaf (other than one only subtracted) that neither
        /// `owner` nor what it was given owns.
        foreign: bool,
    }
    struct Frame<'a> {
        node: &'a Node,
        /// The innermost part this node is in (itself included).
        within: Option<&'a str>,
        /// Whether opening it added `within` to the owners given geometry
        /// (a `minkowski()`, `hull()` or `resize()` outside `owner`).
        gives: bool,
        kids: Vec<Kid>,
    }
    fn gathers(k: &NodeKind) -> bool {
        matches!(
            k,
            NodeKind::Minkowski { .. } | NodeKind::Hull | NodeKind::Resize { .. }
        )
    }
    // Every part whose geometry the layer is being given, innermost last;
    // a stack in step with the frames' `gives`.
    let mut given: Vec<Option<&str>> = Vec::new();
    fn open<'a>(
        node: &'a Node,
        above: Option<&'a str>,
        owner: Option<&str>,
        given: &mut Vec<Option<&'a str>>,
    ) -> Frame<'a> {
        let within = match &node.kind {
            NodeKind::Part { name } => Some(name.as_str()),
            _ => above,
        };
        let gives = gathers(&node.kind) && within != owner && !given.contains(&within);
        if gives {
            given.push(within);
        }
        Frame {
            node,
            within,
            gives,
            kids: Vec::with_capacity(node.children.len()),
        }
    }
    // Post-order from an explicit stack: a tree as deep as the evaluator
    // allows must not overflow the native stack here.
    let mut stack = vec![open(top, None, owner, &mut given)];
    loop {
        let f = stack.last_mut().expect("the top is popped last");
        let next = f.kids.len();
        if let Some(c) = f.node.children.get(next) {
            if is_background(c) {
                // Not rendered either way: kept as it is.
                f.kids.push(Kid {
                    node: Some(c.clone()),
                    background: true,
                    hit: false,
                    foreign: false,
                });
            } else {
                let within = f.within;
                let frame = open(c, within, owner, &mut given);
                stack.push(frame);
            }
            continue;
        }
        let f = stack.pop().expect("just looked at it");
        if f.gives {
            given.pop();
        }
        let n = f.node;
        let done = if n.children.is_empty() {
            let mine = f.within == owner;
            let kept = mine || given.contains(&f.within);
            Kid {
                node: kept.then(|| n.clone()),
                background: false,
                hit: mine,
                foreign: !kept,
            }
        } else {
            let owned = |k: &Kid| !k.background && k.node.is_some();
            let any = |p: fn(&Kid) -> bool| f.kids.iter().any(|k| !k.background && p(k));
            let (hit, foreign) = (any(|k| k.hit), any(|k| k.foreign));
            match &n.kind {
                NodeKind::Csg(CsgOp::Difference) => {
                    // The first child the render does not skip is the one
                    // the others are cut from.
                    let base = f.kids.iter().position(|k| !k.background);
                    let (hit, foreign) =
                        base.map_or((false, false), |b| (f.kids[b].hit, f.kids[b].foreign));
                    let node = base.and_then(|b| {
                        let mut kids = f.kids;
                        let first = kids.swap_remove(b).node?;
                        let mut children: Vec<Node> = n.children.clone();
                        children[b] = first;
                        Some(with_children(n, children))
                    });
                    Kid {
                        node,
                        background: false,
                        hit,
                        foreign,
                    }
                }
                NodeKind::Csg(CsgOp::Intersection) | NodeKind::IntersectionFor => {
                    let node = f.kids.iter().any(owned).then(|| {
                        let children = f
                            .kids
                            .into_iter()
                            .zip(&n.children)
                            .map(|(k, c)| k.node.unwrap_or_else(|| c.clone()))
                            .collect();
                        with_children(n, children)
                    });
                    Kid {
                        node,
                        background: false,
                        hit,
                        foreign,
                    }
                }
                kind => {
                    // A `minkowski()`, `hull()` or `resize()` that gave its
                    // geometry to the parts in it draws nothing of its own
                    // here; one that gave it to `owner` draws only if
                    // `owner` has a leaf in it.
                    let drop = (gathers(kind) && f.within == owner && foreign) || (f.gives && !hit);
                    let node = (!drop && f.kids.iter().any(owned)).then(|| {
                        let children = f.kids.into_iter().filter_map(|k| k.node).collect();
                        with_children(n, children)
                    });
                    Kid {
                        node,
                        background: false,
                        hit,
                        foreign,
                    }
                }
            }
        };
        match stack.last_mut() {
            Some(parent) => parent.kids.push(done),
            None => return done.node,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq(x: f64) -> Polygon2d {
        Polygon2d::from_outline(vec![[x, 0.0], [x + 2.0, 0.0], [x + 2.0, 2.0], [x, 2.0]])
    }

    /// The guard on the split staying linear: a layer is clipped only
    /// against the model's outlines near it, never against all of them.
    #[test]
    fn a_layer_is_clipped_against_the_outlines_near_it() {
        let root = Polygon2d {
            outlines: (0..1000)
                .flat_map(|i| sq(f64::from(i) * 16.0).outlines)
                .collect(),
            sanitized: true,
        };
        let boxes: Vec<_> = root.outlines.iter().map(outline_bounds).collect();
        let layer = sq(160.0);
        let near = near(&root, &boxes, &layer);
        assert_eq!(near.outlines.len(), 1);
        assert_eq!(near.outlines[0].vertices, layer.outlines[0].vertices);
        // Touching bounds count, as does an outline round the layer.
        let wide = Polygon2d::from_outline(vec![[-1.0, -1.0], [18.0, -1.0], [18.0, 3.0]]);
        assert_eq!(super::near(&root, &boxes, &wide).outlines.len(), 2);
        let empty = Polygon2d {
            outlines: Vec::new(),
            sanitized: true,
        };
        assert!(super::near(&root, &boxes, &empty).is_empty());
    }
}
