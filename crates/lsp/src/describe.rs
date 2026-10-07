//! Text about a definition, for hover, completion and signature help:
//! builtins from `crates/docs`' reference, user and library code from
//! its signature and leading comment (BOSL2's structured blocks shown
//! compactly, as `neoscad docs` shows them).

use std::path::Path;
use std::sync::Arc;

use lang::syntax::SyntaxKind as K;

use crate::index::{Def, DefKind};
use crate::world::{Analyzed, Found, World};

/// `module name(a, b=1)`: a definition's signature, parameters as the
/// formatter writes them.
pub fn signature(d: &Def) -> String {
    let kw = match d.kind {
        DefKind::Module => "module",
        DefKind::Function => "function",
        _ => return d.name.clone(),
    };
    format!("{kw} {}({})", d.name, params_text(d))
}

pub fn params_text(d: &Def) -> String {
    d.params
        .iter()
        .map(|p| match &p.default {
            Some(v) => format!("{}={}", p.name, one_line(v, 40)),
            None => p.name.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `s` on one line, at most `max` characters.
pub fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max.saturating_sub(3)).collect();
    format!("{cut}...")
}

/// The comment lines right above byte `start` (no blank line between),
/// for definitions `docs::definitions` does not cover: variables and
/// nested modules and functions.
pub fn leading_comment(f: &Analyzed, start: u32) -> Vec<String> {
    let toks = f.program.cst.tokens();
    let mut i = toks.partition_point(|t| t.start < start);
    let mut lines: Vec<String> = Vec::new();
    while i > 0 {
        i -= 1;
        let t = &toks[i];
        let text = f.slice((t.start, t.end()));
        match t.kind {
            K::Whitespace if text.matches('\n').count() >= 2 => break,
            K::Whitespace => {}
            K::LineComment => lines.push(text.trim_start_matches('/').trim().to_string()),
            K::BlockComment => {
                let inner = text.trim_start_matches("/*").trim_end_matches("*/");
                for l in inner.lines().rev() {
                    lines.push(l.trim().trim_start_matches('*').trim().to_string());
                }
            }
            _ => break,
        }
    }
    lines.reverse();
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// Where a file is, for a reader: relative to a library directory
/// (`BOSL2/shapes3d.scad`) or to the document's directory, else in full.
pub fn location(world_main: &Path, libs: &[std::path::PathBuf], file: &Path) -> String {
    for dir in libs {
        if let Ok(r) = file.strip_prefix(session::normal(dir)) {
            return r.display().to_string();
        }
    }
    if let Some(dir) = world_main.parent()
        && let Ok(r) = file.strip_prefix(dir)
    {
        return r.display().to_string();
    }
    file.display().to_string()
}

/// The one-line summary of a definition: BOSL2's synopsis, else the
/// first comment line.
pub fn summary(found: &Found) -> Option<String> {
    let d = found.def();
    if let Some(u) = found.file.user_doc(d) {
        if let Some(s) = u.sections.iter().find(|s| s.title == "Synopsis") {
            return Some(s.text.clone());
        }
        return u
            .comment
            .iter()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .map(str::to_string);
    }
    if matches!(d.kind, DefKind::Parameter | DefKind::Binding) {
        return None;
    }
    leading_comment(&found.file, d.span.0).into_iter().next()
}

/// A builtin's reference entry as Markdown.
pub fn builtin_markdown(e: &docs::Entry) -> String {
    let mut out = format!(
        "```openscad\n{} {}\n```\n{}\n",
        e.kind.name(),
        e.signature,
        e.summary
    );
    if let Some(l) = e.extension_label() {
        out.push_str(&format!("\n*{l}*\n"));
    }
    if !e.params.is_empty() {
        out.push('\n');
        for p in &e.params {
            out.push_str(&format!("- `{}`", p.name));
            if !p.ty.is_empty() {
                out.push_str(&format!(" *{}*", p.ty));
            }
            if let Some(d) = &p.default {
                out.push_str(&format!(" = `{d}`"));
            }
            if !p.doc.is_empty() {
                out.push_str(&format!(": {}", p.doc));
            }
            out.push('\n');
        }
    }
    if let Some(r) = &e.returns {
        out.push_str(&format!("\nReturns {r}\n"));
    }
    if let Some(n) = &e.notes {
        out.push_str(&format!("\n{n}\n"));
    }
    out.push_str(&format!("\n```openscad\n{}\n```\n", e.example));
    out
}

/// A user or library definition as Markdown: its signature, where it is,
/// and its documentation (compact for BOSL2 blocks, as `neoscad docs`).
pub fn def_markdown(world: &World, libs: &[std::path::PathBuf], found: &Found) -> String {
    let d = found.def();
    let file: &Arc<Analyzed> = &found.file;
    let loc = location(&world.main().path, libs, &file.path);
    let line = file.source().line_of(d.span.0);
    let mut out = String::new();
    match d.kind {
        DefKind::Module | DefKind::Function => {
            out.push_str(&format!("```openscad\n{}\n```\n", signature(d)));
            out.push_str(&format!("*{loc}:{line}*\n"));
            if let Some(u) = file.user_doc(d) {
                let text = docs::user::render(u, &loc, false);
                // The first line repeats the signature and location.
                let body: Vec<&str> = text.lines().skip(1).collect();
                if !body.is_empty() {
                    out.push_str("\n```text\n");
                    for l in body {
                        out.push_str(l.strip_prefix("  ").unwrap_or(l));
                        out.push('\n');
                    }
                    out.push_str("```\n");
                }
            } else {
                push_comment(&mut out, &leading_comment(file, d.span.0));
            }
        }
        DefKind::Variable => {
            let value = d.value.map(|v| file.slice(v)).unwrap_or_default();
            out.push_str(&format!(
                "```openscad\n{} = {}\n```\n",
                d.name,
                one_line(&value, 120)
            ));
            if d.scope == 0
                && let Some(v) = crate::value::constant(world, file, &d.name)
                && v != one_line(&value, 120)
            {
                out.push_str(&format!("Value: `{}`\n\n", one_line(&v, 200)));
            }
            out.push_str(&format!("*{loc}:{line}*\n"));
            push_comment(&mut out, &leading_comment(file, d.span.0));
        }
        DefKind::Parameter => {
            let owner = d.owner.map(|o| &file.index.defs[o]);
            let head = match d.value {
                Some(v) => format!("{} = {}", d.name, one_line(&file.slice(v), 80)),
                None => d.name.clone(),
            };
            out.push_str(&format!("```openscad\n{head}\n```\n"));
            match owner {
                Some(o) => {
                    out.push_str(&format!("Parameter of `{}`\n", signature(o)));
                    if let Some(doc) = argument_doc(
                        &Found {
                            file: file.clone(),
                            def: d.owner.unwrap_or(0),
                        },
                        &d.name,
                    ) {
                        out.push_str(&format!("\n{doc}\n"));
                    }
                }
                None => out.push_str("Parameter of a function literal\n"),
            }
        }
        DefKind::Binding => {
            let value = d.value.map(|v| file.slice(v)).unwrap_or_default();
            out.push_str(&format!(
                "```openscad\n{} = {}\n```\nBound by `let` or `for`\n",
                d.name,
                one_line(&value, 120)
            ));
        }
    }
    out
}

fn push_comment(out: &mut String, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    out.push('\n');
    for l in lines.iter().take(12) {
        out.push_str(l);
        out.push_str("  \n");
    }
    if lines.len() > 12 {
        out.push_str(&format!("... {} more lines\n", lines.len() - 12));
    }
}

/// A parameter's line in a BOSL2 `Arguments:` section (`size = How big.`).
pub fn argument_doc(owner: &Found, param: &str) -> Option<String> {
    let u = owner.file.user_doc(owner.def())?;
    let s = u
        .sections
        .iter()
        .find(|s| s.title.starts_with("Arguments"))?;
    s.lines.iter().find_map(|l| {
        let (name, doc) = l.split_once('=')?;
        (name.trim() == param).then(|| doc.trim().to_string())
    })
}
