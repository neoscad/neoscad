//! The statement driver: statements and module instantiation on a heap
//! stack.
//!
//! The recursive statement evaluator this replaced instantiated a
//! statement by calling into it: `instantiate` → `user_module` →
//! `instantiate_scope` → `instantiate` …, one set of native frames per
//! level, so how deep a module recursion could go was decided by the
//! native stack: 33,000 levels natively, and in a browser whatever the
//! engine gave a wasm thread (about 30 levels of a module chain through
//! `translate` in WebKit). Here the same work runs in one loop over an
//! explicit stack of [`Frame`]s on the heap, so a module recursion holds
//! no native stack at all and ends at the counted limit
//! [`crate::limits::Limits::depth`], the same in every build and browser.
//! (The recursive driver was removed once this one was on in every build;
//! the names in the frames' documentation are its functions.)
//!
//! What a frame is: the part of a native function that runs after its
//! callee returns. Each `begin_*` function below is the part of a native
//! function before its first nested statement; it pushes the frames that
//! finish it and returns `None`, or finishes at once (a primitive, an
//! error) and returns the result for the frame on top. The driver pops a
//! frame and hands it the result of the one above it, until the frame it
//! started from has its result. Arguments, scopes' assignments, bindings,
//! lookups, node construction and the call memo are ordinary functions
//! shared with the rest of the evaluator, run in the order the recursive
//! driver ran them, which kept the output byte-identical when it was
//! replaced (the conformance A/B in `docs/audits/heap-evaluator.md`).
//!
//! Expressions start from this loop's own native frame, at any module
//! depth, so the frame budget of [`crate::recursion`] counts only the
//! calls and expressions themselves: statements add no weight. Past a few
//! native call levels, calls and the expressions around them run on a
//! heap stack of their own ([`crate::heap_expr`]).

use std::rc::Rc;

use lang::ast::{Arg, ModuleDef};
use lang::diag::DiagCode;

use crate::builtins::modules::{BuiltinModule, geometry_params, is_leaf};
use crate::call::Instantiable;
use crate::context::{Children, Ctx, CtxKind, ScopeRef};
use crate::eval::Evaluator;
use crate::message::{Loc, R, UnwindKind};
use crate::node::{Node, NodeKind};
use crate::resolve::NO_SLOT;
use crate::sym::Sym;
use crate::value::Value;

/// What a finished frame hands to the frame below it. Small on purpose:
/// it passes through the driver at every step, and a node in it (248
/// bytes) made every step copy it several times, which cost the first
/// version of this driver 10-20% more instructions than the recursive
/// evaluator on statement-heavy models. A statement's node goes straight
/// onto the node stack instead ([`Evaluator::heap_out`]), where the scope
/// collecting it will find it.
pub(crate) enum Ret {
    /// A statement finished; its node, if any, is in the scope below.
    Done(R<()>),
    /// A scope or a `for` loop finished: its nodes when it collects them,
    /// or empty when they stay on the node stack for an enclosing loop.
    Kids(R<Vec<Node>>),
}

/// One suspended native function of the recursive driver. Small (40
/// bytes) and unboxed, as frames are pushed and popped at every statement:
/// what does not fit waits on side stacks of the evaluator, popped in the
/// same order as the frames (the nodes being filled, `heap_nodes`; the
/// finished nodes, `heap_out`; `children(index)`'s indices and an `if`'s
/// arguments), and only a `for` loop's state is boxed.
pub(crate) enum Frame<'a> {
    /// The statement the driver started: its node is the node stack's
    /// above this height.
    Root(usize),
    /// The end of `instantiate_frame` for a statement whose module runs
    /// above: `--hardwarnings` and the "called by" trace line. For a user
    /// module, `user_module`'s pop of the module names first.
    Called { name: Sym, loc: Loc, user: bool },
    /// The end of `user_module_inner`: its node (the top of the node
    /// stack), the trace of its parameters, its context off the stack and
    /// its call-memo recording.
    UserBody(UserBody<'a>),
    /// `instantiate_scope`: the statements of `sr`, or of the indices on
    /// top of `heap_indices`, in `ctx`. With a `mark`,
    /// `instantiate_children` around it, whose context leaves the stack at
    /// the end. Run in place.
    Scope(ScopeRun),
    /// The end of a builtin module with children (`with_children` and the
    /// builtin around it): the children into its node (the top of
    /// `heap_nodes`), then `post`.
    Wrap(Post),
    /// One variable of a `for` statement (`for_each`).
    For(Box<ForVar<'a>>),
}

// Kept at 40 bytes on 64-bit targets: a frame is moved at every push and
// pop, and the first version's large frames and results cost 10-20% more
// instructions than the recursive driver on statement-heavy models.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Frame<'static>>() <= 40);

/// See [`Frame::UserBody`].
pub(crate) struct UserBody<'a> {
    mark: usize,
    mu: u32,
    def: &'a ModuleDef,
    mctx: Rc<Ctx>,
}

