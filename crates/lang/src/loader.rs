//! Loading a program: lexing the main file and splicing in `include`d files.
//!
//! `include <f>` is textual in OpenSCAD: the scanner switches to `f`'s
//! tokens and returns to the including file at its end, so an include can
//! even sit in the middle of an expression. The loader reproduces that by
//! building one token stream in which each include directive (trivia to the
//! parser) is followed by the included file's tokens. `use <f>` is resolved
//! here too, because OpenSCAD resolves it in the scanner and warns there.
//!
//! With a [`crate::fragment::FragmentCache`], an include may instead come
//! as the included file's parse, which the stream then leaves out: the
//! loader numbers everything (tokens, files, message order) as if its
//! tokens were there, and [`crate::fragment`] puts the parse in after the
//! includer's is done.
//!
//! File access goes through [`FileSystem`] so the WASM build and tests can
//! supply their own files.

use std::collections::HashSet;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::diag::{DiagCode, Diagnostic, Severity};
use crate::fragment::{self, Ctx, Dep, Fragment, FragmentKey, SpliceStats};
use crate::source::{FileId, SourceMap, Span};
use crate::syntax::SyntaxKind;
use crate::syntax::lexer::{LexDiag, Token, directive_path, lex};

/// Every file operation the pipeline performs: includes and `use`,
/// `import()`, `surface()`, `dxf_dim()`, fonts, parameter files and the
/// cache keys of imported files. Nothing below the command line touches the
/// real file system except through this trait, so the WASM build and tests
/// can supply their own files ([`crate::vfs`]).
pub trait FileSystem {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn exists(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    /// Resolve symlinks and `..`; `None` if the path does not exist.
    fn canonicalize(&self, path: &Path) -> Option<PathBuf>;
    /// A file's modification time and size, which key the cache of
    /// imported geometry and print as the `.csg` `timestamp`. `None` when
    /// the file does not exist or the file system cannot say; the default
    /// says nothing, which makes every version of a file share one cache
    /// key, so a long-lived host whose files change should implement it.
    fn metadata(&self, path: &Path) -> Option<Metadata> {
        let _ = path;
        None
    }
    /// The entries of a directory (full paths, in any order), for font
    /// directories. The default lists nothing.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let _ = path;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// What [`FileSystem::metadata`] knows about a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Metadata {
    /// Modification time in nanoseconds since the Unix epoch, if known.
    /// An in-memory file system may use any value that changes whenever
    /// the contents do (a write counter, say).
    pub modified: Option<i128>,
    /// Size in bytes.
    pub len: u64,
}

/// Options structs that hold a shared file system derive `Debug`; the file
/// system itself has nothing useful to show.
impl std::fmt::Debug for dyn FileSystem + Send + Sync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("dyn FileSystem")
    }
}

/// The real file system, for hosts (the `host` feature; see
/// [`crate::host`]).
#[cfg(feature = "host")]
pub use crate::host::StdFs;

/// Library directories searched after the including file's directory, in
/// order (OpenSCAD's `librarypath`). A host that bundles libraries appends
/// their directory last, as OpenSCAD appends `<resources>/libraries`. A
/// host reads the process environment's with `LibraryPath::from_env` (the
/// `host` feature).
#[derive(Debug, Clone, Default)]
pub struct LibraryPath(pub Vec<PathBuf>);

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

/// A file read and lexed once, kept by a [`LexCache`]. Its tokens are
/// tagged with `FileId(0)`; the loader retags them with the id the file
/// gets in each load.
#[derive(Debug)]
pub struct LexedFile {
    pub text: Arc<[u8]>,
    pub tokens: Vec<Token>,
    pub diags: Vec<LexDiag>,
}

/// Included files a host has already read and lexed, keyed by path and
/// validated by [`FileSystem::metadata`]. A long-lived host re-parses a
/// program after every edit to its main file; its includes (a library
/// such as BOSL2 is thousands of lines of includes) are part of that
/// parse, because OpenSCAD's includes are textual, but reading and lexing
/// them again is not needed while they are unchanged.
pub trait LexCache {
    /// `path`'s cached text and tokens, if they were cached for `meta`.
    fn get(&self, path: &Path, meta: &Metadata) -> Option<Arc<LexedFile>>;
    fn put(&self, path: &Path, meta: Metadata, file: Arc<LexedFile>);
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
    load_cached(main_path, main_text, fs, libs, None)
}

