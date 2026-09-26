//! `neoscad fmt`: an OpenSCAD formatter over the lossless syntax tree
//! (`lang::syntax`).
//!
//! Guarantees, each checked on every file before its output is returned:
//!
//! - **only whitespace changes**: the significant tokens and the comments
//!   are the input's, in the same order ([`equivalent`]);
//! - **the program is the same**: the `.ast` dump, customizer annotations
//!   included, is byte-identical before and after. OpenSCAD reads those
//!   annotations from the raw lines at the top of the file (a `// [0:10]`
//!   after an assignment, a `// description` on the line before it, a
//!   `/* [Group] */`), so where a comment sits is part of the program
//!   there; the layout keeps those lines, and the check proves it;
//! - **idempotent**: formatting the output again changes nothing (tested
//!   over OpenSCAD's test inputs, its examples, MCAD and BOSL2).
//!
//! A file that fails the checks is an internal error and is left as it
//! is, so a formatter bug can cost a reformat, never a program.
//!
//! Style ([`Config`] sets the indent and width): 4-space indent, 100
//! columns, one statement per line, `{` on the line of its statement,
//! `name = value` for assignments and `for`/`let` bindings but
//! `name=value` for named arguments and parameter defaults (the corpus'
//! majority: BOSL2, MCAD and OpenSCAD's tests), spaces around binary
//! operators, `[0:n]` ranges of plain bounds, at most one blank line kept
//! between items, and lists that break one item per line (numbers fill
//! the lines) when they do not fit.

mod build;
mod diff;
mod doc;

pub use diff::unified as unified_diff;

use std::path::{Path, PathBuf};

use lang::loader::FileSystem;
use lang::syntax::SyntaxKind;
use lang::syntax::lexer::lex;

/// The configuration file, looked up from a file's directory upward.
pub const CONFIG_FILE: &str = ".neoscad-fmt.toml";

/// Layout settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Spaces per indentation level.
    pub indent: usize,
    /// The line width lists and expressions are wrapped to.
    pub width: usize,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            indent: 4,
            width: 100,
        }
    }
}

/// Read a `.neoscad-fmt.toml`: `indent = N` and `width = N` lines, `#`
/// comments. Anything else is an error, so a misspelt key is not
/// silently ignored.
pub fn parse_config(text: &str) -> Result<Config, String> {
    let mut cfg = Config::default();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let err = |m: &str| format!("line {}: {m}", i + 1);
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| err("expected `key = value`"))?;
        let n: usize = v
            .trim()
            .parse()
            .map_err(|_| err("expected a whole number"))?;
        match k.trim() {
            "indent" if (1..=16).contains(&n) => cfg.indent = n,
            "indent" => return Err(err("indent must be 1 to 16")),
            "width" if (20..=1000).contains(&n) => cfg.width = n,
            "width" => return Err(err("width must be 20 to 1000")),
            other => return Err(err(&format!("unknown key '{other}' (indent, width)"))),
        }
    }
    Ok(cfg)
}

/// The nearest `.neoscad-fmt.toml` in `dir` or above it, read through
/// `fs`: its path and settings, or `None` when there is none.
pub fn find_config(fs: &dyn FileSystem, dir: &Path) -> Result<Option<(PathBuf, Config)>, String> {
    for d in dir.ancestors() {
        let p = d.join(CONFIG_FILE);
        if fs.exists(&p) && !fs.is_dir(&p) {
            let bytes = fs.read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            let cfg = parse_config(&String::from_utf8_lossy(&bytes))
                .map_err(|e| format!("{}: {e}", p.display()))?;
            return Ok(Some((p, cfg)));
        }
    }
    Ok(None)
}

/// A syntax error that stops a file from being formatted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    pub line: u32,
    pub message: String,
}

/// Why a file was left as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The file does not parse.
    Syntax(Vec<SyntaxError>),
    /// Something the formatter does not lay out (a NUL byte, an
    /// out-of-range number literal OpenSCAD drops).
    Unsupported(String),
    /// The output failed the formatter's own checks: a formatter bug.
    Internal(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Syntax(errs) => {
                match errs.first() {
                    Some(e) => write!(f, "line {}: {}", e.line, e.message)?,
                    None => write!(f, "syntax error")?,
                }
                if errs.len() > 1 {
                    write!(f, " (and {} more)", errs.len() - 1)?;
                }
                Ok(())
            }
            Error::Unsupported(m) => write!(f, "not formatted: the file contains {m}"),
            Error::Internal(m) => write!(
                f,
                "internal formatter error, file left unchanged (please report): {m}"
            ),
        }
    }
}

const PATH: &str = "/fmt/input.scad";

/// Format one file's text.
pub fn format(text: &[u8], cfg: &Config) -> Result<Vec<u8>, Error> {
    match std::str::from_utf8(text) {
        Ok(_) => format_utf8(text, cfg),
        Err(_) => {
            // OpenSCAD strings and comments are bytes, and old files are
            // often Latin-1. Each byte becomes the character of the same
            // number for the layout (widths count characters), and back.
            let wide: String = text.iter().map(|&b| char::from(b)).collect();
            let out = format_utf8(wide.as_bytes(), cfg)?;
            let out = String::from_utf8(out).map_err(|e| Error::Internal(e.to_string()))?;
            let bytes: Vec<u8> = out.chars().map(|c| c as u32 as u8).collect();
            equivalent(text, &bytes).map_err(Error::Internal)?;
            Ok(bytes)
        }
    }
}