/// See [`Frame::Scope`].
pub(crate) struct ScopeRun {
    sr: ScopeRef,
    ctx: Rc<Ctx>,
    /// The stack mark of `ctx` when this frame pushed it, or `NO_MARK`.
    mark: u32,
    /// The next statement (or index into the indices).
    pos: u32,
    /// The node stack's height when the scope started: its nodes are the
    /// ones above.
    out: u32,
    /// Whether it runs the indices on top of `heap_indices`.
    indices: bool,
    /// Whether it hands its nodes down at the end (`for` bodies leave
    /// them for the loop).
    collect: bool,
}

const NO_MARK32: u32 = u32::MAX;

const NO_MARK: usize = usize::MAX;

/// What a builtin does once its children are instantiated.
#[derive(Clone, Copy)]
pub(crate) struct Post {
    /// `echo` and `assert`: no node when there are no children.
    filter: bool,
    /// `part()`: leave the part.
    part: bool,
    /// `sketch()`: [`SKETCH_TOP`] for one that begins a sketch (solve it
    /// into its node), [`SKETCH_MERGE`] for one adding to the sketch being
    /// built (no node), or 0.
    sketch: u8,
    /// `if`: its arguments are on top of `heap_args`, kept alive until
    /// now, as its native frame holds them, for the memory estimate.
    args: bool,
    /// The stack mark of the builtin's own frame (its parameters, or a
    /// `let`'s variables), or `NO_MARK`.
    mark: usize,
}

const SKETCH_TOP: u8 = 1;
const SKETCH_MERGE: u8 = 2;

const PLAIN: Post = Post {
    filter: false,
    part: false,
    sketch: 0,
    args: false,
    mark: NO_MARK,
};

/// See [`Frame::For`].
pub(crate) struct ForVar<'a> {
    u: u32,
    /// The variables after this one.
    rest: &'a [Arg],
    region: u32,
    loc: Loc,
    /// The context the loop runs in (the previous variable's iteration).
    ctx: Rc<Ctx>,
    /// The scope of the loop's body.
    body: ScopeRef,
    /// What it iterates over, alive until the loop ends as in `for_each`.
    values: Value,
    /// The next value's index (a byte offset for a string).
    pos: usize,
    /// How many values a range gives.
    len: u32,
    mode: ForMode,
    /// The iteration's own context and its stack mark, while it runs.
    cur: Option<(Rc<Ctx>, usize)>,
    /// The node stack's height when the loop started: its nodes so far
    /// are the ones above.
    out: usize,
    /// Whether it hands its nodes down at the end (the outermost
    /// variable's loop does).
    collect: bool,
}

#[derive(Clone, Copy)]
enum ForMode {
    /// A register region (`for_each_reg`): register `i`, and the base the
    /// loop's instance replaced.
    Reg { i: usize, old: u32 },
    /// A context per iteration binding `name` (at `slot`, or by name).
    Ctx { slot: u32, name: Sym, config: bool },
}

impl<'a> Evaluator<'a> {
    /// `ModuleInstantiation::evaluate`, on the heap: instantiation `i` of
    /// scope `sr` and everything inside it.
    pub fn instantiate(&mut self, sr: ScopeRef, i: usize, ctx: &Rc<Ctx>) -> R<Option<Node>> {
        let base = self.heap.len();
        self.heap.push(Frame::Root(self.heap_out.len()));
        let mut ret = self.begin_inst(sr, i, ctx);
        while self.heap.len() > base + 1 {
            ret = self.step(ret);
        }
        let Some(Frame::Root(out)) = self.heap.pop() else {
            unreachable!("the driver's root frame")
        };
        let node = (self.heap_out.len() > out).then(|| self.heap_out.pop());
        debug_assert_eq!(self.heap_out.len(), out);
        match ret {
            Some(Ret::Done(r)) => r.map(|()| node.flatten()),
            _ => unreachable!("the driver ends with a statement"),
        }
    }

