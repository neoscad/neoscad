//! Function calls: argument binding, name lookup and the tail-call loop.

use std::rc::Rc;

use lang::ast::{Arg, Ast, ExprId, ExprKind, Param};
use lang::diag::DiagCode;

use crate::builtins::functions::Builtin;
use crate::builtins::modules::BuiltinModule;
use crate::context::{Ctx, CtxKind, ScopeRef, Vars};
use crate::eval::{Evaluator, Step};
use crate::message::{Loc, R, UnwindKind};
use crate::sym::Sym;
use crate::value::{FunctionValue, Value};

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
    User { ctx: Rc<Ctx>, unit: u32, scope: u32, index: u32 },
    Literal(Rc<FunctionValue>),
}

/// Something `m(...)` can instantiate.
pub(crate) enum Instantiable {
    Builtin(BuiltinModule),
    User { ctx: Rc<Ctx>, unit: u32, scope: u32, index: u32 },
}

impl<'a> Evaluator<'a> {
    /// `Arguments`: evaluate call arguments in order.
    pub fn eval_args(&mut self, u: u32, args: &'a [Arg], ctx: &Rc<Ctx>) -> R<Vec<ArgVal>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            let value = self.eval(u, a.expr, ctx)?;
            let name = a.name.map(|n| self.units[u as usize].sym(n));
            out.push(ArgVal { name, value });
        }
        Ok(out)
    }

    /// `parse_without_defaults`: match arguments to parameter names.
    /// `params` gives the `n` parameter names by position (a closure, so
    /// calls need not collect them).
    fn bind(&mut self, args: Vec<ArgVal>, loc: Loc, n: usize, params: impl Fn(usize) -> Sym, warn: bool) -> Vars {
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
                        let t = format!("argument {} overrides positional argument", self.quote_sym(n));
                        self.warn(loc, DiagCode::ArgumentMismatch, t);
                    } else if warn && !self.syms.is_config(n) && !(0..n_params).any(|i| params(i) == n) {
                        let t = format!("variable {} not specified as parameter", self.quote_sym(n));
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
                                self.warn(loc, DiagCode::ArgumentMismatch, "Too many unnamed arguments supplied");
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
    pub fn bind_builtin(&mut self, args: Vec<ArgVal>, loc: Loc, required: &[Sym], optional: &[Sym], warn: bool) -> Vars {
        let r = required.len();
        let mut frame = self.bind(args, loc, r + optional.len(), |i| if i < r { required[i] } else { optional[i - r] }, warn);
        for &p in required {
            if frame.get(p).is_none() {
                let config = self.syms.is_config(p);
                frame.set(p, Value::Undef, config);
            }
        }
        frame
    }

    /// `Parameters::parse` for user functions and modules: defaults are
    /// evaluated in the defining context.
    pub fn bind_user(&mut self, args: Vec<ArgVal>, loc: Loc, unit: u32, params: &'a [Param], defining: &Rc<Ctx>) -> R<Vars> {
        let warn = self.opts.check_parameters;
        // A cheap clone (reference count), so the closure does not borrow
        // `self` while `bind` needs it mutably.
        let unit_syms = self.units[unit as usize].syms.clone();
        let mut frame = self.bind(args, loc, params.len(), |i| unit_syms[params[i].name.0 as usize], warn);
        for p in params {
            let s = unit_syms[p.name.0 as usize];
            if frame.get(s).is_none() {
                let v = match p.default {
                    Some(d) => self.eval(unit, d, defining)?,
                    None => Value::Undef,
                };
                let config = self.syms.is_config(s);
                frame.set(s, v, config);
            }
        }
        Ok(frame)
    }

    /// Copy a frame into a context's variables.
    pub fn apply_frame(&mut self, ctx: &Ctx, frame: Vars) {
        let mut vars = ctx.vars.borrow_mut();
        if vars.is_empty() {
            *vars = frame;
            return;
        }
        for (s, v) in frame.into_items() {
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
        let ExprKind::Call(callee, _) = &ast.expr(call).kind else { return Vec::new() };
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
        if self.stack_exhausted() {
            let loc = self.expr_loc(u, id);
            let mut t = b"Recursion detected calling function '".to_vec();
            t.extend_from_slice(&self.call_name(u, id));
            t.push(b'\'');
            self.error(Some(loc), DiagCode::RecursionLimit, t);
            return Err(self.unwind(UnwindKind::Recursion));
        }
        self.check_interrupt()?;
        let slot = self.push(Ctx::child(ctx));
        let mut cur = self.stack[slot].clone();
        let mut unit = u;
        let mut expr = Some(id);
        let mut call = (u, id);
        let mut depth: u32 = 0;
        let result = loop {
            match self.simplify(unit, expr, &cur) {
                Ok(Step::Done(v)) => break Ok(v),
                Ok(Step::Next { unit: nu, expr: ne, ctx: nc, call: c }) => {
                    unit = nu;
                    expr = ne;
                    if let Some(nc) = nc {
                        self.truncate(slot);
                        self.push(nc.clone());
                        cur = nc;
                    }
                    if let Some(c) = c {
                        call = c;
                        let hit_limit = depth == 1_000_000;
                        depth += 1;
                        let err = if hit_limit {
                            let loc = expr.map(|e| self.expr_loc(unit, e)).unwrap_or(self.expr_loc(c.0, c.1));
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
        result
    }

    fn trace_call(&mut self, e: &mut crate::message::Unwind, call: (u32, ExprId)) {
        let mut t = b"called by '".to_vec();
        t.extend_from_slice(&self.call_name(call.0, call.1));
        t.push(b'\'');
        let loc = self.expr_loc(call.0, call.1);
        self.trace(e, loc, t);
    }

    /// `simplify_function_body`: one step of the tail-call loop.
    fn simplify(&mut self, u: u32, expr: Option<ExprId>, ctx: &Rc<Ctx>) -> R<Step> {
        let Some(id) = expr else { return Ok(Step::Done(Value::Undef)) };
        let ast: &'a Ast = self.units[u as usize].ast;
        let e = ast.expr(id);
        let next = |expr: Option<ExprId>| Step::Next { unit: u, expr, ctx: None, call: None };
        match &e.kind {
            ExprKind::Ternary(c, a, b) => {
                let pick = if self.eval(u, *c, ctx)?.to_bool() { *a } else { *b };
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
                let c = Ctx::child(ctx);
                self.push(c.clone());
                self.copy_config(ctx, &c);
                self.sequential_assign(u, args, e.span, &c)?;
                Ok(Step::Next { unit: u, expr: Some(*body), ctx: Some(c), call: None })
            }
            ExprKind::Call(callee, args) => {
                let loc = Loc { unit: u, span: e.span };
                let callable = match &ast.expr(*callee).kind {
                    ExprKind::Var(n) => {
                        let s = self.units[u as usize].sym(*n);
                        self.lookup_function(ctx, s, loc)?
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
                let (fu, params, body, defining): (u32, &'a [Param], ExprId, Rc<Ctx>) = match callable {
                    None => return Ok(Step::Done(Value::Undef)),
                    Some(Callable::Builtin(b)) => {
                        let v = self.call_builtin(b, u, id, args, ctx)?;
                        return Ok(Step::Done(v));
                    }
                    Some(Callable::User { ctx: dctx, unit, scope, index }) => {
                        let scope: &'a lang::ast::Scope = self.units[unit as usize].scopes[scope as usize].scope;
                        let f = &scope.functions[index as usize];
                        (unit, &f.params, f.body, dctx)
                    }
                    Some(Callable::Literal(f)) => {
                        let fast: &'a Ast = self.units[f.unit as usize].ast;
                        match &fast.expr(f.expr).kind {
                            ExprKind::Function(params, body) => (f.unit, params.as_slice(), *body, f.ctx.clone()),
                            _ => return Ok(Step::Done(Value::Undef)),
                        }
                    }
                };
                let body_ctx = Ctx::child(&defining);
                self.push(body_ctx.clone());
                self.copy_config(ctx, &body_ctx);
                let argv = self.eval_args(u, args, ctx)?;
                let frame = self.bind_user(argv, loc, fu, params, &defining)?;
                self.apply_frame(&body_ctx, frame);
                Ok(Step::Next { unit: fu, expr: Some(body), ctx: Some(body_ctx), call: Some((u, id)) })
            }
            _ => Ok(Step::Done(self.eval(u, id, ctx)?)),
        }
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

    fn var_function(c: &Ctx, s: Sym) -> Option<Callable> {
        match c.vars.borrow().get(s) {
            Some(Value::Function(f)) => Some(Callable::Literal(f.clone())),
            _ => None,
        }
    }

    /// `lookup_local_function` of each context kind.
    fn local_function(&mut self, c: &Rc<Ctx>, s: Sym, loc: Loc) -> R<Option<Callable>> {
        match &c.kind {
            CtxKind::Plain => Ok(Self::var_function(c, s)),
            CtxKind::Builtin => {
                if let Some(&b) = self.builtin_fns.get(&s) {
                    if b.enabled() {
                        return Ok(Some(Callable::Builtin(b)));
                    }
                    let t = format!("Experimental builtin function '{}' is not enabled", self.name(s));
                    self.warn(loc, DiagCode::ExperimentalFeature, t);
                }
                Ok(Self::var_function(c, s))
            }
            CtxKind::Scope(sr) | CtxKind::Module(sr, _) | CtxKind::File(sr) => {
                if let Some(&index) = self.units[sr.unit as usize].scopes[sr.scope as usize].functions.get(&s) {
                    return Ok(Some(Callable::User { ctx: c.clone(), unit: sr.unit, scope: sr.scope, index }));
                }
                if let Some(f) = Self::var_function(c, s) {
                    return Ok(Some(f));
                }
                if let CtxKind::File(sr) = &c.kind {
                    let uses = self.units[sr.unit as usize].uses.clone();
                    for lib in uses {
                        if let Some(&index) = self.units[lib as usize].scopes[0].functions.get(&s) {
                            let lctx = self.library_context(c, lib)?;
                            return Ok(Some(Callable::User { ctx: lctx, unit: lib, scope: 0, index }));
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
        let sr = ScopeRef { unit: lib, scope: 0 };
        let lctx = Ctx::new(file.parent(), CtxKind::File(sr));
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
                    let t = format!("Experimental builtin module '{}' is not enabled", self.name(s));
                    self.warn(loc, DiagCode::ExperimentalFeature, t);
                }
                Ok(None)
            }
            CtxKind::Scope(sr) | CtxKind::Module(sr, _) | CtxKind::File(sr) => {
                if let Some(&index) = self.units[sr.unit as usize].scopes[sr.scope as usize].modules.get(&s) {
                    return Ok(Some(Instantiable::User { ctx: c.clone(), unit: sr.unit, scope: sr.scope, index }));
                }
                if let CtxKind::File(sr) = &c.kind {
                    let uses = self.units[sr.unit as usize].uses.clone();
                    for lib in uses {
                        if let Some(&index) = self.units[lib as usize].scopes[0].modules.get(&s) {
                            let lctx = self.library_context(c, lib)?;
                            return Ok(Some(Instantiable::User { ctx: lctx, unit: lib, scope: 0, index }));
                        }
                    }
                }
                Ok(None)
            }
        }
    }
}
