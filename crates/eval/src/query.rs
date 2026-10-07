//! NeoSCAD's render-free geometry queries (`--enable query`): `anchor()`
//! and `child_anchors()` (`docs/language-extensions.md`, sections 5.2 and
//! 5.3).
//!
//! **Anchors.** `anchor(name, point, dir)` records a named point on the
//! node of the instantiation it is written in ([`crate::node::Anchor`]),
//! in that node's frame. It makes no node of its own, so a model's tree,
//! `.csg` and geometry are those of the same model without it. A sketch
//! exports its named entities' solved positions the same way
//! (`crate::sketch`).
//!
//! **Queries.** `child_anchors(i)`, inside a user module, is what
//! `children(i)` would instantiate at that point, with every anchor in its
//! subtree carried through the transforms between (plain products, as the
//! CSG tree's own walk composes them) into the module's frame. It needs no
//! rendering; its cost is the child's evaluation and the size of its
//! subtree.
//!
//! **The sandbox.** Asking about a child means instantiating it early,
//! from inside an expression, which must not change anything the model
//! would otherwise print or build:
//!
//! - its messages are held back ([`Evaluator::hold`]) and printed only by
//!   a `children()` that reuses the instance, or with the error if the
//!   child fails, since an error stops evaluation as `children(i)`'s would;
//! - the node counter, the limit-check counter, the `rands()` state, the
//!   deprecations and part names already printed and the sketch number
//!   are restored afterwards, and a sketch being built around the query is
//!   set aside while it runs, so the child cannot add to it.
//!
//! **Reuse.** The instance is kept ([`Held`]) for the rest of the module
//! call. A later `children(i)` of the same call, with the same indices,
//! takes it instead of instantiating again, under the call memo's rule
//! (`crate::callmemo`): every `$` variable the instance read from outside
//! it must have the same value where `children(i)` runs, and nothing it
//! did may be something no key covers (`rands()`, file reads, a `$`-named
//! function found outside it, a passed limit). The reused nodes are
//! renumbered from the counter and its messages printed in order, so the
//! output is the same as with no query. A nested query therefore costs
//! one evaluation per level, not 2^n. Where reuse is in doubt the child
//! is simply instantiated again, which is always correct.

use std::rc::Rc;

use lang::diag::DiagCode;

use crate::call::ArgVal;
use crate::context::{Children, Ctx, CtxKind, ScopeRef};
use crate::eval::Evaluator;
use crate::memo::{Digest, Recorded};
use crate::message::{Loc, R, UnwindKind};
use crate::node::{Anchor, IDENTITY, Matrix, Node, NodeKind};
use crate::sym::Sym;
use crate::value::{MAX_RANGE_STEPS, ObjectBuilder, Str, Value};

/// Which children a query asked about: the module call (its context) and
/// `children()`'s indices (`None`: all of them).
pub(crate) type Key = (Rc<Ctx>, Option<Vec<usize>>);

/// A child a query instantiated, kept for the `children()` that follows.
pub(crate) struct Held {
    /// The module call whose children these are. Holding it keeps its
    /// address from being reused while the entry lives; the entry goes
    /// when the call ends (`heap::user_body_end`).
    mctx: Rc<Ctx>,
    indices: Option<Vec<usize>>,
    /// A group node holding what `children()` would put in its own node:
    /// the children's nodes and the anchors written directly among them.
    node: Node,
    /// The node counter where the instance started, and the node indices
    /// and limit checks it used.
    first_index: usize,
    count: usize,
    ticks: u32,
    /// What it printed, held back.
    messages: Vec<Recorded>,
    /// The `$` variables it read from outside itself with their values'
    /// digests, or `None` when it cannot be reused.
    deps: Option<Vec<(Sym, Option<Digest>)>>,
}

/// The module call `ctx` is lexically inside, as `children()` finds it
/// (`Context::user_module_children`).
fn module_ctx(ctx: &Rc<Ctx>) -> Option<Rc<Ctx>> {
    let mut c = ctx;
    loop {
        if let CtxKind::Module(..) = c.kind {
            return Some(c.clone());
        }
        c = c.parent.as_ref()?;
    }
}

