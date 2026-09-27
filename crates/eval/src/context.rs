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
//! A context keeps ordinary variables in slots numbered by its region (see
//! `resolve`), so a resolved reference reads one by index, and `$`
//! variables in a small map that the dynamic lookup scans.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::resolve::Region;
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
#[derive(Debug, Default, Clone)]
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
    /// The enclosing context. Fixed at creation, so lookups can walk the
    /// chain by reference (a C-style `for` makes each iteration's context
    /// afresh rather than re-parenting it; see `LcForC` in `eval.rs`).
    pub parent: Option<Rc<Ctx>>,
    pub kind: CtxKind,
    /// The [`Region`] this context is an instance of: which names its
    /// slots hold, and what a resolved reference matches it by.
    pub region: u32,
    /// The region's slot count: `slots` is sized on the first write, so a
    /// context whose slots are all replaced at once (a call's parameters)
    /// allocates them once. (Keeping up to three slots inline instead, to
    /// save that allocation, measured 2-4% slower: every context grows.)
    nslots: u32,
    /// Ordinary variables, by the region's slot numbers; `None` is not set
    /// (yet), and a lookup goes on outward as for an absent name.
    pub slots: RefCell<Vec<Option<Value>>>,
    /// `$` variables, and named arguments that are neither parameters nor
    /// otherwise bound in the region (see `resolve::Cand::Extra`).
    pub vars: RefCell<Vars>,
}

impl Ctx {
    pub fn new(parent: Option<Rc<Ctx>>, kind: CtxKind, region: u32, nslots: usize) -> Rc<Ctx> {
        Rc::new(Ctx {
            parent,
            kind,
            region,
            nslots: nslots as u32,
            slots: RefCell::new(Vec::new()),
            vars: RefCell::new(Vars::default()),
        })
    }

    /// A plain context of a one-variable `region` (a `for` variable), with
    /// the variable set.
    #[inline]
    pub fn with_slot(parent: &Rc<Ctx>, region: u32, slot: u32, v: Value) -> Rc<Ctx> {
        debug_assert_eq!(slot, 0);
        Rc::new(Ctx {
            parent: Some(parent.clone()),
            kind: CtxKind::Plain,
            region,
            nslots: 1,
            slots: RefCell::new(vec![Some(v)]),
            vars: RefCell::new(Vars::default()),
        })
    }

    pub fn parent(&self) -> Option<Rc<Ctx>> {
        self.parent.clone()
    }

    /// The value in slot `i`, if set.
    #[inline]
    pub fn slot(&self, i: u32) -> Option<Value> {
        self.slots.borrow().get(i as usize).and_then(Option::clone)
    }

    #[inline]
    pub fn has_slot(&self, i: u32) -> bool {
        self.slots
            .borrow()
            .get(i as usize)
            .is_some_and(Option::is_some)
    }

    #[inline]
    pub fn set_slot(&self, i: u32, v: Value) {
        let mut slots = self.slots.borrow_mut();
        if slots.is_empty() {
            slots.resize(self.nslots as usize, None);
        }
        slots[i as usize] = Some(v);
    }

    /// Replace the slots wholesale (a call's bound parameters), keeping any
    /// already set that `new` leaves unset.
    pub fn merge_slots(&self, new: Vec<Option<Value>>) {
        let mut slots = self.slots.borrow_mut();
        if slots.is_empty() {
            *slots = new;
            return;
        }
        for (i, v) in new.into_iter().enumerate() {
            if v.is_some() {
                slots[i] = v;
            }
        }
    }

    /// This frame's own variable, lexical or special, by name.
    pub fn get_local(&self, s: Sym, regions: &[Region]) -> Option<Value> {
        if let Some(i) = regions[self.region as usize].slot_of(s) {
            return self.slot(i);
        }
        self.vars.borrow().get(s).cloned()
    }

    pub fn has_local(&self, s: Sym, regions: &[Region]) -> bool {
        if let Some(i) = regions[self.region as usize].slot_of(s) {
            return self.has_slot(i);
        }
        self.vars.borrow().get(s).is_some()
    }

    /// Move a variable's value out, leaving `undef` bound in its place (so
    /// lookups still stop here).
    pub fn take_local(&self, s: Sym, regions: &[Region]) -> Option<Value> {
        if let Some(i) = regions[self.region as usize].slot_of(s) {
            let mut slots = self.slots.borrow_mut();
            let v = slots.get_mut(i as usize)?.as_mut()?;
            return Some(std::mem::take(v));
        }
        self.vars.borrow_mut().take(s)
    }

    /// Walk the lexical chain for a non-`$` variable by name: the fallback
    /// for references the resolver did not reach, and for cold paths.
    pub fn lookup_lexical(&self, s: Sym, regions: &[Region]) -> Option<Value> {
        let mut c = self;
        loop {
            if let Some(v) = c.get_local(s, regions) {
                return Some(v);
            }
            c = c.parent.as_deref()?;
        }
    }

    /// The context a lexical lookup of `s` from here finds it in, as an
    /// address to compare with (see `Evaluator::take_moved`).
    pub fn binder(&self, s: Sym, regions: &[Region]) -> Option<*const Ctx> {
        let mut c = self;
        loop {
            if c.has_local(s, regions) {
                return Some(std::ptr::from_ref(c));
            }
            c = c.parent.as_deref()?;
        }
    }

    /// The children of the innermost user module call, if any
    /// (`Context::user_module_children`).
    pub fn module_children(&self) -> Option<Children> {
        let mut c = self;
        loop {
            if let CtxKind::Module(_, ch) = &c.kind {
                return Some(ch.clone());
            }
            c = c.parent.as_deref()?;
        }
    }
}
