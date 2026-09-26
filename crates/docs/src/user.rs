//! The documentation of user and library code: each top-level module and
//! function with the comment block right above it (`//` lines with no
//! blank line between them and the definition, or a `/* */` block).
//!
//! BOSL2 documents its API in structured blocks (`// Module: cuboid()`,
//! `// Synopsis:`, `// Usage:` with indented lines, `// Arguments:`,
//! `// Example:` ...). Those are split into [`Section`]s so they can be
//! shown compactly, and a definition without a block of its own (BOSL2
//! often documents the function and module forms of a name once, as
//! `// Function&Module: cuboid()`) gets the block whose header names it.

use std::collections::HashMap;
use std::path::PathBuf;

use lang::Program;
use lang::ast::Param;
use lang::source::FileId;
use lang::syntax::SyntaxKind;

use crate::Kind;

/// One `Title: text` section of a structured block and its indented
/// lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// `Usage`, `Arguments`, `Example(VPR=[...])`, ...
    pub title: String,
    /// The rest of the header line.
    pub text: String,
    /// The lines under it, with their common indent removed.
    pub lines: Vec<String>,
}

/// A user or library module or function with its documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserDoc {
    pub kind: Kind,
    pub name: String,
    /// `module name(a, b = 1)`.
    pub signature: String,
    pub file: PathBuf,
    pub line: u32,
    /// The comment block, one entry per line, without the comment
    /// markers.
    pub comment: Vec<String>,
    /// The block's sections when it is structured (BOSL2's style).
    pub sections: Vec<Section>,
}

/// The text of a comment token, without `//` or `/* */`, as lines.
fn comment_lines(text: &str) -> Vec<String> {
    if let Some(t) = text.strip_prefix("//") {
        return vec![t.trim_end().to_string()];
    }
    let inner = text
        .strip_prefix("/*")
        .and_then(|t| t.strip_suffix("*/"))
        .unwrap_or(text);
    let inner = inner.trim_start_matches('*');
    let mut lines: Vec<String> = inner
        .lines()
        .map(|l| {
            let l = l.trim_end();
            let t = l.trim_start();
            // ` * text` continuation lines.
            match t.strip_prefix('*') {
                Some(rest) => rest.strip_prefix(' ').unwrap_or(rest).to_string(),
                None => t.to_string(),
            }
        })
        .skip_while(String::is_empty)
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// `Title: text` when `line` (a `//` comment's text) is a section header:
/// one space after the `//`, a capitalised word (or `Function&Module`,
/// `See Also`), an optional `(...)`, then a colon.
fn header(line: &str) -> Option<(String, String)> {
    let t = line.strip_prefix(' ')?;
    if !t.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    let colon = t.find(':')?;
    let title = &t[..colon];
    let word = title.split('(').next().unwrap_or(title);
    let ok = word.len() <= 24
        && word.split(' ').count() <= 2
        && word
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == '&' || c == ' ');
    if !ok || title.contains('(') && !title.ends_with(')') {
        return None;
    }
    Some((title.to_string(), t[colon + 1..].trim().to_string()))
}

/// Split a block into sections, if at least two lines are headers.
fn sections(lines: &[String]) -> Vec<Section> {
    let headers = lines.iter().filter(|l| header(l).is_some()).count();
    if headers < 2 {
        return Vec::new();
    }
    let mut out: Vec<Section> = Vec::new();
    for l in lines {
        if let Some((title, text)) = header(l) {
            out.push(Section {
                title,
                text,
                lines: Vec::new(),
            });
        } else if let Some(s) = out.last_mut() {
            s.lines.push(l.clone());
        }
    }
    for s in &mut out {
        while s.lines.last().is_some_and(|l| l.trim().is_empty()) {
            s.lines.pop();
        }
        let indent = s
            .lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        for l in &mut s.lines {
            *l = l.get(indent..).unwrap_or("").to_string();
        }
    }
    out
}

/// The name a block's first header documents (`Module: cuboid()` names
/// `cuboid`), with whether it covers modules and functions.
fn documents(first: &str) -> Option<(String, bool, bool)> {
    let (title, text) = header(first)?;
    let (m, f) = match title.as_str() {
        "Module" => (true, false),
        "Function" => (false, true),
        "Function&Module" => (true, true),
        _ => return None,
    };
    let name = text.split('(').next()?.trim();
    (!name.is_empty()).then(|| (name.to_string(), m, f))
}