impl Evaluator<'_> {
    // --- anchor() -----------------------------------------------------------

    /// `anchor(name, point, dir = undef)`: a named point on the node of
    /// the instantiation around it. Inside a sketch body `point` may be an
    /// entity, whose solved position the anchor takes.
    pub(crate) fn anchor_statement(&mut self, sr: ScopeRef, i: usize, ctx: &Rc<Ctx>) -> R<()> {
        let loc = self.inst_loc(sr, i);
        let args = self.inst_args(sr, i, ctx)?;
        self.no_children(sr, i);
        let p = self.params(args, loc, &[], &["name", "point", "dir"], "anchor");
        let name = self.get(&p, "name");
        let point = self.get(&p, "point");
        let dir = self.get(&p, "dir");
        self.end(p);
        let Value::Str(name) = name else {
            self.anchor_warning(loc, "name", &name, "a string");
            return Ok(());
        };
        let name = String::from_utf8_lossy(name.as_bytes()).into_owned();
        if let Value::Entity(e) = &point {
            if !dir.is_undef() {
                let t = "anchor(): an entity's anchor takes its direction from the entity; 'dir' is ignored";
                self.warn(loc, DiagCode::InvalidArgument, t);
            }
            self.sketch_anchor(name, e.clone(), loc);
            return Ok(());
        }
        let Some(point) = coords(&point) else {
            self.anchor_warning(loc, "point", &point, "[x, y] or [x, y, z]");
            return Ok(());
        };
        let dir = match &dir {
            Value::Undef => None,
            d => match coords(d) {
                Some(v) if v != [0.0; 3] => Some(v),
                _ => {
                    self.anchor_warning(loc, "dir", d, "a non-zero [x, y] or [x, y, z]");
                    return Ok(());
                }
            },
        };
        self.add_anchors(vec![Anchor { name, point, dir }]);
        Ok(())
    }

    fn anchor_warning(&mut self, loc: Loc, what: &str, found: &Value, expected: &str) {
        let mut t = format!("anchor(): '{what}' must be {expected}, found ").into_bytes();
        self.write_echo_nothrow(found, &mut t);
        t.extend_from_slice(b"; the anchor is ignored");
        self.warn(loc, DiagCode::InvalidArgument, t);
    }

    /// Put anchors on the node being filled: the innermost instantiation
    /// around the statement running now. At the top level there is none,
    /// and no query could see them, so they are dropped.
    pub(crate) fn add_anchors(&mut self, a: Vec<Anchor>) {
        if a.is_empty() {
            return;
        }
        if let Some(n) = self.heap_nodes.last_mut() {
            n.anchors.get_or_insert_with(Default::default).extend(a);
        }
    }

    // --- child_anchors() ----------------------------------------------------

    /// `child_anchors(index)`: an object from each anchor name in what
    /// `children(index)` would instantiate here to `[point, dir]`, in the
    /// module's frame.
    pub(crate) fn child_anchors(&mut self, args: Vec<ArgVal>, loc: Loc, ctx: &Rc<Ctx>) -> R<Value> {
        let syms = [self.syms.intern("index")];
        let vars = self.bind_builtin(args, loc, &[], &syms, true);
        let index = vars.get(syms[0]).cloned().unwrap_or_default();
        let Some(mctx) = module_ctx(ctx) else {
            let t = "child_anchors() is only valid inside a module, where it asks about the module's children";
            self.warn(loc, DiagCode::QueryOutsideModule, t);
            return Ok(Value::Undef);
        };
        let CtxKind::Module(_, children) = &mctx.kind else {
            unreachable!("a module context")
        };
        let children = children.clone();
        let size = self.scope(children.scope).instantiations.len();
        let Some(indices) = self.query_indices("child_anchors", &index, size, loc) else {
            return Ok(Value::Undef);
        };
        let k = self.query_instance(&mctx, &children, indices, loc)?;
        let mut found = Vec::new();
        collect_anchors(&self.held[k].node, &mut found);
        let mut out = ObjectBuilder::new();
        let mut seen = std::collections::HashSet::new();
        let mut twice: Vec<String> = Vec::new();
        for a in found {
            if !seen.insert(a.name.clone()) {
                if !twice.contains(&a.name) {
                    twice.push(a.name);
                }
                continue;
            }
            let num = |v: [f64; 3]| Value::vector(v.iter().map(|&x| Value::Number(x)).collect());
            let entry = Value::vector(vec![num(a.point), a.dir.map_or(Value::Undef, num)]);
            out.set(Str::new(a.name.as_bytes()), entry);
        }
        if !twice.is_empty() {
            let names: Vec<String> = twice.iter().map(|n| format!("'{n}'")).collect();
            let t = format!(
                "child_anchors(): more than one anchor named {} among the children; the first is used",
                names.join(", ")
            );
            self.warn(loc, DiagCode::QueryDuplicateAnchor, t);
        }
        Ok(Value::Object(out.finish(|_| false)))
    }

    /// A query's index as `children()` takes it: none (all the children),
    /// a number, a list of numbers or a range. `None`, with a warning, when
    /// one is out of range or not a number.
    fn query_indices(
        &mut self,
        fname: &str,
        index: &Value,
        size: usize,
        loc: Loc,
    ) -> Option<Option<Vec<usize>>> {
        let mut out = Vec::new();
        let mut one = |ev: &mut Self, v: &Value| -> bool {
            let Value::Number(x) = v else {
                let mut t = format!("{fname}(): bad index (").into_bytes();
                ev.write_echo_nothrow(v, &mut t);
                t.extend_from_slice(b"); it must be a number, a list of numbers or a range");
                ev.warn(loc, DiagCode::QueryIndex, t);
                return false;
            };
            // `children()`'s conversion, which truncates.
            let n = *x as i32;
            if n < 0 || n as usize >= size {
                let t = format!("{fname}(): Children index ({n}) out of bounds ({size} children)");
                ev.warn(loc, DiagCode::QueryIndex, t);
                return false;
            }
            out.push(n as usize);
            true
        };
        let ok = match index {
            Value::Undef => return Some(None),
            Value::Vector(v) => v.as_slice().iter().all(|e| one(self, e)),
            Value::Range(r) => {
                if r.num_values() > MAX_RANGE_STEPS {
                    let t = format!("{fname}(): the range has more than {MAX_RANGE_STEPS} values");
                    self.warn(loc, DiagCode::QueryIndex, t);
                    false
                } else {
                    let r = **r;
                    r.iter().all(|x| one(self, &Value::Number(x)))
                }
            }
            v => one(self, v),
        };
        ok.then_some(Some(out))
    }

    // --- the sandbox --------------------------------------------------------

    /// The index in [`Evaluator::held`] of the instance of `children`
    /// (module call `mctx`) at `indices` as it would be here: one kept
    /// whose `$` reads still hold, or a new one.
    fn query_instance(
        &mut self,
        mctx: &Rc<Ctx>,
        children: &Children,
        indices: Option<Vec<usize>>,
        loc: Loc,
    ) -> R<usize> {
        if let Some(k) = self.find_held(mctx, &indices) {
            return Ok(k);
        }
        if self
            .querying
            .iter()
            .any(|(c, ix)| Rc::ptr_eq(c, mctx) && *ix == indices)
        {
            let t = "Recursion detected: child_anchors() asks about the child it is inside";
            self.error(Some(loc), DiagCode::RecursionLimit, t);
            return Err(self.unwind(UnwindKind::Recursion));
        }
        // The driver runs nested here, inside an expression, so each level
        // of queries inside queried children holds native stack: counted
        // like a nested expression loop, it stops cleanly where the stack
        // or the frame budget would run out (`crate::recursion`).
        self.frames += crate::recursion::HEAP_LOOP_FRAMES;
        if self.recursion_exhausted() {
            self.frames -= crate::recursion::HEAP_LOOP_FRAMES;
            let t = "Recursion detected calling function 'child_anchors'";
            self.error(Some(loc), DiagCode::RecursionLimit, t);
            return Err(self.unwind(UnwindKind::Recursion));
        }
        self.querying.push((mctx.clone(), indices.clone()));
        let first_index = self.node_counter();
        let first_ticks = self.ticks();
        let rng = self.rng.clone();
        let deprecations = self.deprecations.clone();
        let part_names = self.part_names.clone();
        let serial = self.sketch_serial;
        let sketch = self.sketch.take();
        let outer_hold = self.hold.replace(Vec::new());
        let impure = self.query_impure;
        self.cm.begin_sandbox(self.stack.len());

        let wrapper = Node::new(NodeKind::Group { name: None }, None, 0);
        let r = self.instantiate_children_into(wrapper, children.clone(), indices.clone());

        let deps = self.cm.end_sandbox();
        let messages = std::mem::replace(&mut self.hold, outer_hold).unwrap_or_default();
        let count = self.node_counter() - first_index;
        let ticks = self.ticks().wrapping_sub(first_ticks);
        let pure = self.query_impure == impure && sketch.is_none();
        self.restore_counters(first_index, first_ticks);
        self.rng = rng;
        self.deprecations = deprecations;
        self.part_names = part_names;
        self.sketch_serial = serial;
        self.sketch = sketch;
        self.querying.pop();
        self.frames -= crate::recursion::HEAP_LOOP_FRAMES;
        let node = match r {
            Ok(n) => n,
            Err(e) => {
                // Evaluation stops here, as it would have in `children()`:
                // what the child printed on the way is printed with it.
                for m in messages {
                    self.replay_recorded(m);
                }
                return Err(e);
            }
        };
        // Reusable only where nothing outside a key happened, and not
        // under `--hardwarnings`, whose stop at a warning a replay would
        // not make (the call memo is off then for the same reason).
        let reusable = pure && !self.opts.hardwarnings && !self.limit_passed();
        let deps = deps.filter(|_| reusable).and_then(|names| {
            names
                .into_iter()
                .map(|s| {
                    crate::callmemo::dep_value_digest(self.lookup_special_quiet(s).as_ref())
                        .ok()
                        .map(|d| (s, d))
                })
                .collect::<Option<Vec<_>>>()
        });
        self.held_nodes += count;
        self.held.push(Held {
            mctx: mctx.clone(),
            indices,
            node,
            first_index,
            count,
            ticks,
            messages,
            deps,
        });
        Ok(self.held.len() - 1)
    }

    /// A kept instance of `mctx`'s children at `indices` that a fresh
    /// instantiation here would equal: every `$` variable it read from
    /// outside has the same value here. The lookups are the noting ones, so
    /// a call being recorded around this point depends on them, as it would
    /// on the instantiation's own reads.
    fn find_held(&mut self, mctx: &Rc<Ctx>, indices: &Option<Vec<usize>>) -> Option<usize> {
        if self.limit_passed() || crate::limits::live::over() {
            return None;
        }
        for k in (0..self.held.len()).rev() {
            let h = &self.held[k];
            if !Rc::ptr_eq(&h.mctx, mctx) || h.indices != *indices {
                continue;
            }
            let Some(deps) = h.deps.clone() else {
                continue;
            };
            let same = deps.iter().all(|&(s, d)| {
                crate::callmemo::dep_value_digest(self.lookup_special(s).as_ref()) == Ok(d)
            });
            if same {
                return Some(k);
            }
        }
        None
    }

    /// `children(indices)` at `ctx`, its node made and empty: if a query
    /// of the same module call kept an instance that a fresh one would
    /// equal, it becomes `node`'s children (renumbered from the node
    /// counter, its messages printed) and this returns true.
    pub(crate) fn reuse_held(
        &mut self,
        node: &mut Node,
        ctx: &Rc<Ctx>,
        indices: &Option<Vec<usize>>,
    ) -> bool {
        let Some(mctx) = module_ctx(ctx) else {
            return false;
        };
        let Some(k) = self.find_held(&mctx, indices) else {
            return false;
        };
        let h = self.held.remove(k);
        self.held_nodes -= h.count;
        let mut w = h.node;
        let shift = self.node_counter() as i64 - h.first_index as i64;
        for c in &mut w.children {
            crate::callmemo::renumber(c, shift);
        }
        node.children = std::mem::take(&mut w.children);
        node.anchors = w.anchors.take();
        self.advance(h.count, h.ticks);
        for m in h.messages {
            self.replay_recorded(m);
        }
        true
    }

    /// Forget the instances kept for module call `mctx`, which has ended.
    pub(crate) fn drop_held(&mut self, mctx: &Rc<Ctx>) {
        let mut freed = 0;
        self.held.retain(|h| {
            let keep = !Rc::ptr_eq(&h.mctx, mctx);
            if !keep {
                freed += h.count;
            }
            keep
        });
        self.held_nodes -= freed;
    }
}

