//! `scad.toml`, the project manifest:
//!
//! ```toml
//! [package]
//! name = "sky-view-shed"
//!
//! [dependencies]
//! BOSL2 = { git = "https://github.com/BelfrySCAD/BOSL2", tag = "v2.0.716" }
//! threads = { git = "https://github.com/rcolyer/threads-scad", rev = "6e3d2b4", name = "threads-scad" }
//! parts = { path = "../shared-parts" }
//!
//! [paths]
//! libraries = ["lib"]
//! ```
//!
//! A dependency is a git source (with at most one of `tag`, `rev` and
//! `branch`; none means the remote's default branch) or a `path`; an
//! `include_root` names the sub-folder that holds the library's files,
//! and `name` the folder name `include <Name/…>` uses (the key by
//! default). `[paths] libraries` (option B of the proposal) adds
//! project-relative directories to the search path, for libraries the
//! project keeps itself (a git submodule, a vendored folder).
//!
//! `add` and `remove` edit the text line by line ([`set_dependency`],
//! [`remove_dependency`]), so the rest of the file (comments, order,
//! other tables) is kept as written.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Context, Error, Result, bail};

/// The manifest's file name.
pub const MANIFEST: &str = "scad.toml";
/// The lockfile's file name, next to the manifest.
pub const LOCKFILE: &str = "scad.lock";
/// Where a project's dependencies are linked, under its root.
pub const LIBRARIES: &str = ".scad/libraries";

/// A parsed manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The directory holding `scad.toml`.
    pub root: PathBuf,
    /// `[package] name`, if given.
    pub name: Option<String>,
    /// By key, sorted.
    pub deps: Vec<Dep>,
    /// `[paths] libraries`, as written (relative to the root).
    pub paths: Vec<String>,
}

/// One `[dependencies]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dep {
    /// The key: the dependency's identity in the graph (one version per
    /// key).
    pub key: String,
    /// The folder name in `.scad/libraries` (`name`, or the key).
    pub folder: String,
    pub source: Source,
    /// The sub-folder that is the library's root (`include_root`), `/`
    /// separated; None for the repository's root.
    pub include_root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Git {
        url: String,
        reference: GitRef,
    },
    /// As written, relative to the manifest's directory (or absolute).
    Path(String),
}

/// Which revision of a git source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GitRef {
    Tag(String),
    Rev(String),
    Branch(String),
    /// The remote's default branch.
    Default,
}

impl GitRef {
    /// `tag = "v1"`, as the manifest and the lockfile write it; empty for
    /// the default branch.
    pub fn toml(&self) -> String {
        match self {
            GitRef::Tag(t) => format!("tag = {}", quote(t)),
            GitRef::Rev(r) => format!("rev = {}", quote(r)),
            GitRef::Branch(b) => format!("branch = {}", quote(b)),
            GitRef::Default => String::new(),
        }
    }

    /// For messages: `tag v1`, `rev abc123`, `branch main`, `the default branch`.
    pub fn describe(&self) -> String {
        match self {
            GitRef::Tag(t) => format!("tag {t}"),
            GitRef::Rev(r) => format!("rev {r}"),
            GitRef::Branch(b) => format!("branch {b}"),
            GitRef::Default => "the default branch".into(),
        }
    }