/// [`load`], taking included files from `cache` when they are unchanged
/// and adding the ones it reads. The result is the same as [`load`]'s.
pub fn load_cached(
    main_path: PathBuf,
    main_text: Vec<u8>,
    fs: &dyn FileSystem,
    libs: &LibraryPath,
    cache: Option<&dyn LexCache>,
) -> Loaded {
    let stats = std::cell::Cell::default();
    let main = main_path.clone();
    let ctx = Ctx {
        fs,
        libs,
        lex: cache,
        frags: None,
        main: &main,
        stats: &stats,
    };
    assemble(
        &ctx,
        main_path,
        main_text,
        None,
        Vec::new(),
        &HashSet::new(),
        true,
    )
    .loaded
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

/// An included file put in as its parse ([`Fragment`]) rather than its
/// tokens.
#[derive(Debug)]
pub(crate) struct Pending {
    /// Index in [`Loaded::tokens`] of the directive it follows.
    pub at: u32,
    /// The directive: its file and byte offset, which stay the same when
    /// an earlier include is spliced as tokens instead.
    pub key: (u32, u32),
    pub frag: Arc<Fragment>,
    /// The file id and token index the fragment's own ids start from in
    /// the program.
    pub file_base: u32,
    pub token_base: u32,
}

/// What [`assemble`] built: the token stream without the fragments'
/// tokens (they are parsed already), and what the fragments need.
#[derive(Debug)]
pub(crate) struct Assembly {
    /// Sources and diagnostics as the whole program has them, uses and
    /// diagnostics numbered by the program's tokens; `tokens` lacks the
    /// fragments' tokens.
    pub loaded: Loaded,
    pub inserts: Vec<Pending>,
    /// The scanner's `filename` at the end (see [`Loader::last_name`]).
    pub last_name: String,
    /// Everything the assembly depended on, for a [`Fragment`] of it.
    pub deps: Vec<Dep>,
    /// Whether every file read had metadata and could be read.
    pub cacheable: bool,
    pub counts: SpliceStats,
}

/// Build the token stream for `path` (text `text`, lexed as `pre` if
/// given): its tokens with each include's spliced in, except that an
/// include between top-level statements, when a fragment cache is given,
/// comes as its parse ([`Pending`]) unless `no_frags` or the directive is
/// in `textual`. `open` is the chain of included files `path` is read
/// inside (empty for a main file).
pub(crate) fn assemble(
    ctx: &Ctx<'_>,
    path: PathBuf,
    text: Vec<u8>,
    pre: Option<Arc<LexedFile>>,
    open: Vec<String>,
    textual: &HashSet<(u32, u32)>,
    no_frags: bool,
) -> Assembly {
    let mut l = Loader {
        ctx,
        out: Loaded::default(),
        open,
        last_name: String::new(),
        virt: 0,
        textual,
        no_frags: no_frags || ctx.frags.is_none(),
        inserts: Vec::new(),
        deps: Vec::new(),
        cacheable: true,
        counts: SpliceStats::default(),
    };
    let root = l.out.sources.add(path, text);
    l.splice(root, pre);
    Assembly {
        loaded: l.out,
        inserts: l.inserts,
        last_name: l.last_name,
        deps: l.deps,
        cacheable: l.cacheable,
        counts: l.counts,
    }
}

struct Loader<'a> {
    ctx: &'a Ctx<'a>,
    out: Loaded,
    /// Full names of the included files currently being read, to stop
    /// circular includes (OpenSCAD's `openfilenames`).
    open: Vec<String>,
    /// The scanner's global `filename`, which survives between directives:
    /// an empty `use <>` reuses the previous directive's name.
    last_name: String,
    /// Tokens in the program so far, fragments' included: the index the
    /// next token has in the program's stream (and in its syntax tree).
    virt: u32,
    textual: &'a HashSet<(u32, u32)>,
    no_frags: bool,
    inserts: Vec<Pending>,
    deps: Vec<Dep>,
    cacheable: bool,
    counts: SpliceStats,
}

