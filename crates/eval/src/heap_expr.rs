//! The heap evaluator's expressions: function calls, list
//! comprehensions, `let`, `assert` and `echo`, and every expression
//! around them.
//!
//! The recursive (native) evaluator in `eval.rs` and `call.rs` evaluates
//! `1 + f(n - 1)` by calling into it: `eval` → `eval_binary` → `eval` →
//! `eval_call` → `simplify` … → `eval`, several native frames per level of
//! the recursion, so on its own how deep a function recursion could go
//! was decided by the native stack: 110,000 levels natively, and in a
//! browser whatever the engine gives a wasm thread (67 levels in WebKit).
//! Here an expression that can reach a user call runs in one loop over an
//! explicit stack of [`XFrame`]s, so a recursion through functions and
//! comprehensions holds no native stack and stops at the counted limit
//! ([`crate::limits::Limits::depth`]), which counts the calls in progress
//! together with the user modules (see [`Evaluator::depth_used`]).
//!
//! When: the first [`NATIVE_CALLS`] nested user calls still run natively,
//! where nearly all of a program's work is done; `eval_call` hands the
//! next one to this loop, and everything it reaches runs here until it
//! returns. The native stack then holds a bounded number of call levels
//! at any depth, and the native evaluator's tuned code does the common
//! work at native speed.
//!
//! Which expressions, on the heap: [`Evaluator::may_call`], a bit per
//! expression, is set when evaluating it can reach a call that is not to
//! a builtin the resolver pinned down (`Evaluator::static_builtin`). The
//! others, by far the most (arithmetic, indexing, variables, `len(v)`),
//! run through the native evaluator unchanged, whose depth is then the
//! source's nesting, not the recursion's.
//!
//! How: as in `crate::heap`, a frame is the part of a native function that
//! runs after its callee returns. A step either asks the loop to evaluate
//! an expression ([`Next::Eval`]), having pushed the frame that takes its
//! value, or hands a result to the frame on top ([`Next::Val`]). The leaves
//! are the native evaluator's own: operators, lookups, binding, the
//! builtins, the tail-call steps, the accumulator moves and the register
//! regions run the same functions in the same order, so output is
//! byte-identical. What differs is only what a native frame held: here a
//! frame holds a clone of the context it evaluates in, where the native
//! frame borrowed it. That changes no value, but it can make a context
//! look shared to `call::private_binder` where natively it was not, which
//! would only cost an accumulator its in-place growth; the tail-call loop
//! itself borrows its contexts as the native one does, which is where the
//! moves that matter are made.
//!
//! A few rare shapes stay native even on the heap, and a call they reach
//! starts a nested loop (`eval_call` past `NATIVE_CALLS`). Each such level
//! costs native stack and is still bounded by the native checks
//! (`recursion_exhausted`), which in a browser stop it after a few dozen
//! levels (`recursion::HEAP_LOOP_FRAMES`):
//! - C-style `for` comprehensions (`for (i = f(n); ...)`): their loop
//!   keeps two contexts and an iteration's state across its parts, a
//!   state machine of its own to move;
//! - `object()`'s arguments: it builds the object as it goes and stops at
//!   the first argument it cannot use, so its state (an `ObjectBuilder`)
//!   would wait on a side stack of its own; and the function is
//!   experimental;
//! - parameter defaults, evaluated in the middle of binding a call's
//!   arguments (`bind_user`), which would have to be split around them;
//! - `use`d libraries' assignments, evaluated once, not per level.
//!
//! Ranges, `is_undef()`'s argument, callees that are expressions
//! (`f(x)(y)`) and methods' arguments moved here from that list: a
//! recursion through them stopped after 30 to 37 levels in a browser, and
//! now reaches the counted limit there as natively.

use std::rc::Rc;

use lang::ast::{Arg, Ast, BinaryOp, ExprId, ExprKind, Param, UnaryOp};
use lang::diag::DiagCode;
use lang::source::Span;

use crate::builtins::functions::Builtin;
use crate::call::{ArgVal, Callable};
use crate::context::{Ctx, CtxKind};
use crate::eval::{Evaluator, Mode, Step};
use crate::message::{Loc, R, UnwindKind};
use crate::ops;
use crate::resolve::NO_SLOT;
use crate::sym::Sym;
use crate::value::{Growable, Object, Value};

/// How many user calls run natively, nested, before the next one goes on
/// the heap (`Evaluator::eval_call`). The native evaluator's tuned code
/// does the common work, at the shallow depths most programs never leave;
/// the heap takes over for a recursion past this depth, so the native
/// stack holds at most this many call levels at any depth. Running every
/// call on the heap cost 26% on BOSL2's examples: every expression node on
/// the way to a call is a frame there, 1.3-1.6 times the native cost per
/// call. With 8 native levels the examples run at parity.
///
/// Output is the same whichever runs a call. A debug build runs every call
/// on the heap: its native frames are many times larger (8 levels overflow
/// a 128 KiB thread), and its tests then cover the heap paths everywhere,
/// while the release build's mix is covered by conformance and the corpus
/// A/B.
pub(crate) const NATIVE_CALLS: u32 = if cfg!(debug_assertions) { 0 } else { 8 };

/// What a requested evaluation produces.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Want {
    /// A value (`Evaluator::eval`).
    Value,
    /// A vector element, appended to the top of [`Stacks::outs`]
    /// (`Evaluator::eval_element`).
    Element,
    /// A comprehension's values, appended there (`Evaluator::eval_lc`).
    Lc,
}

/// The loop's next move.
pub(crate) enum Next {
    /// Evaluate `id` in `ctx`, and hand the result to the frame on top.
    Eval {
        u: u32,
        id: ExprId,
        ctx: Rc<Ctx>,
        want: Want,
    },
    /// A result for the frame on top: a value, or `undef` for an element
    /// or comprehension, whose values went onto the output stack.
    Val(R<Value>),
}

/// One suspended native function of the native evaluator. Most name
/// their expression by `(u, id)` and read the rest from the syntax tree;
/// the larger ones keep their state on a side stack of [`Stacks`].
pub(crate) enum XFrame<'a> {
    /// `eval_binary` waiting for its left operand, then its right.
    Bin {
        u: u32,
        id: ExprId,
        ctx: Rc<Ctx>,
    },
    Bin2 {
        u: u32,
        id: ExprId,
        a: Value,
    },
    /// `&&` and `||`: the left operand, then the right unless it decided.
    Logic {
        u: u32,
        id: ExprId,
        ctx: Rc<Ctx>,
    },
    Logic2,
    /// A ternary's condition (outside a tail call; see `Phase::Cond`).
    Tern {
        u: u32,
        id: ExprId,
        ctx: Rc<Ctx>,
    },
    /// An index expression's list, then its index.
    Idx {
        u: u32,
        id: ExprId,
        ctx: Rc<Ctx>,
    },
    Idx2 {
        a: Value,
    },
    Unary {
        u: u32,
        id: ExprId,
    },
    Member {
        u: u32,
        id: ExprId,
    },
    /// A vector's element `k` (its list on top of `outs`; with `grow`, the
    /// `[each x, ...]` list it ends up appended to on top of `grow`).
    Vector {
        u: u32,
        id: ExprId,
        k: u32,
        grow: bool,
        ctx: Rc<Ctx>,
    },
    /// `[each x, ...]`'s `x` (`Evaluator::each_then`).
    EachHead {
        u: u32,
        id: ExprId,
        ctx: Rc<Ctx>,
    },
    /// A comprehension evaluated as a value: its list is on top of `outs`.
    LcVal,
    /// An element's value, to append (and check the list limit).
    ElemPush {
        u: u32,
        id: ExprId,
    },
    /// A comprehension element done: check the list limit.
    ElemCheck {
        u: u32,
        id: ExprId,
    },
    /// A comprehension `if`'s condition.
    LcIf {
        u: u32,
        id: ExprId,
        ctx: Rc<Ctx>,
    },
    /// `each x`'s value, or (`Inner`) the list of a comprehension `x`.
    LcEach {
        u: u32,
        id: ExprId,
    },
    LcEachInner {
        u: u32,
        id: ExprId,
    },
    /// What comprehension `for` variable `k` iterates over.
    ForValues {
        u: u32,
        id: ExprId,
        k: u32,
        region: u32,
        ctx: Rc<Ctx>,
    },
    /// A comprehension `for` variable's loop (top of `fors`).
    For,
    /// A `let` (top of `lets`).
    Let,
    /// Arguments: `args[k]`'s value, into the top of [`Stacks::args`].
    Args {
        u: u32,
        args: &'a [Arg],
        k: u32,
        ctx: Rc<Ctx>,
    },
    /// A call that is always to builtin `b`, once its arguments are in.
    Builtin {
        b: Builtin,
        u: u32,
        id: ExprId,
    },
    /// `assert` and `echo` expressions (outside a tail call), once their
    /// arguments are in.
    Assert {
        u: u32,
        id: ExprId,
        ctx: Rc<Ctx>,
    },
    Echo {
        u: u32,
        id: ExprId,
        ctx: Rc<Ctx>,
    },
    /// A range's begin, then its end, then its step
    /// (`Evaluator::eval_range`). The step is evaluated last, and only
    /// when begin and end are numbers.
    RangeBegin {
        u: u32,
        id: ExprId,
        ctx: Rc<Ctx>,
    },
    RangeEnd {
        u: u32,
        id: ExprId,
        ctx: Rc<Ctx>,
        b: Value,
    },
    RangeStep {
        u: u32,
        id: ExprId,
        bd: f64,
        ed: f64,
    },
    /// `is_undef(x)`'s argument, when `x` is not a plain variable (one is
    /// looked up without its unknown-variable warning, natively).
    IsUndef {
        u: u32,
        id: ExprId,
    },
    /// A user call's tail-call loop (top of `calls`).
    Call,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<XFrame<'static>>() <= 48);

/// The expression frames and the state that waits beside them.
#[derive(Default)]
pub(crate) struct Stacks<'a> {
    frames: Vec<XFrame<'a>>,
    /// The lists being filled by vectors and comprehensions.
    outs: Vec<Vec<Value>>,
    /// The arguments being evaluated (vectors from `arg_pool`).
    args: Vec<Vec<ArgVal>>,
    // Boxed on purpose: a state moves between here and the loop at every
    // wait, as a pointer rather than a few hundred bytes.
    #[allow(clippy::vec_box)]
    calls: Vec<Box<CallSt<'a>>>,
    /// Finished calls' states, for the next calls: a call's state moves
    /// between the loop and this stack whenever it waits, and boxed it
    /// moves as a pointer.
    #[allow(clippy::vec_box)]
    call_pool: Vec<Box<CallSt<'a>>>,
    fors: Vec<ForSt>,
    lets: Vec<LetSt>,
    grow: Vec<Growable>,
    /// Scratch for [`Evaluator::may_call`]'s walk.
    walk: Vec<(ExprId, bool)>,
}

