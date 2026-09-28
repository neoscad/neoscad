//! Function calls: argument binding, name lookup and the tail-call loop.

use std::rc::Rc;

use lang::ast::{Arg, Ast, ExprId, ExprKind, Param};
use lang::diag::DiagCode;

use crate::builtins::functions::Builtin;
use crate::builtins::modules::BuiltinModule;
use crate::context::{Ctx, CtxKind, ScopeRef, Vars};
use crate::eval::{Evaluator, Step};
use crate::message::{Loc, R, UnwindKind};
use crate::resolve::{BUILTIN_REGION, Cand, NO_SLOT, Region};
use crate::sym::Sym;
use crate::value::{FunctionValue, Value};

/// A user call's bound arguments: the callee region's slots, and the
/// `$` names and other names the region has no slot for.
pub(crate) struct Frame {
    pub slots: Vec<Option<Value>>,
    pub vars: Vars,
}

impl Frame {
    fn has(&self, slot: u32, s: Sym) -> bool {
        match slot {
            NO_SLOT => self.vars.get(s).is_some(),
            i => self.slots[i as usize].is_some(),
        }
    }

    fn set(&mut self, slot: u32, s: Sym, v: Value, config: bool) {
        match slot {
            NO_SLOT => {
                self.vars.set(s, v, config);
            }
            i => self.slots[i as usize] = Some(v),
        }
    }
}

/// An evaluated argument.
#[derive(Debug, Clone)]
pub(crate) struct ArgVal {
    pub name: Option<Sym>,
    pub value: Value,
}

/// Something `f(...)` can call.
pub(crate) enum Callable {
    Builtin(Builtin),
    /// A `function` definition, with the scope context it was found in.
    User {
        ctx: Rc<Ctx>,
        unit: u32,
        scope: u32,
        index: u32,
    },
    Literal(Rc<FunctionValue>),
}

/// Something `m(...)` can instantiate.
pub(crate) enum Instantiable {
    Builtin(BuiltinModule),
    User {
        ctx: Rc<Ctx>,
        unit: u32,
        scope: u32,
        index: u32,
    },
}

/// The context binding `s`, found from a tail call's `ctx` when that and
/// every context up to it can be seen by nothing but the call loop (see
/// `Evaluator::move_accumulators`): `ctx` is held by the loop's stack slot
/// and its `cur`, each context above it only by its child. Only plain
/// contexts (function bodies, `let`) qualify.
///
/// `entry`: `ctx` is the caller's context, which the loop borrows rather
/// than holding in `cur` (see `Evaluator::eval_call`). Its bar is one
/// lower, so it is the same "held by exactly one other owner" test that
/// applied when the loop cloned it; a caller's loop context, held by its
/// own slot and `cur`, must never pass it, or a non-tail call would move
/// a value its caller still reads.
fn private_binder(ctx: &Rc<Ctx>, s: Sym, entry: bool, regions: &[Region]) -> Option<Rc<Ctx>> {
    // The counts include the clone held here.
    let mut c = ctx.clone();
    let mut expected = if entry { 2 } else { 3 };
    loop {
        if Rc::strong_count(&c) != expected
            || Rc::weak_count(&c) != 0
            || !matches!(c.kind, CtxKind::Plain)
        {
            return None;
        }
        if c.has_local(s, regions) {
            return Some(c);
        }
        c = c.parent()?;
        expected = 2;
    }
}

impl<'a> Evaluator<'a> {
    /// `Arguments`: evaluate call arguments in order.
    pub fn eval_args(&mut self, u: u32, args: &'a [Arg], ctx: &Rc<Ctx>) -> R<Vec<ArgVal>> {
        let mut out = Vec::with_capacity(args.len());
        self.eval_args_into(u, args, ctx, &mut out)?;
        Ok(out)
    }

    /// [`Self::eval_args`] into `out` (a vector from `arg_pool`).
    pub(crate) fn eval_args_into(
        &mut self,
        u: u32,
        args: &'a [Arg],
        ctx: &Rc<Ctx>,
        out: &mut Vec<ArgVal>,
    ) -> R<()> {
        for a in args {
            let value = self.eval(u, a.expr, ctx)?;
            let name = a.name.map(|n| self.units[u as usize].sym(n));
            out.push(ArgVal { name, value });
        }
        Ok(())
    }

    /// `parse_without_defaults`: match arguments to parameter names.
    /// `params` gives the `n` parameter names by position (a closure, so
    /// calls need not collect them).
    fn bind(
        &mut self,
        args: Vec<ArgVal>,
        loc: Loc,
        n: usize,
        params: impl Fn(usize) -> Sym,
        warn: bool,
    ) -> Vars {
        let n_params = n;
        let mut out = Vars::default();
        let mut named: Vec<Sym> = Vec::new();
        let mut position = 0;
        let mut warned_extra = false;
        for a in args {
            let name = match a.name {
                Some(n) => {
                    if named.contains(&n) {
                        let t = format!("argument {} supplied more than once", self.quote_sym(n));
                        self.warn(loc, DiagCode::ArgumentMismatch, t);
                    } else if out.get(n).is_some() {
                        let t = format!(
                            "argument {} overrides positional argument",
                            self.quote_sym(n)
                        );
                        self.warn(loc, DiagCode::ArgumentMismatch, t);
                    } else if warn
                        && !self.syms.is_config(n)
                        && !(0..n_params).any(|i| params(i) == n)
                    {
                        let t =
                            format!("variable {} not specified as parameter", self.quote_sym(n));
                        self.warn(loc, DiagCode::ArgumentMismatch, t);
                    }
                    named.push(n);
                    n
                }
                None => {
                    let mut found = None;
                    while position < n_params {
                        let candidate = params(position);
                        position += 1;
                        if !named.contains(&candidate) {
                            found = Some(candidate);
                            break;
                        }
                    }
                    match found {
                        Some(n) => n,
                        None => {
                            if warn && !warned_extra {
                                self.warn(
                                    loc,
                                    DiagCode::ArgumentMismatch,
                                    "Too many unnamed arguments supplied",
                                );
                                warned_extra = true;
                            }
                            continue;
                        }
                    }
                }
            };
            let config = self.syms.is_config(name);
            out.set(name, a.value, config);
        }
        out
    }

