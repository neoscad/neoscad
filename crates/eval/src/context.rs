//! Evaluation contexts: where variables live.
//!
//! OpenSCAD has two kinds of variable lookup, and both are reproduced:
//!
//! - **lexical** names walk the `parent` chain of the context an expression
//!   is evaluated in (a function body's parent is the scope that defined the
//!   function, not its caller);
//! - **special** `$` names (except `$children`) are dynamically scoped: they
//!   are looked up in the evaluator's stack of live contexts, newest first
//!   (`EvaluationSession::try_lookup_special_variable`).
//!
//! A context holds both kinds in one small map; which kind a name is follows
//! from the name itself, so a context never needs two maps.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::sym::{FxBuild, Sym};
use crate::value::Value;

/// A lexical scope's identity: a unit (source file) and a scope within it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScopeRef {
    pub unit: u32,
    pub scope: u32,
}

/// The children of a module instantiation, with the context they are
/// instantiated in (`Children`).
#[derive(Clone, Debug)]
pub(crate) struct Children {
    pub scope: ScopeRef,
    pub ctx: Rc<Ctx>,
}

#[derive(Debug)]
pub(crate) enum CtxKind {
    /// Let, for, function bodies, argument frames.
    Plain,
    /// The builtin context at the bottom of every stack.
    Builtin,
    /// A statement scope: its functions and modules are visible from here.
    Scope(ScopeRef),
    /// A file's top-level scope; also searches the file's `use`d libraries.
    File(ScopeRef),
    /// A user module's body scope, with the children it was called with.
    Module(ScopeRef, Children),
}

/// Variables of one context: a vector with linear search while small (most
/// contexts hold a few parameters), indexed by a hash map beyond that.
#[derive(Debug, Default)]
pub(crate) struct Vars {
    items: Vec<(Sym, Value)>,
    index: Option<HashMap<Sym, u32, FxBuild>>,
    /// Whether any `$` variable is set, so dynamic lookups can skip this
    /// frame without searching it.
    pub has_config: bool,
}

const INDEX_THRESHOLD: usize = 12;

impl Vars {
    fn position(&self, s: Sym) -> Option<usize> {
        match &self.index {
            Some(m) => m.get(&s).map(|&i| i as usize),
            None => self.items.iter().position(|(k, _)| *k == s),
        }
    }

    pub fn get(&self, s: Sym) -> Option<&Value> {
        self.position(s).map(|i| &self.items[i].1)
    }

    /// Set a variable; returns whether it is new.
    pub fn set(&mut self, s: Sym, v: Value, config: bool) -> bool {
        if let Some(i) = self.position(s) {
            self.items[i].1 = v;
            return false;
        }
        self.has_config |= config;
        self.items.push((s, v));
        if let Some(m) = &mut self.index {
            m.insert(s, self.items.len() as u32 - 1);
        } else if self.items.len() > INDEX_THRESHOLD {
            self.index = Some(
                self.items
                    .iter()
                    .enumerate()
                    .map(|(i, (k, _))| (*k, i as u32))
                    .collect(),
            );
        }
        true
    }

    /// Move a variable's value out, leaving `undef` bound in its place.
    pub fn take(&mut self, s: Sym) -> Option<Value> {
        let i = self.position(s)?;
        Some(std::mem::take(&mut self.items[i].1))
    }

    pub fn iter(&self) -> impl Iterator<Item = &(Sym, Value)> {
        self.items.iter()
    }

    pub fn into_items(self) -> Vec<(Sym, Value)> {
        self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.index = None;
        self.has_config = false;
    }
}

#[derive(Debug)]
pub(crate) struct Ctx {
    /// Mutable only for C-style `for` comprehensions, which re-parent each
    /// iteration's context to the initial one (`LcForC::evaluate`).
    pub parent: RefCell<Option<Rc<Ctx>>>,
    pub kind: CtxKind,
    pub vars: RefCell<Vars>,
}

impl Ctx {
    pub fn new(parent: Option<Rc<Ctx>>, kind: CtxKind) -> Rc<Ctx> {
        Rc::new(Ctx {
            parent: RefCell::new(parent),
            kind,
            vars: RefCell::new(Vars::default()),
        })
    }

    pub fn child(parent: &Rc<Ctx>) -> Rc<Ctx> {
        Ctx::new(Some(parent.clone()), CtxKind::Plain)
    }

    pub fn parent(&self) -> Option<Rc<Ctx>> {
        self.parent.borrow().clone()
    }

    /// This frame's own variable, lexical or special.
    pub fn get_local(&self, s: Sym) -> Option<Value> {
        self.vars.borrow().get(s).cloned()
    }

    pub fn has_local(&self, s: Sym) -> bool {
        self.vars.borrow().get(s).is_some()
    }

    /// Walk the lexical chain for a non-`$` variable.
    pub fn lookup_lexical(&self, s: Sym) -> Option<Value> {
        if let Some(v) = self.vars.borrow().get(s) {
            return Some(v.clone());
        }
        match &*self.parent.borrow() {
            Some(p) => p.lookup_lexical(s),
            None => None,
        }
    }

    /// The context a lexical lookup of `s` from here finds it in, as an
    /// address to compare with (see `Evaluator::take_moved`).
    pub fn binder(&self, s: Sym) -> Option<*const Ctx> {
        if self.has_local(s) {
            return Some(std::ptr::from_ref(self));
        }
        match &*self.parent.borrow() {
            Some(p) => p.binder(s),
            None => None,
        }
    }

    /// The children of the innermost user module call, if any
    /// (`Context::user_module_children`).
    pub fn module_children(&self) -> Option<Children> {
        if let CtxKind::Module(_, c) = &self.kind {
            return Some(c.clone());
        }
        match &*self.parent.borrow() {
            Some(p) => p.module_children(),
            None => None,
        }
    }
}
