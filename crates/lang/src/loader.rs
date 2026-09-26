//! Loading a program: lexing the main file and splicing in `include`d files.
//!
//! `include <f>` is textual in OpenSCAD: the scanner switches to `f`'s
//! tokens and returns to the including file at its end, so an include can
//! even sit in the middle of an expression. The loader reproduces that by
//! building one token stream in which each include directive (trivia to the
//! parser) is followed by the included file's tokens. `use <f>` is resolved
//! here too, because OpenSCAD resolves it in the scanner and warns there.
//!
//! File access goes through [`FileSystem`] so the WASM build and tests can
//! supply their own files.

use std::io;
use std::path::{Component, Path, PathBuf};

use crate::diag::{DiagCode, Diagnostic, Severity};
use crate::source::{FileId, SourceMap, Span};
use crate::syntax::SyntaxKind;
use crate::syntax::lexer::{Token, directive_path, lex};

/// The file operations include resolution needs.
pub trait FileSystem {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn exists(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    /// Resolve symlinks and `..`; `None` if the path does not exist.
    fn canonicalize(&self, path: &Path) -> Option<PathBuf>;
}

/// The real file system.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdFs;

impl FileSystem for StdFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }
    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        path.canonicalize().ok()
    }
}

/// Library directories searched after the including file's directory, in
/// order (OpenSCAD's `librarypath`).
#[derive(Debug, Clone, Default)]
pub struct LibraryPath(pub Vec<PathBuf>);

impl LibraryPath {
    /// `OPENSCADPATH` entries, then the per-user library directory, as
    /// `parser_init()` in parsersettings.cc builds it.
    pub fn from_env() -> Self {
        let mut dirs = Vec::new();
        let cwd = std::env::current_dir().unwrap_or_default();
        if let Some(paths) = std::env::var_os("OPENSCADPATH") {
            let sep = if cfg!(windows) { ';' } else { ':' };
            for p in paths.to_string_lossy().split(sep) {
                dirs.push(if p.is_empty() {
                    cwd.clone()
                } else {
                    cwd.join(p)
                });
            }
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            let user = if cfg!(target_os = "macos") {
                home.join("Documents/OpenSCAD/libraries")
            } else {
                home.join(".local/share/OpenSCAD/libraries")
            };
            dirs.push(user);
        }
        Self(dirs)
    }
}

/// A `use` directive after resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseRef {
    /// Index of the directive in [`Loaded::tokens`].
    pub token: u32,
    /// The resolved absolute path, or the name as written if not found.
    pub path: String,
    pub found: bool,
}

#[derive(Debug, Default)]
pub struct Loaded {
    pub sources: SourceMap,
    /// The spliced token stream, trivia included.
    pub tokens: Vec<Token>,
    /// Scanner and include/use messages, `seq` set to their token order.
    pub diags: Vec<Diagnostic>,
    pub uses: Vec<UseRef>,
    /// `include` paths as written and what they resolved to.
    pub includes: Vec<(String, String)>,
}

/// Sequence key for a message emitted while scanning token `index`. Parser
/// messages at the same token use `+1`, so scanner messages come first.
pub fn seq_for_token(index: u32) -> u64 {
    (index as u64) << 2
}

/// Load `main_text` (already including any `-D` suffix) as the file
/// `main_path`, splicing includes.
pub fn load(
    main_path: PathBuf,
    main_text: Vec<u8>,
    fs: &dyn FileSystem,
    libs: &LibraryPath,
) -> Loaded {
    let mut l = Loader {
        fs,
        libs,
        out: Loaded::default(),
        open: Vec::new(),
        last_name: String::new(),
    };
    let main = l.out.sources.add(main_path, main_text);
    l.splice(main);
    l.out
}

/// Lex a single file without following includes (for formatters and
/// editors, which work on one file at a time).
pub fn load_single(path: PathBuf, text: Vec<u8>) -> Loaded {
    let mut out = Loaded::default();
    let id = out.sources.add(path, text);
    let lexed = lex(&out.sources.get(id).text, id);
    for d in lexed.diags {
        let f = out.sources.get(id);
        out.diags.push(
            Diagnostic::new(d.code, d.severity, d.message)
                .at(Span::new(id, d.start, d.end), f.line_of(d.line_at))
                .with_seq(seq_for_token(d.token)),
        );
    }
    out.tokens = lexed.tokens;
    out
}

struct Loader<'a> {
    fs: &'a dyn FileSystem,
    libs: &'a LibraryPath,
    out: Loaded,
    /// Full names of the included files currently being read, to stop
    /// circular includes (OpenSCAD's `openfilenames`).
    open: Vec<String>,
    /// The scanner's global `filename`, which survives between directives:
    /// an empty `use <>` reuses the previous directive's name.
    last_name: String,
}