    /// What `children(indices)` instantiates, into `wrapper` as its
    /// children (and its anchors), as the `children()` builtin does with
    /// its own node: a query's sandboxed instantiation (`crate::query`).
    /// The driver starts here from inside an expression, so unlike
    /// [`Self::instantiate`] it runs above frames that are still in
    /// progress, all of which sit below `base` and are left alone.
    pub(crate) fn instantiate_children_into(
        &mut self,
        wrapper: Node,
        children: Children,
        indices: Option<Vec<usize>>,
    ) -> R<Node> {
        let base = self.heap.len();
        self.heap.push(Frame::Root(self.heap_out.len()));
        self.heap_nodes.push(wrapper);
        self.heap.push(Frame::Wrap(PLAIN));
        let mut ret = self.begin_children(children, indices, true);
        while self.heap.len() > base + 1 {
            ret = self.step(ret);
        }
        let Some(Frame::Root(out)) = self.heap.pop() else {
            unreachable!("the driver's root frame")
        };
        let node = (self.heap_out.len() > out).then(|| self.heap_out.pop());
        debug_assert_eq!(self.heap_out.len(), out);
        match ret {
            Some(Ret::Done(r)) => r.map(|()| node.flatten().expect("an unfiltered wrapper")),
            _ => unreachable!("the driver ends with a statement"),
        }
    }

    /// Run the top frame on: `ret` is the result of the frame that was
    /// above it, or `None` when it starts. A scope, the most common, runs
    /// where it is; the others are popped.
    #[inline(always)]
    fn step(&mut self, ret: Option<Ret>) -> Option<Ret> {
        if let Some(Frame::Scope(_)) = self.heap.last() {
            return self.scope_step(ret);
        }
        match self.heap.pop() {
            Some(Frame::Called { name, loc, user }) => {
                let Some(Ret::Done(r)) = ret else {
                    unreachable!("a statement's result")
                };
                Some(Ret::Done(self.called(name, loc, user, r)))
            }
            Some(Frame::UserBody(b)) => {
                let Some(Ret::Kids(r)) = ret else {
                    unreachable!("a module body's nodes")
                };
                let r = self.user_body_end(b, r);
                self.done(r)
            }
            Some(Frame::Wrap(post)) => {
                let Some(Ret::Kids(r)) = ret else {
                    unreachable!("a builtin's children")
                };
                if post.args {
                    self.heap_args.pop();
                }
                let mut node = self.heap_nodes.pop().expect("a builtin's node");
                let r = r.map(|kids| node.children = kids);
                if post.part {
                    self.part_stack.pop();
                }
                if post.sketch != 0 {
                    let top = post.sketch == SKETCH_TOP;
                    let r = match r {
                        Ok(()) => self.sketch_close(node, top),
                        Err(e) => {
                            if top {
                                self.sketch_abandon();
                            }
                            Err(e)
                        }
                    };
                    if post.mark != NO_MARK {
                        self.truncate(post.mark);
                    }
                    return self.done(r);
                }
                if post.mark != NO_MARK {
                    self.truncate(post.mark);
                }
                let r = r.map(|()| {
                    if !post.filter || !node.children.is_empty() {
                        return Some(node);
                    }
                    // An `echo` or `assert` whose only children were
                    // `anchor()`s leaves no node; the anchors, in the same
                    // frame, go to the node around it.
                    if let Some(a) = node.anchors.take() {
                        self.add_anchors(*a);
                    }
                    None
                });
                self.done(r)
            }
            Some(Frame::For(f)) => self.for_step(f, ret),
            _ => unreachable!("a frame above the root"),
        }
    }

    /// A statement's result: its node goes on the node stack, where the
    /// scope (or root) below the `Called` frame that finishes it collects
    /// it. Statements finish in order, so a scope's nodes are always the
    /// top of the stack.
    #[inline(always)]
    fn done(&mut self, r: R<Option<Node>>) -> Option<Ret> {
        Some(Ret::Done(r.map(|n| {
            if let Some(n) = n {
                self.heap_out.push(n);
            }
        })))
    }

    /// The end of `instantiate_frame` (and of `user_module`).
    fn called(&mut self, name: Sym, loc: Loc, user: bool, r: R<()>) -> R<()> {
        if user {
            self.module_names.pop();
        }
        // A builtin module's own warnings (its argument checks) are raised
        // inside the try block, so they get the "called by" line.
        let r = r.and_then(|()| self.check_hard());
        r.map_err(|mut e| {
            let t = format!("called by '{}'", self.name(name));
            self.trace(&mut e, loc, t.into_bytes());
            e
        })
    }