impl Stacks<'_> {
    /// Estimated bytes these stacks hold, for the memory limit
    /// (`Evaluator::held_bytes`): every stack's elements, each call's
    /// boxed state, and a small allocation per list being filled and per
    /// argument vector (their values are counted where they are made).
    /// O(1), so the evaluator's periodic limit check can afford it.
    pub(crate) fn held_bytes(&self) -> u64 {
        use crate::eval::held;
        use crate::limits::live::BOX;
        let call = std::mem::size_of::<CallSt<'static>>() as u64 + 16;
        held(&self.frames)
            + held(&self.outs)
            + self.outs.len() as u64 * BOX
            + held(&self.args)
            + self.args.len() as u64 * BOX
            + held(&self.calls)
            + held(&self.call_pool)
            + (self.calls.len() + self.call_pool.len()) as u64 * call
            + held(&self.fors)
            + held(&self.lets)
            + held(&self.grow)
            + held(&self.walk)
    }
}

/// `eval_call`'s loop: its locals, and what its step waits for.
pub(crate) struct CallSt<'a> {
    slot: usize,
    regs: usize,
    saves: usize,
    cur: Option<Rc<Ctx>>,
    /// The caller's context, which the first step runs in (`None` once
    /// the call is over and its state pooled).
    entry: Option<Rc<Ctx>>,
    mode: Mode,
    unit: u32,
    expr: Option<ExprId>,
    call: (u32, ExprId),
    depth: u32,
    phase: Phase<'a>,
}

/// What a tail-call step (`simplify`) waits for.
enum Phase<'a> {
    Step,
    /// A ternary's condition.
    Cond {
        a: ExprId,
        b: ExprId,
    },
    /// `assert` and `echo`'s arguments (top of `args`).
    Assert {
        args: &'a [Arg],
        span: Span,
        body: Option<ExprId>,
    },
    Echo {
        args: &'a [Arg],
        body: Option<ExprId>,
    },
    /// A tail `let`'s argument `k`, in registers or into context `c`.
    LetReg {
        args: &'a [Arg],
        span: Span,
        region: u32,
        k: u32,
        body: ExprId,
    },
    LetCtx {
        args: &'a [Arg],
        span: Span,
        k: u32,
        body: ExprId,
        c: Rc<Ctx>,
    },
    /// A builtin's arguments (top of `args`).
    Builtin {
        b: Builtin,
        u: u32,
        id: ExprId,
    },
    /// A user call's arguments (top of `args`): bound into a pure frame
    /// (`pure` holds the defining context) or into `body_ctx`.
    User {
        u: u32,
        id: ExprId,
        fu: u32,
        params: &'a [Param],
        body: ExprId,
        region: u32,
        loc: Loc,
        moved: Option<u32>,
        pure: Option<Rc<Ctx>>,
        body_ctx: Option<Rc<Ctx>>,
        /// A method's object, bound to its `this` parameter.
        this: Option<Object>,
    },
    /// `is_undef()`'s argument (see `Evaluator::heap_is_undef_arg`).
    IsUndef,
    /// A callee that is an expression, for the call `id`.
    Callee {
        id: ExprId,
        args: &'a [Arg],
        loc: Loc,
    },
    /// The step's own value: an expression in tail position that is not
    /// one of the above.
    Done,
}

/// A step of the tail-call loop: done, or waiting.
enum S {
    Step(R<Step>),
    Wait(Next),
}

/// A comprehension `for` variable's loop (`for_each`).
pub(crate) struct ForSt {
    u: u32,
    id: ExprId,
    k: u32,
    region: u32,
    ctx: Rc<Ctx>,
    values: Value,
    pos: usize,
    len: u32,
    mode: ForMode,
    cur: Option<(Rc<Ctx>, usize)>,
}

#[derive(Clone, Copy)]
enum ForMode {
    Reg { i: usize, old: u32 },
    Ctx { slot: u32, name: Sym, config: bool },
}

/// A `let` (or comprehension `let`): its next assignment, or its body.
pub(crate) struct LetSt {
    u: u32,
    id: ExprId,
    k: u32,
    want: Want,
    body: bool,
    /// The region's previous register base, for a register `let`.
    old: u32,
    /// The context it binds into and its stack mark, otherwise.
    c: Option<(Rc<Ctx>, usize)>,
    /// The context it is evaluated in.
    ctx: Rc<Ctx>,
}

fn next(u: u32, expr: Option<ExprId>) -> Step {
    Step::Next {
        unit: u,
        expr,
        ctx: None,
        call: None,
    }
}

impl<'a> Evaluator<'a> {
    // --- which expressions ---------------------------------------------

    /// Whether the next user call goes on the heap: `NATIVE_CALLS` run
    /// natively already.
    #[inline(always)]
    // `NATIVE_CALLS` is 0 in a debug build, where this is always true.
    #[allow(clippy::absurd_extreme_comparisons)]
    pub(crate) fn calls_deep(&self) -> bool {
        self.native_calls >= NATIVE_CALLS
    }

    /// Whether evaluating `id` can reach a call that is not always a
    /// builtin: a user function, a function literal, a method, a name only
    /// a dynamic lookup can answer. Remembered per expression.
    #[inline(always)]
    pub(crate) fn may_call(&mut self, u: u32, id: ExprId) -> bool {
        match self.units[u as usize].may_call.get(id.0 as usize) {
            Some(1) => false,
            Some(2) => true,
            _ => self.find_may_call(u, id),
        }
    }

    /// [`Self::may_call`] for an expression not seen yet: the subtree
    /// walked bottom-up with an explicit stack (expressions can nest
    /// deeper than the native stack), and every node in it remembered.
    /// A function literal's body runs only when the literal is called,
    /// and that call is what may call, so the walk stops at literals.
    #[cold]
    #[inline(never)]
    fn find_may_call(&mut self, u: u32, id: ExprId) -> bool {
        let ast: &'a Ast = self.units[u as usize].ast;
        let mut cache = std::mem::take(&mut self.units[u as usize].may_call);
        if cache.is_empty() {
            cache = vec![0; ast.exprs.len()];
        }
        let mut walk = std::mem::take(&mut self.xs.walk);
        walk.push((id, false));
        let mut kids: Vec<ExprId> = Vec::new();
        while let Some((e, done)) = walk.pop() {
            if cache[e.0 as usize] != 0 {
                continue;
            }
            kids.clear();
            children(ast, e, &mut kids);
            if !done {
                walk.push((e, true));
                walk.extend(
                    kids.iter()
                        .filter(|k| cache[k.0 as usize] == 0)
                        .map(|&k| (k, false)),
                );
                continue;
            }
            let own = matches!(ast.expr(e).kind, ExprKind::Call(..))
                && self.static_builtin(u, e).is_none();
            let yes = own || kids.iter().any(|k| cache[k.0 as usize] == 2);
            cache[e.0 as usize] = if yes { 2 } else { 1 };
        }
        self.xs.walk = walk;
        let yes = cache[id.0 as usize] == 2;
        self.units[u as usize].may_call = cache;
        yes
    }

    fn args_may_call(&mut self, u: u32, args: &[Arg]) -> bool {
        args.iter().any(|a| self.may_call(u, a.expr))
    }

    // --- the loop ------------------------------------------------------

    /// [`Self::eval`] on the heap, for an expression that may call.
    #[inline(never)]
    pub(crate) fn heap_eval(&mut self, u: u32, id: ExprId, ctx: &Rc<Ctx>) -> R<Value> {
        // Started from native code: a native level of the frame budget,
        // so a chain of the rare shapes that stay native (see the module
        // docs) is still counted, at what this loop's large native frames
        // cost (`HEAP_LOOP_FRAMES`).
        self.frames += crate::recursion::HEAP_LOOP_FRAMES;
        let base = self.xs.frames.len();
        let mut next = self.x_value(u, id, ctx.clone());
        let r = loop {
            next = match next {
                Next::Eval { u, id, ctx, want } => match want {
                    Want::Value => self.x_value(u, id, ctx),
                    Want::Element => self.x_element(u, id, ctx),
                    Want::Lc => self.x_lc(u, id, ctx),
                },
                Next::Val(r) => {
                    if self.xs.frames.len() == base {
                        break r;
                    }
                    self.x_resume(r)
                }
            };
        };
        self.frames -= crate::recursion::HEAP_LOOP_FRAMES;
        self.hard(r)
    }

    /// What `eval` does after every expression: raise a pending
    /// `--hardwarnings` abort or passed limit. The frame that takes a
    /// value calls it, so it runs where the native `eval` returned.
    #[inline(always)]
    fn hard(&self, r: R<Value>) -> R<Value> {
        let v = r?;
        self.check_hard()?;
        Ok(v)
    }

    /// The top of the output stack, taken for a native function that fills
    /// a `&mut Vec`; [`Self::put_out`] puts it back.
    fn take_out(&mut self) -> Vec<Value> {
        std::mem::take(self.xs.outs.last_mut().expect("a list being filled"))
    }

    fn put_out(&mut self, out: Vec<Value>) {
        *self.xs.outs.last_mut().expect("a list being filled") = out;
    }

