//! NeoSCAD's own extensions to the OpenSCAD language, switched on with
//! `--enable` as OpenSCAD's experiments are (`docs/language-extensions.md`,
//! section 2).
//!
//! They are a separate set from [`crate::Features`] on purpose. `--enable
//! all` means "every OpenSCAD experiment" in OpenSCAD, and a program run
//! with OpenSCAD's own flags must behave as it does in OpenSCAD, so `all`
//! never turns an extension on: each one has to be named. Off, an
//! extension's names do not exist at all (not even as disabled builtins),
//! so a program that calls one gets OpenSCAD's own "unknown module" or
//! "unknown function" warning, and a program that defines one keeps its
//! own definition.
//!
//! The names are unprefixed (`sketch`, not `neoscad-sketch`), which only
//! works while OpenSCAD has no experiment of the same name; a test checks
//! them against the reference checkout's `src/Feature.cc`, so a reference
//! update that introduces a clash fails rather than silently changing
//! what `--enable sketch` means.

/// One NeoSCAD extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Extension {
    /// `part("name") { ... }`: named parts for `check`, `measure` and
    /// `snapshot` (see `node::NodeKind::Part`).
    Part,
    /// Constrained 2D sketches: `sketch() { ... }` and its vocabulary
    /// (see `crate::sketch`).
    Sketch,
    /// Geometry queries on a module's children: `anchor()` and
    /// `child_anchors()` (see `crate::query`).
    Query,
    /// STEP export with exact surfaces (`-o x.step`; `geom::exact`). It
    /// adds no names to the language: it only makes `.step` and `.stp`
    /// output suffixes, which stay OpenSCAD's "Invalid suffix" without it.
    Exact,
}

impl Extension {
    /// Every extension, in the order `--help` and the docs list them.
    pub const ALL: [Extension; 4] = [
        Extension::Part,
        Extension::Sketch,
        Extension::Query,
        Extension::Exact,
    ];

    /// The `--enable` name.
    pub fn name(self) -> &'static str {
        match self {
            Extension::Part => "part",
            Extension::Sketch => "sketch",
            Extension::Query => "query",
            Extension::Exact => "exact",
        }
    }

    pub fn from_name(name: &str) -> Option<Extension> {
        Extension::ALL.into_iter().find(|e| e.name() == name)
    }

    /// Whether the extension does anything yet. A host advertises only
    /// these (`serve`'s capabilities): a client that saw `sketch` listed
    /// would send sketches and get "unknown module" warnings back. The
    /// others are still accepted, silently, so a command line written for
    /// a later version is not rejected.
    pub fn implemented(self) -> bool {
        matches!(
            self,
            Extension::Part | Extension::Sketch | Extension::Query | Extension::Exact
        )
    }

    fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A set of [`Extension`]s: empty by default, so a program behaves as it
/// does in OpenSCAD.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Extensions(u8);

impl Extensions {
    pub const NONE: Extensions = Extensions(0);

    pub fn has(self, e: Extension) -> bool {
        self.0 & e.bit() != 0
    }

    pub fn with(self, e: Extension) -> Extensions {
        Extensions(self.0 | e.bit())
    }

    pub fn insert(&mut self, e: Extension) {
        self.0 |= e.bit();
    }

    /// Turn `e` on when `on` (a host's boolean switch, such as a request's
    /// `parts: true`); never turns one off.
    pub fn with_if(self, e: Extension, on: bool) -> Extensions {
        if on { self.with(e) } else { self }
    }

    pub fn union(self, o: Extensions) -> Extensions {
        Extensions(self.0 | o.0)
    }

    /// The set as a number, for fingerprints.
    pub fn bits(self) -> u8 {
        self.0
    }

    /// The extensions `--enable` names turn on. Only an exact name does:
    /// `all` is OpenSCAD's and turns none on, and unlike for OpenSCAD's
    /// features it does not end the list, so `--enable all --enable part`
    /// still has parts (as it always has). Other names (OpenSCAD's
    /// features, unknown ones) are skipped; they are [`crate::Features`]'
    /// business and its warnings'.
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> Extensions {
        let mut out = Extensions::NONE;
        for n in names {
            if let Some(e) = Extension::from_name(n.as_ref()) {
                out.insert(e);
            }
        }
        out
    }

    /// The extensions in the set, in [`Extension::ALL`]'s order.
    pub fn iter(self) -> impl Iterator<Item = Extension> {
        Extension::ALL.into_iter().filter(move |e| self.has(*e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Feature, Features};

    #[test]
    fn names_round_trip() {
        for e in Extension::ALL {
            assert_eq!(Extension::from_name(e.name()), Some(e));
        }
        let s = Extensions::from_names(&["textmetrics", "sketch", "nope", "all"]);
        assert_eq!(s.iter().collect::<Vec<_>>(), [Extension::Sketch]);
        assert!(
            Extensions::NONE
                .with_if(Extension::Part, true)
                .has(Extension::Part)
        );
        assert_eq!(
            Extensions::NONE.with_if(Extension::Part, false),
            Extensions::NONE
        );
    }

    /// `--enable all` is OpenSCAD's "every experiment": it must leave every
    /// extension off, or a program run with OpenSCAD's flags would see
    /// names OpenSCAD does not have (a file with its own `module part`
    /// still works, but one calling an undefined `sketch` would stop
    /// warning as OpenSCAD does).
    #[test]
    fn all_turns_on_no_extension() {
        assert_eq!(Extensions::from_names(&["all"]), Extensions::NONE);
        let all = Features::from_names(&["all"]);
        assert_eq!(all.iter().count(), Feature::ALL.len());
        // And the reverse: an extension name is no OpenSCAD feature, so
        // `Features` skips it.
        for e in Extension::ALL {
            assert_eq!(Feature::from_name(e.name()), None, "{}", e.name());
            assert_eq!(Features::from_names(&[e.name()]), Features::NONE);
        }
        // `all` does not end the list for extensions.
        assert!(Extensions::from_names(&["all", "part"]).has(Extension::Part));
    }
}