    /// The start of `instantiate`: the statement's module, and the start
    /// of that.
    fn begin_inst(&mut self, sr: ScopeRef, i: usize, ctx: &Rc<Ctx>) -> Option<Ret> {
        if let Err(e) = self.check_interrupt() {
            return Some(Ret::Done(Err(e)));
        }
        self.work += 1;
        let name = self.inst_name(sr, i);
        let loc = self.inst_loc(sr, i);
        let found = match self.inst_res(sr, i).0 {
            0 => {
                if !self.syms.is_config(name) {
                    self.stats.fallbacks += 1;
                }
                self.lookup_module(ctx, name, loc)
            }
            r => self.find_module(sr.unit, r - 1, ctx, name, loc),
        };
        let found = match found {
            Ok(f) => f,
            Err(e) => return Some(Ret::Done(Err(e))),
        };
        let Some(m) = found else {
            // "Ignoring unknown module" is printed by the lookup, before
            // `ModuleInstantiation::evaluate`'s try block: no trace.
            return Some(Ret::Done(self.check_hard()));
        };
        match m {
            Instantiable::Builtin(b) => {
                self.heap.push(Frame::Called {
                    name,
                    loc,
                    user: false,
                });
                self.begin_builtin(b, sr, i, ctx)
            }
            Instantiable::User {
                ctx: dctx,
                unit,
                scope,
                index,
            } => self.begin_user(dctx, ScopeRef { unit, scope }, index, sr, i, ctx, name, loc),
        }
    }

    /// The start of `user_module` and `user_module_inner`.
    #[allow(clippy::too_many_arguments)]
    fn begin_user(
        &mut self,
        dctx: Rc<Ctx>,
        def_scope: ScopeRef,
        index: u32,
        sr: ScopeRef,
        i: usize,
        ctx: &Rc<Ctx>,
        name: Sym,
        loc: Loc,
    ) -> Option<Ret> {
        let mu = def_scope.unit;
        let def = &self.scope(def_scope).modules[index as usize];
        // The counted limit is what stops a module recursion. The native
        // checks (`recursion_exhausted`) are not asked here: this driver
        // only starts from a top-level statement (`memo.rs`), never from
        // inside an expression, so at a module call the native stack is
        // the driver's own at any depth and no expression frame is held.
        // They could only fire for a `frame_limit` of 0, and the call
        // checks still guard every expression a module's arguments run.
        if self.depth_exhausted() {
            self.heap.push(Frame::Called {
                name,
                loc,
                user: false,
            });
            let def_loc = Loc {
                unit: mu,
                span: def.span,
            };
            let t = format!("Recursion detected calling module '{}'", self.name(name));
            self.error(Some(def_loc), DiagCode::RecursionLimit, t);
            return Some(Ret::Done(Err(self.unwind(UnwindKind::Recursion))));
        }
        self.module_names.push(name);
        self.heap.push(Frame::Called {
            name,
            loc,
            user: true,
        });
        let body = ScopeRef {
            unit: mu,
            scope: self.units[mu as usize].scopes[def_scope.scope as usize].bodies[index as usize],
        };
        let inst = self.inst(sr, i);
        let args = match self.eval_args(sr.unit, &inst.args, ctx) {
            Ok(a) => a,
            Err(e) => return Some(Ret::Done(Err(e))),
        };
        let children = Children {
            scope: self.children_scope(sr, i),
            ctx: ctx.clone(),
        };
        let n_children = self.scope(children.scope).instantiations.len();
        let region = self.module_region(mu, def_scope.scope, index);
        let mctx = self.new_ctx(&dctx, CtxKind::Module(body, children), region);
        let (sc, sp) = (self.k.children, self.k.parent_modules);
        // `$children` is the region's last binder.
        let last = self.regions[region as usize].binds.len().wrapping_sub(1);
        self.set_bound(&mctx, last, sc, Value::Number(n_children as f64));
        self.set_var(&mctx, sp, Value::Number(self.module_names.len() as f64));
        if let Err(e) = self.bind_module(args, loc, mu, &def.params, &dctx, &mctx) {
            return Some(Ret::Done(Err(e)));
        }
        // Reuse a repeated call (see `crate::callmemo`), its children
        // included in the key.
        if self.cm.on
            && let Some(node) = self.call_enter((mu, def_scope.scope, index), &dctx, &mctx, (sr, i))
        {
            return self.done(Ok(Some(*node)));
        }
        let mark = self.push(mctx.clone());
        if let Err(e) = self.init_scope(&mctx, body) {
            // No parameter trace: OpenSCAD's try block starts after the
            // assignments.
            self.truncate(mark);
            if self.cm.recording_at(mark) {
                self.call_end(None);
            }
            return Some(Ret::Done(Err(e)));
        }
        let group = format!("module {}", self.units[mu as usize].ast.name(def.name));
        let node = self.new_node(NodeKind::Group { name: Some(group) }, sr, i);
        self.heap_nodes.push(node);
        self.heap.push(Frame::UserBody(UserBody {
            mark,
            mu,
            def,
            mctx: mctx.clone(),
        }));
        let out = self.heap_out.len() as u32;
        self.heap.push(Frame::Scope(ScopeRun {
            sr: body,
            ctx: mctx,
            mark: NO_MARK32,
            pos: 0,
            out,
            indices: false,
            collect: true,
        }));
        None
    }

