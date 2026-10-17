//! Environment variables that have a neutral name and an older,
//! NeoSCAD-named alias (`SCAD_STORE` and `NEOSCAD_STORE`): the first one
//! set to a non-empty value wins, so an empty variable counts as unset.

use std::ffi::OsString;

/// The first of `names` the process environment sets to a non-empty
/// value.
pub(crate) fn var(names: &[&str]) -> Option<OsString> {
    lookup(names, |n| std::env::var_os(n))
}

/// [`var`] over any environment (`get` reads one variable).
pub(crate) fn lookup(names: &[&str], get: impl Fn(&str) -> Option<OsString>) -> Option<OsString> {
    names.iter().find_map(|n| get(n).filter(|v| !v.is_empty()))
}
