//! The examples every app offers (File > Examples on the desktop, the
//! picker on the web): one list and one copy of each file.
//!
//! The files and their manifest live in `web/examples/`, where the web
//! demo's bundle serves them from; the core embeds the same files at build
//! time, so the desktop apps show the same list without a second copy to
//! drift (and without reading the disk: this is a library crate). A test
//! checks that every file the manifest names is embedded and nothing else.

use serde::{Deserialize, Serialize};

use crate::CoreError;

const MANIFEST: &str = include_str!("../../../web/examples/manifest.json");

/// Each example file, by the name the manifest gives it.
const FILES: &[(&str, &str)] = &[
    ("CSG.scad", include_str!("../../../web/examples/CSG.scad")),
    ("sign.scad", include_str!("../../../web/examples/sign.scad")),
    ("GEB.scad", include_str!("../../../web/examples/GEB.scad")),
    (
        "example024.scad",
        include_str!("../../../web/examples/example024.scad"),
    ),
    (
        "helical-gear.scad",
        include_str!("../../../web/examples/helical-gear.scad"),
    ),
    (
        "box-lid.scad",
        include_str!("../../../web/examples/box-lid.scad"),
    ),
    (
        "threaded-ring.scad",
        include_str!("../../../web/examples/threaded-ring.scad"),
    ),
    (
        "gearbox.scad",
        include_str!("../../../web/examples/gearbox.scad"),
    ),
];

/// One example.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Example {
    pub id: String,
    pub title: String,
    /// The file's name (`CSG.scad`): what an untitled document opened from
    /// it may be called.
    pub file_name: String,
    /// Where it comes from ("OpenSCAD examples/Basics/CSG.scad").
    pub origin: String,
    /// SPDX licence id.
    pub license: String,
    /// A sentence to show with it (why it does not run at once), or empty.
    pub note: String,
    /// Libraries it `use`s or `include`s that are not bundled everywhere
    /// (BOSL2): a host without them can say so, or leave it out.
    pub libraries: Vec<String>,
    /// Whether it needs neoscad's `part()` extension turned on.
    pub parts: bool,
    /// Whether opening it runs it at once. Heavy examples (seconds of
    /// geometry) wait for Preview or Render, as the web demo's do.
    pub autorun: bool,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    default: Option<String>,
    examples: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    title: String,
    file: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    license: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    libraries: Vec<String>,
    #[serde(default)]
    parts: bool,
    #[serde(default)]
    heavy: bool,
    #[serde(default)]
    autorun: Option<bool>,
}

fn manifest() -> Manifest {
    // The manifest is embedded and checked by the tests, so this cannot
    // fail in a build that passed them; an empty list is the safe answer
    // if it somehow did.
    serde_json::from_str(MANIFEST).unwrap_or(Manifest {
        default: None,
        examples: Vec::new(),
    })
}

/// The examples, in menu order.
pub fn examples() -> Vec<Example> {
    manifest()
        .examples
        .into_iter()
        .filter(|e| FILES.iter().any(|(f, _)| *f == e.file))
        .map(|e| Example {
            autorun: e.autorun.unwrap_or(!e.heavy),
            id: e.id,
            title: e.title,
            file_name: e.file,
            origin: e.source,
            license: e.license,
            note: e.note,
            libraries: e.libraries,
            parts: e.parts,
        })
        .collect()
}

/// The example a new window starts with (the web demo's first page).
pub fn default_example() -> Option<String> {
    manifest().default
}

/// An example's text.
pub fn example_source(id: &str) -> Result<String, CoreError> {
    let file = manifest()
        .examples
        .into_iter()
        .find(|e| e.id == id)
        .map(|e| e.file)
        .ok_or_else(|| CoreError::InvalidArgument {
            message: format!("no example '{id}'"),
        })?;
    FILES
        .iter()
        .find(|(f, _)| *f == file)
        .map(|(_, text)| (*text).to_string())
        .ok_or_else(|| CoreError::Failed {
            message: format!("example '{id}' names '{file}', which is not embedded"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_manifest_file_is_embedded_and_nothing_else() {
        let m: Manifest = serde_json::from_str(MANIFEST).expect("the manifest parses");
        let named: Vec<&str> = m.examples.iter().map(|e| e.file.as_str()).collect();
        let embedded: Vec<&str> = FILES.iter().map(|(f, _)| *f).collect();
        assert_eq!(
            named, embedded,
            "the manifest and FILES list the same files"
        );
        assert_eq!(examples().len(), m.examples.len());
    }

    #[test]
    fn examples_have_sources_and_heavy_ones_wait() {
        for e in examples() {
            let text = example_source(&e.id).unwrap();
            assert!(!text.trim().is_empty(), "{}", e.id);
            assert!(!e.title.is_empty());
        }
        let by = |id: &str| examples().into_iter().find(|e| e.id == id).unwrap();
        assert!(by("csg").autorun);
        assert!(!by("gearbox").autorun, "heavy");
        assert!(!by("threaded-ring").autorun, "autorun: false");
        assert!(by("box-lid").parts);
        assert_eq!(by("gear").libraries, ["BOSL2"]);
        assert_eq!(default_example().as_deref(), Some("csg"));
        assert!(example_source("nope").is_err());
    }
}