    /// The end of `user_module_inner`.
    fn user_body_end(&mut self, b: UserBody<'a>, r: R<Vec<Node>>) -> R<Option<Node>> {
        let UserBody {
            mark,
            mu,
            def,
            mctx,
        } = b;
        let mut node = self.heap_nodes.pop().expect("a module's node");
        let r = match r {
            Ok(kids) => {
                node.children = kids;
                Ok(Some(node))
            }
            Err(mut e) => {
                drop(node);
                if self.opts.trace_usermodule_parameters {
                    let t = self.module_call_text(mu, def, &mctx);
                    let def_loc = Loc {
                        unit: mu,
                        span: def.span,
                    };
                    self.trace(&mut e, def_loc, t);
                }
                Err(e)
            }
        };
        self.truncate(mark);
        // Children a query instantiated for this call and no `children()`
        // took: the call is over, so nothing can take them now.
        if !self.held.is_empty() {
            self.drop_held(&mctx);
        }
        // A recording this call started sits at its stack index (and the
        // ones its body started have ended).
        if self.cm.recording_at(mark) {
            self.call_end(r.as_ref().ok().and_then(Option::as_ref));
        }
        r
    }

    /// `instantiate_scope`, on the top frame where it is: take the last
    /// statement's result (its node is already in `out`) and start the
    /// next statement.
    fn scope_step(&mut self, ret: Option<Ret>) -> Option<Ret> {
        if let Some(r) = ret {
            let Ret::Done(r) = r else {
                unreachable!("a statement's result")
            };
            if let Err(e) = r {
                return self.scope_end(Err(e));
            }
        }
        let Some(Frame::Scope(s)) = self.heap.last_mut() else {
            unreachable!("a scope on top")
        };
        let pos = s.pos as usize;
        let i = if s.indices {
            self.heap_indices.last().and_then(|ix| ix.get(pos)).copied()
        } else {
            (pos < self.units[s.sr.unit as usize].scopes[s.sr.scope as usize]
                .scope
                .instantiations
                .len())
            .then_some(pos)
        };
        let Some(i) = i else {
            return self.scope_end(Ok(()));
        };
        s.pos += 1;
        let (sr, ctx) = (s.sr, s.ctx.clone());
        self.begin_inst(sr, i, &ctx)
    }

    /// A scope's end: its context off the stack, its nodes (or the error)
    /// to the frame below.
    fn scope_end(&mut self, r: R<()>) -> Option<Ret> {
        let Some(Frame::Scope(s)) = self.heap.pop() else {
            unreachable!("a scope on top")
        };
        if s.mark != NO_MARK32 {
            self.truncate(s.mark as usize);
        }
        if s.indices {
            self.heap_indices.pop();
        }
        let out = s.out as usize;
        Some(Ret::Kids(match r {
            Ok(()) if s.collect => Ok(self.heap_out.split_off(out)),
            Ok(()) => Ok(Vec::new()),
            Err(e) => {
                // The nodes die with the error, as the native frames
                // holding them would drop them.
                self.heap_out.truncate(out);
                Err(e)
            }
        }))
    }

    /// The start of `instantiate_children`: a new scope context for
    /// `children`, its assignments, then its statements (or `indices`);
    /// `collect` as for [`ScopeRun::collect`].
    fn begin_children(
        &mut self,
        children: Children,
        indices: Option<Vec<usize>>,
        collect: bool,
    ) -> Option<Ret> {
        let region = self.units[children.scope.unit as usize].res.scope_region
            [children.scope.scope as usize];
        let c = self.new_ctx(&children.ctx, CtxKind::Scope(children.scope), region);
        let mark = self.push(c.clone());
        if let Err(e) = self.init_scope(&c, children.scope) {
            self.truncate(mark);
            return Some(Ret::Kids(Err(e)));
        }
        let has = indices.is_some();
        if let Some(ix) = indices {
            self.heap_indices.push(ix);
        }
        let out = self.heap_out.len() as u32;
        self.heap.push(Frame::Scope(ScopeRun {
            sr: children.scope,
            ctx: c,
            mark: mark as u32,
            pos: 0,
            out,
            indices: has,
            collect,
        }));
        None
    }

