//! `neoscad docs`: reference text for OpenSCAD's builtins (a checked-in
//! data file, `builtins.toml`, compiled into the binary) and for the
//! modules and functions of user and library code, from their leading
//! comment blocks (BOSL2's structured `// Module:` / `// Usage:` /
//! `// Arguments:` blocks are recognised and shown compactly).
//!
//! Output is terse by default, sized for an agent's context: a signature,
//! a summary, parameters one per line and an example.

mod toml;
pub mod user;

use std::sync::OnceLock;

pub use user::{Section, UserDoc, definitions};

const DATA: &str = include_str!("../builtins.toml");

/// What a name is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Module,
    Function,
    Variable,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Module => "module",
            Kind::Function => "function",
            Kind::Variable => "variable",
        }
    }
}

/// One parameter of a builtin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// The name, or names sharing a line (`d, d1, d2`).
    pub name: String,
    /// `number`, `bool`, `[x, y, z]`, ... (empty when not given).
    pub ty: String,
    pub default: Option<String>,
    pub doc: String,
}

/// A builtin's reference entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    pub signature: String,
    pub summary: String,
    pub params: Vec<Param>,
    /// What a function returns.
    pub returns: Option<String>,
    pub example: String,
    pub notes: Option<String>,
    /// The `--enable` name of the NeoSCAD extension this builtin belongs
    /// to (`part`, `sketch`, `query`); `None` for OpenSCAD's own. Every
    /// surface labels such an entry with [`extension_label`], so a reader
    /// can always tell an OpenSCAD builtin from a NeoSCAD one.
    pub extension: Option<String>,
}

/// The one label every surface (`neoscad docs`, MCP `docs`, LSP hover and
/// completion) shows for a NeoSCAD extension's builtin. It is built from
/// the entry's structured `extension` field rather than written into each
/// summary, so the wording cannot drift between entries or surfaces.
pub fn extension_label(extension: &str) -> String {
    format!("NeoSCAD extension (`--enable {extension}`); not in OpenSCAD")
}

impl Entry {
    /// [`extension_label`] for an extension's entry.
    pub fn extension_label(&self) -> Option<String> {
        self.extension.as_deref().map(extension_label)
    }
}

/// `name: type = default -- meaning`.
fn param(s: &str) -> Param {
    let (left, doc) = s.split_once(" -- ").unwrap_or((s, ""));
    let (name, rest) = left.split_once(": ").unwrap_or((left, ""));
    let (ty, default) = match rest.split_once(" = ") {
        Some((t, d)) => (t, Some(d.trim().to_string())),
        None => (rest, None),
    };
    Param {
        name: name.trim().to_string(),
        ty: ty.trim().to_string(),
        default,
        doc: doc.trim().to_string(),
    }
}

fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    for t in toml::parse(text)? {
        let kind = match t.kind.as_str() {
            "module" => Kind::Module,
            "function" => Kind::Function,
            "variable" => Kind::Variable,
            k => return Err(format!("line {}: unknown table [[{k}]]", t.line)),
        };
        for (k, _) in &t.values {
            if !matches!(
                k.as_str(),
                "name"
                    | "signature"
                    | "summary"
                    | "params"
                    | "returns"
                    | "example"
                    | "notes"
                    | "extension"
            ) {
                return Err(format!("line {}: unknown key '{k}'", t.line));
            }
        }
        let req = |k: &str| {
            t.str(k)
                .map(str::to_string)
                .ok_or_else(|| format!("line {}: [[{}]] needs '{k}'", t.line, t.kind))
        };
        out.push(Entry {
            kind,
            name: req("name")?,
            signature: req("signature")?,
            summary: req("summary")?,
            params: t
                .list("params")
                .unwrap_or(&[])
                .iter()
                .map(|p| param(p))
                .collect(),
            returns: t.str("returns").map(str::to_string),
            example: req("example")?,
            notes: t.str("notes").map(str::to_string),
            extension: t.str("extension").map(str::to_string),
        });
    }
    Ok(out)
}

/// Every builtin entry, in the file's order.
pub fn builtins() -> &'static [Entry] {
    static ENTRIES: OnceLock<Vec<Entry>> = OnceLock::new();
    // The file is part of the binary and a test parses it, so a failure
    // here is a build that should not have passed its tests.
    ENTRIES.get_or_init(|| parse(DATA).unwrap_or_else(|e| panic!("builtins.toml: {e}")))
}

/// The entries named `name` (a module and a function may share one:
/// `echo`, `assert`, `let`, `import`).
pub fn builtin(name: &str) -> Vec<&'static Entry> {
    builtins().iter().filter(|e| e.name == name).collect()
}

/// An entry as text: signature, summary, parameters, returns, example.
pub fn entry_text(e: &Entry) -> String {
    let mut out = format!("{} {}\n  {}\n", e.kind.name(), e.signature, e.summary);
    if let Some(l) = e.extension_label() {
        out.push_str(&format!("  {l}\n"));
    }
    if !e.params.is_empty() {
        let w = e.params.iter().map(|p| p.name.len()).max().unwrap_or(0);
        for p in &e.params {
            let mut line = format!("  {:w$}", p.name);
            if !p.ty.is_empty() {
                line.push_str(&format!("  {}", p.ty));
            }
            if let Some(d) = &p.default {
                line.push_str(&format!(" = {d}"));
            }
            if !p.doc.is_empty() {
                line.push_str(&format!("  -- {}", p.doc));
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    if let Some(r) = &e.returns {
        out.push_str(&format!("  returns: {r}\n"));
    }
    if let Some(n) = &e.notes {
        out.push_str(&format!("  note: {n}\n"));
    }
    out.push_str("  example:\n");
    for l in e.example.lines() {
        out.push_str(&format!("    {l}\n"));
    }
    out
}

/// A compact index of the builtins: names by kind.
pub fn index_text() -> String {
    let mut out = String::new();
    for kind in [Kind::Module, Kind::Function, Kind::Variable] {
        let names: Vec<&str> = builtins()
            .iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.name.as_str())
            .collect();
        out.push_str(&format!(
            "{}s ({}): {}\n",
            kind.name(),
            names.len(),
            names.join(" ")
        ));
    }
    out.push_str("neoscad docs NAME for one; --in FILE for a file's own modules and functions\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_parses() {
        let all = builtins();
        assert!(all.len() > 80);
        let cube = builtin("cube");
        assert_eq!(cube.len(), 1);
        assert_eq!(cube[0].params[0].name, "size");
        assert_eq!(cube[0].params[0].default.as_deref(), Some("1"));
        assert_eq!(builtin("echo").len(), 2);
        let t = entry_text(cube[0]);
        assert!(t.starts_with("module cube(size=1, center=false)\n"), "{t}");
        assert!(!t.contains("NeoSCAD extension"), "{t}");
        // An extension's entry carries the label on its second line.
        let part = builtin("part");
        assert_eq!(part[0].extension.as_deref(), Some("part"));
        let t = entry_text(part[0]);
        assert_eq!(
            t.lines().nth(2),
            Some("  NeoSCAD extension (`--enable part`); not in OpenSCAD"),
            "{t}"
        );
    }

    #[test]
    fn params_split() {
        let p = param("size: number | [x, y, z] = 1 -- edge length");
        assert_eq!(
            (
                p.name.as_str(),
                p.ty.as_str(),
                p.default.as_deref(),
                p.doc.as_str()
            ),
            ("size", "number | [x, y, z]", Some("1"), "edge length")
        );
        let p = param("x: number");
        assert_eq!(
            (p.ty.as_str(), p.default, p.doc.as_str()),
            ("number", None, "")
        );
    }
}
