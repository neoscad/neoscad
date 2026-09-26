//! The customizer: parameter annotations from comments, and parameter sets.
//!
//! OpenSCAD runs [`annotate::collect_parameters`] on every program it
//! parses, which is why `.ast` dumps show `//Parameter("")` lines above
//! plain top-level assignments.

pub mod annotate;
pub mod comment;
pub mod json;
pub mod params;

pub use annotate::collect_parameters;
pub use params::{ParameterSet, Parameters, read_parameter_sets};