/// `[x, y]` or `[x, y, z]` of finite numbers, as three coordinates.
fn coords(v: &Value) -> Option<[f64; 3]> {
    let Value::Vector(v) = v else {
        return None;
    };
    let s = v.as_slice();
    if !(2..=3).contains(&s.len()) {
        return None;
    }
    let mut out = [0.0; 3];
    for (o, x) in out.iter_mut().zip(s) {
        match x {
            Value::Number(n) if n.is_finite() => *o = *n,
            _ => return None,
        }
    }
    Some(out)
}

/// `a × b` for 4x4 matrices, the plain sums of products the CSG walk uses
/// (`geom::csg`), so anchors move exactly as the geometry they mark, and
/// identically on every platform.
fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..4).map(|k| a[r][k] * b[k][c]).sum()))
}

/// The matrix a node's children are placed by, relative to the node's
/// own frame; `None` when its children's anchors cannot be placed without
/// geometry (`resize()` scales by the child's size; `rotate_extrude()`
/// sweeps its profile into a solid) or the geometry is removed (a
/// transform with a NaN or infinite entry, which OpenSCAD drops with a
/// warning).
fn own_matrix(n: &Node) -> Option<Matrix> {
    match &n.kind {
        NodeKind::Transform { matrix, .. } => matrix
            .iter()
            .flatten()
            .all(|v| v.is_finite())
            .then_some(*matrix),
        // The profile's plane is the extrusion's base: z = 0, or with
        // `center` the height vector's half below it (`geom::extrude`).
        NodeKind::LinearExtrude(e) if e.center => {
            let mut m = IDENTITY;
            for (row, h) in m.iter_mut().zip(e.height) {
                row[3] = -h / 2.0;
            }
            Some(m)
        }
        // A projection flattens onto z = 0.
        NodeKind::Projection { .. } => {
            let mut m = IDENTITY;
            m[2][2] = 0.0;
            Some(m)
        }
        NodeKind::Resize { .. } | NodeKind::RotateExtrude { .. } => None,
        _ => Some(IDENTITY),
    }
}