    /// The start of `eval`.
    fn x_value(&mut self, u: u32, id: ExprId, ctx: Rc<Ctx>) -> Next {
        if !self.may_call(u, id) {
            return Next::Val(self.eval_native(u, id, &ctx));
        }
        let ast: &'a Ast = self.units[u as usize].ast;
        let e = ast.expr(id);
        let eval = |id: ExprId, ctx: Rc<Ctx>| Next::Eval {
            u,
            id,
            ctx,
            want: Want::Value,
        };
        match &e.kind {
            ExprKind::Binary(op, l, _) => {
                let logic = matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr);
                // An operand that cannot call is evaluated here, rather
                // than through a frame and the loop.
                if !self.may_call(u, *l) {
                    return match self.eval_native(u, *l, &ctx) {
                        Ok(a) if logic => self.logic_rhs(u, id, a, ctx),
                        Ok(a) => self.bin_rhs(u, id, a, ctx),
                        Err(e) => Next::Val(Err(e)),
                    };
                }
                let c = ctx.clone();
                self.xs.frames.push(if logic {
                    XFrame::Logic { u, id, ctx: c }
                } else {
                    XFrame::Bin { u, id, ctx: c }
                });
                eval(*l, ctx)
            }
            ExprKind::Ternary(c, a, b) => {
                if !self.may_call(u, *c) {
                    return match self.eval_native(u, *c, &ctx) {
                        Ok(v) => eval(if v.to_bool() { *a } else { *b }, ctx),
                        Err(e) => Next::Val(Err(e)),
                    };
                }
                self.xs.frames.push(XFrame::Tern {
                    u,
                    id,
                    ctx: ctx.clone(),
                });
                eval(*c, ctx)
            }
            ExprKind::Index(a, i) => {
                if !self.may_call(u, *a) {
                    return match self.eval_native(u, *a, &ctx) {
                        Ok(a) => self.idx_rhs(u, *i, a, ctx),
                        Err(e) => Next::Val(Err(e)),
                    };
                }
                self.xs.frames.push(XFrame::Idx {
                    u,
                    id,
                    ctx: ctx.clone(),
                });
                eval(*a, ctx)
            }
            ExprKind::Call(..) => self.x_call(u, id, ctx),
            ExprKind::Unary(_, x) => {
                self.xs.frames.push(XFrame::Unary { u, id });
                eval(*x, ctx)
            }
            ExprKind::Member(x, _) => {
                self.xs.frames.push(XFrame::Member { u, id });
                eval(*x, ctx)
            }
            ExprKind::Vector(items) => {
                if let Some(&first) = items.first()
                    && let ExprKind::LcEach(x) = ast.expr(first).kind
                    && !self.is_lc(u, x)
                {
                    self.xs.frames.push(XFrame::EachHead {
                        u,
                        id,
                        ctx: ctx.clone(),
                    });
                    return eval(x, ctx);
                }
                self.xs.outs.push(Vec::with_capacity(items.len()));
                self.vec_next(u, id, 0, false, ctx)
            }
            ExprKind::Let(..) => self.x_let(u, id, ctx, Want::Value),
            ExprKind::Assert(args, body) | ExprKind::Echo(args, body) => {
                let assert = matches!(e.kind, ExprKind::Assert(..));
                if self.args_may_call(u, args) {
                    let argv = self.arg_pool.pop().unwrap_or_default();
                    self.xs.args.push(argv);
                    let c = ctx.clone();
                    self.xs.frames.push(if assert {
                        XFrame::Assert { u, id, ctx: c }
                    } else {
                        XFrame::Echo { u, id, ctx: c }
                    });
                    return self.x_args(u, args, 0, ctx);
                }
                let r = if assert {
                    self.perform_assert(u, args, e.span, &ctx)
                } else {
                    self.echo(u, args, &ctx)
                };
                match (r, body) {
                    (Err(e), _) => Next::Val(Err(e)),
                    (Ok(()), Some(b)) => eval(*b, ctx),
                    (Ok(()), None) => Next::Val(Ok(Value::Undef)),
                }
            }
            ExprKind::LcIf(..)
            | ExprKind::LcEach(_)
            | ExprKind::LcFor(..)
            | ExprKind::LcForC { .. }
            | ExprKind::LcLet(..) => {
                self.xs.outs.push(Vec::new());
                self.xs.frames.push(XFrame::LcVal);
                Next::Eval {
                    u,
                    id,
                    ctx,
                    want: Want::Lc,
                }
            }
            ExprKind::Range { begin, .. } => {
                self.xs.frames.push(XFrame::RangeBegin {
                    u,
                    id,
                    ctx: ctx.clone(),
                });
                eval(*begin, ctx)
            }
            // Nothing else may call: a function literal's body runs only
            // when it is called.
            _ => Next::Val(self.eval_native(u, id, &ctx)),
        }
    }

    /// The start of `eval_element`.
    fn x_element(&mut self, u: u32, id: ExprId, ctx: Rc<Ctx>) -> Next {
        if !self.may_call(u, id) {
            let mut out = self.take_out();
            let r = self.eval_element(u, id, &ctx, &mut out);
            self.put_out(out);
            return Next::Val(r.map(|()| Value::Undef));
        }
        if self.is_lc(u, id) {
            self.xs.frames.push(XFrame::ElemCheck { u, id });
            return self.x_lc(u, id, ctx);
        }
        self.xs.frames.push(XFrame::ElemPush { u, id });
        Next::Eval {
            u,
            id,
            ctx,
            want: Want::Value,
        }
    }

    /// The start of `eval_lc_frame`.
    fn x_lc(&mut self, u: u32, id: ExprId, ctx: Rc<Ctx>) -> Next {
        let ast: &'a Ast = self.units[u as usize].ast;
        let e = ast.expr(id);
        let native = !self.may_call(u, id) || matches!(e.kind, ExprKind::LcForC { .. });
        if native || !self.is_lc(u, id) {
            // Native: C-style `for` comprehensions, with a nested loop for
            // a part that calls (see the module docs).
            let mut out = self.take_out();
            let r = self.eval_lc(u, id, &ctx, &mut out);
            self.put_out(out);
            return Next::Val(r.map(|()| Value::Undef));
        }
        match &e.kind {
            ExprKind::LcIf(c, _, _) => {
                self.xs.frames.push(XFrame::LcIf {
                    u,
                    id,
                    ctx: ctx.clone(),
                });
                Next::Eval {
                    u,
                    id: *c,
                    ctx,
                    want: Want::Value,
                }
            }
            ExprKind::LcEach(x) => {
                if self.is_lc(u, *x) {
                    self.xs.outs.push(Vec::new());
                    self.xs.frames.push(XFrame::LcEachInner { u, id });
                    return Next::Eval {
                        u,
                        id: *x,
                        ctx,
                        want: Want::Lc,
                    };
                }
                self.xs.frames.push(XFrame::LcEach { u, id });
                Next::Eval {
                    u,
                    id: *x,
                    ctx,
                    want: Want::Value,
                }
            }
            ExprKind::LcFor(..) => {
                let region = self.units[u as usize].res.expr[id.0 as usize];
                self.lc_for_var(u, id, 0, region, ctx)
            }
            ExprKind::LcLet(..) => self.x_let(u, id, ctx, Want::Element),
            _ => unreachable!("a comprehension"),
        }
    }

    /// Hand `r` to the frame on top.
    fn x_resume(&mut self, r: R<Value>) -> Next {
        match self.xs.frames.last() {
            Some(XFrame::Call) => {
                let st = self.xs.calls.pop().expect("a call's state");
                return self.call_run(st, Some(r));
            }
            Some(XFrame::For) => return self.lc_for_resume(r),
            Some(XFrame::Let) => return self.let_resume(r),
            _ => {}
        }
        let ast = |ev: &Self, u: u32| -> &'a Ast { ev.units[u as usize].ast };
        match self.xs.frames.pop().expect("a frame above the base") {
            XFrame::Bin { u, id, ctx } => {
                let a = match self.hard(r) {
                    Ok(a) => a,
                    Err(e) => return Next::Val(Err(e)),
                };
                self.bin_rhs(u, id, a, ctx)
            }
            XFrame::Bin2 { u, id, a } => {
                let b = match self.hard(r) {
                    Ok(b) => b,
                    Err(e) => return Next::Val(Err(e)),
                };
                let e = ast(self, u).expr(id);
                let ExprKind::Binary(op, _, _) = e.kind else {
                    unreachable!("a binary operator")
                };
                Next::Val(self.binary_values(op, a, b, u, e.span))
            }
            XFrame::Logic { u, id, ctx } => match self.hard(r) {
                Ok(a) => self.logic_rhs(u, id, a, ctx),
                Err(e) => Next::Val(Err(e)),
            },
            XFrame::Logic2 => Next::Val(self.hard(r).map(|b| Value::Bool(b.to_bool()))),
            XFrame::Tern { u, id, ctx } => {
                let c = match self.hard(r) {
                    Ok(c) => c.to_bool(),
                    Err(e) => return Next::Val(Err(e)),
                };
                let ExprKind::Ternary(_, a, b) = ast(self, u).expr(id).kind else {
                    unreachable!("a ternary")
                };
                Next::Eval {
                    u,
                    id: if c { a } else { b },
                    ctx,
                    want: Want::Value,
                }
            }
            XFrame::Idx { u, id, ctx } => {
                let a = match self.hard(r) {
                    Ok(a) => a,
                    Err(e) => return Next::Val(Err(e)),
                };
                let ExprKind::Index(_, i) = ast(self, u).expr(id).kind else {
                    unreachable!("an index")
                };
                self.idx_rhs(u, i, a, ctx)
            }
            XFrame::Idx2 { a } => Next::Val(self.hard(r).map(|i| ops::index(&a, &i))),
            XFrame::Unary { u, id } => {
                let v = match self.hard(r) {
                    Ok(v) => v,
                    Err(e) => return Next::Val(Err(e)),
                };
                let e = ast(self, u).expr(id);
                let ExprKind::Unary(op, _) = e.kind else {
                    unreachable!("a unary operator")
                };
                let r = match op {
                    UnaryOp::Not => return Next::Val(Ok(Value::Bool(!v.to_bool()))),
                    UnaryOp::Negate => ops::neg(&v),
                    UnaryOp::BinaryNot => ops::bit_not(&v),
                };
                Next::Val(Ok(self.check_undef(r, u, e.span)))
            }
            XFrame::Member { u, id } => {
                let v = match self.hard(r) {
                    Ok(v) => v,
                    Err(e) => return Next::Val(Err(e)),
                };
                Next::Val(Ok(self.member(u, id, v)))
            }
            XFrame::Vector {
                u,
                id,
                k,
                grow,
                ctx,
            } => {
                if let Err(e) = r {
                    self.xs.outs.pop();
                    if grow {
                        self.xs.grow.pop();
                    }
                    return Next::Val(Err(e));
                }
                if grow {
                    // `each_then`'s check: on the whole list so far.
                    let n = self.xs.grow.last().map_or(0, Growable::len)
                        + self.xs.outs.last().map_or(0, Vec::len);
                    if n > self.caps.list {
                        let ExprKind::Vector(items) = &ast(self, u).expr(id).kind else {
                            unreachable!("a vector")
                        };
                        if let Err(e) = self.list_overflow(u, items[k as usize], n) {
                            self.xs.outs.pop();
                            self.xs.grow.pop();
                            return Next::Val(Err(e));
                        }
                    }
                }
                self.vec_next(u, id, k + 1, grow, ctx)
            }
            XFrame::EachHead { u, id, ctx } => {
                let v = match self.hard(r) {
                    Ok(v) => v,
                    Err(e) => return Next::Val(Err(e)),
                };
                self.each_head(u, id, v, ctx)
            }
            XFrame::LcVal => {
                let out = self.xs.outs.pop().expect("a comprehension's list");
                Next::Val(r.map(|_| Value::vector(out)))
            }
            XFrame::ElemPush { u, id } => {
                let v = match self.hard(r) {
                    Ok(v) => v,
                    Err(e) => return Next::Val(Err(e)),
                };
                let out = self.xs.outs.last_mut().expect("a list being filled");
                out.push(v);
                self.elem_check(u, id)
            }
            XFrame::ElemCheck { u, id } => match r {
                Ok(_) => self.elem_check(u, id),
                Err(e) => Next::Val(Err(e)),
            },
            XFrame::LcIf { u, id, ctx } => {
                let c = match self.hard(r) {
                    Ok(c) => c.to_bool(),
                    Err(e) => return Next::Val(Err(e)),
                };
                let ExprKind::LcIf(_, a, b) = ast(self, u).expr(id).kind else {
                    unreachable!("a comprehension if")
                };
                let pick = if c { Some(a) } else { b };
                match pick {
                    Some(x) => Next::Eval {
                        u,
                        id: x,
                        ctx,
                        want: Want::Element,
                    },
                    None => Next::Val(Ok(Value::Undef)),
                }
            }
            XFrame::LcEach { u, id } => {
                let v = match self.hard(r) {
                    Ok(v) => v,
                    Err(e) => return Next::Val(Err(e)),
                };
                let loc = self.expr_loc(u, id);
                let mut out = self.take_out();
                self.each_value(v, loc, &mut out);
                self.put_out(out);
                Next::Val(Ok(Value::Undef))
            }
            XFrame::LcEachInner { u, id } => {
                let inner = self.xs.outs.pop().expect("a comprehension's list");
                if let Err(e) = r {
                    return Next::Val(Err(e));
                }
                let loc = self.expr_loc(u, id);
                let mut out = self.take_out();
                for v in inner {
                    self.each_value(v, loc, &mut out);
                }
                self.put_out(out);
                Next::Val(Ok(Value::Undef))
            }
            XFrame::ForValues {
                u,
                id,
                k,
                region,
                ctx,
            } => match self.hard(r) {
                Ok(values) => self.lc_for_start(u, id, k, region, ctx, values),
                Err(e) => Next::Val(Err(e)),
            },
            XFrame::Args { u, args, k, ctx } => {
                let value = match self.hard(r) {
                    Ok(v) => v,
                    Err(e) => return Next::Val(Err(e)),
                };
                let a = &args[k as usize];
                let name = a.name.map(|n| self.units[u as usize].sym(n));
                let argv = self.xs.args.last_mut().expect("arguments being evaluated");
                argv.push(ArgVal { name, value });
                self.x_args(u, args, k + 1, ctx)
            }
            XFrame::Builtin { b, u, id } => {
                let mut argv = self.xs.args.pop().expect("a builtin's arguments");
                let loc = self.expr_loc(u, id);
                let r = r.and_then(|_| self.apply_builtin(b, loc, &mut argv));
                argv.clear();
                self.arg_pool.push(argv);
                // `direct_builtin`: the loop's check and trace.
                let r = r.and_then(|v| self.check_hard().map(|()| v));
                Next::Val(r.map_err(|mut e| {
                    self.trace_call(&mut e, (u, id));
                    e
                }))
            }
            XFrame::Assert { u, id, ctx } | XFrame::Echo { u, id, ctx } => {
                let mut argv = self.xs.args.pop().expect("arguments");
                let e = ast(self, u).expr(id);
                let (r, body) = match &e.kind {
                    ExprKind::Assert(args, body) => match r {
                        Ok(_) => (self.assert_values(u, args, e.span, argv), *body),
                        Err(e) => (Err(e), None),
                    },
                    ExprKind::Echo(args, body) => {
                        let r = r.and_then(|_| self.echo_values(u, args, &argv));
                        argv.clear();
                        self.arg_pool.push(argv);
                        (r, *body)
                    }
                    _ => unreachable!("assert or echo"),
                };
                match (r, body) {
                    (Err(e), _) => Next::Val(Err(e)),
                    (Ok(()), Some(b)) => Next::Eval {
                        u,
                        id: b,
                        ctx,
                        want: Want::Value,
                    },
                    (Ok(()), None) => Next::Val(Ok(Value::Undef)),
                }
            }
            XFrame::RangeBegin { u, id, ctx } => {
                let b = match self.hard(r) {
                    Ok(b) => b,
                    Err(e) => return Next::Val(Err(e)),
                };
                let ExprKind::Range { end, .. } = ast(self, u).expr(id).kind else {
                    unreachable!("a range")
                };
                self.xs.frames.push(XFrame::RangeEnd {
                    u,
                    id,
                    ctx: ctx.clone(),
                    b,
                });
                Next::Eval {
                    u,
                    id: end,
                    ctx,
                    want: Want::Value,
                }
            }
            XFrame::RangeEnd { u, id, ctx, b } => {
                let e = match self.hard(r) {
                    Ok(e) => e,
                    Err(e) => return Next::Val(Err(e)),
                };
                let Some((bd, ed)) = self.range_ends(u, id, &b, &e) else {
                    return Next::Val(Ok(Value::Undef));
                };
                let ExprKind::Range { step, .. } = ast(self, u).expr(id).kind else {
                    unreachable!("a range")
                };
                match step {
                    Some(s) => {
                        self.xs.frames.push(XFrame::RangeStep { u, id, bd, ed });
                        Next::Eval {
                            u,
                            id: s,
                            ctx,
                            want: Want::Value,
                        }
                    }
                    None => Next::Val(Ok(self.range_value(u, id, bd, 1.0, ed))),
                }
            }
            XFrame::RangeStep { u, id, bd, ed } => {
                let sv = match self.hard(r) {
                    Ok(sv) => sv,
                    Err(e) => return Next::Val(Err(e)),
                };
                Next::Val(Ok(match self.range_step(u, id, &sv) {
                    Some(sd) => self.range_value(u, id, bd, sd, ed),
                    None => Value::Undef,
                }))
            }
            XFrame::IsUndef { u, id } => {
                // `call_builtin`'s `is_undef`, then `direct_builtin`'s
                // check and trace.
                let r = self
                    .hard(r)
                    .map(|v| Value::Bool(v.is_undef()))
                    .and_then(|v| self.check_hard().map(|()| v));
                Next::Val(r.map_err(|mut e| {
                    self.trace_call(&mut e, (u, id));
                    e
                }))
            }
            XFrame::Call | XFrame::For | XFrame::Let => unreachable!("handled above"),
        }
    }

    /// A binary operator's right operand, once the left one is in.
    fn bin_rhs(&mut self, u: u32, id: ExprId, a: Value, ctx: Rc<Ctx>) -> Next {
        let e = self.units[u as usize].ast.expr(id);
        let ExprKind::Binary(op, _, rhs) = e.kind else {
            unreachable!("a binary operator")
        };
        if !self.may_call(u, rhs) {
            return Next::Val(
                self.eval_native(u, rhs, &ctx)
                    .and_then(|b| self.binary_values(op, a, b, u, e.span)),
            );
        }
        self.xs.frames.push(XFrame::Bin2 { u, id, a });
        Next::Eval {
            u,
            id: rhs,
            ctx,
            want: Want::Value,
        }
    }

    /// `&&` and `||` once the left operand is in.
    fn logic_rhs(&mut self, u: u32, id: ExprId, a: Value, ctx: Rc<Ctx>) -> Next {
        let a = a.to_bool();
        let ExprKind::Binary(op, _, rhs) = self.units[u as usize].ast.expr(id).kind else {
            unreachable!("a binary operator")
        };
        if a != (op == BinaryOp::LogicalAnd) {
            // `false && …` and `true || …`: decided.
            return Next::Val(Ok(Value::Bool(a)));
        }
        if !self.may_call(u, rhs) {
            return Next::Val(
                self.eval_native(u, rhs, &ctx)
                    .map(|b| Value::Bool(b.to_bool())),
            );
        }
        self.xs.frames.push(XFrame::Logic2);
        Next::Eval {
            u,
            id: rhs,
            ctx,
            want: Want::Value,
        }
    }

    /// An index expression's index, once its list is in.
    fn idx_rhs(&mut self, u: u32, i: ExprId, a: Value, ctx: Rc<Ctx>) -> Next {
        if !self.may_call(u, i) {
            return Next::Val(self.eval_native(u, i, &ctx).map(|i| ops::index(&a, &i)));
        }
        self.xs.frames.push(XFrame::Idx2 { a });
        Next::Eval {
            u,
            id: i,
            ctx,
            want: Want::Value,
        }
    }

    // --- leaves shared in shape with the native code -------------------

    /// `eval_binary` once both operands are in.
    fn binary_values(&mut self, op: BinaryOp, a: Value, b: Value, u: u32, span: Span) -> R<Value> {
        if let (Value::Number(x), Value::Number(y)) = (&a, &b) {
            let (x, y) = (*x, *y);
            match op {
                BinaryOp::Plus => return Ok(Value::Number(x + y)),
                BinaryOp::Minus => return Ok(Value::Number(x - y)),
                BinaryOp::Multiply => return Ok(Value::Number(x * y)),
                BinaryOp::Divide => return Ok(Value::Number(x / y)),
                BinaryOp::Less => return Ok(Value::Bool(x < y)),
                BinaryOp::LessEqual => return Ok(Value::Bool(x <= y)),
                BinaryOp::Greater => return Ok(Value::Bool(x > y)),
                BinaryOp::GreaterEqual => return Ok(Value::Bool(x >= y)),
                BinaryOp::Equal => return Ok(Value::Bool(x == y)),
                BinaryOp::NotEqual => return Ok(Value::Bool(x != y)),
                _ => {}
            }
        }
        self.binary_slow(op, &a, &b, u, span)
    }

    /// `eval_cold`'s member lookup once its value is in.
    fn member(&mut self, u: u32, id: ExprId, v: Value) -> Value {
        let ast: &'a Ast = self.units[u as usize].ast;
        let ExprKind::Member(_, n) = ast.expr(id).kind else {
            unreachable!("a member lookup")
        };
        let name = ast.name(n);
        let i = match (&v, name) {
            (Value::Vector(_), "x") | (Value::Range(_), "begin") => 0.0,
            (Value::Vector(_), "y") | (Value::Range(_), "step") => 1.0,
            (Value::Vector(_), "z") | (Value::Range(_), "end") => 2.0,
            (Value::Object(o), _) => return o.get(name.as_bytes()),
            (Value::Vector(_), _) if self.opts.features.has(crate::Feature::VectorSwizzle) => {
                return crate::eval::swizzle(&v, name);
            }
            _ => return Value::Undef,
        };
        ops::index(&v, &Value::Number(i))
    }

    /// The list limit after an element (`eval_element`).
    fn elem_check(&mut self, u: u32, id: ExprId) -> Next {
        let n = self.xs.outs.last().map_or(0, Vec::len);
        if n > self.caps.list {
            return Next::Val(self.list_overflow(u, id, n).map(|()| Value::Undef));
        }
        Next::Val(Ok(Value::Undef))
    }

    /// The next element of a vector, or the vector.
    fn vec_next(&mut self, u: u32, id: ExprId, k: u32, grow: bool, ctx: Rc<Ctx>) -> Next {
        let ast: &'a Ast = self.units[u as usize].ast;
        let ExprKind::Vector(items) = &ast.expr(id).kind else {
            unreachable!("a vector")
        };
        let mut k = k;
        while let Some(&item) = items.get(k as usize) {
            if self.may_call(u, item) {
                break;
            }
            // `eval_element` (and `each_then`'s check), here.
            let mut out = self.take_out();
            let mut r = self.eval_element(u, item, &ctx, &mut out);
            if grow && r.is_ok() {
                let n = self.xs.grow.last().map_or(0, Growable::len) + out.len();
                if n > self.caps.list {
                    r = self.list_overflow(u, item, n);
                }
            }
            self.put_out(out);
            if let Err(e) = r {
                self.xs.outs.pop();
                if grow {
                    self.xs.grow.pop();
                }
                return Next::Val(Err(e));
            }
            k += 1;
        }
        if let Some(&item) = items.get(k as usize) {
            self.xs.frames.push(XFrame::Vector {
                u,
                id,
                k,
                grow,
                ctx: ctx.clone(),
            });
            return Next::Eval {
                u,
                id: item,
                ctx,
                want: Want::Element,
            };
        }
        let out = self.xs.outs.pop().expect("a vector's list");
        if grow {
            let mut g = self.xs.grow.pop().expect("a growing list");
            g.reserve(out.len());
            g.extend(out);
            return Next::Val(Ok(Value::Vector(g.finish())));
        }
        Next::Val(Ok(Value::vector(out)))
    }

    /// `each_then` once `x`'s value is in: the rest of the elements append
    /// to it in place when nothing else holds it.
    fn each_head(&mut self, u: u32, id: ExprId, v: Value, ctx: Rc<Ctx>) -> Next {
        let ast: &'a Ast = self.units[u as usize].ast;
        let ExprKind::Vector(items) = &ast.expr(id).kind else {
            unreachable!("a vector")
        };
        let (first, rest) = (items[0], &items[1..]);
        let v = match v {
            Value::Vector(v) => match v.into_growable() {
                Ok(g) => {
                    if g.len() > self.caps.list
                        && let Err(e) = self.list_overflow(u, first, g.len())
                    {
                        return Next::Val(Err(e));
                    }
                    self.xs.grow.push(g);
                    self.xs.outs.push(Vec::with_capacity(rest.len()));
                    return self.vec_next(u, id, 1, true, ctx);
                }
                Err(v) => Value::Vector(v),
            },
            other => other,
        };
        // `each_copied`.
        let loc = self.expr_loc(u, first);
        let mut out = Vec::new();
        self.each_value(v, loc, &mut out);
        if out.len() > self.caps.list
            && let Err(e) = self.list_overflow(u, first, out.len())
        {
            return Next::Val(Err(e));
        }
        out.reserve(rest.len());
        self.xs.outs.push(out);
        self.vec_next(u, id, 1, false, ctx)
    }

    /// `eval_args_into` from argument `k`, into the top of `args`: the
    /// arguments that cannot call are evaluated here, the others by the
    /// loop. Ends with `undef`, or the first error.
    fn x_args(&mut self, u: u32, args: &'a [Arg], mut k: u32, ctx: Rc<Ctx>) -> Next {
        while let Some(a) = args.get(k as usize) {
            if self.may_call(u, a.expr) {
                self.xs.frames.push(XFrame::Args {
                    u,
                    args,
                    k,
                    ctx: ctx.clone(),
                });
                return Next::Eval {
                    u,
                    id: a.expr,
                    ctx,
                    want: Want::Value,
                };
            }
            let value = match self.eval(u, a.expr, &ctx) {
                Ok(v) => v,
                Err(e) => return Next::Val(Err(e)),
            };
            let name = a.name.map(|n| self.units[u as usize].sym(n));
            let argv = self.xs.args.last_mut().expect("arguments being evaluated");
            argv.push(ArgVal { name, value });
            k += 1;
        }
        Next::Val(Ok(Value::Undef))
    }

    /// `assign_regs`'s assignment of argument `k`'s value.
    fn assign_reg(&mut self, u: u32, span: Span, region: u32, k: usize, a: &Arg, v: Value) {
        let loc = Loc { unit: u, span };
        match a.name {
            None => self.unnamed_assignment(loc, &v),
            Some(n) => {
                let slot = self.regions[region as usize].binds[k];
                debug_assert_ne!(slot, NO_SLOT, "a register region binds no `$` name");
                let i = self.reg_base[region as usize] as usize + slot as usize;
                if self.regs[i].is_some() {
                    let s = self.units[u as usize].sym(n);
                    self.duplicate_assignment(loc, s, &v);
                } else {
                    self.regs[i] = Some(v);
                }
            }
        }
    }

    /// `sequential_assign`'s assignment of argument `k`'s value into
    /// `target`. Its list of the names bound earlier without a slot is
    /// read off the arguments before `k` instead: a name joins that list
    /// at its first such binding, and only then.
    fn assign_ctx(&mut self, u: u32, span: Span, args: &[Arg], k: usize, target: &Ctx, v: Value) {
        let loc = Loc { unit: u, span };
        let Some(n) = args[k].name else {
            self.unnamed_assignment(loc, &v);
            return;
        };
        let s = self.units[u as usize].sym(n);
        let slot_of = |ev: &Self, j: usize| {
            ev.regions[target.region as usize]
                .binds
                .get(j)
                .copied()
                .unwrap_or(NO_SLOT)
        };
        let slot = slot_of(self, k);
        let duplicate = match slot {
            NO_SLOT => args[..k].iter().enumerate().any(|(j, b)| {
                b.name.is_some_and(|m| {
                    self.units[u as usize].sym(m) == s && slot_of(self, j) == NO_SLOT
                })
            }),
            i => target.has_slot(i),
        };
        if duplicate {
            self.duplicate_assignment(loc, s, &v);
        } else if slot == NO_SLOT {
            let config = self.syms.is_config(s);
            target.vars.borrow_mut().set(s, v, config);
        } else {
            target.set_slot(slot, v);
        }
    }

    // --- `let` ---------------------------------------------------------

    /// A `let` expression (`eval_cold`) or comprehension `let`
    /// (`eval_lc_frame`) outside a tail call.
    fn x_let(&mut self, u: u32, id: ExprId, ctx: Rc<Ctx>, want: Want) -> Next {
        let region = self.units[u as usize].res.expr[id.0 as usize];
        let (old, c) = if self.regions[region as usize].reg() {
            (self.reg_open(region), None)
        } else {
            let c = self.new_ctx(&ctx, CtxKind::Plain, region);
            let mark = self.push(c.clone());
            (0, Some((c, mark)))
        };
        self.xs.lets.push(LetSt {
            u,
            id,
            k: 0,
            want,
            body: false,
            old,
            c,
            ctx,
        });
        self.xs.frames.push(XFrame::Let);
        self.let_run()
    }

    /// The `let` on top: its assignments from `k`, then its body.
    fn let_run(&mut self) -> Next {
        let st = self.xs.lets.last().expect("a let");
        let (u, id, want) = (st.u, st.id, st.want);
        let ast: &'a Ast = self.units[u as usize].ast;
        let e = ast.expr(id);
        let (ExprKind::Let(args, body) | ExprKind::LcLet(args, body)) = &e.kind else {
            unreachable!("a let")
        };
        let region = self.units[u as usize].res.expr[id.0 as usize];
        loop {
            let st = self.xs.lets.last_mut().expect("a let");
            let k = st.k as usize;
            let ctx = match &st.c {
                Some((c, _)) => c.clone(),
                None => st.ctx.clone(),
            };
            let Some(a) = args.get(k) else {
                st.body = true;
                return Next::Eval {
                    u,
                    id: *body,
                    ctx,
                    want: if want == Want::Value {
                        Want::Value
                    } else {
                        Want::Element
                    },
                };
            };
            if self.may_call(u, a.expr) {
                return Next::Eval {
                    u,
                    id: a.expr,
                    ctx,
                    want: Want::Value,
                };
            }
            let v = match self.eval(u, a.expr, &ctx) {
                Ok(v) => v,
                Err(e) => return self.let_end(Err(e)),
            };
            self.let_assign(u, e.span, args, region, k, &ctx, v);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn let_assign(
        &mut self,
        u: u32,
        span: Span,
        args: &'a [Arg],
        region: u32,
        k: usize,
        ctx: &Ctx,
        v: Value,
    ) {
        if self.regions[region as usize].reg() {
            self.assign_reg(u, span, region, k, &args[k], v);
        } else {
            self.assign_ctx(u, span, args, k, ctx, v);
        }
        self.xs.lets.last_mut().expect("a let").k += 1;
    }

    fn let_resume(&mut self, r: R<Value>) -> Next {
        let st = self.xs.lets.last().expect("a let");
        if st.body {
            return self.let_end(r);
        }
        let v = match self.hard(r) {
            Ok(v) => v,
            Err(e) => return self.let_end(Err(e)),
        };
        let (u, id, k) = (st.u, st.id, st.k as usize);
        let ctx = match &st.c {
            Some((c, _)) => c.clone(),
            None => st.ctx.clone(),
        };
        let ast: &'a Ast = self.units[u as usize].ast;
        let e = ast.expr(id);
        let (ExprKind::Let(args, _) | ExprKind::LcLet(args, _)) = &e.kind else {
            unreachable!("a let")
        };
        let region = self.units[u as usize].res.expr[id.0 as usize];
        self.let_assign(u, e.span, args, region, k, &ctx, v);
        drop(ctx);
        self.let_run()
    }

    /// The end of a `let`: its registers or context gone, where the native
    /// one drops them.
    fn let_end(&mut self, r: R<Value>) -> Next {
        let st = self.xs.lets.pop().expect("a let");
        let top = self.xs.frames.pop();
        debug_assert!(matches!(top, Some(XFrame::Let)));
        match st.c {
            None => {
                let region = self.units[st.u as usize].res.expr[st.id.0 as usize];
                self.reg_close(region, st.old);
            }
            Some((c, mark)) => {
                self.truncate(mark);
                // `eval_cold` recycles its context; `eval_lc_frame` lets
                // it drop.
                if st.want == Want::Value {
                    Ctx::recycle(c, &mut self.ctx_pool);
                }
            }
        }
        Next::Val(r)
    }

    // --- comprehension `for` -------------------------------------------

    /// `for_each` from variable `k` of comprehension `id`, in `ctx`.
    fn lc_for_var(&mut self, u: u32, id: ExprId, k: u32, region: u32, ctx: Rc<Ctx>) -> Next {
        let ast: &'a Ast = self.units[u as usize].ast;
        let ExprKind::LcFor(args, body) = &ast.expr(id).kind else {
            unreachable!("a comprehension for")
        };
        let Some(a) = args.get(k as usize) else {
            return Next::Eval {
                u,
                id: *body,
                ctx,
                want: Want::Element,
            };
        };
        if self.may_call(u, a.expr) {
            self.xs.frames.push(XFrame::ForValues {
                u,
                id,
                k,
                region,
                ctx: ctx.clone(),
            });
            return Next::Eval {
                u,
                id: a.expr,
                ctx,
                want: Want::Value,
            };
        }
        match self.eval(u, a.expr, &ctx) {
            Ok(values) => self.lc_for_start(u, id, k, region, ctx, values),
            Err(e) => Next::Val(Err(e)),
        }
    }

    /// `for_each` once its values are in: the loop's frame, and its first
    /// iteration.
    fn lc_for_start(
        &mut self,
        u: u32,
        id: ExprId,
        k: u32,
        region: u32,
        ctx: Rc<Ctx>,
        values: Value,
    ) -> Next {
        let ast: &'a Ast = self.units[u as usize].ast;
        let e = ast.expr(id);
        let ExprKind::LcFor(args, _) = &e.kind else {
            unreachable!("a comprehension for")
        };
        let name = args[k as usize]
            .name
            .map_or(self.k.empty, |n| self.units[u as usize].sym(n));
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
                let loc = Loc {
                    unit: u,
                    span: e.span,
                };
                self.warn(
                    loc,
                    DiagCode::IterationLimit,
                    format!("Bad range parameter in for statement: too many elements ({n})"),
                );
            } else {
                len = r.iter_len();
            }
        }
        self.xs.fors.push(ForSt {
            u,
            id,
            k,
            region,
            ctx,
            values,
            pos: 0,
            len,
            mode,
            cur: None,
        });
        self.xs.frames.push(XFrame::For);
        self.lc_for_next()
    }

    /// The next value a `for` variable takes (`iterate_over`'s iterators).
    fn lc_for_value(f: &mut ForSt) -> Option<Value> {
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

    /// The `for` on top: its next iteration, or its end.
    fn lc_for_next(&mut self) -> Next {
        let f = self.xs.fors.last_mut().expect("a for");
        let Some(v) = Self::lc_for_value(f) else {
            return self.lc_for_end(Ok(()));
        };
        if let Err(e) = self.check_interrupt() {
            return self.lc_for_end(Err(e));
        }
        let f = self.xs.fors.last_mut().expect("a for");
        let (u, id, k, region, mode) = (f.u, f.id, f.k, f.region, f.mode);
        let outer = f.ctx.clone();
        let ctx = match mode {
            ForMode::Reg { i, .. } => {
                self.regs[i] = Some(v);
                outer
            }
            ForMode::Ctx { slot, name, config } => {
                let c = match slot {
                    NO_SLOT => self.iteration_vars(&outer, region, name, config, v),
                    s => Ctx::with_slot(&mut self.ctx_pool, &outer, region, s, v),
                };
                drop(outer);
                let mark = self.push(c.clone());
                self.xs.fors.last_mut().expect("a for").cur = Some((c.clone(), mark));
                c
            }
        };
        let ast: &'a Ast = self.units[u as usize].ast;
        let ExprKind::LcFor(args, body) = &ast.expr(id).kind else {
            unreachable!("a comprehension for")
        };
        if k as usize + 1 >= args.len() {
            Next::Eval {
                u,
                id: *body,
                ctx,
                want: Want::Element,
            }
        } else {
            self.lc_for_var(u, id, k + 1, crate::resolve::next_region(region), ctx)
        }
    }

    fn lc_for_resume(&mut self, r: R<Value>) -> Next {
        let f = self.xs.fors.last_mut().expect("a for");
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
        match r {
            Ok(_) => self.lc_for_next(),
            Err(e) => self.lc_for_end(Err(e)),
        }
    }

    fn lc_for_end(&mut self, r: R<()>) -> Next {
        let f = self.xs.fors.pop().expect("a for");
        let top = self.xs.frames.pop();
        debug_assert!(matches!(top, Some(XFrame::For)));
        if let ForMode::Reg { old, .. } = f.mode {
            self.reg_close(f.region, old);
        }
        Next::Val(r.map(|()| Value::Undef))
    }

    // --- calls ---------------------------------------------------------

    /// `is_undef()`'s argument, when it is evaluated on the heap: when
    /// there is exactly one (otherwise `call_builtin` warns), it is not a
    /// plain variable (which `call_builtin` reads without evaluating it),
    /// and it may call. Then the builtin is only `v.is_undef()` of its
    /// value, and a recursion through it (`is_undef(f(n - 1))`) needs no
    /// native stack per level.
    fn heap_is_undef_arg(&mut self, u: u32, args: &[Arg]) -> Option<ExprId> {
        let [a] = args else { return None };
        let ast: &'a Ast = self.units[u as usize].ast;
        if matches!(ast.expr(a.expr).kind, ExprKind::Var(_)) || !self.may_call(u, a.expr) {
            return None;
        }
        Some(a.expr)
    }

    /// The start of `eval_call`, for a call that may reach a user
    /// function: the checks, then the tail-call loop as a frame.
    fn x_call(&mut self, u: u32, id: ExprId, ctx: Rc<Ctx>) -> Next {
        if self.call_exhausted(u, id) {
            let loc = self.expr_loc(u, id);
            let mut t = b"Recursion detected calling function '".to_vec();
            t.extend_from_slice(&self.call_name(u, id));
            t.push(b'\'');
            self.error(Some(loc), DiagCode::RecursionLimit, t);
            return Next::Val(Err(self.unwind(UnwindKind::Recursion)));
        }
        if let Err(e) = self.check_interrupt() {
            return Next::Val(Err(e));
        }
        self.work += 1;
        if let Some(b) = self.static_builtin(u, id) {
            let ast: &'a Ast = self.units[u as usize].ast;
            let ExprKind::Call(_, args) = &ast.expr(id).kind else {
                unreachable!("a call")
            };
            if b == Builtin::IsUndef
                && let Some(x) = self.heap_is_undef_arg(u, args)
            {
                self.xs.frames.push(XFrame::IsUndef { u, id });
                return Next::Eval {
                    u,
                    id: x,
                    ctx,
                    want: Want::Value,
                };
            }
            // `object()` and `is_undef()` evaluate their own arguments:
            // `object()` natively (see the module docs), `is_undef()` when
            // its argument is a variable or cannot call.
            if matches!(b, Builtin::Object | Builtin::IsUndef) {
                return Next::Val(self.direct_builtin(b, u, id, &ctx));
            }
            let argv = self.arg_pool.pop().unwrap_or_default();
            self.xs.args.push(argv);
            self.xs.frames.push(XFrame::Builtin { b, u, id });
            return self.x_args(u, args, 0, ctx);
        }
        let slot = self.push(self.placeholder.clone());
        let mut st = self.xs.call_pool.pop().unwrap_or_else(|| {
            Box::new(CallSt {
                slot: 0,
                regs: 0,
                saves: 0,
                cur: None,
                entry: None,
                mode: Mode::Entry,
                unit: u,
                expr: None,
                call: (u, id),
                depth: 0,
                phase: Phase::Step,
            })
        });
        st.slot = slot;
        st.regs = self.regs.len();
        st.saves = self.reg_saves.len();
        st.entry = Some(ctx);
        st.mode = Mode::Entry;
        st.unit = u;
        st.expr = Some(id);
        st.call = (u, id);
        st.depth = 0;
        self.xs.frames.push(XFrame::Call);
        self.fn_depth += 1;
        self.call_run(st, None)
    }

    /// `eval_call`'s loop, from a fresh step or with the result the step
    /// was waiting for.
    fn call_run(&mut self, mut st: Box<CallSt<'a>>, mut resumed: Option<R<Value>>) -> Next {
        loop {
            let s = match resumed.take() {
                Some(r) => self.call_phase(&mut st, r),
                None => self.call_simplify(&mut st),
            };
            let step = match s {
                S::Step(step) => step,
                S::Wait(Next::Val(r)) => {
                    resumed = Some(r);
                    continue;
                }
                S::Wait(n) => {
                    self.xs.calls.push(st);
                    return n;
                }
            };
            let c = match step {
                Ok(Step::Done(v)) => match self.check_hard() {
                    Ok(()) => return self.x_call_end(st, Ok(v)),
                    Err(mut e) => {
                        self.trace_call(&mut e, st.call);
                        return self.x_call_end(st, Err(e));
                    }
                },
                Ok(Step::Next {
                    unit: nu,
                    expr: ne,
                    ctx: nc,
                    call: c,
                }) => {
                    st.unit = nu;
                    st.expr = ne;
                    if let Some(nc) = nc {
                        debug_assert_eq!(self.stack.len(), st.slot + 2);
                        self.stack.swap_remove(st.slot);
                        debug_assert!(c.is_some() || self.regs.len() == st.regs);
                        self.reg_unwind(st.saves, st.regs);
                        if self.regions[nc.region as usize].reg() {
                            self.frame_in_ctx(nc.region);
                        }
                        st.mode = Mode::Ctx;
                        if let Some(old) = st.cur.replace(nc) {
                            Ctx::recycle(old, &mut self.ctx_pool);
                        }
                    }
                    c
                }
                Ok(Step::Pure {
                    unit: nu,
                    expr: ne,
                    ctx: nc,
                    call: c,
                    region,
                    base,
                }) => {
                    st.unit = nu;
                    st.expr = Some(ne);
                    self.enter_pure(st.slot, st.regs, st.saves, region, base);
                    st.mode = Mode::Pure;
                    if let Some(old) = st.cur.replace(nc) {
                        Ctx::recycle(old, &mut self.ctx_pool);
                    }
                    Some(c)
                }
                Err(mut e) => {
                    self.trace_call(&mut e, st.call);
                    return self.x_call_end(st, Err(e));
                }
            };
            if let Some(c) = c {
                st.call = c;
                let hit_limit = st.depth == 1_000_000;
                st.depth += 1;
                let err = if hit_limit {
                    let loc = st
                        .expr
                        .map(|e| self.expr_loc(st.unit, e))
                        .unwrap_or(self.expr_loc(c.0, c.1));
                    let mut t = b"Recursion detected calling function '".to_vec();
                    t.extend_from_slice(&self.call_name(c.0, c.1));
                    t.push(b'\'');
                    self.error(Some(loc), DiagCode::RecursionLimit, t);
                    Some(self.unwind(UnwindKind::Recursion))
                } else {
                    self.check_interrupt().err()
                };
                if let Some(mut e) = err {
                    self.trace_call(&mut e, st.call);
                    return self.x_call_end(st, Err(e));
                }
            }
        }
    }

    /// The end of `eval_call`.
    fn x_call_end(&mut self, mut st: Box<CallSt<'a>>, r: R<Value>) -> Next {
        self.truncate(st.slot);
        self.reg_unwind(st.saves, st.regs);
        if let Some(c) = st.cur.take() {
            Ctx::recycle(c, &mut self.ctx_pool);
        }
        st.entry = None;
        st.phase = Phase::Step;
        self.xs.call_pool.push(st);
        self.fn_depth -= 1;
        let top = self.xs.frames.pop();
        debug_assert!(matches!(top, Some(XFrame::Call)));
        Next::Val(r)
    }

    /// `simplify`: one step of the loop, or the evaluation it waits for.
    fn call_simplify(&mut self, st: &mut CallSt<'a>) -> S {
        let Some(id) = st.expr else {
            return S::Step(Ok(Step::Done(Value::Undef)));
        };
        let (u, mode) = (st.unit, st.mode);
        let ast: &'a Ast = self.units[u as usize].ast;
        let e = ast.expr(id);
        let ctx = match &st.cur {
            Some(c) => c,
            None => st.entry.as_ref().expect("the caller's context"),
        };
        match &e.kind {
            ExprKind::Ternary(c, a, b) => {
                if self.may_call(u, *c) {
                    let ctx = ctx.clone();
                    st.phase = Phase::Cond { a: *a, b: *b };
                    return S::Wait(Next::Eval {
                        u,
                        id: *c,
                        ctx,
                        want: Want::Value,
                    });
                }
                S::Step(
                    self.eval(u, *c, ctx)
                        .map(|v| next(u, Some(if v.to_bool() { *a } else { *b }))),
                )
            }
            ExprKind::Assert(args, body) | ExprKind::Echo(args, body) => {
                let assert = matches!(e.kind, ExprKind::Assert(..));
                if self.args_may_call(u, args) {
                    let ctx = ctx.clone();
                    st.phase = if assert {
                        Phase::Assert {
                            args,
                            span: e.span,
                            body: *body,
                        }
                    } else {
                        Phase::Echo { args, body: *body }
                    };
                    let argv = self.arg_pool.pop().unwrap_or_default();
                    self.xs.args.push(argv);
                    return S::Wait(self.x_args(u, args, 0, ctx));
                }
                let r = if assert {
                    self.perform_assert(u, args, e.span, ctx)
                } else {
                    self.echo(u, args, ctx)
                };
                S::Step(r.map(|()| next(u, *body)))
            }
            ExprKind::Let(args, body) => {
                let region = self.units[u as usize].res.expr[id.0 as usize];
                let reg = self.regions[region as usize].reg();
                if !self.args_may_call(u, args) {
                    // `simplify`'s `let`, as it is.
                    if reg {
                        let r = self.tail_let_regs(u, args, e.span, region, ctx);
                        return S::Step(r.map(|()| next(u, Some(*body))));
                    }
                    let c = self.new_ctx(ctx, CtxKind::Plain, region);
                    self.push(c.clone());
                    if mode == Mode::Ctx {
                        self.copy_config(ctx, &c);
                    }
                    let r = self.sequential_assign(u, args, e.span, &c);
                    return S::Step(r.map(|()| Step::Next {
                        unit: u,
                        expr: Some(*body),
                        ctx: Some(c),
                        call: None,
                    }));
                }
                if reg {
                    let old = self.reg_open(region);
                    self.reg_saves.push((region, old));
                    return self.call_let_reg(st, args, e.span, region, 0, *body);
                }
                let c = self.new_ctx(ctx, CtxKind::Plain, region);
                self.push(c.clone());
                if mode == Mode::Ctx {
                    self.copy_config(ctx, &c);
                }
                self.call_let_ctx(st, args, e.span, 0, *body, c)
            }
            ExprKind::Call(callee, args) => {
                if let Some(b) = self.static_builtin(u, id) {
                    if b == Builtin::IsUndef
                        && let Some(x) = self.heap_is_undef_arg(u, args)
                    {
                        let ctx = ctx.clone();
                        st.phase = Phase::IsUndef;
                        return S::Wait(Next::Eval {
                            u,
                            id: x,
                            ctx,
                            want: Want::Value,
                        });
                    }
                    if self.args_may_call(u, args)
                        && !matches!(b, Builtin::Object | Builtin::IsUndef)
                    {
                        let ctx = ctx.clone();
                        st.phase = Phase::Builtin { b, u, id };
                        let argv = self.arg_pool.pop().unwrap_or_default();
                        self.xs.args.push(argv);
                        return S::Wait(self.x_args(u, args, 0, ctx));
                    }
                    return S::Step(self.call_builtin(b, u, id, args, ctx).map(Step::Done));
                }
                self.call_simplify_call(st, u, id, e, *callee, args)
            }
            _ => {
                if self.may_call(u, id) {
                    let ctx = ctx.clone();
                    st.phase = Phase::Done;
                    return S::Wait(Next::Eval {
                        u,
                        id,
                        ctx,
                        want: Want::Value,
                    });
                }
                S::Step(self.eval(u, id, ctx).map(Step::Done))
            }
        }
    }

    /// A tail `let`'s assignments into its registers from `k`.
    fn call_let_reg(
        &mut self,
        st: &mut CallSt<'a>,
        args: &'a [Arg],
        span: Span,
        region: u32,
        mut k: u32,
        body: ExprId,
    ) -> S {
        let u = st.unit;
        while let Some(a) = args.get(k as usize) {
            let ctx = match &st.cur {
                Some(c) => c,
                None => st.entry.as_ref().expect("the caller's context"),
            };
            if self.may_call(u, a.expr) {
                let ctx = ctx.clone();
                st.phase = Phase::LetReg {
                    args,
                    span,
                    region,
                    k,
                    body,
                };
                return S::Wait(Next::Eval {
                    u,
                    id: a.expr,
                    ctx,
                    want: Want::Value,
                });
            }
            match self.eval(u, a.expr, ctx) {
                Ok(v) => self.assign_reg(u, span, region, k as usize, a, v),
                Err(e) => return S::Step(Err(e)),
            }
            k += 1;
        }
        S::Step(Ok(next(u, Some(body))))
    }

    /// A tail `let`'s assignments into its context `c` from `k`.
    fn call_let_ctx(
        &mut self,
        st: &mut CallSt<'a>,
        args: &'a [Arg],
        span: Span,
        mut k: u32,
        body: ExprId,
        c: Rc<Ctx>,
    ) -> S {
        let u = st.unit;
        while let Some(a) = args.get(k as usize) {
            if self.may_call(u, a.expr) {
                let ctx = c.clone();
                st.phase = Phase::LetCtx {
                    args,
                    span,
                    k,
                    body,
                    c,
                };
                return S::Wait(Next::Eval {
                    u,
                    id: a.expr,
                    ctx,
                    want: Want::Value,
                });
            }
            match self.eval(u, a.expr, &c) {
                Ok(v) => self.assign_ctx(u, span, args, k as usize, &c, v),
                Err(e) => return S::Step(Err(e)),
            }
            k += 1;
        }
        S::Step(Ok(Step::Next {
            unit: u,
            expr: Some(body),
            ctx: Some(c),
            call: None,
        }))
    }

    /// `simplify_call`: the callee looked up, then a builtin evaluated or
    /// a user function's frame bound, its arguments on the heap when one
    /// may call.
    fn call_simplify_call(
        &mut self,
        st: &mut CallSt<'a>,
        u: u32,
        id: ExprId,
        e: &'a lang::ast::Expr,
        callee: ExprId,
        args: &'a [Arg],
    ) -> S {
        let ast: &'a Ast = self.units[u as usize].ast;
        let loc = Loc {
            unit: u,
            span: e.span,
        };
        let ctx = match &st.cur {
            Some(c) => c,
            None => st.entry.as_ref().expect("the caller's context"),
        };
        let callable = match &ast.expr(callee).kind {
            ExprKind::Var(n) => {
                let s = self.units[u as usize].sym(*n);
                match self.units[u as usize].res.expr[id.0 as usize] {
                    0 => {
                        if !self.syms.is_config(s) {
                            self.stats.fallbacks += 1;
                        }
                        self.lookup_function(ctx, s, loc)
                    }
                    r => self.find_function(u, r - 1, ctx, s, loc),
                }
            }
            // A callee that is an expression (`f(x)(y)`, `fs[i](x)`) and
            // may call: on the heap too, so a recursion through it holds
            // no native stack per level.
            _ if self.may_call(u, callee) => {
                let ctx = ctx.clone();
                st.phase = Phase::Callee { id, args, loc };
                return S::Wait(Next::Eval {
                    u,
                    id: callee,
                    ctx,
                    want: Want::Value,
                });
            }
            _ => self.eval(u, callee, ctx).map(|v| self.callee_value(v, loc)),
        };
        self.call_callable(st, u, id, args, loc, callable)
    }

    /// What a callee expression's value calls: a function literal, or
    /// nothing, with the warning.
    fn callee_value(&mut self, v: Value, loc: Loc) -> Option<Callable> {
        match v {
            Value::Function(f) => Some(Callable::Literal(f)),
            other => {
                let t = format!("Can't call function on {}", other.type_name());
                self.warn(loc, DiagCode::UnknownFunction, t);
                None
            }
        }
    }

    /// The rest of [`Self::call_simplify_call`], once the callee is known.
    fn call_callable(
        &mut self,
        st: &mut CallSt<'a>,
        u: u32,
        id: ExprId,
        args: &'a [Arg],
        loc: Loc,
        callable: R<Option<Callable>>,
    ) -> S {
        let mode = st.mode;
        let ctx = match &st.cur {
            Some(c) => c,
            None => st.entry.as_ref().expect("the caller's context"),
        };
        let callable = match callable {
            Ok(c) => c,
            Err(e) => return S::Step(Err(e)),
        };
        // `this`: the call is of a method, bound into a context of its own
        // with `this` set (`Evaluator::method_call`).
        #[allow(clippy::type_complexity)]
        let (fu, params, body, defining, region, this): (
            u32,
            &'a [Param],
            ExprId,
            Rc<Ctx>,
            u32,
            Option<Object>,
        ) = match callable {
            None => return S::Step(Ok(Step::Done(Value::Undef))),
            Some(Callable::Builtin(b)) => {
                if b == Builtin::IsUndef
                    && let Some(x) = self.heap_is_undef_arg(u, args)
                {
                    let ctx = ctx.clone();
                    st.phase = Phase::IsUndef;
                    return S::Wait(Next::Eval {
                        u,
                        id: x,
                        ctx,
                        want: Want::Value,
                    });
                }
                if self.args_may_call(u, args) && !matches!(b, Builtin::Object | Builtin::IsUndef) {
                    let ctx = ctx.clone();
                    st.phase = Phase::Builtin { b, u, id };
                    let argv = self.arg_pool.pop().unwrap_or_default();
                    self.xs.args.push(argv);
                    return S::Wait(self.x_args(u, args, 0, ctx));
                }
                return S::Step(self.call_builtin(b, u, id, args, ctx).map(Step::Done));
            }
            Some(Callable::User {
                ctx: dctx,
                unit,
                scope: scope_id,
                index,
            }) => {
                let scope: &'a lang::ast::Scope =
                    self.units[unit as usize].scopes[scope_id as usize].scope;
                let f = &scope.functions[index as usize];
                let region = self.function_region(unit, scope_id, index);
                (unit, &f.params, f.body, dctx, region, None)
            }
            Some(Callable::Literal(f)) => {
                let fast: &'a Ast = self.units[f.unit as usize].ast;
                match &fast.expr(f.expr).kind {
                    ExprKind::Function(params, body) => {
                        let region = self.units[f.unit as usize].res.expr[f.expr.0 as usize];
                        let this = f.this.clone();
                        (
                            f.unit,
                            params.as_slice(),
                            *body,
                            f.ctx.clone(),
                            region,
                            this,
                        )
                    }
                    _ => return S::Step(Ok(Step::Done(Value::Undef))),
                }
            }
        };
        // A method is never pure: `this` is bound in its context.
        let pure = this.is_none()
            && self.regions[region as usize].reg()
            && args.len() <= params.len()
            && args.iter().all(|a| a.name.is_none())
            && (mode != Mode::Ctx || !ctx.vars.borrow().has_config);
        if !self.args_may_call(u, args) {
            if pure {
                return S::Step(
                    self.pure_frame(u, id, args, ctx, mode, fu, params, body, defining, region),
                );
            }
            let body_ctx = self.new_ctx_in(defining, CtxKind::Plain, region);
            self.push(body_ctx.clone());
            if mode == Mode::Ctx {
                self.copy_config(ctx, &body_ctx);
            }
            let this = this.as_ref();
            let r = self.call_frame(u, id, args, ctx, mode, loc, fu, params, &body_ctx, this);
            return S::Step(r.map(|()| Step::Next {
                unit: fu,
                expr: Some(body),
                ctx: Some(body_ctx),
                call: Some((u, id)),
            }));
        }
        // The arguments on the heap: what `pure_frame` and `call_frame` do
        // before them, then wait.
        let (pure, body_ctx) = if pure {
            (Some(defining), None)
        } else {
            let body_ctx = self.new_ctx_in(defining, CtxKind::Plain, region);
            self.push(body_ctx.clone());
            if mode == Mode::Ctx {
                self.copy_config(ctx, &body_ctx);
            }
            (None, Some(body_ctx))
        };
        let argv = self.arg_pool.pop().unwrap_or_default();
        let moved = if self.accumulates(u, id, args) {
            // `eval_args_moving`.
            let mark = self.moved.len() as u32;
            self.move_accumulators(u, id, args, ctx, mode);
            Some(mark)
        } else {
            None
        };
        let ctx = ctx.clone();
        self.xs.args.push(argv);
        st.phase = Phase::User {
            u,
            id,
            fu,
            params,
            body,
            region,
            loc,
            moved,
            pure,
            body_ctx,
            this,
        };
        S::Wait(self.x_args(u, args, 0, ctx))
    }

    /// The step waiting in `st` once its evaluation is done.
    fn call_phase(&mut self, st: &mut CallSt<'a>, r: R<Value>) -> S {
        let u = st.unit;
        match std::mem::replace(&mut st.phase, Phase::Step) {
            Phase::Step => unreachable!("a step waiting"),
            Phase::Cond { a, b } => S::Step(
                self.hard(r)
                    .map(|v| next(u, Some(if v.to_bool() { a } else { b }))),
            ),
            Phase::Assert { args, span, body } => {
                let mut argv = self.xs.args.pop().expect("assert's arguments");
                match r {
                    Ok(_) => S::Step(
                        self.assert_values(u, args, span, argv)
                            .map(|()| next(u, body)),
                    ),
                    Err(e) => {
                        argv.clear();
                        self.arg_pool.push(argv);
                        S::Step(Err(e))
                    }
                }
            }
            Phase::Echo { args, body } => {
                let mut argv = self.xs.args.pop().expect("echo's arguments");
                let r = r.and_then(|_| self.echo_values(u, args, &argv));
                argv.clear();
                self.arg_pool.push(argv);
                S::Step(r.map(|()| next(u, body)))
            }
            Phase::LetReg {
                args,
                span,
                region,
                k,
                body,
            } => match self.hard(r) {
                Ok(v) => {
                    self.assign_reg(u, span, region, k as usize, &args[k as usize], v);
                    self.call_let_reg(st, args, span, region, k + 1, body)
                }
                Err(e) => S::Step(Err(e)),
            },
            Phase::LetCtx {
                args,
                span,
                k,
                body,
                c,
            } => match self.hard(r) {
                Ok(v) => {
                    self.assign_ctx(u, span, args, k as usize, &c, v);
                    self.call_let_ctx(st, args, span, k + 1, body, c)
                }
                Err(e) => S::Step(Err(e)),
            },
            Phase::Callee { id, args, loc } => {
                let callable = self.hard(r).map(|v| self.callee_value(v, loc));
                self.call_callable(st, u, id, args, loc, callable)
            }
            // `call_builtin`'s `is_undef`, its argument evaluated.
            Phase::IsUndef => S::Step(self.hard(r).map(|v| Step::Done(Value::Bool(v.is_undef())))),
            Phase::Builtin { b, u, id } => {
                let mut argv = self.xs.args.pop().expect("a builtin's arguments");
                let loc = self.expr_loc(u, id);
                let r = r.and_then(|_| self.apply_builtin(b, loc, &mut argv));
                argv.clear();
                self.arg_pool.push(argv);
                S::Step(r.map(Step::Done))
            }
            Phase::User {
                u,
                id,
                fu,
                params,
                body,
                region,
                loc,
                moved,
                pure,
                body_ctx,
                this,
            } => {
                let mut argv = self.xs.args.pop().expect("a call's arguments");
                if let Some(mark) = moved {
                    self.moved.truncate(mark as usize);
                }
                match (pure, body_ctx) {
                    (Some(defining), _) => match r {
                        Ok(_) => {
                            S::Step(self.pure_bind(u, id, argv, fu, params, body, defining, region))
                        }
                        Err(e) => {
                            argv.clear();
                            self.arg_pool.push(argv);
                            S::Step(Err(e))
                        }
                    },
                    (None, Some(body_ctx)) => {
                        let this = this.as_ref();
                        let r =
                            self.frame_bind(r.map(|_| ()), argv, loc, fu, params, &body_ctx, this);
                        S::Step(r.map(|()| Step::Next {
                            unit: fu,
                            expr: Some(body),
                            ctx: Some(body_ctx),
                            call: Some((u, id)),
                        }))
                    }
                    (None, None) => unreachable!("a frame to bind into"),
                }
            }
            Phase::Done => S::Step(self.hard(r).map(Step::Done)),
        }
    }
}