fn format_utf8(text: &[u8], cfg: &Config) -> Result<Vec<u8>, Error> {
    let program = lang::parse_file(PathBuf::from(PATH), text.to_vec());
    let errors: Vec<SyntaxError> = program
        .diags
        .iter()
        .filter(|d| d.is_error())
        .map(|d| SyntaxError {
            line: d.line,
            message: d.message.clone(),
        })
        .collect();
    if !errors.is_empty() {
        return Err(Error::Syntax(errors));
    }
    let file = program.sources.get(program.main);
    let region_end = lang::customizer::annotate::parameter_region_end(&with_suffix(text));
    let builder =
        build::Builder::new(&program.cst, file, region_end).map_err(|u| Error::Unsupported(u.0))?;
    let mut d = builder.file(program.cst.root()).map_err(Error::Internal)?;
    d.propagate();
    let crlf = uses_crlf(text);
    let mut out = doc::print(&d, cfg.width, cfg.indent, if crlf { "\r\n" } else { "\n" });
    // One line break at the end (a final comment has already written
    // one).
    let end = out.trim_end_matches(['\r', '\n']).len();
    out.truncate(end);
    if !out.is_empty() {
        out.push_str(if crlf { "\r\n" } else { "\n" });
    }
    let out = out.into_bytes();
    equivalent(text, &out).map_err(Error::Internal)?;
    Ok(out)
}

/// Whether most line breaks are CRLF: the output keeps the file's line
/// endings.
fn uses_crlf(text: &[u8]) -> bool {
    let lf = text.iter().filter(|&&b| b == b'\n').count();
    let crlf = text.windows(2).filter(|w| w == b"\r\n").count();
    lf > 0 && crlf * 2 > lf
}

/// The text as OpenSCAD's command line parses it: with the end-of-text
/// marker it appends, which moves the last line the customizer scans.
fn with_suffix(text: &[u8]) -> Vec<u8> {
    let mut t = text.to_vec();
    t.extend_from_slice(b"\n\x03\n");
    t
}

/// The `.ast` export of a file as the command line would write it (its
/// own statements, customizer annotations included; `include`d files
/// are not read).
pub fn ast_dump(text: &[u8]) -> Vec<u8> {
    let full = with_suffix(text);
    let mut program = lang::parse_file(PathBuf::from(PATH), full.clone());
    let main = program.main;
    lang::customizer::collect_parameters(&mut program.ast, &full, |f| f == main);
    lang::dump::dump(&program.ast)
}

/// Check that `b` is `a` with only whitespace changed: the same
/// significant tokens and comments in the same order (a `//` comment may
/// lose trailing blanks), and the same `.ast` dump.
pub fn equivalent(a: &[u8], b: &[u8]) -> Result<(), String> {
    let pieces = |t: &[u8]| -> Vec<(SyntaxKind, Vec<u8>)> {
        lex(t, lang::source::FileId(0))
            .tokens
            .iter()
            .filter(|k| k.kind != SyntaxKind::Whitespace)
            .map(|k| {
                let mut s = t[k.start as usize..k.end() as usize].to_vec();
                // Trailing blanks of a `//` comment are layout too.
                if k.kind == SyntaxKind::LineComment {
                    while matches!(s.last(), Some(b'\r' | b' ' | b'\t')) {
                        s.pop();
                    }
                }
                (k.kind, s)
            })
            .collect()
    };
    let (pa, pb) = (pieces(a), pieces(b));
    if let Some(i) = (0..pa.len().max(pb.len())).find(|&i| pa.get(i) != pb.get(i)) {
        let show = |p: Option<&(SyntaxKind, Vec<u8>)>| {
            p.map_or("end of file".to_string(), |(k, s)| {
                format!("{k:?} {:?}", String::from_utf8_lossy(s))
            })
        };
        return Err(format!(
            "token {i} changed: {} became {}",
            show(pa.get(i)),
            show(pb.get(i))
        ));
    }
    let (da, db) = (ast_dump(a), ast_dump(b));
    if da != db {
        let (la, lb): (Vec<&[u8]>, Vec<&[u8]>) = (
            da.split(|&c| c == b'\n').collect(),
            db.split(|&c| c == b'\n').collect(),
        );
        let i = (0..la.len().max(lb.len()))
            .find(|&i| la.get(i) != lb.get(i))
            .unwrap_or(0);
        return Err(format!(
            "the program changed (.ast line {}: {:?} became {:?})",
            i + 1,
            String::from_utf8_lossy(la.get(i).copied().unwrap_or(b"")),
            String::from_utf8_lossy(lb.get(i).copied().unwrap_or(b""))
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(s: &str) -> String {
        String::from_utf8(format(s.as_bytes(), &Config::default()).unwrap()).unwrap()
    }

    #[test]
    fn basic_layout() {
        assert_eq!(
            fmt("module m(a,b=2){cube(a,center=true);}\nx=1+2*3;"),
            "module m(a, b=2) {\n    cube(a, center=true);\n}\nx = 1 + 2 * 3;\n"
        );
    }

    #[test]
    fn config_file() {
        assert_eq!(
            parse_config("# c\nindent = 2\nwidth=80\n"),
            Ok(Config {
                indent: 2,
                width: 80
            })
        );
        assert!(parse_config("indnt = 2").is_err());
    }
}