fn signature(p: &Program, kind: &str, name: &str, params: &[Param]) -> String {
    let mut out = format!("{kind} {name}(").into_bytes();
    lang::dump::write_params(&p.ast, params, &mut out);
    out.push(b')');
    String::from_utf8_lossy(&out).into_owned()
}

/// Every top-level module and function of `p` (and of the files it
/// includes), with its documentation.
pub fn definitions(p: &Program) -> Vec<UserDoc> {
    let toks = p.cst.tokens();
    let text = |i: usize| {
        let t = &toks[i];
        String::from_utf8_lossy(p.sources.get(t.file).slice(t.start, t.end())).into_owned()
    };
    let has_newline = |i: usize| text(i).contains('\n');
    let blank = |i: usize| text(i).matches('\n').count() >= 2;
    // The comment block ending just before token `end`.
    let block_before = |end: usize| -> Vec<String> {
        let file = toks.get(end).map_or(FileId(0), |t| t.file);
        let mut lines: Vec<Vec<String>> = Vec::new();
        let mut i = end;
        while i > 0 {
            i -= 1;
            let t = &toks[i];
            if t.file != file {
                break;
            }
            match t.kind {
                SyntaxKind::Whitespace if blank(i) => break,
                SyntaxKind::Whitespace => {}
                SyntaxKind::LineComment | SyntaxKind::BlockComment => {
                    // Only comments on lines of their own.
                    let own = i == 0
                        || toks[i - 1].file != file
                        || matches!(toks[i - 1].kind, SyntaxKind::LineComment)
                        || (toks[i - 1].kind == SyntaxKind::Whitespace
                            && (has_newline(i - 1) || i == 1));
                    if !own {
                        break;
                    }
                    lines.push(comment_lines(&text(i)));
                }
                _ => break,
            }
        }
        lines.reverse();
        lines.concat()
    };
    // Structured blocks by the name their header documents.
    let mut named: HashMap<(String, bool), Vec<String>> = HashMap::new();
    let mut i = 0;
    while i < toks.len() {
        if toks[i].kind == SyntaxKind::LineComment
            && let Some((name, m, f)) = documents(comment_lines(&text(i))[0].as_str())
        {
            let file = toks[i].file;
            let mut lines = Vec::new();
            let mut j = i;
            while j < toks.len() && toks[j].file == file {
                match toks[j].kind {
                    SyntaxKind::LineComment => lines.extend(comment_lines(&text(j))),
                    SyntaxKind::Whitespace if !blank(j) => {}
                    _ => break,
                }
                j += 1;
            }
            for (is_mod, yes) in [(true, m), (false, f)] {
                if yes {
                    named.entry((name.clone(), is_mod)).or_insert(lines.clone());
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
    let index: HashMap<(FileId, u32), usize> = toks
        .iter()
        .enumerate()
        .filter(|(_, t)| matches!(t.kind, SyntaxKind::KwModule | SyntaxKind::KwFunction))
        .map(|(i, t)| ((t.file, t.start), i))
        .collect();
    let root = &p.ast.root;
    let defs = root
        .modules
        .iter()
        .map(|m| (Kind::Module, m.name, &m.params[..], m.span))
        .chain(
            root.functions
                .iter()
                .map(|f| (Kind::Function, f.name, &f.params[..], f.span)),
        );
    let mut out: Vec<UserDoc> = defs
        .map(|(kind, name, params, span)| {
            let name = p.ast.name(name).to_string();
            let mut comment = index
                .get(&(span.file, span.start))
                .map(|&i| block_before(i))
                .unwrap_or_default();
            let own = comment
                .first()
                .and_then(|l| documents(l))
                .is_none_or(|(n, m, f)| n == name && (if kind == Kind::Module { m } else { f }));
            if (comment.is_empty() || !own)
                && let Some(b) = named.get(&(name.clone(), kind == Kind::Module))
            {
                comment = b.clone();
            }
            UserDoc {
                kind,
                signature: signature(p, kind.name(), &name, params),
                name,
                file: p.sources.path(span.file).to_path_buf(),
                line: p.sources.get(span.file).line_of(span.start),
                sections: sections(&comment),
                comment,
            }
        })
        .collect();
    out.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    out
}

/// Sections a compact rendering shows, and how many lines of each.
fn shown(title: &str) -> Option<usize> {
    let base = title.split('(').next().unwrap_or(title);
    match base {
        "Usage" => Some(12),
        "Arguments" => Some(40),
        "Description" => Some(3),
        _ => None,
    }
}

/// A definition as text. `full`: the whole comment block; otherwise the
/// signature, the synopsis, usage and arguments (BOSL2 blocks) or up to
/// 12 comment lines.
pub fn render(d: &UserDoc, location: &str, full: bool) -> String {
    let mut out = format!("{}  ({location}:{})\n", d.signature, d.line);
    if full || d.sections.is_empty() {
        let n = if full { d.comment.len() } else { 12 };
        for l in d.comment.iter().take(n) {
            out.push_str(&format!("  {}\n", l.strip_prefix(' ').unwrap_or(l)));
        }
        if d.comment.len() > n {
            out.push_str(&format!(
                "  ... {} more lines (--full)\n",
                d.comment.len() - n
            ));
        }
        return out;
    }
    let mut hidden = Vec::new();
    for s in &d.sections {
        let base = s.title.split('(').next().unwrap_or(&s.title);
        if base == "Synopsis" {
            out.push_str(&format!("  {}\n", s.text));
            continue;
        }
        let Some(max) = shown(base) else {
            if !hidden.contains(&base) {
                hidden.push(base);
            }
            continue;
        };
        let mut head = format!("  {}:", base.to_lowercase());
        if !s.text.is_empty() {
            head.push_str(&format!(" {}", s.text));
        }
        out.push_str(&head);
        out.push('\n');
        let lines: Vec<&String> = s
            .lines
            .iter()
            .filter(|l| !l.trim().is_empty() && l.trim() != "---")
            .collect();
        for l in lines.iter().take(max) {
            out.push_str(&format!("    {l}\n"));
        }
        if lines.len() > max {
            out.push_str(&format!("    ... ({} more)\n", lines.len() - max));
        }
    }
    let hidden: Vec<&str> = hidden
        .into_iter()
        .filter(|h| !matches!(*h, "Module" | "Function" | "Function&Module" | "SynTags"))
        .collect();
    if !hidden.is_empty() {
        out.push_str(&format!("  (--full adds: {})\n", hidden.join(", ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(src: &str) -> Vec<UserDoc> {
        let p = lang::parse_file(PathBuf::from("/t.scad"), src.as_bytes().to_vec());
        definitions(&p)
    }

    #[test]
    fn leading_comments() {
        let d = defs(
            "x = 1; // not mine\n// Makes a box.\n// Size in mm.\nmodule box(s = 1) cube(s);\n\n// far\n\nfunction f(a) = a;\n",
        );
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].signature, "module box(s = 1)");
        assert_eq!(d[0].comment, [" Makes a box.", " Size in mm."]);
        assert_eq!(d[0].line, 4);
        assert!(d[1].comment.is_empty());
    }

    #[test]
    fn bosl2_blocks() {
        let src = "\
// Function&Module: thing()
// Synopsis: Makes a thing.
// Usage:
//   thing(size);
// Arguments:
//   size = How big.
//   ---
//   anchor = Where.  Default: CENTER
// Example:
//   thing(3);
function thing(size, anchor) = 1;
module thing(size, anchor) cube(size);
";
        let d = defs(src);
        assert_eq!(d.len(), 2);
        let m = d.iter().find(|d| d.kind == Kind::Module).unwrap();
        assert_eq!(m.sections.len(), 5);
        let t = render(m, "t.scad", false);
        assert_eq!(
            t,
            "module thing(size, anchor)  (t.scad:12)\n  Makes a thing.\n  usage:\n    thing(size);\n  arguments:\n    size = How big.\n    anchor = Where.  Default: CENTER\n  (--full adds: Example)\n"
        );
    }

    #[test]
    fn block_comments() {
        let d = defs("/**\n * Rounded box.\n * @param r radius\n */\nmodule rbox(r) {}\n");
        assert_eq!(d[0].comment, ["Rounded box.", "@param r radius"]);
    }
}