/// Every anchor in `top`'s subtree, placed in `top`'s parent's frame, in
/// pre-order (a node's own before its children's). The walk keeps its
/// pending nodes on a heap stack, since a recursive module makes trees as
/// deep as the depth limit.
fn collect_anchors(top: &Node, out: &mut Vec<Anchor>) {
    let mut stack: Vec<(&Node, Matrix)> = vec![(top, IDENTITY)];
    while let Some((n, parent)) = stack.pop() {
        let Some(own) = own_matrix(n) else {
            continue;
        };
        let m = if own == IDENTITY {
            parent
        } else {
            mul(&parent, &own)
        };
        if let Some(a) = &n.anchors {
            out.extend(a.iter().map(|a| place(a, &m)));
        }
        stack.extend(n.children.iter().rev().map(|c| (c, m)));
    }
}

/// `a` moved by `m`: its point as a point, its direction as a direction
/// (the linear part alone), scaled to unit length. A direction the matrix
/// flattens to nothing (a projection of a vertical one) is dropped.
fn place(a: &Anchor, m: &Matrix) -> Anchor {
    let p = a.point;
    let point = std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3]);
    let dir = a.dir.and_then(|d| {
        let v: [f64; 3] = std::array::from_fn(|r| m[r][0] * d[0] + m[r][1] * d[1] + m[r][2] * d[2]);
        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        (len > 0.0 && len.is_finite()).then(|| v.map(|x| x / len))
    });
    Anchor {
        name: a.name.clone(),
        point,
        dir,
    }
}