    /// [`Self::begin_children`] for a sketch body: after the body's
    /// assignments ran, the entities they made are named after their
    /// variables (`crate::sketch`), before any statement can print one.
    fn begin_sketch_body(&mut self, children: Children) -> Option<Ret> {
        let region = self.units[children.scope.unit as usize].res.scope_region
            [children.scope.scope as usize];
        let c = self.new_ctx(&children.ctx, CtxKind::Scope(children.scope), region);
        let mark = self.push(c.clone());
        let before = self.sketch_entities();
        if let Err(e) = self.init_scope(&c, children.scope) {
            self.truncate(mark);
            return Some(Ret::Kids(Err(e)));
        }
        self.sketch_label(before, children.scope, &c);
        let out = self.heap_out.len() as u32;
        self.heap.push(Frame::Scope(ScopeRun {
            sr: children.scope,
            ctx: c,
            mark: mark as u32,
            pos: 0,
            out,
            indices: false,
            collect: true,
        }));
        None
    }

    /// The children of instantiation `i` of `sr` in `ctx` into `node`,
    /// then `post` (`with_children`).
    fn begin_wrap(
        &mut self,
        node: Node,
        post: Post,
        sr: ScopeRef,
        i: usize,
        ctx: &Rc<Ctx>,
    ) -> Option<Ret> {
        let ch = Children {
            scope: self.children_scope(sr, i),
            ctx: ctx.clone(),
        };
        self.heap_nodes.push(node);
        self.heap.push(Frame::Wrap(post));
        self.begin_children(ch, None, true)
    }

