//! Module instantiation: scopes, user modules and `children()`.

use std::rc::Rc;

use lang::ast::{BinaryOp, ExprKind, Instantiation, Scope};
use lang::diag::DiagCode;

use crate::context::{Ctx, ScopeRef};
use crate::eval::Evaluator;
use crate::message::{Loc, R};
use crate::node::{Node, NodeKind, Origin};
use crate::sym::Sym;

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
            // `$v = $v * e` reads `$v` only to make the next `$v`, which a
            // recorded call may key on by shape (`callmemo::Dep::Shape`).
            let armed = self.cm.active.get()
                && self.syms.is_config(s)
                && matches!(ast.expr(a.expr).kind,
                    ExprKind::Binary(BinaryOp::Multiply, l, _)
                        if matches!(ast.expr(l).kind,
                            ExprKind::Var(n) if self.units[sr.unit as usize].sym(n) == s));
            if armed {
                self.cm.arm(s);
            }
            let r = self.eval(sr.unit, a.expr, ctx);
            if armed {
                self.cm.disarm();
            }
            match r {
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

    /// Bind a module call's arguments into its context. Out of line, so the
    /// bound frame is not part of the instantiation's stack frame, which
    /// every level of a recursive module holds.
    #[inline(never)]
    pub(crate) fn bind_module(
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
    pub(crate) fn module_call_text(
        &mut self,
        mu: u32,
        def: &'a lang::ast::ModuleDef,
        mctx: &Rc<Ctx>,
    ) -> Vec<u8> {
        let ast = self.units[mu as usize].ast;
        let mut t = format!("call of '{}(", ast.name(def.name)).into_bytes();
        if !def.params.is_empty() {
            // OpenSCAD writes `...` for the parameters while its stack
            // check fires (`print_trace` in `UserModule.cc`); a module
            // recursion ends at the counted limit here, so that is the
            // check. The native ones cannot fire while the heap driver
            // writes a module's trace (see `heap::begin_user`).
            if self.depth_exhausted() {
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
                    if let Err(e) = self.write_quoted(&v, &mut t) {
                        t.truncate(start);
                        self.print_failed(e, "a trace");
                        t.extend_from_slice(b"...");
                    }
                }
            }
        }
        t.extend_from_slice(b")'");
        t
    }
}
