//! Included files parsed once and put into each program that includes
//! them as their parse.
//!
//! OpenSCAD's `include` is textual: the scanner switches to the included
//! file's tokens, so a program and everything it includes is one token
//! stream and one parse. A host that re-parses a program after every edit
//! to its main file (the session, an editor) would parse a large library
//! such as BOSL2 again each time. Instead, an included file's parse and
//! lowering are kept as a [`Fragment`] and spliced into the program, which
//! comes out exactly as the textual parse would have made it: the same
//! tokens, syntax tree, AST, diagnostics and `use` list, with the same
//! numbering. That holds under two conditions, checked for every include:
//!
//! - **The file parses on its own, without errors, as whole statements**
//!   (and has no end-of-text marker, which only `-D` handling puts in a
//!   main file). Then the parser, arriving at its tokens at a statement
//!   boundary, parses them as it does alone: no statement's parse looks
//!   past its own last token except an `if`'s check for `else`, and a
//!   file cannot start with `else`.
//! - **The directive sits between top-level statements of the includer**:
//!   in the includer's parse without the file's tokens, the directive is a
//!   child of the root (so no statement is open around it, and an `if`
//!   before it did not take an `else` after it: that `else` would have
//!   pulled the directive into the `if`), and no syntax error is in a
//!   statement that started before it (error recovery could otherwise
//!   have run on into the file's tokens).
//!
//! An include that fails either condition, whether inside a module body
//! or an expression, or of a file whose tokens only make sense once
//! spliced, is spliced as tokens, as before. Top-level assignments are
//! the one part of a file's lowering that depends on the includer (a name
//! assigned before the include is reassigned in place, with a warning that
//! names both places), so they are kept as events and replayed against
//! the includer ([`crate::ast::FragmentAst`]).
//!
//! A fragment is cached by its file, the chain of includes it is read
//! inside (which decides circular includes) and the program's main file
//! (reassignment warnings name it), and is used again while every file it
//! read has the same metadata and every path it resolved (includes and
//! `use`s, its own included files' too) resolves to the same file.

use std::cell::Cell;
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ast::{self, FragmentAst, Placed};
use crate::diag::Diagnostic;
use crate::loader::{
    self, Assembly, FileSystem, LexCache, LexedFile, LibraryPath, Loaded, Metadata, Pending,
    UseRef, find_valid_path,
};
use crate::source::{SourceFile, SourceMap};
use crate::syntax::cst::Insert;
use crate::syntax::parser::SyntaxError;
use crate::syntax::{self, Cst, SyntaxKind};

/// Included files' parses kept by a host between parses. Entries are
/// checked by the loader ([`Fragment::is_current`]) before use, so a cache
/// only has to store them.
pub trait FragmentCache {
    fn get(&self, key: &FragmentKey) -> Option<Arc<Fragment>>;
    fn put(&self, key: FragmentKey, fragment: Arc<Fragment>);
}

/// What a fragment's parse depends on beyond the files it read.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FragmentKey {
    /// The included file.
    pub path: PathBuf,
    /// The included files it is read inside, outermost first (they cannot
    /// be included again inside it).
    pub open: Vec<String>,
    /// The program's main file, which reassignment warnings name.
    pub main: PathBuf,
}

/// One included file's parse, or the record that it cannot be used as
/// one (so that it is not parsed alone again while it is unchanged).
#[derive(Debug)]
pub struct Fragment {
    pub(crate) deps: Vec<Dep>,
    pub(crate) body: Option<Body>,
}

#[derive(Debug)]
pub(crate) struct Body {
    /// The file and everything it includes, in load order; the file is
    /// `FileId(0)` in the fragment's own numbering.
    pub files: Vec<SourceFile>,
    /// Its syntax tree, includes spliced (tokens numbered from 0).
    pub cst: Cst,
    /// Scanner and directive messages.
    pub diags: Vec<Diagnostic>,
    pub uses: Vec<UseRef>,
    pub includes: Vec<(String, String)>,
    /// The scanner's `filename` after the file, which an empty `use <>`
    /// after the include would reuse.
    pub last_name: String,
    pub ast: FragmentAst,
}

/// Something a fragment's parse depended on.
#[derive(Debug, Clone)]
pub(crate) enum Dep {
    /// A file read, with its metadata then.
    File(PathBuf, Metadata),
    /// A path resolved by [`find_valid_path`], with the answer then.
    Path {
        dir: PathBuf,
        local: PathBuf,
        open: Vec<String>,
        found: Option<PathBuf>,
    },
}