impl Loader<'_> {
    fn splice(&mut self, file: FileId) {
        let lexed = lex(&self.out.sources.get(file).text, file);
        let mut diags = lexed.diags.into_iter().peekable();
        for (k, tok) in lexed.tokens.into_iter().enumerate() {
            let global = self.out.tokens.len() as u32;
            while let Some(d) = diags.next_if(|d| d.token as usize == k) {
                let line = self.out.sources.get(file).line_of(d.line_at);
                self.out.diags.push(
                    Diagnostic::new(d.code, d.severity, d.message)
                        .at(Span::new(file, d.start, d.end), line)
                        .with_seq(seq_for_token(global)),
                );
            }
            self.out.tokens.push(tok);
            match tok.kind {
                SyntaxKind::IncludeDirective => self.include(file, tok, global),
                SyntaxKind::UseDirective => self.use_(file, tok, global),
                _ => {}
            }
        }
    }

    /// Where OpenSCAD reports directive messages: the keyword's line, moved
    /// on by newlines inside the brackets.
    fn directive_line(&self, file: FileId, tok: Token) -> u32 {
        let f = self.out.sources.get(file);
        let text = f.slice(tok.start, tok.end());
        let lt = text.iter().position(|&b| b == b'<').unwrap_or(0);
        let at = match text[lt..].iter().rposition(|&b| b == b'\n') {
            Some(p) => tok.start + (lt + p + 1) as u32,
            None => tok.start,
        };
        f.line_of(at)
    }

    fn warn(&mut self, code: DiagCode, message: String, file: FileId, tok: Token, global: u32) {
        let line = self.directive_line(file, tok);
        self.out.diags.push(
            Diagnostic::new(code, Severity::Warning, message)
                .at(Span::new(file, tok.start, tok.end()), line)
                .with_seq(seq_for_token(global)),
        );
    }

    fn include(&mut self, file: FileId, tok: Token, global: u32) {
        let parts = {
            let f = self.out.sources.get(file);
            directive_path(f.slice(tok.start, tok.end()), true)
        };
        let dir = parts.dir.unwrap_or_default();
        let name = parts.name.unwrap_or_default();
        self.last_name = name.clone();
        let local = join_generic(&dir, &name);
        let source_dir = parent(self.out.sources.path(file));
        let Some(full) = self.find_valid_path(&source_dir, Path::new(&local)) else {
            self.out.includes.push((local.clone(), local.clone()));
            self.warn(
                DiagCode::IncludeNotFound,
                format!("Can't find include file '{local}'."),
                file,
                tok,
                global,
            );
            return;
        };
        let full_name = generic(&full);
        self.out.includes.push((local.clone(), full_name.clone()));
        let Ok(text) = self.fs.read(&full) else {
            self.warn(
                DiagCode::IncludeNotFound,
                format!("Can't open include file '{local}'."),
                file,
                tok,
                global,
            );
            return;
        };
        self.last_name.clear();
        let id = self.out.sources.add(full, text);
        self.open.push(full_name);
        self.splice(id);
        self.open.pop();
    }

    fn use_(&mut self, file: FileId, tok: Token, global: u32) {
        let parts = {
            let f = self.out.sources.get(file);
            directive_path(f.slice(tok.start, tok.end()), false)
        };
        if let Some(name) = parts.name {
            self.last_name = name;
        }
        let name = self.last_name.clone();
        let source_dir = parent(self.out.sources.path(file));
        match self.find_valid_path(&source_dir, Path::new(&name)) {
            Some(full) => self.out.uses.push(UseRef {
                token: global,
                path: generic(&full),
                found: true,
            }),
            None => {
                self.warn(
                    DiagCode::LibraryNotFound,
                    format!("Can't open library '{name}'."),
                    file,
                    tok,
                    global,
                );
                self.out.uses.push(UseRef {
                    token: global,
                    path: name,
                    found: false,
                });
            }
        }
    }

    fn find_valid_path(&self, source_dir: &Path, local: &Path) -> Option<PathBuf> {
        find_valid_path(self.fs, self.libs, source_dir, local, &self.open)
    }
}

