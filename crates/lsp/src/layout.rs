//! Formatting (`crates/fmt` through the session, so the file's
//! `.neoscad-fmt.toml` applies), the outline and folding ranges.

use serde_json::{Value, json};

use crate::Ctx;
use crate::describe;
use crate::index::{DefKind, ScopeId};
use crate::proto::{self, code};

/// The formatter's changes as edits, line runs rather than the whole
/// text, so the cursor and markers outside them stay put.
///
/// With `range` (bytes), only the top-level statements it touches are
/// formatted, as a file of their own: the formatter lays out whole
/// programs, and a run of its changes over the whole file can reach far
/// outside the range (one run from the first changed line to the last),
/// which a range request must not touch.
pub fn format(ctx: &Ctx<'_>, range: Option<(u32, u32)>) -> Result<Value, (i64, String)> {
    let f = ctx.file();
    let (from, to) = match range {
        None => (0, f.text().len() as u32),
        Some((a, b)) => {
            let spans: Vec<(u32, u32)> = f
                .program
                .cst
                .root()
                .children()
                .filter_map(|n| n.span().map(|s| (s.start, s.end)))
                .filter(|&(s, e)| s <= b && a <= e)
                .collect();
            match (spans.first(), spans.last()) {
                (Some(first), Some(last)) => (first.0, last.1),
                _ => return Ok(json!([])),
            }
        }
    };
    let old_bytes = &f.text()[from as usize..to as usize];
    let req = session::format::FormatRequest {
        input: Some(f.path.to_string_lossy().into_owned()),
        text: Some(old_bytes.to_vec()),
        cwd: f.path.parent().map(std::path::Path::to_path_buf),
        ..Default::default()
    };
    let out = ctx.session.format(&req);
    let mut new = match out.result {
        Ok(t) => t,
        Err(session::format::FormatFailure::Format(scadfmt::Error::Syntax(errors))) => {
            let at = errors
                .first()
                .map_or(String::new(), |e| format!(" (line {})", e.line));
            return Err((
                code::REQUEST_FAILED,
                format!("not formatted: the file has a syntax error{at}"),
            ));
        }
        Err(e) => return Err((code::REQUEST_FAILED, format!("not formatted: {e}"))),
    };
    // A fragment ends where its last statement does; the formatter ends
    // a file with a newline.
    if range.is_some() && !old_bytes.ends_with(b"\n") && new.ends_with(b"\n") {
        new.pop();
    }
    let old = String::from_utf8_lossy(old_bytes).into_owned();
    let new = String::from_utf8_lossy(&new).into_owned();
    // Byte offset of each old line's start (lines keep their `\n`).
    let mut starts = vec![from];
    for l in old.split_inclusive('\n') {
        starts.push(starts.last().copied().unwrap_or(from) + l.len() as u32);
    }
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    let edits: Vec<Value> = scadfmt::line_changes(&old, &new)
        .into_iter()
        .map(|c| {
            json!({
                "range": proto::range(f.source(), (starts[c.old.start], starts[c.old.end])),
                "newText": new_lines[c.new.clone()].concat(),
            })
        })
        .collect();
    Ok(Value::Array(edits))
}

// The protocol's `SymbolKind`s.
const SYMBOL_MODULE: u8 = 2;
const SYMBOL_FUNCTION: u8 = 12;
const SYMBOL_VARIABLE: u8 = 13;

/// The outline: modules, functions and assignments, nested as they are
/// written (a module's own assignments and modules inside it).
pub fn symbols(ctx: &Ctx<'_>) -> Value {
    Value::Array(scope_symbols(ctx, 0))
}

fn scope_symbols(ctx: &Ctx<'_>, scope: ScopeId) -> Vec<Value> {
    let f = ctx.file();
    let src = f.source();
    let mut out = Vec::new();
    for (_, d) in f.index.defs_in(scope) {
        let (kind, detail, children) = match d.kind {
            DefKind::Module => (
                SYMBOL_MODULE,
                format!("({})", describe::params_text(d)),
                d.inner.map(|s| scope_symbols(ctx, s)).unwrap_or_default(),
            ),
            DefKind::Function => (
                SYMBOL_FUNCTION,
                format!("({})", describe::params_text(d)),
                Vec::new(),
            ),
            DefKind::Variable => (
                SYMBOL_VARIABLE,
                d.value
                    .map(|v| format!("= {}", describe::one_line(&f.slice(v), 40)))
                    .unwrap_or_default(),
                Vec::new(),
            ),
            _ => continue,
        };
        let mut v = json!({
            "name": d.name,
            "detail": detail,
            "kind": kind,
            "range": proto::range(src, d.span),
            "selectionRange": proto::range(src, d.name_span),
        });
        if !children.is_empty() {
            v["children"] = Value::Array(children);
        }
        out.push(v);
    }
    out
}

/// Folding ranges: blocks, lists and argument lists that span lines,
/// comment runs, and runs of `include`/`use` lines. A range ending in a
/// closing bracket on its own line stops the line before, so the bracket
/// stays visible.
pub fn folding(ctx: &Ctx<'_>) -> Value {
    let f = ctx.file();
    let src = f.source();
    let mut ranges: Vec<(u32, u32, Option<&str>)> = Vec::new();
    for fold in &f.index.folds {
        let a = src.line_of(fold.start) - 1;
        let mut b = src.line_of(fold.end) - 1;
        let last = f.text().get(fold.end.saturating_sub(1) as usize).copied();
        if !fold.comment && matches!(last, Some(b'}' | b']' | b')')) {
            // `}` alone on its line: keep that line out of the fold.
            let line_start = src.line_start(b + 1);
            let before = &f.text()[line_start as usize..fold.end as usize - 1];
            if before.iter().all(u8::is_ascii_whitespace) {
                b = b.saturating_sub(1);
            }
        }
        if b > a {
            ranges.push((a, b, fold.comment.then_some("comment")));
        }
    }
    // Consecutive directive lines.
    let mut run: Option<(u32, u32)> = None;
    for d in &f.index.directives {
        let l = src.line_of(d.span.0) - 1;
        run = match run {
            Some((a, b)) if l == b + 1 => Some((a, l)),
            Some((a, b)) => {
                if b > a {
                    ranges.push((a, b, Some("imports")));
                }
                Some((l, l))
            }
            None => Some((l, l)),
        };
    }
    if let Some((a, b)) = run
        && b > a
    {
        ranges.push((a, b, Some("imports")));
    }
    // One range per first line (the outermost), as editors expect.
    ranges.sort_by(|x, y| x.0.cmp(&y.0).then(y.1.cmp(&x.1)));
    ranges.dedup_by_key(|r| r.0);
    Value::Array(
        ranges
            .into_iter()
            .map(|(a, b, kind)| {
                let mut v = json!({"startLine": a, "endLine": b});
                if let Some(k) = kind {
                    v["kind"] = json!(k);
                }
                v
            })
            .collect(),
    )
}

/// The byte range of a range request.
pub fn range_of(ctx: &Ctx<'_>, params: &Value) -> Option<(u32, u32)> {
    proto::offsets(ctx.file().source(), params.get("range")?)
}