impl Fragment {
    /// Whether every file it read and every path it resolved is as it was.
    pub fn is_current(&self, fs: &dyn FileSystem, libs: &LibraryPath) -> bool {
        self.deps.iter().all(|d| match d {
            Dep::File(p, m) => fs.metadata(p).as_ref() == Some(m),
            Dep::Path {
                dir,
                local,
                open,
                found,
            } => find_valid_path(fs, libs, dir, local, open) == *found,
        })
    }

    /// Whether programs can take it as a parse (see the module docs).
    pub fn is_usable(&self) -> bool {
        self.body.is_some()
    }

    /// Estimated bytes held: text, tokens, tree and AST.
    pub fn cost(&self) -> usize {
        let deps = self.deps.len() * 96;
        match &self.body {
            None => deps + 64,
            Some(b) => {
                let text: usize = b.files.iter().map(|f| f.text.len()).sum();
                deps + text + b.cst.tokens().len() * 16 + b.cst.len() * 12 + b.ast.expr_count() * 64
            }
        }
    }

    fn body(&self) -> &Body {
        self.body
            .as_ref()
            .expect("only usable fragments are inserted")
    }
}

/// How the includes of the parses were put in, for tests and tuning.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SpliceStats {
    /// Includes put in as a fragment.
    pub fragments: u32,
    /// Of those, and of unusable ones, how many came from the cache and
    /// how many were parsed now.
    pub reused: u32,
    pub built: u32,
    /// Includes spliced as tokens because the directive is not between
    /// top-level statements (inside a module body, a block or a
    /// statement).
    pub nested: u32,
    /// Spliced as tokens because the includer has a syntax error before
    /// them.
    pub after_error: u32,
    /// Spliced as tokens because the file does not parse alone as whole
    /// statements (or has no metadata to validate a cached parse by).
    pub unusable: u32,
}

impl std::ops::AddAssign for SpliceStats {
    fn add_assign(&mut self, o: Self) {
        self.fragments += o.fragments;
        self.reused += o.reused;
        self.built += o.built;
        self.nested += o.nested;
        self.after_error += o.after_error;
        self.unusable += o.unusable;
    }
}

/// The caches and settings a parse runs with.
pub(crate) struct Ctx<'a> {
    pub fs: &'a dyn FileSystem,
    pub libs: &'a LibraryPath,
    pub lex: Option<&'a dyn LexCache>,
    pub frags: Option<&'a dyn FragmentCache>,
    /// The main file, for reassignment warnings.
    pub main: &'a Path,
    pub stats: &'a Cell<SpliceStats>,
}

/// A file parsed with its includes, fragments spliced in.
pub(crate) struct Parsed {
    pub sources: SourceMap,
    pub cst: Cst,
    /// Numbered by the tokens of `cst`.
    pub errors: Vec<SyntaxError>,
    /// Scanner and directive messages.
    pub diags: Vec<Diagnostic>,
    pub uses: Vec<UseRef>,
    includes: Vec<(String, String)>,
    last_name: String,
    deps: Vec<Dep>,
    cacheable: bool,
    /// An end-of-text marker outside the fragments.
    eot: bool,
    inserts: Vec<Pending>,
    /// The entries of `cst` each insert's statements took.
    ranges: Vec<Range<u32>>,
}

impl Parsed {
    /// A stream without fragments (one file, or includes spliced).
    pub fn plain(loaded: Loaded) -> Parsed {
        let Loaded {
            sources,
            tokens,
            diags,
            uses,
            includes,
        } = loaded;
        let parse = syntax::parse(tokens);
        Parsed {
            sources,
            cst: parse.cst,
            errors: parse.errors,
            diags,
            uses,
            includes,
            last_name: String::new(),
            deps: Vec::new(),
            cacheable: false,
            eot: false,
            inserts: Vec::new(),
            ranges: Vec::new(),
        }
    }

    /// The fragments' lowered statements and where they go.
    pub fn placed(&self) -> Vec<Placed<'_>> {
        self.inserts
            .iter()
            .zip(&self.ranges)
            .map(|(p, r)| Placed {
                entries: r.clone(),
                ast: &p.frag.body().ast,
                file_base: p.file_base,
                token_base: p.token_base,
            })
            .collect()
    }
}