    /// Whether the revision it names can move upstream (a branch), so a
    /// fetch without a lock entry asks the remote again.
    pub fn moves(&self) -> bool {
        matches!(self, GitRef::Branch(_) | GitRef::Default)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    package: Option<RawPackage>,
    #[serde(default)]
    dependencies: BTreeMap<String, RawDep>,
    paths: Option<RawPaths>,
}

/// Other tools may put more in `[package]`; only the name is read.
#[derive(Deserialize)]
struct RawPackage {
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPaths {
    #[serde(default)]
    libraries: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDep {
    git: Option<String>,
    path: Option<String>,
    tag: Option<String>,
    rev: Option<String>,
    branch: Option<String>,
    include_root: Option<String>,
    name: Option<String>,
}

impl Manifest {
    /// Read `<root>/scad.toml`.
    pub fn read(root: &Path) -> Result<Manifest> {
        let path = root.join(MANIFEST);
        let text = std::fs::read_to_string(&path).at("read", &path)?;
        Manifest::parse(root, &text).map_err(|e| Error(format!("{}: {e}", path.display())))
    }

    /// Parse a manifest's text; `root` is its directory.
    pub fn parse(root: &Path, text: &str) -> Result<Manifest> {
        let raw: Raw = basic_toml::from_str(text).map_err(|e| Error(e.to_string()))?;
        let mut deps = Vec::new();
        for (key, d) in raw.dependencies {
            deps.push(dep(&key, d)?);
        }
        let mut folders: BTreeMap<&str, &str> = BTreeMap::new();
        for d in &deps {
            if let Some(other) = folders.insert(&d.folder, &d.key) {
                bail!(
                    "dependencies `{other}` and `{}` both use the folder name `{}`; give one a different `name`",
                    d.key,
                    d.folder
                );
            }
        }
        let paths = raw.paths.map(|p| p.libraries).unwrap_or_default();
        for p in &paths {
            if p.is_empty() {
                bail!("[paths] libraries: an entry is empty");
            }
        }
        Ok(Manifest {
            root: root.to_path_buf(),
            name: raw.package.and_then(|p| p.name),
            deps,
            paths,
        })
    }

    /// The name the project goes by in messages and `tree`:
    /// `[package] name`, else its directory's name.
    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            self.root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "project".into())
        })
    }
}

fn dep(key: &str, d: RawDep) -> Result<Dep> {
    check_folder(key).map_err(|e| Error(format!("dependency `{key}`: {e}")))?;
    let refs = [&d.tag, &d.rev, &d.branch]
        .iter()
        .filter(|r| r.is_some())
        .count();
    let source = match (d.git, d.path) {
        (Some(_), Some(_)) => bail!("dependency `{key}`: give `git` or `path`, not both"),
        (None, None) => bail!(
            "dependency `{key}`: give a source, `git = \"URL\"` (with `tag`, `rev` or `branch`) or `path = \"DIR\"`"
        ),
        (Some(url), None) => {
            if refs > 1 {
                bail!("dependency `{key}`: give at most one of `tag`, `rev` and `branch`");
            }
            if url.is_empty() {
                bail!("dependency `{key}`: `git` is empty");
            }
            let reference = match (d.tag, d.rev, d.branch) {
                (Some(t), _, _) => GitRef::Tag(t),
                (_, Some(r), _) => {
                    if r.len() < 4 || !r.bytes().all(|b| b.is_ascii_hexdigit()) {
                        bail!(
                            "dependency `{key}`: `rev` must be a commit id (at least 4 hex digits); use `tag` or `branch` for a name"
                        );
                    }
                    GitRef::Rev(r.to_ascii_lowercase())
                }
                (_, _, Some(b)) => GitRef::Branch(b),
                _ => GitRef::Default,
            };
            Source::Git { url, reference }
        }
        (None, Some(p)) => {
            if refs > 0 {
                bail!("dependency `{key}`: `tag`, `rev` and `branch` are for git sources");
            }
            Source::Path(p)
        }
    };
    let include_root = match d.include_root {
        None => None,
        Some(r) => normal_root(&r).map_err(|e| Error(format!("dependency `{key}`: {e}")))?,
    };
    let folder = d.name.unwrap_or_else(|| key.to_string());
    check_folder(&folder).map_err(|e| Error(format!("dependency `{key}`: `name`: {e}")))?;
    Ok(Dep {
        key: key.to_string(),
        folder,
        source,
        include_root,
    })
}