impl Loader<'_> {
    /// Splice `file`'s tokens into the stream: `pre`'s when it was lexed
    /// before (retagged with `file`), otherwise lexed now.
    fn splice(&mut self, file: FileId, pre: Option<Arc<LexedFile>>) {
        let (tokens, diags): (Vec<Token>, Vec<LexDiag>) = match pre {
            Some(f) => (
                f.tokens.iter().map(|t| Token { file, ..*t }).collect(),
                f.diags.clone(),
            ),
            None => {
                let lexed = lex(&self.out.sources.get(file).text, file);
                (lexed.tokens, lexed.diags)
            }
        };
        let mut diags = diags.into_iter().peekable();
        for (k, tok) in tokens.into_iter().enumerate() {
            let global = self.virt;
            while let Some(d) = diags.next_if(|d| d.token as usize == k) {
                let line = self.out.sources.get(file).line_of(d.line_at);
                self.out.diags.push(
                    Diagnostic::new(d.code, d.severity, d.message)
                        .at(Span::new(file, d.start, d.end), line)
                        .with_seq(seq_for_token(global)),
                );
            }
            self.out.tokens.push(tok);
            self.virt += 1;
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
        let meta = if self.ctx.lex.is_some() || self.ctx.frags.is_some() {
            self.fs().metadata(&full)
        } else {
            None
        };
        match meta {
            Some(m) => self.deps.push(Dep::File(full.clone(), m)),
            None => self.cacheable = false,
        }
        if self.ctx.frags.is_some() {
            if self.no_frags {
                self.counts.after_error += 1;
            } else if self.textual.contains(&(file.0, tok.start)) {
                self.counts.nested += 1;
            } else if let Some(m) = meta {
                if self.fragment(file, tok, &full, &full_name, m) {
                    return;
                }
            } else {
                self.counts.unusable += 1;
            }
        }
        let Some((text, pre)) = self.read(&full, meta) else {
            self.cacheable = false;
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
        self.splice(id, pre);
        self.open.pop();
    }

    /// `full`'s text, and its tokens when a [`LexCache`] has or takes
    /// them; `None` when it cannot be read.
    fn read(
        &self,
        full: &Path,
        meta: Option<Metadata>,
    ) -> Option<(Vec<u8>, Option<Arc<LexedFile>>)> {
        let cache = self.ctx.lex;
        let cached = match (cache, &meta) {
            (Some(c), Some(m)) => c.get(full, m),
            _ => None,
        };
        if let Some(f) = cached {
            return Some((f.text.to_vec(), Some(f)));
        }
        let text = self.fs().read(full).ok()?;
        let pre = match (cache, meta) {
            (Some(c), Some(m)) => {
                // Lexed with `FileId(0)` for the cache; `splice` retags.
                let lexed = lex(&text, FileId(0));
                let f = Arc::new(LexedFile {
                    text: text.as_slice().into(),
                    tokens: lexed.tokens,
                    diags: lexed.diags,
                });
                c.put(full, m, f.clone());
                Some(f)
            }
            _ => None,
        };
        Some((text, pre))
    }

    /// Put `full` in as its parse, from the fragment cache or parsed now;
    /// `false` when it has to be spliced as tokens.
    fn fragment(
        &mut self,
        file: FileId,
        tok: Token,
        full: &Path,
        full_name: &str,
        meta: Metadata,
    ) -> bool {
        let Some(cache) = self.ctx.frags else {
            return false;
        };
        let key = FragmentKey {
            path: full.to_path_buf(),
            open: self.open.clone(),
            main: self.ctx.main.to_path_buf(),
        };
        let frag = match cache
            .get(&key)
            .filter(|f| f.is_current(self.fs(), self.ctx.libs))
        {
            Some(f) => {
                self.counts.reused += 1;
                f
            }
            None => {
                let Some((text, pre)) = self.read(full, Some(meta)) else {
                    return false;
                };
                let mut open = self.open.clone();
                open.push(full_name.to_string());
                let f = Arc::new(fragment::build(
                    self.ctx,
                    full.to_path_buf(),
                    text,
                    pre,
                    open,
                    meta,
                ));
                cache.put(key, f.clone());
                self.counts.built += 1;
                f
            }
        };
        self.deps.extend(frag.deps.iter().cloned());
        let Some(body) = &frag.body else {
            self.counts.unusable += 1;
            return false;
        };
        let file_base = self.out.sources.len() as u32;
        let token_base = self.virt;
        for f in &body.files {
            self.out.sources.push(f.duplicate());
        }
        let seq = seq_for_token(token_base);
        self.out.diags.extend(
            body.diags
                .iter()
                .map(|d| crate::ast::rebase_diag(d, file_base, seq)),
        );
        self.out.uses.extend(body.uses.iter().map(|u| UseRef {
            token: u.token + token_base,
            ..u.clone()
        }));
        self.out.includes.extend(body.includes.iter().cloned());
        self.last_name = body.last_name.clone();
        self.inserts.push(Pending {
            at: self.out.tokens.len() as u32 - 1,
            key: (file.0, tok.start),
            frag: frag.clone(),
            file_base,
            token_base,
        });
        self.virt += body.cst.tokens().len() as u32;
        self.counts.fragments += 1;
        true
    }

    fn use_(&mut self, file: FileId, tok: Token, global: u32) {
        // `SourceFile::registerUse`: a `.ttf` or `.otf` is a font to
        // register, never a library. It stays in `uses` (hosts register
        // the fonts from there), but `deps` and the session's loader skip
        // it, which kept a font from being parsed as OpenSCAD source.

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
                if is_font_path(&name) {
                    // No span: OpenSCAD logs it without a location, so
                    // the line has no "in file" part. The sequence number
                    // keeps it right after the warning.
                    self.out.diags.push(
                        Diagnostic::new(
                            DiagCode::FontNotFound,
                            Severity::Error,
                            format!("Can't read font with path '{name}'"),
                        )
                        .with_seq(seq_for_token(global)),
                    );
                }
                self.out.uses.push(UseRef {
                    token: global,
                    path: name,
                    found: false,
                });
            }
        }
    }

    fn fs(&self) -> &dyn FileSystem {
        self.ctx.fs
    }

    /// [`find_valid_path`], noting the answer: a fragment is only good
    /// while every path in it still resolves to the same file (a new file
    /// next to the includer would shadow a library's).
    fn find_valid_path(&mut self, source_dir: &Path, local: &Path) -> Option<PathBuf> {
        let found = find_valid_path(self.fs(), self.ctx.libs, source_dir, local, &self.open);
        if self.ctx.frags.is_some() {
            self.deps.push(Dep::Path {
                dir: source_dir.to_path_buf(),
                local: local.to_path_buf(),
                open: self.open.clone(),
                found: found.clone(),
            });
        }
        found
    }
}