    /// `Parameters::parse` for builtins: `required` parameters are set to
    /// `undef` when absent, `optional` ones are left unset.
    pub fn bind_builtin(
        &mut self,
        args: Vec<ArgVal>,
        loc: Loc,
        required: &[Sym],
        optional: &[Sym],
        warn: bool,
    ) -> Vars {
        let r = required.len();
        let mut frame = self.bind(
            args,
            loc,
            r + optional.len(),
            |i| if i < r { required[i] } else { optional[i - r] },
            warn,
        );
        for &p in required {
            if frame.get(p).is_none() {
                let config = self.syms.is_config(p);
                frame.set(p, Value::Undef, config);
            }
        }
        frame
    }

    /// `Parameters::parse` for user functions and modules, into the slots
    /// of the callee's `region`: `parse_without_defaults` as [`Self::bind`]
    /// does it, then each missing parameter's default, evaluated in the
    /// defining context (so a default sees neither the other parameters
    /// nor the body; `Parameters.cc`).
    pub fn bind_user(
        &mut self,
        args: &mut Vec<ArgVal>,
        loc: Loc,
        unit: u32,
        params: &'a [Param],
        defining: &Rc<Ctx>,
        region: u32,
    ) -> R<Frame> {
        if let Some(f) = self.bind_positional(args, unit, params, defining, region) {
            return f;
        }
        let warn = self.opts.check_parameters;
        // A cheap clone (reference count), so the closure does not borrow
        // `self` while warnings need it mutably.
        let unit_syms = self.units[unit as usize].syms.clone();
        let psym = |i: usize| unit_syms[params[i].name.0 as usize];
        let n_params = params.len();
        let mut f = Frame {
            slots: vec![None; self.regions[region as usize].len()],
            vars: Vars::default(),
        };
        let mut named: Vec<Sym> = Vec::new();
        let mut position = 0;
        let mut warned_extra = false;
        for a in args.drain(..) {
            let (name, slot) = match a.name {
                Some(n) => {
                    let slot = self.slot_in(region, n);
                    if named.contains(&n) {
                        let t = format!("argument {} supplied more than once", self.quote_sym(n));
                        self.warn(loc, DiagCode::ArgumentMismatch, t);
                    } else if f.has(slot, n) {
                        let t = format!(
                            "argument {} overrides positional argument",
                            self.quote_sym(n)
                        );
                        self.warn(loc, DiagCode::ArgumentMismatch, t);
                    } else if warn
                        && !self.syms.is_config(n)
                        && !(0..n_params).any(|i| psym(i) == n)
                    {
                        let t =
                            format!("variable {} not specified as parameter", self.quote_sym(n));
                        self.warn(loc, DiagCode::ArgumentMismatch, t);
                    }
                    named.push(n);
                    (n, slot)
                }
                None => {
                    let mut found = None;
                    while position < n_params {
                        let candidate = psym(position);
                        position += 1;
                        if !named.contains(&candidate) {
                            found = Some((candidate, self.param_slot(region, position - 1)));
                            break;
                        }
                    }
                    match found {
                        Some(found) => found,
                        None => {
                            if warn && !warned_extra {
                                self.warn(
                                    loc,
                                    DiagCode::ArgumentMismatch,
                                    "Too many unnamed arguments supplied",
                                );
                                warned_extra = true;
                            }
                            continue;
                        }
                    }
                }
            };
            let config = self.syms.is_config(name);
            f.set(slot, name, a.value, config);
        }
        for (k, p) in params.iter().enumerate() {
            let s = psym(k);
            let slot = self.param_slot(region, k);
            if !f.has(slot, s) {
                let v = match p.default {
                    Some(d) => self.eval(unit, d, defining)?,
                    None => Value::Undef,
                };
                let config = self.syms.is_config(s);
                f.set(slot, s, v, config);
            }
        }
        Ok(f)
    }