/// A folder name usable as `include <Name/…>`, and safe as a name on
/// every platform: ASCII letters, digits, `.`, `_` and `-` only, no
/// leading `.` or `-`, no trailing `.`, and none of Windows' reserved
/// device names. The name comes from `scad.toml` and `scad.lock`, which a
/// cloned project brings with it, and becomes a path under
/// `.scad/libraries` that a fetch replaces and removes: a separator or
/// `..` there would reach outside the project, a leading `-` reads as an
/// option to whatever is handed the path, and `NUL` or `COM1` open a
/// device on Windows instead of a folder.
pub fn check_folder(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(format!("'{name}' is not a folder name"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        || name.starts_with(['.', '-'])
        || name.ends_with('.')
    {
        return Err(format!(
            "'{name}' is not a usable folder name (letters, digits, '.', '_' and '-' only, not starting with '.' or '-')"
        ));
    }
    // Windows reserves these whatever the extension (`CON.scad` too).
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Err(format!(
            "'{name}' is a device name on Windows, not a usable folder name"
        ));
    }
    Ok(())
}

/// Why a git URL that a dependency's own manifest declares (not the
/// project's) is refused, if it is: a local path, `file://`, or a
/// `<transport>::<address>` remote helper (`fd::`, `ext::`, which runs a
/// command). A cloned project's dependencies are someone else's
/// manifests, so they may only name remotes; the project's own
/// `scad.toml` may name anything, as git itself allows.
pub fn transitive_url_problem(url: &str) -> Option<&'static str> {
    let u = url.trim();
    if u.starts_with('-') {
        return Some("it starts with '-'");
    }
    if let Some(i) = u.find("://") {
        return u[..i]
            .eq_ignore_ascii_case("file")
            .then_some("it is a file:// URL");
    }
    if let Some(i) = u.find("::")
        && u[..i]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'+' | b'.'))
    {
        return Some("it names a git remote helper (`transport::address`)");
    }
    // git reads anything without a `:` before the first `/` (and a
    // Windows drive, `C:\…`) as a local path; `host:path` is scp-like
    // ssh.
    match u.find(':') {
        None => Some("it is a local path"),
        Some(i) if u[..i].contains(['/', '\\']) || i == 1 => Some("it is a local path"),
        Some(_) => None,
    }
}

/// `include_root` as a relative `/`-separated path, None for the root.
pub(crate) fn normal_root(r: &str) -> std::result::Result<Option<String>, String> {
    let parts: Vec<&str> = r
        .split(['/', '\\'])
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if r.starts_with('/') || r.contains(':') || parts.contains(&"..") {
        return Err(format!(
            "`include_root` '{r}' must be a folder inside the repository"
        ));
    }
    Ok((!parts.is_empty()).then(|| parts.join("/")))
}