/// The anchors in `kids` (and below), in their parent's frame: a sketch
/// keeps those of the helper modules its body calls, whose nodes it
/// drops.
pub(crate) fn anchors_in(kids: &[Node]) -> Vec<Anchor> {
    let mut out = Vec::new();
    for k in kids {
        collect_anchors(k, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_follow_transforms_and_skip_what_needs_geometry() {
        let anchor = |name: &str| {
            Some(Box::new(vec![Anchor {
                name: name.into(),
                point: [1.0, 0.0, 0.0],
                dir: Some([2.0, 0.0, 0.0]),
            }]))
        };
        let mut t = IDENTITY;
        t[0][3] = 10.0; // translate([10, 0, 0])
        let mut rot = IDENTITY; // rotate([0, 0, 90])
        rot[0][0] = 0.0;
        rot[0][1] = -1.0;
        rot[1][0] = 1.0;
        rot[1][1] = 0.0;
        let mut leaf = Node::new(NodeKind::Group { name: None }, None, 3);
        leaf.anchors = anchor("b");
        let mut inner = Node::new(
            NodeKind::Transform {
                matrix: rot,
                verb: "rotate",
            },
            None,
            2,
        );
        inner.anchors = anchor("a");
        inner.children.push(leaf);
        let mut resize = Node::new(
            NodeKind::Resize {
                newsize: [1.0; 3],
                autosize: [false; 3],
                convexity: 1,
            },
            None,
            4,
        );
        resize.anchors = anchor("hidden");
        let mut top = Node::new(
            NodeKind::Transform {
                matrix: t,
                verb: "translate",
            },
            None,
            1,
        );
        top.children = vec![inner, resize];
        let got = anchors_in(std::slice::from_ref(&top));
        let names: Vec<&str> = got.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        // translate(rotate(p)): [1, 0, 0] -> [0, 1, 0] -> [10, 1, 0].
        assert_eq!(got[0].point, [10.0, 1.0, 0.0]);
        assert_eq!(got[0].dir, Some([0.0, 1.0, 0.0]));
        assert_eq!(
            got[1],
            Anchor {
                name: "b".into(),
                ..got[0].clone()
            }
        );
    }
}