/// Whether a `use`d name is a font file (`.ttf` or `.otf`, any case),
/// which OpenSCAD registers as a font instead of loading as a library
/// (`SourceFile::registerUse`).
pub fn is_font_path(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"))
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

/// A path as OpenSCAD's `generic_string()` prints it (`/`-separated),
/// which is how used libraries are keyed (`deps::Library::path`).
pub fn generic(p: &Path) -> String {
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
    /// A cache the tests can inspect.
    #[derive(Default)]
    struct Cache(std::sync::Mutex<HashMap<PathBuf, (Metadata, Arc<LexedFile>)>>);

    impl LexCache for Cache {
        fn get(&self, path: &Path, meta: &Metadata) -> Option<Arc<LexedFile>> {
            let m = self.0.lock().unwrap();
            m.get(path)
                .filter(|(k, _)| k == meta)
                .map(|(_, f)| f.clone())
        }
        fn put(&self, path: &Path, meta: Metadata, file: Arc<LexedFile>) {
            self.0
                .lock()
                .unwrap()
                .insert(path.to_path_buf(), (meta, file));
        }
    }

    #[test]
    fn cached_loads_equal_fresh_ones() {
        let fs = crate::vfs::MemFs::new();
        fs.insert("/d/a.scad", b"x = 1;\ninclude <b.scad>\n".to_vec());
        fs.insert("/d/b.scad", b"y = \"\\q\";\n".to_vec());
        let libs = LibraryPath::default();
        let main = b"include <a.scad>\ninclude <b.scad>\ncube(x);\n".to_vec();
        let cache = Cache::default();
        let fresh = load(PathBuf::from("/d/m.scad"), main.clone(), &fs, &libs);
        let key = |l: &Loaded| {
            (
                l.tokens.clone(),
                l.diags.clone(),
                l.sources
                    .iter()
                    .map(|(_, f)| (f.path.clone(), f.text.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        for _ in 0..2 {
            let cached = load_cached(
                PathBuf::from("/d/m.scad"),
                main.clone(),
                &fs,
                &libs,
                Some(&cache),
            );
            assert_eq!(key(&cached), key(&fresh));
        }
        assert_eq!(cache.0.lock().unwrap().len(), 2, "both includes cached");
        assert!(!fresh.diags.is_empty(), "the undefined escape warns");
        // A changed file is read again.
        fs.insert("/d/b.scad", b"y = 2;\n".to_vec());
        let after = load_cached(
            PathBuf::from("/d/m.scad"),
            main.clone(),
            &fs,
            &libs,
            Some(&cache),
        );
        assert_eq!(
            key(&after),
            key(&load(PathBuf::from("/d/m.scad"), main, &fs, &libs))
        );
    }
}