/// A TOML basic string.
pub fn quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => o.push_str(&format!("\\u{:04X}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// A key as TOML writes it: bare when it can be.
pub fn toml_key(k: &str) -> String {
    if !k.is_empty()
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        k.to_string()
    } else {
        quote(k)
    }
}

impl Dep {
    /// The entry's inline table, as `add` writes it.
    pub fn inline(&self) -> String {
        let mut parts = Vec::new();
        match &self.source {
            Source::Git { url, reference } => {
                parts.push(format!("git = {}", quote(url)));
                let r = reference.toml();
                if !r.is_empty() {
                    parts.push(r);
                }
            }
            Source::Path(p) => parts.push(format!("path = {}", quote(p))),
        }
        if let Some(r) = &self.include_root {
            parts.push(format!("include_root = {}", quote(r)));
        }
        if self.folder != self.key {
            parts.push(format!("name = {}", quote(&self.folder)));
        }
        format!("{{ {} }}", parts.join(", "))
    }
}

/// Whether `line` is a `[table]` (or `[[array]]`) header, and its name.
fn header(line: &str) -> Option<String> {
    let t = line.trim();
    let t = t.split('#').next().unwrap_or("").trim();
    let inner = t.strip_prefix('[')?.strip_suffix(']')?;
    let inner = inner
        .strip_prefix('[')
        .and_then(|i| i.strip_suffix(']'))
        .unwrap_or(inner);
    Some(
        inner
            .split('.')
            .map(|p| p.trim().trim_matches('"'))
            .collect::<Vec<_>>()
            .join("."),
    )
}

/// The key a `key = value` line assigns, unquoted.
fn line_key(line: &str) -> Option<String> {
    let t = line.trim_start();
    if t.starts_with('#') || t.is_empty() {
        return None;
    }
    let (k, _) = t.split_once('=')?;
    let k = k.trim();
    Some(k.trim_matches('"').trim_matches('\'').to_string())
}

/// `text` with dependency `dep` set (replaced where it is, else added at
/// the end of `[dependencies]`, which is created if missing). The result
/// is checked by parsing it.
pub fn set_dependency(text: &str, dep: &Dep) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();
    let entry = format!("{} = {}", toml_key(&dep.key), dep.inline());
    let mut out: Vec<String> = Vec::new();
    let sub_table = format!("dependencies.{}", dep.key);
    let mut table: Option<String> = None;
    let mut done = false;
    let mut deps_end: Option<usize> = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(h) = header(line) {
            if table.as_deref() == Some("dependencies") && deps_end.is_none() {
                deps_end = Some(last_content(&out));
            }
            if h == sub_table {
                // `[dependencies.KEY]` as a table of its own: drop it,
                // and write the entry inline in `[dependencies]`.
                i += 1;
                while i < lines.len() && header(lines[i]).is_none() {
                    i += 1;
                }
                continue;
            }
            table = Some(h);
        } else if table.as_deref() == Some("dependencies")
            && !done
            && line_key(line).as_deref() == Some(dep.key.as_str())
        {
            out.push(entry.clone());
            done = true;
            i += 1;
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }
    if table.as_deref() == Some("dependencies") && deps_end.is_none() {
        deps_end = Some(last_content(&out));
    }
    if !done {
        match deps_end {
            Some(at) => out.insert(at, entry),
            None => {
                if out.last().is_some_and(|l| !l.trim().is_empty()) {
                    out.push(String::new());
                }
                out.push("[dependencies]".into());
                out.push(entry);
            }
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    Manifest::parse(Path::new("."), &s)?;
    Ok(s)
}

/// The index after the last non-blank line of `out` (where an entry
/// appended to the table that `out` ends with goes).
fn last_content(out: &[String]) -> usize {
    out.iter()
        .rposition(|l| !l.trim().is_empty())
        .map_or(0, |i| i + 1)
}

/// `text` without dependency `key`, or None if it has none.
pub fn remove_dependency(text: &str, key: &str) -> Result<Option<String>> {
    let lines: Vec<&str> = text.lines().collect();
    let sub_table = format!("dependencies.{key}");
    let mut out: Vec<&str> = Vec::new();
    let mut table: Option<String> = None;
    let mut found = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(h) = header(line) {
            if h == sub_table {
                found = true;
                i += 1;
                while i < lines.len() && header(lines[i]).is_none() {
                    i += 1;
                }
                continue;
            }
            table = Some(h);
        } else if table.as_deref() == Some("dependencies") && line_key(line).as_deref() == Some(key)
        {
            found = true;
            i += 1;
            continue;
        }
        out.push(line);
        i += 1;
    }
    if !found {
        return Ok(None);
    }
    let mut s = out.join("\n");
    s.push('\n');
    Manifest::parse(Path::new("."), &s)?;
    Ok(Some(s))
}

/// A new manifest for a project called `name`.
pub fn new_manifest(name: &str) -> String {
    format!("[package]\nname = {}\n\n[dependencies]\n", quote(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Manifest> {
        Manifest::parse(Path::new("/p"), s)
    }

    #[test]
    fn reads_the_proposal_example() {
        let m = parse(
            r#"
[package]
name = "sky-view-shed"
version = "1.0"

[dependencies]
BOSL2       = { git = "https://github.com/BelfrySCAD/BOSL2", tag = "v2.0.716" }
FramingSCAD = { git = "https://github.com/the-ned-workshop/FramingSCAD", rev = "21645b1" }
threads     = { git = "https://github.com/rcolyer/threads-scad", rev = "abcd", include_root = "." }
local-parts = { path = "../shared-parts" }

[paths]
libraries = ["lib"]
"#,
        )
        .unwrap();
        assert_eq!(m.name.as_deref(), Some("sky-view-shed"));
        assert_eq!(m.deps.len(), 4);
        assert_eq!(m.deps[0].key, "BOSL2");
        assert_eq!(
            m.deps[0].source,
            Source::Git {
                url: "https://github.com/BelfrySCAD/BOSL2".into(),
                reference: GitRef::Tag("v2.0.716".into())
            }
        );
        assert_eq!(m.deps[3].include_root, None);
        assert_eq!(m.deps[2].source, Source::Path("../shared-parts".into()));
        assert_eq!(m.paths, vec!["lib"]);
    }

    #[test]
    fn refuses_what_it_cannot_resolve() {
        for (s, want) in [
            (
                "[dependencies]\nA = { git = \"u\", tag = \"x\", rev = \"abcd\" }",
                "at most one",
            ),
            ("[dependencies]\nA = { }", "give a source"),
            (
                "[dependencies]\nA = { path = \"p\", tag = \"x\" }",
                "git sources",
            ),
            (
                "[dependencies]\nA = { git = \"u\", rev = \"main\" }",
                "commit id",
            ),
            (
                "[dependencies]\nA = { git = \"u\", include_root = \"../x\" }",
                "inside the repository",
            ),
            (
                "[dependencies]\nA = { git = \"u\", gti = \"x\" }",
                "unknown field",
            ),
            (
                "[dependencies]\nA = { path = \"a\" }\nB = { path = \"b\", name = \"A\" }",
                "both use the folder name",
            ),
            ("[dependencies]\n\"a/b\" = { path = \"a\" }", "folder name"),
            ("[dependencies]\n\"a&b\" = { path = \"a\" }", "folder name"),
            ("[dependencies]\n\"-x\" = { path = \"a\" }", "folder name"),
            (
                "[dependencies]\nA = { path = \"a\", name = \"x%y\" }",
                "folder name",
            ),
            ("[dependencies]\nnul = { path = \"a\" }", "device name"),
            ("[dependencies]\nCOM1 = { path = \"a\" }", "device name"),
            (
                "[dependencies]\n\"CON.scad\" = { path = \"a\" }",
                "device name",
            ),
        ] {
            let e = parse(s).unwrap_err().0;
            assert!(e.contains(want), "{s}: {e}");
        }
    }

    #[test]
    fn folder_names_and_transitive_urls() {
        for ok in ["BOSL2", "threads-scad", "a.b_c", "COM", "CONSOLE", "LPT10"] {
            assert_eq!(check_folder(ok), Ok(()), "{ok}");
        }
        for bad in ["a b", "a^b", "x.", "\u{e9}t\u{e9}", "lpt3", "aux.txt"] {
            assert!(check_folder(bad).is_err(), "{bad}");
        }
        for remote in [
            "https://github.com/a/b",
            "ssh://git@host/a",
            "git@github.com:a/b.git",
            "host:path",
        ] {
            assert_eq!(transitive_url_problem(remote), None, "{remote}");
        }
        for local in [
            "/abs/repo",
            "../sibling",
            ".",
            "repo",
            "file:///r",
            "FILE://host/r",
            "C:\\r",
            "a/b:c",
            "fd::3",
            "ext::sh -c touch% x",
            "--upload-pack=x",
        ] {
            assert!(transitive_url_problem(local).is_some(), "{local}");
        }
    }

    #[test]
    fn add_and_remove_keep_the_rest() {
        let text = "# my project\n[package]\nname = \"p\"\n\n[dependencies]\nA = { path = \"a\" } # keep\n\n[paths]\nlibraries = [\"lib\"]\n";
        let b = Dep {
            key: "B".into(),
            folder: "B".into(),
            source: Source::Git {
                url: "file:///r".into(),
                reference: GitRef::Tag("v1".into()),
            },
            include_root: Some("src".into()),
        };
        let added = set_dependency(text, &b).unwrap();
        assert_eq!(
            added,
            "# my project\n[package]\nname = \"p\"\n\n[dependencies]\nA = { path = \"a\" } # keep\nB = { git = \"file:///r\", tag = \"v1\", include_root = \"src\" }\n\n[paths]\nlibraries = [\"lib\"]\n"
        );
        let mut b2 = b.clone();
        b2.source = Source::Path("x".into());
        b2.include_root = None;
        let replaced = set_dependency(&added, &b2).unwrap();
        assert!(replaced.contains("B = { path = \"x\" }\n"), "{replaced}");
        assert_eq!(remove_dependency(&replaced, "B").unwrap().unwrap(), text);
        assert_eq!(remove_dependency(text, "Z").unwrap(), None);
        // No table yet.
        let fresh = set_dependency("[package]\nname = \"p\"\n", &b2).unwrap();
        assert_eq!(
            fresh,
            "[package]\nname = \"p\"\n\n[dependencies]\nB = { path = \"x\" }\n"
        );
        // A table of its own becomes an inline entry.
        let sub = "[dependencies.B]\ngit = \"u\"\n\n[paths]\n";
        assert_eq!(
            set_dependency(sub, &b2).unwrap(),
            "[paths]\n\n[dependencies]\nB = { path = \"x\" }\n"
        );
    }
}
