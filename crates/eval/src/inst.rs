//! Module instantiation: scopes, user modules and `children()`.

use std::rc::Rc;

use lang::ast::{Instantiation, Scope};
use lang::diag::DiagCode;

use crate::call::Instantiable;
use crate::context::{Children, Ctx, CtxKind, ScopeRef};
use crate::eval::Evaluator;
use crate::message::{Loc, R, UnwindKind};
use crate::node::{Node, NodeKind, Origin};
use crate::sym::Sym;
use crate::value::Value;

impl<'a> Evaluator<'a> {
    pub fn scope(&self, sr: ScopeRef) -> &'a Scope {
        self.units[sr.unit as usize].scopes[sr.scope as usize].scope
    }

    pub fn inst(&self, sr: ScopeRef, i: usize) -> &'a Instantiation {
        &self.scope(sr).instantiations[i]
    }

    /// The scope holding an instantiation's children.
    pub fn children_scope(&self, sr: ScopeRef, i: usize) -> ScopeRef {
        ScopeRef {
            unit: sr.unit,
            scope: self.units[sr.unit as usize].scopes[sr.scope as usize].children[i],
        }
    }

    pub fn else_scope(&self, sr: ScopeRef, i: usize) -> Option<ScopeRef> {
        let s = self.units[sr.unit as usize].scopes[sr.scope as usize].else_children[i];
        (s != u32::MAX).then_some(ScopeRef {
            unit: sr.unit,
            scope: s,
        })
    }

    /// Instantiation `i`'s resolution: its module reference plus one and
    /// its first binding region (see `resolve::UnitRes::inst`), or zeros.
    pub fn inst_res(&self, sr: ScopeRef, i: usize) -> (u32, u32) {
        let insts = &self.units[sr.unit as usize].res.inst[sr.scope as usize];
        insts.get(i).copied().unwrap_or((0, 0))
    }

    /// `ScopeContext::init`: evaluate a scope's assignments in order.
    pub fn init_scope(&mut self, ctx: &Rc<Ctx>, sr: ScopeRef) -> R<()> {
        let scope = self.scope(sr);
        let ast = self.units[sr.unit as usize].ast;
        // The region's binders are a module's parameters, then these
        // assignments (see `resolve::Region::binds`).
        let region = &self.regions[ctx.region as usize];
        let base = region
            .binds
            .len()
            .saturating_sub(scope.assignments.len() + usize::from(region.params));
        for (k, a) in scope.assignments.iter().enumerate() {
            let s = self.units[sr.unit as usize].sym(a.name);
            let loc = Loc {
                unit: sr.unit,
                span: a.loc.span,
            };
            let slot = self.regions[ctx.region as usize]
                .binds
                .get(base + k)
                .copied()
                .unwrap_or(crate::resolve::NO_SLOT);
            let bound = match slot {
                crate::resolve::NO_SLOT => ctx.vars.borrow().get(s).is_some(),
                i => ctx.has_slot(i),
            };
            if ast.is_literal(a.expr) && bound {
                let t = format!(
                    "Parameter {} is overwritten with a literal",
                    self.quote_sym(s)
                );
                self.warn(loc, DiagCode::Overwrite, t);
                // Printed before `ScopeContext::init`'s try block, so it
                // stops without an "assignment to" trace.
                self.check_hard()?;
            }
            match self.eval(sr.unit, a.expr, ctx) {
                Ok(v) => self.set_bound(ctx, base + k, s, v),
                Err(mut e) => {
                    let q = self.quote_sym(s);
                    match a.overwrite {
                        None => self.trace(&mut e, loc, format!("assignment to {q}").into_bytes()),
                        Some(ow) => {
                            let t = format!(
                                "overwritten assignment to {q} (this is where the assignment is evaluated)"
                            );
                            if let Some(p) = e.log(self.pending_trace(loc, t)) {
                                self.emit_pending(p);
                            }
                            let ow = Loc {
                                unit: sr.unit,
                                span: ow.span,
                            };
                            let t = format!("overwriting assignment to {q}");
                            self.trace(&mut e, ow, t.into_bytes());
                        }
                    }
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    pub fn pending_trace(&self, loc: Loc, text: String) -> crate::message::Pending {
        crate::message::Pending {
            severity: lang::diag::Severity::Trace,
            code: DiagCode::Trace,
            text: text.into_bytes(),
            loc: Some(loc),
        }
    }

    /// `LocalScope::instantiateModules`: instantiate a scope's modules (or
    /// the ones at `indices`) into `out`.
    pub fn instantiate_scope(
        &mut self,
        sr: ScopeRef,
        ctx: &Rc<Ctx>,
        out: &mut Vec<Node>,
        indices: Option<&[usize]>,
    ) -> R<()> {
        let n = self.scope(sr).instantiations.len();
        match indices {
            None => {
                for i in 0..n {
                    if let Some(node) = self.instantiate(sr, i, ctx)? {
                        out.push(node);
                    }
                }
            }
            Some(ix) => {
                for &i in ix {
                    if let Some(node) = self.instantiate(sr, i, ctx)? {
                        out.push(node);
                    }
                }
            }
        }
        Ok(())
    }

    /// `Children::instantiate`: a new scope context for the children, whose
    /// assignments are evaluated each time.
    pub fn instantiate_children(
        &mut self,
        children: &Children,
        out: &mut Vec<Node>,
        indices: Option<&[usize]>,
    ) -> R<()> {
        let region = self.units[children.scope.unit as usize].res.scope_region
            [children.scope.scope as usize];
        let c = self.new_ctx(&children.ctx, CtxKind::Scope(children.scope), region);
        let mark = self.push(c.clone());
        let r = self
            .init_scope(&c, children.scope)
            .and_then(|_| self.instantiate_scope(children.scope, &c, out, indices));
        self.truncate(mark);
        r
    }

    pub fn origin(&self, sr: ScopeRef, i: usize) -> Box<Origin> {
        let inst = self.inst(sr, i);
        let unit = &self.units[sr.unit as usize];
        let line = unit
            .program
            .sources
            .get(inst.span.file)
            .line_of(inst.span.start);
        Box::new(Origin {
            name: unit.ast.name(inst.name).to_string(),
            unit: sr.unit,
            span: inst.span,
            line,
            tag_root: inst.tag_root,
            tag_highlight: inst.tag_highlight,
            tag_background: inst.tag_background,
        })
    }

    pub fn new_node(&mut self, kind: NodeKind, sr: ScopeRef, i: usize) -> Node {
        let index = self.next_node_index();
        Node {
            kind,
            children: Vec::new(),
            origin: Some(self.origin(sr, i)),
            index,
        }
    }

    pub fn inst_loc(&self, sr: ScopeRef, i: usize) -> Loc {
        Loc {
            unit: sr.unit,
            span: self.inst(sr, i).span,
        }
    }

    pub fn inst_name(&self, sr: ScopeRef, i: usize) -> Sym {
        self.units[sr.unit as usize].sym(self.inst(sr, i).name)
    }

    /// `ModuleInstantiation::evaluate`.
    pub fn instantiate(&mut self, sr: ScopeRef, i: usize, ctx: &Rc<Ctx>) -> R<Option<Node>> {
        self.check_interrupt()?;
        // Every nested statement holds frames of the frame budget (see
        // `crate::recursion`), builtin ones included: a module recursing
        // through `if`, `for` or `children()` nests those too, and each
        // becomes a level of the node tree that rendering walks later.
        self.frames += crate::recursion::STATEMENT_FRAMES;
        let r = self.instantiate_frame(sr, i, ctx);
        self.frames -= crate::recursion::STATEMENT_FRAMES;
        r
    }

    #[inline(always)]
    fn instantiate_frame(&mut self, sr: ScopeRef, i: usize, ctx: &Rc<Ctx>) -> R<Option<Node>> {
        let name = self.inst_name(sr, i);
        let loc = self.inst_loc(sr, i);
        let found = match self.inst_res(sr, i).0 {
            0 => {
                if !self.syms.is_config(name) {
                    self.stats.fallbacks += 1;
                }
                self.lookup_module(ctx, name, loc)?
            }
            r => self.find_module(sr.unit, r - 1, ctx, name, loc)?,
        };
        let Some(m) = found else {
            // "Ignoring unknown module" is printed by the lookup, before
            // `ModuleInstantiation::evaluate`'s try block: no trace.
            self.check_hard()?;
            return Ok(None);
        };
        let r = match m {
            // OpenSCAD checks the stack only for user modules, but a chain
            // of builtins can nest as deep as the user modules around it
            // (`children()` of `children()` of ...), so the frame budget
            // is checked here too, with a quarter more room so that a
            // recursive module still stops at its own call, with
            // OpenSCAD's message, rather than at an `if` inside it.
            // Natively the budget is unlimited, and this never fires.
            Instantiable::Builtin(b) => self.builtin_module(b, sr, i, ctx),
            Instantiable::User {
                ctx: dctx,
                unit,
                scope,
                index,
            } => self.user_module(&dctx, ScopeRef { unit, scope }, index, sr, i, ctx),
        };
        // A builtin module's own warnings (its argument checks) are raised
        // inside the try block, so they get the "called by" line.
        let r = r.and_then(|n| self.check_hard().map(|_| n));
        r.map_err(|mut e| {
            let t = format!("called by '{}'", self.name(name));
            self.trace(&mut e, loc, t.into_bytes());
            e
        })
    }

    /// The frame budget ran out at a builtin module (see `builtin_module`).
    /// Kept out of line: `instantiate` is on every level of a recursion,
    /// and its frame should stay small.
    #[cold]
    #[inline(never)]
    pub fn builtin_recursion(&mut self, sr: ScopeRef, i: usize) -> Box<crate::message::Unwind> {
        let (name, loc) = (self.inst_name(sr, i), self.inst_loc(sr, i));
        let t = format!("Recursion detected calling module '{}'", self.name(name));
        self.error(Some(loc), DiagCode::RecursionLimit, t);
        self.unwind(UnwindKind::Recursion)
    }

    /// `UserModule::instantiate`.
    fn user_module(
        &mut self,
        dctx: &Rc<Ctx>,
        def_scope: ScopeRef,
        index: u32,
        sr: ScopeRef,
        i: usize,
        ctx: &Rc<Ctx>,
    ) -> R<Option<Node>> {
        let mu = def_scope.unit;
        let def = &self.scope(def_scope).modules[index as usize];
        let def_loc = Loc {
            unit: mu,
            span: def.span,
        };
        let inst_name = self.inst_name(sr, i);
        if self.recursion_exhausted() {
            let t = format!(
                "Recursion detected calling module '{}'",
                self.name(inst_name)
            );
            self.error(Some(def_loc), DiagCode::RecursionLimit, t);
            return Err(self.unwind(UnwindKind::Recursion));
        }
        self.module_names.push(inst_name);
        let r = self.user_module_inner(dctx, def_scope, index, sr, i, ctx);
        self.module_names.pop();
        r
    }

    fn user_module_inner(
        &mut self,
        dctx: &Rc<Ctx>,
        def_scope: ScopeRef,
        index: u32,
        sr: ScopeRef,
        i: usize,
        ctx: &Rc<Ctx>,
    ) -> R<Option<Node>> {
        let mu = def_scope.unit;
        let def = &self.scope(def_scope).modules[index as usize];
        let body = ScopeRef {
            unit: mu,
            scope: self.units[mu as usize].scopes[def_scope.scope as usize].bodies[index as usize],
        };
        let inst = self.inst(sr, i);
        let loc = self.inst_loc(sr, i);
        let args = self.eval_args(sr.unit, &inst.args, ctx)?;
        let children = Children {
            scope: self.children_scope(sr, i),
            ctx: ctx.clone(),
        };
        let n_children = self.scope(children.scope).instantiations.len();
        let region = self.module_region(mu, def_scope.scope, index);
        let mctx = self.new_ctx(dctx, CtxKind::Module(body, children), region);
        let (sc, sp) = (self.k.children, self.k.parent_modules);
        // `$children` is the region's last binder.
        let last = self.regions[region as usize].binds.len().wrapping_sub(1);
        self.set_bound(&mctx, last, sc, Value::Number(n_children as f64));
        self.set_var(&mctx, sp, Value::Number(self.module_names.len() as f64));
        self.bind_module(args, loc, mu, &def.params, dctx, &mctx)?;
        let mark = self.push(mctx.clone());
        let r = (|| {
            self.init_scope(&mctx, body)?;
            let group = format!("module {}", self.units[mu as usize].ast.name(def.name));
            let mut node = self.new_node(NodeKind::Group { name: Some(group) }, sr, i);
            match self.instantiate_scope(body, &mctx, &mut node.children, None) {
                Ok(()) => Ok(Some(node)),
                Err(mut e) => {
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
            }
        })();
        self.truncate(mark);
        r
    }

    /// Bind a module call's arguments into its context. Out of line, so the
    /// bound frame is not part of the instantiation's stack frame, which
    /// every level of a recursive module holds.
    #[inline(never)]
    fn bind_module(
        &mut self,
        mut args: Vec<crate::call::ArgVal>,
        loc: Loc,
        mu: u32,
        params: &'a [lang::ast::Param],
        dctx: &Rc<Ctx>,
        mctx: &Ctx,
    ) -> R<()> {
        let frame = self.bind_user(&mut args, loc, mu, params, dctx, mctx.region)?;
        self.apply_frame(mctx, frame);
        Ok(())
    }

    /// `call of 'name(a = 1, b = "x")'` for a module's trace line.
    fn module_call_text(
        &mut self,
        mu: u32,
        def: &'a lang::ast::ModuleDef,
        mctx: &Rc<Ctx>,
    ) -> Vec<u8> {
        let ast = self.units[mu as usize].ast;
        let mut t = format!("call of '{}(", ast.name(def.name)).into_bytes();
        if !def.params.is_empty() {
            if self.recursion_exhausted() {
                t.extend_from_slice(b"...");
            } else {
                for (k, p) in def.params.iter().enumerate() {
                    if k > 0 {
                        t.extend_from_slice(b", ");
                    }
                    t.extend_from_slice(ast.name(p.name).as_bytes());
                    t.extend_from_slice(b" = ");
                    let s = self.units[mu as usize].sym(p.name);
                    let v = self.try_lookup(mctx, s).unwrap_or_default();
                    let start = t.len();
                    if self.write_quoted(&v, &mut t).is_err() {
                        t.truncate(start);
                        self.log_exhausted();
                        t.extend_from_slice(b"...");
                    }
                }
            }
        }
        t.extend_from_slice(b")'");
        t
    }
}