/// The subexpressions `eval` can reach from `id` without a call: the
/// resolver's walk (`resolve::Resolver::expr`), stopping at function
/// literals.
fn children(ast: &Ast, id: ExprId, out: &mut Vec<ExprId>) {
    let args = |out: &mut Vec<ExprId>, a: &[Arg]| out.extend(a.iter().map(|a| a.expr));
    match &ast.expr(id).kind {
        ExprKind::Var(_)
        | ExprKind::Undef
        | ExprKind::Bool(_)
        | ExprKind::Number(_)
        | ExprKind::String(_)
        | ExprKind::Invalid
        | ExprKind::Function(..) => {}
        ExprKind::Unary(_, x) | ExprKind::Member(x, _) | ExprKind::LcEach(x) => out.push(*x),
        ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => out.extend([*a, *b]),
        ExprKind::Ternary(a, b, c) => out.extend([*a, *b, *c]),
        ExprKind::LcIf(a, b, c) => {
            out.extend([*a, *b]);
            out.extend(*c);
        }
        ExprKind::Range { begin, step, end } => {
            out.extend([*begin, *end]);
            out.extend(*step);
        }
        ExprKind::Vector(items) => out.extend(items.iter().copied()),
        ExprKind::Call(callee, a) => {
            if !matches!(ast.expr(*callee).kind, ExprKind::Var(_)) {
                out.push(*callee);
            }
            args(out, a);
        }
        ExprKind::Let(a, body) | ExprKind::LcLet(a, body) | ExprKind::LcFor(a, body) => {
            args(out, a);
            out.push(*body);
        }
        ExprKind::LcForC {
            init,
            cond,
            incr,
            body,
        } => {
            args(out, init);
            args(out, incr);
            out.extend([*cond, *body]);
        }
        ExprKind::Assert(a, body) | ExprKind::Echo(a, body) => {
            args(out, a);
            out.extend(*body);
        }
    }
}