/// `find_valid_path` in parsersettings.cc: `local` relative to
/// `source_dir`, then in each library directory. A file named in `open`
/// (an include currently being read) is refused, which stops circular
/// includes.
pub fn find_valid_path(
    fs: &dyn FileSystem,
    libs: &LibraryPath,
    source_dir: &Path,
    local: &Path,
    open: &[String],
) -> Option<PathBuf> {
    let check_valid = |p: &Path| -> bool {
        if p.as_os_str().is_empty() || p.parent().is_none_or(|q| q.as_os_str().is_empty()) {
            return false;
        }
        if !fs.exists(p) || fs.is_dir(p) {
            return false;
        }
        !open.contains(&generic(p))
    };
    if local.is_absolute() {
        return check_valid(local).then(|| {
            fs.canonicalize(local)
                .unwrap_or_else(|| local.to_path_buf())
        });
    }
    let mut p = join_path(source_dir, local);
    if fs.exists(&p)
        && let Some(c) = fs.canonicalize(&p)
    {
        p = c;
    }
    if check_valid(&p) {
        return Some(p);
    }
    for dir in &libs.0 {
        let candidate = join_path(dir, local);
        if fs.exists(&candidate) && !fs.is_dir(&candidate) {
            return check_valid(&candidate).then_some(candidate);
        }
    }
    None
}

fn parent(p: &Path) -> PathBuf {
    p.parent().map(Path::to_path_buf).unwrap_or_default()
}

/// `fs::path(a) / b` for the relative, slash-separated pieces OpenSCAD
/// builds include paths from.
fn join_generic(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// `dir / local`, keeping a trailing slash of `local` (so `test/` names a
/// directory and fails validation, as in OpenSCAD).
fn join_path(dir: &Path, local: &Path) -> PathBuf {
    let s = local.to_string_lossy();
    let mut p = dir.to_path_buf();
    if s.is_empty() {
        p.push("");
        return p;
    }
    p.push(local);
    if s.ends_with('/') {
        p.push("");
    }
    p
}

pub(crate) fn generic(p: &Path) -> String {
    let s = p.to_string_lossy();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}

/// Lexically normalise a path (drop `.`; keep `..`), for display.
pub fn normalize(p: &Path) -> PathBuf {
    p.components().filter(|c| *c != Component::CurDir).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// In-memory files for tests; directories are implied by file paths.
    struct MemFs(HashMap<PathBuf, Vec<u8>>);

    impl FileSystem for MemFs {
        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            self.0
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }
        fn exists(&self, path: &Path) -> bool {
            self.0.contains_key(path) || self.is_dir(path)
        }
        fn is_dir(&self, path: &Path) -> bool {
            let s = path.to_string_lossy();
            let s = s.trim_end_matches('/');
            self.0
                .keys()
                .any(|k| k.to_string_lossy().starts_with(&format!("{s}/")))
        }
        fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
            self.exists(path)
                .then(|| PathBuf::from(path.to_string_lossy().trim_end_matches('/')))
        }
    }

    fn memfs(files: &[(&str, &str)]) -> MemFs {
        MemFs(
            files
                .iter()
                .map(|(p, t)| (PathBuf::from(p), t.as_bytes().to_vec()))
                .collect(),
        )
    }

    #[test]
    fn splices_includes_in_place() {
        let fs = memfs(&[("/p/sub/a.scad", "b = 2;"), ("/lib/l.scad", "c = 3;")]);
        let libs = LibraryPath(vec!["/lib".into()]);
        let l = load(
            "/p/main.scad".into(),
            b"a = 1; include <sub/a.scad> include <l.scad>".to_vec(),
            &fs,
            &libs,
        );
        let kinds: Vec<_> = l
            .tokens
            .iter()
            .filter(|t| !t.kind.is_trivia())
            .map(|t| (t.kind, t.file))
            .collect();
        assert_eq!(kinds.len(), 12);
        assert_eq!(kinds[4].1, FileId(1));
        assert_eq!(kinds[8].1, FileId(2));
        assert!(l.diags.is_empty(), "{:?}", l.diags);
    }

    #[test]
    fn missing_and_circular_includes_warn() {
        let fs = memfs(&[("/p/self.scad", "include <self.scad>\nx = 1;")]);
        let l = load(
            "/p/main.scad".into(),
            b"include <self.scad>\ninclude <nope/x.scad>".to_vec(),
            &fs,
            &LibraryPath::default(),
        );
        let msgs: Vec<_> = l
            .diags
            .iter()
            .map(|d| (d.message.as_str(), d.line))
            .collect();
        assert_eq!(
            msgs,
            [
                ("Can't find include file 'self.scad'.", 1),
                ("Can't find include file 'nope/x.scad'.", 2)
            ]
        );
    }

    #[test]
    fn empty_use_reuses_previous_name() {
        let fs = memfs(&[("/p/a.scad", "")]);
        let l = load(
            "/p/m.scad".into(),
            b"use <>\ninclude <q/>\nuse <a.scad>\nuse <>".to_vec(),
            &fs,
            &LibraryPath::default(),
        );
        let uses: Vec<_> = l.uses.iter().map(|u| (u.path.as_str(), u.found)).collect();
        assert_eq!(
            uses,
            [("", false), ("/p/a.scad", true), ("/p/a.scad", true)]
        );
    }
}
