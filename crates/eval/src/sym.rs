//! Interned identifiers shared by every file of one evaluation.
//!
//! Each parsed file ([`lang::Program`]) interns its own names, but a call
//! from the main file into a `use`d library passes named arguments across
//! that boundary, so the evaluator maps every file's names into one table.

use std::collections::HashMap;

/// An identifier, unique within one evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Sym(pub u32);

/// FxHash (rustc's): identifiers are short and trusted.
#[derive(Debug, Default, Clone, Copy)]
pub struct FxHasher(u64);

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl std::hash::Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(b)).wrapping_mul(SEED);
        }
    }
    fn write_u8(&mut self, i: u8) {
        self.write_u64(u64::from(i));
    }
    fn write_u32(&mut self, i: u32) {
        self.write_u64(u64::from(i));
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(SEED);
    }
    fn write_usize(&mut self, i: usize) {
        self.write_u64(i as u64);
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type FxBuild = std::hash::BuildHasherDefault<FxHasher>;

#[derive(Debug, Default)]
pub struct Syms {
    map: HashMap<Box<str>, Sym, FxBuild>,
    names: Vec<Box<str>>,
    config: Vec<bool>,
}

impl Syms {
    pub fn intern(&mut self, s: &str) -> Sym {
        if let Some(&n) = self.map.get(s) {
            return n;
        }
        let n = Sym(self.names.len() as u32);
        self.names.push(s.into());
        self.map.insert(s.into(), n);
        // `ContextFrame::is_config_variable`: `$children` is the one `$`
        // name that is lexically scoped.
        self.config.push(s.starts_with('$') && s != "$children");
        n
    }

    /// `s`'s symbol, if it has been interned.
    pub fn get(&self, s: &str) -> Option<Sym> {
        self.map.get(s).copied()
    }

    pub fn name(&self, s: Sym) -> &str {
        &self.names[s.0 as usize]
    }

    /// Whether `s` is a special (dynamically scoped) variable.
    pub fn is_config(&self, s: Sym) -> bool {
        self.config[s.0 as usize]
    }
}