/// Parse `path` with its includes, putting in fragments where the module
/// docs' conditions hold (and a fragment cache is given), splicing tokens
/// elsewhere. The includes that turn out not to be between top-level
/// statements are spliced as tokens and the parse is run again; so is
/// every include when a syntax error comes before one.
pub(crate) fn parse(
    ctx: &Ctx<'_>,
    path: PathBuf,
    text: Vec<u8>,
    pre: Option<Arc<LexedFile>>,
    open: Vec<String>,
) -> Parsed {
    let mut textual = HashSet::new();
    let mut no_frags = ctx.frags.is_none();
    loop {
        let Assembly {
            loaded,
            inserts,
            last_name,
            deps,
            cacheable,
            counts,
        } = loader::assemble(
            ctx,
            path.clone(),
            text.clone(),
            pre.clone(),
            open.clone(),
            &textual,
            no_frags,
        );
        let eot = loaded.tokens.iter().any(|t| t.kind == SyntaxKind::Eot);
        let Loaded {
            sources,
            tokens,
            diags,
            uses,
            includes,
        } = loaded;
        let parse = syntax::parse(tokens);
        if !inserts.is_empty() {
            let roots = parse.cst.root_tokens();
            let mut retry = false;
            for p in &inserts {
                if roots.binary_search(&p.at).is_err() {
                    textual.insert(p.key);
                    retry = true;
                }
            }
            if !retry && let Some(e) = parse.errors.first() {
                // The top-level statement the first error is in (the one
                // before it when the error is at a statement's first
                // token, which only makes this stricter).
                let stmt = parse
                    .cst
                    .root_node_starts()
                    .into_iter()
                    .rev()
                    .find(|&s| s < e.token);
                let last = inserts.last().map_or(0, |p| p.at);
                if stmt.is_none_or(|s| s <= last) {
                    no_frags = true;
                    retry = true;
                }
            }
            if retry {
                continue;
            }
        }
        let mut stats = ctx.stats.get();
        stats += counts;
        ctx.stats.set(stats);
        // A token after `p.at` moves on by the fragments' tokens before it.
        let shift = |t: u32| -> u32 {
            inserts
                .iter()
                .take_while(|p| p.at < t)
                .map(|p| p.frag.body().cst.tokens().len() as u32)
                .sum::<u32>()
                + t
        };
        let errors = parse
            .errors
            .iter()
            .map(|e| SyntaxError {
                token: shift(e.token),
            })
            .collect();
        let (cst, ranges) = if inserts.is_empty() {
            (parse.cst, Vec::new())
        } else {
            let ins: Vec<Insert<'_>> = inserts
                .iter()
                .map(|p| Insert {
                    at: p.at,
                    cst: &p.frag.body().cst,
                    file_base: p.file_base,
                })
                .collect();
            parse.cst.splice(&ins)
        };
        return Parsed {
            sources,
            cst,
            errors,
            diags,
            uses,
            includes,
            last_name,
            deps,
            cacheable,
            eot,
            inserts,
            ranges,
        };
    }
}

/// Parse and lower the included file `path` (text `text`, lexed as
/// `pre`), read inside the includes `open` (which end with it), into a
/// fragment: usable when it parses alone without errors, as whole
/// statements.
pub(crate) fn build(
    ctx: &Ctx<'_>,
    path: PathBuf,
    text: Vec<u8>,
    pre: Option<Arc<LexedFile>>,
    open: Vec<String>,
    meta: Metadata,
) -> Fragment {
    let p = parse(ctx, path.clone(), text, pre, open);
    let mut deps = Vec::with_capacity(p.deps.len() + 1);
    deps.push(Dep::File(path, meta));
    deps.extend(p.deps.iter().cloned());
    if !p.errors.is_empty() || p.eot || !p.cacheable {
        return Fragment { deps, body: None };
    }
    let placed = p.placed();
    let (_, _, frag) = ast::lower_with(&p.cst, &p.sources, ctx.main, &p.uses, &placed, true);
    drop(placed);
    let Parsed {
        sources,
        cst,
        diags,
        uses,
        includes,
        last_name,
        ..
    } = p;
    Fragment {
        deps,
        body: Some(Body {
            files: sources.into_files(),
            cst,
            diags,
            uses,
            includes,
            last_name,
            ast: frag.expect("recorded"),
        }),
    }
}