    /// The start of `builtin_module`. Its frame-budget check is left out:
    /// statements add nothing to the budget here.
    fn begin_builtin(
        &mut self,
        b: BuiltinModule,
        sr: ScopeRef,
        i: usize,
        ctx: &Rc<Ctx>,
    ) -> Option<Ret> {
        use BuiltinModule as B;
        let loc = self.inst_loc(sr, i);
        macro_rules! tri {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(e) => return Some(Ret::Done(Err(e))),
                }
            };
        }
        match b {
            B::Children => {
                let args = tri!(self.inst_args(sr, i, ctx));
                self.no_children(sr, i);
                let p = self.params(args, loc, &[], &["index"], "children");
                let Some(children) = ctx.module_children() else {
                    self.end(p);
                    return Some(Ret::Done(Ok(())));
                };
                let size = self.scope(children.scope).instantiations.len();
                let Some(indices) = self.children_select(&p, size) else {
                    self.end(p);
                    return Some(Ret::Done(Ok(())));
                };
                let mut node = self.new_node(NodeKind::Group { name: None }, sr, i);
                // The same children a query of this call already
                // instantiated (`crate::query`): put in place, not run
                // again.
                if !self.held.is_empty() && self.reuse_held(&mut node, ctx, &indices) {
                    self.end(p);
                    return self.done(Ok(Some(node)));
                }
                let post = Post {
                    mark: p.mark,
                    ..PLAIN
                };
                self.heap_nodes.push(node);
                self.heap.push(Frame::Wrap(post));
                self.begin_children(children, indices, true)
            }
            B::Echo | B::Assert => {
                let inst = self.inst(sr, i);
                if b == B::Echo {
                    tri!(self.echo(sr.unit, &inst.args, ctx));
                } else {
                    tri!(self.perform_assert(sr.unit, &inst.args, inst.span, ctx));
                }
                let node = self.new_node(NodeKind::Group { name: None }, sr, i);
                let post = Post {
                    filter: true,
                    ..PLAIN
                };
                self.begin_wrap(node, post, sr, i, ctx)
            }
            B::Let => {
                let inst = self.inst(sr, i);
                let region = self.inst_res(sr, i).1;
                let c = self.new_ctx(ctx, CtxKind::Plain, region);
                let mark = self.push(c.clone());
                if let Err(e) = self.sequential_assign(sr.unit, &inst.args, inst.span, &c) {
                    self.truncate(mark);
                    return Some(Ret::Done(Err(e)));
                }
                let node = self.new_node(NodeKind::Group { name: None }, sr, i);
                self.begin_wrap(node, Post { mark, ..PLAIN }, sr, i, &c)
            }
            B::For | B::IntersectionFor => {
                let kind = if b == B::For {
                    NodeKind::Group { name: None }
                } else {
                    NodeKind::IntersectionFor
                };
                let node = self.new_node(kind, sr, i);
                let inst = self.inst(sr, i);
                if inst.args.is_empty() {
                    return self.done(Ok(Some(node)));
                }
                let body = self.children_scope(sr, i);
                let region = self.inst_res(sr, i).1;
                self.heap_nodes.push(node);
                self.heap.push(Frame::Wrap(PLAIN));
                self.begin_for(sr.unit, &inst.args, region, loc, ctx.clone(), body, true)
            }
            B::If => {
                let inst = self.inst(sr, i);
                let args = tri!(self.eval_args(sr.unit, &inst.args, ctx));
                let branch = if args.first().is_some_and(|a| a.value.to_bool()) {
                    Some(self.children_scope(sr, i))
                } else {
                    self.else_scope(sr, i)
                };
                let Some(scope) = branch else {
                    return Some(Ret::Done(Ok(())));
                };
                let node = self.new_node(NodeKind::Group { name: None }, sr, i);
                self.heap_nodes.push(node);
                self.heap_args.push(args);
                self.heap.push(Frame::Wrap(Post {
                    args: true,
                    ..PLAIN
                }));
                let ch = Children {
                    scope,
                    ctx: ctx.clone(),
                };
                self.begin_children(ch, None, true)
            }
            B::Part => {
                let args = tri!(self.inst_args(sr, i, ctx));
                let p = self.params(args, loc, &["name"], &[], "part");
                let mark = p.mark;
                match self.part_name(&p, loc) {
                    None => {
                        let node = self.new_node(NodeKind::Group { name: None }, sr, i);
                        self.begin_wrap(node, Post { mark, ..PLAIN }, sr, i, ctx)
                    }
                    Some(full) => {
                        let node = self.new_node(NodeKind::Part { name: full.clone() }, sr, i);
                        self.part_stack.push(full);
                        let post = Post {
                            part: true,
                            mark,
                            ..PLAIN
                        };
                        self.begin_wrap(node, post, sr, i, ctx)
                    }
                }
            }
            B::Sketch => {
                let args = tri!(self.inst_args(sr, i, ctx));
                let p = self.params(args, loc, &[], &["name", "strict", "convexity"], "sketch");
                // What a sketch makes depends on everything its body ran,
                // and it is solved at its end: never replayed from a memo.
                self.untracked_sketch();
                // A `sketch()` met while one is being built (a helper
                // module's body, called from a sketch body) adds to it.
                let merge = self.sketch.is_some();
                if !merge {
                    let body = self.children_scope(sr, i);
                    self.sketch_open(&p, loc, body);
                }
                let node = self.new_node(NodeKind::Group { name: None }, sr, i);
                let post = Post {
                    mark: p.mark,
                    sketch: if merge { SKETCH_MERGE } else { SKETCH_TOP },
                    ..PLAIN
                };
                let ch = Children {
                    scope: self.children_scope(sr, i),
                    ctx: ctx.clone(),
                };
                self.heap_nodes.push(node);
                self.heap.push(Frame::Wrap(post));
                self.begin_sketch_body(ch)
            }
            B::SketchStatement(v) => Some(Ret::Done(self.sketch_statement(v, sr, i, ctx))),
            B::Anchor => Some(Ret::Done(self.anchor_statement(sr, i, ctx))),
            B::FilletEdges | B::ChamferEdges => {
                let args = tri!(self.inst_args(sr, i, ctx));
                let (req, opt) = crate::fillet::params(b);
                let p = self.params(args, loc, req, opt, b.fillet_caller());
                let kind = self.fillet_kind(b, &p, sr, i);
                let node = self.new_node(kind, sr, i);
                let post = Post {
                    mark: p.mark,
                    ..PLAIN
                };
                self.begin_wrap(node, post, sr, i, ctx)
            }
            _ => {
                // `geometry_module` and `geometry_node`.
                let args = tri!(self.inst_args(sr, i, ctx));
                let leaf = is_leaf(b);
                if leaf {
                    self.no_children(sr, i);
                }
                let (req, opt, caller) = geometry_params(b);
                let p = self.params(args, loc, req, opt, caller);
                let kind = self.geometry_kind(b, &p);
                let node = self.new_node(kind, sr, i);
                if leaf {
                    self.end(p);
                    return self.done(Ok(Some(node)));
                }
                let post = Post {
                    mark: p.mark,
                    ..PLAIN
                };
                self.begin_wrap(node, post, sr, i, ctx)
            }
        }
    }

    /// The start of `for_each` over `args` in `ctx`: the first variable's
    /// values, then its loop; with no variables left, the body. `collect`
    /// as for [`ForVar::collect`].
    #[allow(clippy::too_many_arguments)]
    fn begin_for(
        &mut self,
        u: u32,
        args: &'a [Arg],
        region: u32,
        loc: Loc,
        ctx: Rc<Ctx>,
        body: ScopeRef,
        collect: bool,
    ) -> Option<Ret> {
        let Some((first, rest)) = args.split_first() else {
            return self.begin_children(Children { scope: body, ctx }, None, collect);
        };
        let name = first
            .name
            .map_or(self.k.empty, |n| self.units[u as usize].sym(n));
        let values = match self.eval(u, first.expr, &ctx) {
            Ok(v) => v,
            Err(e) => return Some(Ret::Kids(Err(e))),
        };
        let mode = if self.regions[region as usize].reg() {
            debug_assert_eq!(self.regions[region as usize].binds.first(), Some(&0));
            let old = self.reg_open(region);
            ForMode::Reg {
                i: self.reg_base[region as usize] as usize,
                old,
            }
        } else {
            ForMode::Ctx {
                slot: self.regions[region as usize]
                    .binds
                    .first()
                    .copied()
                    .unwrap_or(NO_SLOT),
                name,
                config: self.syms.is_config(name),
            }
        };
        // `iterate_over`'s one check before the loop.
        let mut len = 0;
        if let Value::Range(r) = &values {
            let n = r.num_values();
            if n >= 1_000_000 {
                self.warn(
                    loc,
                    DiagCode::IterationLimit,
                    format!("Bad range parameter in for statement: too many elements ({n})"),
                );
            } else {
                len = r.iter_len();
            }
        }
        self.heap.push(Frame::For(Box::new(ForVar {
            u,
            rest,
            region,
            loc,
            ctx,
            body,
            values,
            pos: 0,
            len,
            mode,
            cur: None,
            out: self.heap_out.len(),
            collect,
        })));
        None
    }

    /// The next value a `for` variable takes (`iterate_over`'s iterators).
    fn for_next(f: &mut ForVar<'a>) -> Option<Value> {
        let v = match &f.values {
            Value::Range(r) => {
                if f.pos >= f.len as usize {
                    return None;
                }
                Value::Number(r.iter_at(f.pos as u32))
            }
            Value::Vector(v) => v.get(f.pos)?.clone(),
            Value::Str(s) => {
                let b = s.as_bytes().get(f.pos..)?;
                let c = crate::utf8::chars(b).next()?;
                f.pos += c.len();
                return Some(Value::str(c));
            }
            // An object iterates over its keys.
            Value::Object(o) => Value::Str(o.keys().get(f.pos)?.clone()),
            Value::Undef => return None,
            other => {
                if f.pos > 0 {
                    return None;
                }
                other.clone()
            }
        };
        f.pos += 1;
        Some(v)
    }

    /// The end of a `for` variable's loop: its register instance closed,
    /// and its nodes handed down (or dropped with an error).
    fn for_end(&mut self, f: &ForVar<'a>, r: R<()>) -> Option<Ret> {
        if let ForMode::Reg { old, .. } = f.mode {
            self.reg_close(f.region, old);
        }
        Some(Ret::Kids(match r {
            Ok(()) if f.collect => Ok(self.heap_out.split_off(f.out)),
            Ok(()) => Ok(Vec::new()),
            Err(e) => {
                self.heap_out.truncate(f.out);
                Err(e)
            }
        }))
    }

    /// A `for` variable's loop: end the iteration that returned `ret`,
    /// then start the next one.
    fn for_step(&mut self, mut f: Box<ForVar<'a>>, ret: Option<Ret>) -> Option<Ret> {
        if let Some(ret) = ret {
            let Ret::Kids(r) = ret else {
                unreachable!("an iteration's nodes")
            };
            match f.mode {
                // The iteration's value dies here, where its context would.
                ForMode::Reg { i, .. } => self.regs[i] = None,
                ForMode::Ctx { .. } => {
                    let (c, mark) = f.cur.take().expect("an iteration's context");
                    self.truncate(mark);
                    // Each iteration's context dies here unless the body
                    // captured it; the next iteration reuses it.
                    Ctx::recycle(c, &mut self.ctx_pool);
                }
            }
            if let Err(e) = r {
                return self.for_end(&f, Err(e));
            }
        }
        let Some(v) = Self::for_next(&mut f) else {
            return self.for_end(&f, Ok(()));
        };
        if let Err(e) = self.check_interrupt() {
            return self.for_end(&f, Err(e));
        }
        let ctx = match f.mode {
            ForMode::Reg { i, .. } => {
                self.regs[i] = Some(v);
                f.ctx.clone()
            }
            ForMode::Ctx { slot, name, config } => {
                let c = match slot {
                    NO_SLOT => self.iteration_vars(&f.ctx, f.region, name, config, v),
                    s => Ctx::with_slot(&mut self.ctx_pool, &f.ctx, f.region, s, v),
                };
                let mark = self.push(c.clone());
                f.cur = Some((c.clone(), mark));
                c
            }
        };
        let (u, rest, region, loc, body) = (f.u, f.rest, f.region, f.loc, f.body);
        self.heap.push(Frame::For(f));
        // The body's nodes stay on the node stack, after the earlier
        // iterations', until the loop ends.
        if rest.is_empty() {
            self.begin_children(Children { scope: body, ctx }, None, false)
        } else {
            let next = crate::resolve::next_region(region);
            self.begin_for(u, rest, next, loc, ctx, body, false)
        }
    }
}