    /// [`Self::bind_user`] for the common call: positional arguments only,
    /// no more of them than parameters, and every parameter in a slot (no
    /// `$` parameter, which binds in the name map). Then argument `k`
    /// binds parameter `k`, nothing can warn, and only the defaults of the
    /// parameters left unset remain, evaluated in the same order as the
    /// general path. `None`: not such a call; `bind_user` does it.
    ///
    /// The general path's bookkeeping (the named-argument list, the unit's
    /// symbol table clone, a position search per argument) was a measurable
    /// part of every user call, and almost every call in BOSL2 is this
    /// shape. Duplicate parameter names share a slot here as they do there:
    /// the later argument wins, and a default is skipped once the slot is
    /// set.
    #[inline]
    fn bind_positional(
        &mut self,
        args: &mut Vec<ArgVal>,
        unit: u32,
        params: &'a [Param],
        defining: &Rc<Ctx>,
        region: u32,
    ) -> Option<R<Frame>> {
        let r = &self.regions[region as usize];
        if args.len() > params.len()
            || args.iter().any(|a| a.name.is_some())
            || r.binds.len() < params.len()
            || r.binds[..params.len()].contains(&NO_SLOT)
        {
            return None;
        }
        let mut slots = vec![None; r.len()];
        let n = args.len();
        for (k, a) in args.drain(..).enumerate() {
            slots[self.regions[region as usize].binds[k] as usize] = Some(a.value);
        }
        for (k, p) in params.iter().enumerate().skip(n) {
            let slot = self.regions[region as usize].binds[k] as usize;
            if slots[slot].is_none() {
                let v = match p.default {
                    Some(d) => match self.eval(unit, d, defining) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    },
                    None => Value::Undef,
                };
                slots[slot] = Some(v);
            }
        }
        Some(Ok(Frame {
            slots,
            vars: Vars::default(),
        }))
    }

    /// The slot of parameter `k` in `region` (its `k`th binder).
    fn param_slot(&self, region: u32, k: usize) -> u32 {
        let binds = &self.regions[region as usize].binds;
        binds.get(k).copied().unwrap_or(NO_SLOT)
    }

    /// The slot of `s` in `region`, or [`NO_SLOT`] for the name map.
    fn slot_in(&self, region: u32, s: Sym) -> u32 {
        if self.syms.is_config(s) {
            return NO_SLOT;
        }
        self.regions[region as usize].slot_of(s).unwrap_or(NO_SLOT)
    }

    /// Put a call's bound arguments into the callee's context.
    pub fn apply_frame(&mut self, ctx: &Ctx, frame: Frame) {
        ctx.merge_slots(frame.slots);
        let mut vars = ctx.vars.borrow_mut();
        if vars.is_empty() {
            *vars = frame.vars;
            return;
        }
        for (s, v) in frame.vars.into_items() {
            let config = self.syms.is_config(s);
            vars.set(s, v, config);
        }
    }

    /// `apply_config_variables`: copy `from`'s own `$` variables.
    fn copy_config(&mut self, from: &Ctx, to: &Ctx) {
        let src = from.vars.borrow();
        if !src.has_config {
            return;
        }
        let mut dst = to.vars.borrow_mut();
        for (s, v) in src.iter() {
            if self.syms.is_config(*s) {
                dst.set(*s, v.clone(), true);
            }
        }
    }

    /// `FunctionCall`'s name: the identifier, or the callee printed in
    /// parentheses.
    pub fn call_name(&self, u: u32, call: ExprId) -> Vec<u8> {
        let ast = self.units[u as usize].ast;
        let ExprKind::Call(callee, _) = &ast.expr(call).kind else {
            return Vec::new();
        };
        match &ast.expr(*callee).kind {
            ExprKind::Var(n) => ast.name(*n).as_bytes().to_vec(),
            _ => {
                let mut out = b"(".to_vec();
                lang::dump::write_expr(ast, *callee, &mut out);
                out.push(b')');
                out
            }
        }
    }

    /// `FunctionCall::evaluate`: evaluate a call, replacing the expression
    /// in place while it is a tail call (a call, or a ternary, `let`,
    /// `assert` or `echo` leading to one), so tail recursion runs in
    /// constant native stack.
    #[inline(never)]
    pub fn eval_call(&mut self, u: u32, id: ExprId, ctx: &Rc<Ctx>) -> R<Value> {
        if self.recursion_exhausted() {
            let loc = self.expr_loc(u, id);
            let mut t = b"Recursion detected calling function '".to_vec();
            t.extend_from_slice(&self.call_name(u, id));
            t.push(b'\'');
            self.error(Some(loc), DiagCode::RecursionLimit, t);
            return Err(self.unwind(UnwindKind::Recursion));
        }
        self.check_interrupt()?;
        // A frame for the frame budget (see `crate::recursion`); tail
        // calls below reuse it, as they reuse the native stack.
        self.frames += crate::recursion::CALL_FRAMES;
        // A call that can only ever reach a builtin makes one step and no
        // context, so it skips the loop and its stack slot. The checks
        // above and the frame charge are the loop's, in the same order, so
        // the recursion limit, interrupts and the frame budget see it as
        // before.
        if let Some(b) = self.static_builtin(u, id) {
            let r = self.direct_builtin(b, u, id, ctx);
            self.frames -= crate::recursion::CALL_FRAMES;
            return r;
        }
        // The loop owns one stack slot, holding the context of the step
        // being evaluated, and `simplify` pushes each callee's (or `let`'s)
        // context just above it, where it is visible to the arguments as
        // in OpenSCAD; the loop then moves it down into the slot rather
        // than popping it and pushing a clone.
        //
        // OpenSCAD evaluates the first call in a fresh empty context, which
        // only a `$` lookup (it binds none) or `copy_config` (it has none
        // to copy) could see, so the slot starts with a shared empty
        // context, `entry` (`cur` is `None`) skips the copy, and the call
        // is evaluated directly in `ctx`, borrowed rather than cloned.
        // (Pushing `ctx` itself would not do: it need not be on the stack,
        // and its `$` variables would then become visible to the
        // arguments.)
        let slot = self.push(self.placeholder.clone());
        let mut cur: Option<Rc<Ctx>> = None;
        let mut unit = u;
        let mut expr = Some(id);
        let mut call = (u, id);
        let mut depth: u32 = 0;
        let result = loop {
            let entry = cur.is_none();
            match self.simplify(unit, expr, cur.as_ref().unwrap_or(ctx), entry) {
                // A warning from the callee itself (an unknown function, a
                // builtin's argument check) is raised inside OpenSCAD's
                // `FunctionCall::evaluate`, so it is traced as its caller.
                Ok(Step::Done(v)) => match self.check_hard() {
                    Ok(()) => break Ok(v),
                    Err(mut e) => {
                        self.trace_call(&mut e, call);
                        break Err(e);
                    }
                },
                Ok(Step::Next {
                    unit: nu,
                    expr: ne,
                    ctx: nc,
                    call: c,
                }) => {
                    unit = nu;
                    expr = ne;
                    if let Some(nc) = nc {
                        // `simplify` left `nc` on top of the stack, just
                        // above the slot: it replaces the previous step's
                        // context there, which only this loop could still
                        // see.
                        debug_assert_eq!(self.stack.len(), slot + 2);
                        self.stack.swap_remove(slot);
                        // The replaced step's context dies here unless a
                        // function literal captured it; if it does die,
                        // its allocation serves the next call or `let`.
                        if let Some(old) = cur.replace(nc) {
                            Ctx::recycle(old, &mut self.ctx_pool);
                        }
                    }
                    if let Some(c) = c {
                        call = c;
                        let hit_limit = depth == 1_000_000;
                        depth += 1;
                        let err = if hit_limit {
                            let loc = expr
                                .map(|e| self.expr_loc(unit, e))
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
                            self.trace_call(&mut e, call);
                            break Err(e);
                        }
                    }
                }
                Err(mut e) => {
                    self.trace_call(&mut e, call);
                    break Err(e);
                }
            }
        };
        self.truncate(slot);
        if let Some(c) = cur {
            Ctx::recycle(c, &mut self.ctx_pool);
        }
        self.frames -= crate::recursion::CALL_FRAMES;
        result
    }

    /// The builtin call `id` always makes, if it always makes one: its
    /// callee is a name the resolver resolved, and no scope, variable,
    /// parameter frame or used library on the way out can bind that name
    /// (no candidates), so [`Self::find_function`] could only end at the
    /// builtin context, where every chain ends. A disabled experimental
    /// builtin is left to that walk, which warns. The same test is written
    /// out in `find_function` rather than shared: a shared helper measured
    /// 2-3% slower on call-heavy code at equal instruction counts (code
    /// layout; see `docs/followups.md`, "Evaluator layout sensitivity").
    #[inline]
    fn static_builtin(&self, u: u32, id: ExprId) -> Option<Builtin> {
        let unit = &self.units[u as usize];
        let r = unit.res.expr[id.0 as usize];
        if r == 0 {
            return None;
        }
        let fr = unit.res.fns[(r - 1) as usize];
        if fr.cands.len != 0 {
            return None;
        }
        fr.builtin.filter(|b| b.enabled())
    }

    /// [`Self::eval_call`]'s one step for a [`Self::static_builtin`] call:
    /// what `simplify` would do with it (the arguments evaluated in the
    /// caller's context, which is what the loop's first step uses), then
    /// the loop's `check_hard` and its trace of a failure as the call.
    /// Out of line, so the direct path adds nothing to `eval_call`'s stack
    /// frame, which every level of a recursion holds.
    #[inline(never)]
    fn direct_builtin(&mut self, b: Builtin, u: u32, id: ExprId, ctx: &Rc<Ctx>) -> R<Value> {
        let ast: &'a Ast = self.units[u as usize].ast;
        let ExprKind::Call(_, args) = &ast.expr(id).kind else {
            unreachable!("a resolved function reference is a call");
        };
        let r = self
            .call_builtin(b, u, id, args, ctx)
            .and_then(|v| self.check_hard().map(|()| v));
        r.map_err(|mut e| {
            self.trace_call(&mut e, (u, id));
            e
        })
    }

    fn trace_call(&mut self, e: &mut crate::message::Unwind, call: (u32, ExprId)) {
        let mut t = b"called by '".to_vec();
        t.extend_from_slice(&self.call_name(call.0, call.1));
        t.push(b'\'');
        let loc = self.expr_loc(call.0, call.1);
        self.trace(e, loc, t);
    }

    /// `simplify_function_body`: one step of the tail-call loop. `entry`:
    /// `ctx` is the caller's context, not one of the loop's own, and its
    /// `$` variables are not copied (see `eval_call`).
    fn simplify(&mut self, u: u32, expr: Option<ExprId>, ctx: &Rc<Ctx>, entry: bool) -> R<Step> {
        let Some(id) = expr else {
            return Ok(Step::Done(Value::Undef));
        };
        let ast: &'a Ast = self.units[u as usize].ast;
        let e = ast.expr(id);
        let next = |expr: Option<ExprId>| Step::Next {
            unit: u,
            expr,
            ctx: None,
            call: None,
        };
        match &e.kind {
            ExprKind::Ternary(c, a, b) => {
                let pick = if self.eval(u, *c, ctx)?.to_bool() {
                    *a
                } else {
                    *b
                };
                Ok(next(Some(pick)))
            }
            ExprKind::Assert(args, body) => {
                self.perform_assert(u, args, e.span, ctx)?;
                Ok(next(*body))
            }
            ExprKind::Echo(args, body) => {
                self.echo(u, args, ctx)?;
                Ok(next(*body))
            }
            ExprKind::Let(args, body) => {
                let region = self.units[u as usize].res.expr[id.0 as usize];
                let c = self.new_ctx(ctx, CtxKind::Plain, region);
                self.push(c.clone());
                if !entry {
                    self.copy_config(ctx, &c);
                }
                self.sequential_assign(u, args, e.span, &c)?;
                Ok(Step::Next {
                    unit: u,
                    expr: Some(*body),
                    ctx: Some(c),
                    call: None,
                })
            }
            ExprKind::Call(callee, args) => {
                let loc = Loc {
                    unit: u,
                    span: e.span,
                };
                let callable = match &ast.expr(*callee).kind {
                    ExprKind::Var(n) => {
                        let s = self.units[u as usize].sym(*n);
                        match self.units[u as usize].res.expr[id.0 as usize] {
                            0 => {
                                if !self.syms.is_config(s) {
                                    self.stats.fallbacks += 1;
                                }
                                self.lookup_function(ctx, s, loc)?
                            }
                            r => self.find_function(u, r - 1, ctx, s, loc)?,
                        }
                    }
                    _ => {
                        let v = self.eval(u, *callee, ctx)?;
                        match v {
                            Value::Function(f) => Some(Callable::Literal(f)),
                            other => {
                                let t = format!("Can't call function on {}", other.type_name());
                                self.warn(loc, DiagCode::UnknownFunction, t);
                                None
                            }
                        }
                    }
                };
                let (fu, params, body, defining, region): (u32, &'a [Param], ExprId, Rc<Ctx>, u32) =
                    match callable {
                        None => return Ok(Step::Done(Value::Undef)),
                        Some(Callable::Builtin(b)) => {
                            let v = self.call_builtin(b, u, id, args, ctx)?;
                            return Ok(Step::Done(v));
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
                            (unit, &f.params, f.body, dctx, region)
                        }
                        Some(Callable::Literal(f)) => {
                            let fast: &'a Ast = self.units[f.unit as usize].ast;
                            match &fast.expr(f.expr).kind {
                                ExprKind::Function(params, body) => {
                                    let region =
                                        self.units[f.unit as usize].res.expr[f.expr.0 as usize];
                                    (f.unit, params.as_slice(), *body, f.ctx.clone(), region)
                                }
                                _ => return Ok(Step::Done(Value::Undef)),
                            }
                        }
                    };
                // `defining` is this call's own reference (the lookup
                // cloned it), so it becomes the body's parent as it is.
                let body_ctx = self.new_ctx_in(defining, CtxKind::Plain, region);
                self.push(body_ctx.clone());
                if !entry {
                    self.copy_config(ctx, &body_ctx);
                }
                self.call_frame(u, id, args, ctx, entry, loc, fu, params, &body_ctx)?;
                Ok(Step::Next {
                    unit: fu,
                    expr: Some(body),
                    ctx: Some(body_ctx),
                    call: Some((u, id)),
                })
            }
            _ => Ok(Step::Done(self.eval(u, id, ctx)?)),
        }
    }

    /// A user call's arguments, evaluated and bound into `body_ctx`. Out
    /// of line, so the argument vector and the bound frame are not part of
    /// `eval_call`'s stack frame, which every level of a recursion holds.
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn call_frame(
        &mut self,
        u: u32,
        id: ExprId,
        args: &'a [Arg],
        ctx: &Rc<Ctx>,
        entry: bool,
        loc: Loc,
        fu: u32,
        params: &'a [Param],
        body_ctx: &Ctx,
    ) -> R<()> {
        // Defaults are evaluated in the defining context: the body's parent.
        let defining = body_ctx
            .parent
            .as_ref()
            .expect("a function body context has its defining context as parent");
        // Argument vectors are reused: a call allocates only its context
        // and slots.
        let mut argv = self.arg_pool.pop().unwrap_or_default();
        let r = if self.accumulates(u, id, args) {
            self.eval_args_moving(u, args, ctx, entry, &mut argv)
        } else {
            self.eval_args_into(u, args, ctx, &mut argv)
        };
        let frame =
            r.and_then(|()| self.bind_user(&mut argv, loc, fu, params, defining, body_ctx.region));
        argv.clear();
        self.arg_pool.push(argv);
        self.apply_frame(body_ctx, frame?);
        Ok(())
    }

    /// Before a tail call's arguments are evaluated, move the accumulator
    /// of an argument `concat(acc, ...)` or `[each acc, ...]` out of the
    /// frame the call replaces, so that the list has one owner and grows in
    /// place (see [`crate::value::Growable`]). The frame still holds it
    /// otherwise, and each step of `f(n, acc) = ... f(n - 1, concat(acc,
    /// [x]))` copied the whole list: quadratic where OpenSCAD is linear.
    ///
    /// Values are immutable, so the move must be unobservable. It is made
    /// only when:
    ///
    /// - `acc` is read exactly once in all the call's arguments (counting
    ///   function literals and comprehensions in them), in a position that
    ///   is evaluated at most once, so no later read can see the hole;
    /// - `acc` is an ordinary variable (a `$` variable is read dynamically,
    ///   from anywhere below);
    /// - every context from `ctx` up to the one binding `acc` is held only
    ///   by the evaluator's tail-call loop and its own child: no function
    ///   literal captured it, the callee is not defined in it, and nothing
    ///   else will look in it after this call replaces it.
    ///
    /// The value is handed to the one read that resolves to that binding
    /// ([`Evaluator::take_moved`]); the frame keeps `undef`.
    #[inline(never)]
    fn eval_args_moving(
        &mut self,
        u: u32,
        args: &'a [Arg],
        ctx: &Rc<Ctx>,
        entry: bool,
        out: &mut Vec<ArgVal>,
    ) -> R<()> {
        let mark = self.moved.len();
        self.move_accumulators(u, args, ctx, entry);
        let r = self.eval_args_into(u, args, ctx, out);
        self.moved.truncate(mark);
        r
    }

    /// Whether call `id` has an argument [`Evaluator::accumulator`] finds,
    /// remembered per call (this runs at every user function call).
    #[inline]
    fn accumulates(&mut self, u: u32, id: ExprId, args: &[Arg]) -> bool {
        let known = &self.units[u as usize].accumulates;
        match known.get(id.0 as usize) {
            Some(1) => false,
            Some(2) => true,
            _ => self.find_accumulators(u, id, args),
        }
    }

    #[cold]
    #[inline(never)]
    fn find_accumulators(&mut self, u: u32, id: ExprId, args: &[Arg]) -> bool {
        let yes = args.iter().any(|a| self.accumulator(u, a.expr).is_some());
        let unit = &mut self.units[u as usize];
        if unit.accumulates.is_empty() {
            unit.accumulates = vec![0; unit.ast.exprs.len()];
        }
        unit.accumulates[id.0 as usize] = if yes { 2 } else { 1 };
        yes
    }

    fn move_accumulators(&mut self, u: u32, args: &'a [Arg], ctx: &Rc<Ctx>, entry: bool) {
        for a in args {
            let Some(s) = self.accumulator(u, a.expr) else {
                continue;
            };
            if self.syms.is_config(s) || self.uses(u, args, s) != 1 {
                continue;
            }
            let Some(owner) = private_binder(ctx, s, entry, &self.regions) else {
                continue;
            };
            let value = owner.take_local(s, &self.regions);
            self.moved.push(crate::eval::Moved {
                owner: Rc::as_ptr(&owner),
                sym: s,
                value,
            });
        }
    }

    /// The variable `acc` of an argument `concat(acc, ...)` or
    /// `[each acc, ...]`.
    fn accumulator(&self, u: u32, e: ExprId) -> Option<Sym> {
        let unit = &self.units[u as usize];
        let ast: &Ast = unit.ast;
        let var = match &ast.expr(e).kind {
            ExprKind::Call(callee, cargs) => {
                let ExprKind::Var(f) = ast.expr(*callee).kind else {
                    return None;
                };
                let first = cargs.first()?;
                if unit.sym(f) != self.k.concat || first.name.is_some() {
                    return None;
                }
                first.expr
            }
            ExprKind::Vector(items) => match ast.expr(*items.first()?).kind {
                ExprKind::LcEach(x) => x,
                _ => return None,
            },
            _ => return None,
        };
        match ast.expr(var).kind {
            ExprKind::Var(n) => Some(unit.sym(n)),
            _ => None,
        }
    }

    /// How many times `s` is named in `args` (stopping at 2), walked with an
    /// explicit stack: expressions can nest deeper than the native stack.
    fn uses(&self, u: u32, args: &[Arg], s: Sym) -> usize {
        let unit = &self.units[u as usize];
        let ast: &Ast = unit.ast;
        let mut todo: Vec<ExprId> = args.iter().map(|a| a.expr).collect();
        let mut n = 0;
        let arg_exprs = |todo: &mut Vec<ExprId>, args: &[Arg]| {
            todo.extend(args.iter().map(|a| a.expr));
        };
        while let Some(e) = todo.pop() {
            match &ast.expr(e).kind {
                ExprKind::Var(v) => {
                    if unit.sym(*v) == s {
                        n += 1;
                        if n > 1 {
                            return n;
                        }
                    }
                }
                ExprKind::Undef
                | ExprKind::Bool(_)
                | ExprKind::Number(_)
                | ExprKind::String(_)
                | ExprKind::Invalid => {}
                ExprKind::Unary(_, x) | ExprKind::Member(x, _) | ExprKind::LcEach(x) => {
                    todo.push(*x);
                }
                ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => todo.extend([*a, *b]),
                ExprKind::Ternary(a, b, c) => todo.extend([*a, *b, *c]),
                ExprKind::LcIf(a, b, c) => {
                    todo.extend([*a, *b]);
                    todo.extend(*c);
                }
                ExprKind::Call(callee, a) => {
                    todo.push(*callee);
                    arg_exprs(&mut todo, a);
                }
                ExprKind::Range { begin, step, end } => {
                    todo.extend([*begin, *end]);
                    todo.extend(*step);
                }
                ExprKind::Vector(items) => todo.extend(items.iter().copied()),
                ExprKind::Function(params, body) => {
                    todo.extend(params.iter().filter_map(|p| p.default));
                    todo.push(*body);
                }
                ExprKind::Let(a, body) | ExprKind::LcFor(a, body) | ExprKind::LcLet(a, body) => {
                    arg_exprs(&mut todo, a);
                    todo.push(*body);
                }
                ExprKind::Assert(a, body) | ExprKind::Echo(a, body) => {
                    arg_exprs(&mut todo, a);
                    todo.extend(*body);
                }
                ExprKind::LcForC {
                    init,
                    cond,
                    incr,
                    body,
                } => {
                    arg_exprs(&mut todo, init);
                    arg_exprs(&mut todo, incr);
                    todo.extend([*cond, *body]);
                }
            }
        }
        n
    }

    /// `Context::lookup_function`.
    pub fn lookup_function(&mut self, ctx: &Rc<Ctx>, s: Sym, loc: Loc) -> R<Option<Callable>> {
        if self.syms.is_config(s) {
            for i in (0..self.stack.len()).rev() {
                let c = self.stack[i].clone();
                if let Some(f) = self.local_function(&c, s, loc)? {
                    return Ok(Some(f));
                }
            }
        } else {
            let mut cur = Some(ctx.clone());
            while let Some(c) = cur {
                if let Some(f) = self.local_function(&c, s, loc)? {
                    return Ok(Some(f));
                }
                cur = c.parent();
            }
        }
        let t = format!("Ignoring unknown function '{}'", self.name(s));
        self.warn(loc, DiagCode::UnknownFunction, t);
        Ok(None)
    }

    /// `Context::lookup_function` for a name the resolver resolved (see
    /// [`crate::resolve`]): the same walk, looking only in the contexts
    /// that can define or bind the name, in the same order. Not inlined:
    /// its frame would add to every level of a recursion.
    #[inline(never)]
    fn find_function(
        &mut self,
        u: u32,
        r: u32,
        ctx: &Rc<Ctx>,
        s: Sym,
        loc: Loc,
    ) -> R<Option<Callable>> {
        let fr = self.units[u as usize].res.fns[r as usize];
        // A name nothing can bind: the walk below would pass each context
        // by and stop at the builtin context. Answering at once saves that
        // walk through every `let` and call frame out to the file, for a
        // builtin called in tail position (`function f(x) = max(x, 0)`);
        // other builtin calls take `eval_call`'s direct path instead.
        if fr.cands.len == 0
            && let Some(b) = fr.builtin
            && b.enabled()
        {
            return Ok(Some(Callable::Builtin(b)));
        }
        let mut c: &Rc<Ctx> = ctx;
        loop {
            if c.region == BUILTIN_REGION
                && let Some(b) = fr.builtin
            {
                if b.enabled() {
                    return Ok(Some(Callable::Builtin(b)));
                }
                let t = format!(
                    "Experimental builtin function '{}' is not enabled",
                    self.name(s)
                );
                self.warn(loc, DiagCode::ExperimentalFeature, t);
            }
            for k in fr.cands.start..fr.cands.start + fr.cands.len {
                let cand = self.units[u as usize].res.cands[k as usize];
                if cand.region() != c.region {
                    continue;
                }
                match cand {
                    Cand::Def { scope, index, .. } => {
                        return Ok(Some(Callable::User {
                            ctx: c.clone(),
                            unit: u,
                            scope,
                            index,
                        }));
                    }
                    Cand::Slot { slot, .. } => {
                        if let Some(Value::Function(f)) = c.slot(slot) {
                            return Ok(Some(Callable::Literal(f)));
                        }
                    }
                    Cand::Extra { .. } => {
                        if let Some(Value::Function(f)) = c.vars.borrow().get(s) {
                            return Ok(Some(Callable::Literal(f.clone())));
                        }
                    }
                    Cand::Use { lib, index, .. } => {
                        let lctx = self.library_context(c, lib)?;
                        return Ok(Some(Callable::User {
                            ctx: lctx,
                            unit: lib,
                            scope: 0,
                            index,
                        }));
                    }
                }
            }
            match &c.parent {
                Some(p) => c = p,
                None => break,
            }
        }
        let t = format!("Ignoring unknown function '{}'", self.name(s));
        self.warn(loc, DiagCode::UnknownFunction, t);
        Ok(None)
    }

    /// `Context::lookup_module` for a resolved name (see
    /// [`Self::find_function`]).
    #[inline(never)]
    pub fn find_module(
        &mut self,
        u: u32,
        r: u32,
        ctx: &Rc<Ctx>,
        s: Sym,
        loc: Loc,
    ) -> R<Option<Instantiable>> {
        let mr = self.units[u as usize].res.mods[r as usize];
        let mut c: &Rc<Ctx> = ctx;
        loop {
            if c.region == BUILTIN_REGION
                && let Some(m) = mr.builtin
            {
                if m.enabled() {
                    return Ok(Some(Instantiable::Builtin(m)));
                }
                let t = format!(
                    "Experimental builtin module '{}' is not enabled",
                    self.name(s)
                );
                self.warn(loc, DiagCode::ExperimentalFeature, t);
            }
            for k in mr.cands.start..mr.cands.start + mr.cands.len {
                let cand = self.units[u as usize].res.cands[k as usize];
                if cand.region() != c.region {
                    continue;
                }
                match cand {
                    Cand::Def { scope, index, .. } => {
                        return Ok(Some(Instantiable::User {
                            ctx: c.clone(),
                            unit: u,
                            scope,
                            index,
                        }));
                    }
                    Cand::Use { lib, index, .. } => {
                        let lctx = self.library_context(c, lib)?;
                        return Ok(Some(Instantiable::User {
                            ctx: lctx,
                            unit: lib,
                            scope: 0,
                            index,
                        }));
                    }
                    Cand::Slot { .. } | Cand::Extra { .. } => {}
                }
            }
            match &c.parent {
                Some(p) => c = p,
                None => break,
            }
        }
        let t = format!("Ignoring unknown module '{}'", self.name(s));
        self.warn(loc, DiagCode::UnknownModule, t);
        Ok(None)
    }

    fn var_function(&self, c: &Ctx, s: Sym) -> Option<Callable> {
        match c.get_local(s, &self.regions) {
            Some(Value::Function(f)) => Some(Callable::Literal(f)),
            _ => None,
        }
    }

    /// `lookup_local_function` of each context kind.
    fn local_function(&mut self, c: &Rc<Ctx>, s: Sym, loc: Loc) -> R<Option<Callable>> {
        match &c.kind {
            CtxKind::Plain => Ok(self.var_function(c, s)),
            CtxKind::Builtin => {
                if let Some(&b) = self.builtin_fns.get(&s) {
                    if b.enabled() {
                        return Ok(Some(Callable::Builtin(b)));
                    }
                    let t = format!(
                        "Experimental builtin function '{}' is not enabled",
                        self.name(s)
                    );
                    self.warn(loc, DiagCode::ExperimentalFeature, t);
                }
                Ok(self.var_function(c, s))
            }
            CtxKind::Scope(sr) | CtxKind::Module(sr, _) | CtxKind::File(sr) => {
                if let Some(&index) = self.units[sr.unit as usize].scopes[sr.scope as usize]
                    .functions
                    .get(&s)
                {
                    return Ok(Some(Callable::User {
                        ctx: c.clone(),
                        unit: sr.unit,
                        scope: sr.scope,
                        index,
                    }));
                }
                if let Some(f) = self.var_function(c, s) {
                    return Ok(Some(f));
                }
                if let CtxKind::File(sr) = &c.kind {
                    let uses = self.units[sr.unit as usize].uses.clone();
                    for lib in uses {
                        if let Some(&index) = self.units[lib as usize].scopes[0].functions.get(&s) {
                            let lctx = self.library_context(c, lib)?;
                            return Ok(Some(Callable::User {
                                ctx: lctx,
                                unit: lib,
                                scope: 0,
                                index,
                            }));
                        }
                    }
                }
                Ok(None)
            }
        }
    }

    /// A fresh `FileContext` for a used library: its top-level assignments
    /// are evaluated anew on every lookup, as OpenSCAD does.
    fn library_context(&mut self, file: &Rc<Ctx>, lib: u32) -> R<Rc<Ctx>> {
        let sr = ScopeRef {
            unit: lib,
            scope: 0,
        };
        self.resolve_root(lib);
        let region = self.units[lib as usize].res.scope_region[0];
        let lctx = Ctx::new(
            file.parent(),
            CtxKind::File(sr),
            region,
            self.regions[region as usize].len(),
        );
        let mark = self.push(lctx.clone());
        let r = self.init_scope(&lctx, sr);
        self.truncate(mark);
        r.map(|_| lctx)
    }

    /// `Context::lookup_module`.
    pub fn lookup_module(&mut self, ctx: &Rc<Ctx>, s: Sym, loc: Loc) -> R<Option<Instantiable>> {
        if self.syms.is_config(s) {
            for i in (0..self.stack.len()).rev() {
                let c = self.stack[i].clone();
                if let Some(m) = self.local_module(&c, s, loc)? {
                    return Ok(Some(m));
                }
            }
        } else {
            let mut cur = Some(ctx.clone());
            while let Some(c) = cur {
                if let Some(m) = self.local_module(&c, s, loc)? {
                    return Ok(Some(m));
                }
                cur = c.parent();
            }
        }
        let t = format!("Ignoring unknown module '{}'", self.name(s));
        self.warn(loc, DiagCode::UnknownModule, t);
        Ok(None)
    }

    fn local_module(&mut self, c: &Rc<Ctx>, s: Sym, loc: Loc) -> R<Option<Instantiable>> {
        match &c.kind {
            CtxKind::Plain => Ok(None),
            CtxKind::Builtin => {
                if let Some(&m) = self.builtin_mods.get(&s) {
                    if m.enabled() {
                        return Ok(Some(Instantiable::Builtin(m)));
                    }
                    let t = format!(
                        "Experimental builtin module '{}' is not enabled",
                        self.name(s)
                    );
                    self.warn(loc, DiagCode::ExperimentalFeature, t);
                }
                Ok(None)
            }
            CtxKind::Scope(sr) | CtxKind::Module(sr, _) | CtxKind::File(sr) => {
                if let Some(&index) = self.units[sr.unit as usize].scopes[sr.scope as usize]
                    .modules
                    .get(&s)
                {
                    return Ok(Some(Instantiable::User {
                        ctx: c.clone(),
                        unit: sr.unit,
                        scope: sr.scope,
                        index,
                    }));
                }
                if let CtxKind::File(sr) = &c.kind {
                    let uses = self.units[sr.unit as usize].uses.clone();
                    for lib in uses {
                        if let Some(&index) = self.units[lib as usize].scopes[0].modules.get(&s) {
                            let lctx = self.library_context(c, lib)?;
                            return Ok(Some(Instantiable::User {
                                ctx: lctx,
                                unit: lib,
                                scope: 0,
                                index,
                            }));
                        }
                    }
                }
                Ok(None)
            }
        }
    }
}
