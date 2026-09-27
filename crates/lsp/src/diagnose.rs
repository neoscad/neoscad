//! Diagnostics: the session's evaluation of a document, as the protocol's
//! diagnostics per file, and their fixes as code actions.
//!
//! Every diagnostic keeps what the session says: the stable code, the
//! OpenSCAD message, the span and the hints (added to the message, where
//! every client shows them). One about a file the document includes (or
//! a library it uses) goes to that file's URI, and the document gets one
//! on its `include`/`use` line that points there (`relatedInformation`),
//! so a problem in an included file is never invisible from the file
//! being edited.
//!
//! Fixes travel in each diagnostic's `data` (`{fixes: [{title, edits}]}`)
//! so a client can offer them from the diagnostic alone (the app's
//! editor does), and `textDocument/codeAction` answers from the same
//! data for clients that ask.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lang::source::SourceFile;
use serde_json::{Value, json};

use crate::describe;
use crate::proto;
use crate::world::World;

// The protocol's severities and tags.
const ERROR: u8 = 1;
const WARNING: u8 = 2;
const INFORMATION: u8 = 3;
const TAG_DEPRECATED: u8 = 2;

/// The quoted name in `Ignoring unknown module 'cub'` or `unknown
/// variable "zz"`.
fn quoted(message: &str) -> Option<&str> {
    let q = message.find(['\'', '"'])?;
    let c = message[q..].chars().next()?;
    let rest = &message[q + 1..];
    rest.find(c).map(|e| &rest[..e])
}

/// The fixes a diagnostic's hints make: an exact replacement when the
/// session gives one, and for an unknown name that has a "did you mean",
/// the name replaced.
fn fixes(d: &Value, src: &SourceFile, start: u32) -> Vec<Value> {
    let mut out = Vec::new();
    let hints = d
        .get("hints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for h in &hints {
        let msg = h.get("message").and_then(Value::as_str).unwrap_or("");
        if let Some(r) = h.get("replace") {
            let (Some(span), Some(text)) = (r.get("span"), r.get("text").and_then(Value::as_str))
            else {
                continue;
            };
            let at = |k: &str| -> Option<u32> {
                let p = span.get(k)?;
                Some(proto::line_col_offset(
                    src,
                    p.get("line")?.as_u64()? as u32,
                    p.get("column")?.as_u64()? as u32,
                ))
            };
            if let (Some(a), Some(b)) = (at("start"), at("end")) {
                out.push(json!({
                    "title": if msg.is_empty() { format!("Replace with '{text}'") } else { msg.to_string() },
                    "edits": [{"range": proto::range(src, (a, b)), "newText": text}],
                }));
            }
            continue;
        }
        let code = d.get("code").and_then(Value::as_str).unwrap_or("");
        let unknown = matches!(
            code,
            "unknown-module" | "unknown-function" | "unknown-variable"
        );
        let (Some(name), Some(suggestion)) = (
            d.get("message").and_then(Value::as_str).and_then(quoted),
            msg.strip_prefix("did you mean '")
                .and_then(|s| s.strip_suffix("'?")),
        ) else {
            continue;
        };
        let end = start + name.len() as u32;
        if unknown && src.text.get(start as usize..end as usize) == Some(name.as_bytes()) {
            out.push(json!({
                "title": format!("Change '{name}' to '{suggestion}'"),
                "edits": [{"range": proto::range(src, (start, end)), "newText": suggestion}],
            }));
        }
    }
    out
}

/// One session diagnostic in `src`, with its byte range.
fn diagnostic(d: &Value, src: &SourceFile) -> (Value, (u32, u32)) {
    let at = |k: &str| -> Option<u32> {
        let p = d.pointer(&format!("/span/{k}"))?;
        Some(proto::line_col_offset(
            src,
            p.get("line")?.as_u64()? as u32,
            p.get("column")?.as_u64()? as u32,
        ))
    };
    let span = match (at("start"), at("end")) {
        (Some(a), Some(b)) => (a, b.max(a)),
        _ => match d.get("line").and_then(Value::as_u64) {
            // A line without a span: the whole line.
            Some(l) => {
                let l = (l as u32).clamp(1, src.line_count());
                (src.line_start(l), src.line_end(l))
            }
            None => (0, 0),
        },
    };
    let severity = d.get("severity").and_then(Value::as_str).unwrap_or("error");
    let mut message = d
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    for h in d
        .get("hints")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(m) = h.get("message").and_then(Value::as_str) {
            message.push('\n');
            message.push_str(m);
        }
    }
    let mut v = json!({
        "range": proto::range(src, span),
        "severity": match severity {
            "error" => ERROR,
            "warning" => WARNING,
            _ => INFORMATION,
        },
        "source": "neoscad",
        "message": message,
    });
    if let Some(c) = d.get("code").and_then(Value::as_str) {
        v["code"] = json!(c);
    }
    if severity == "deprecated" {
        v["tags"] = json!([TAG_DEPRECATED]);
    }
    let fx = fixes(d, src, span.0);
    if !fx.is_empty() {
        v["data"] = json!({"fixes": fx});
    }
    (v, span)
}

/// The directive of the document through which `path` is part of its
/// program: the `include` (possibly of a file that includes it) or the
/// `use`.
fn directive_in_main(world: &World, path: &Path) -> Option<usize> {
    let mut p = path.to_path_buf();
    // A file a used library includes: the library's `use`.
    for lib in &world.libs {
        if lib.iter().any(|f| f.path == p) {
            p = lib[0].path.clone();
        }
    }
    for _ in 0..=world.files.len() {
        let (&(fi, di), _) = world.targets.iter().find(|(_, t)| **t == p)?;
        if fi == 0 {
            return Some(di);
        }
        p = world.files[fi].path.clone();
    }
    None
}

/// The session's diagnostics (`Log::diagnostics_json`) of the document
/// `world` is about, as `publishDiagnostics` parameters per URI: the
/// document's (always present, so an empty list clears it) and each
/// other file's that has any. `text_of` reads a file the world does not
/// hold.
pub fn convert(
    world: &World,
    libs: &[PathBuf],
    diags: &[Value],
    text_of: &dyn Fn(&Path) -> Option<Arc<SourceFile>>,
    uri_for: &dyn Fn(&Path) -> String,
) -> Vec<(String, Vec<Value>)> {
    let main = world.main();
    let mut by_path: Vec<(PathBuf, Vec<Value>)> = vec![(main.path.clone(), Vec::new())];
    let mut sources: HashMap<PathBuf, Option<Arc<SourceFile>>> = HashMap::new();
    for d in diags {
        let path = d
            .get("file")
            .and_then(Value::as_str)
            .map(|f| session::normal(Path::new(f)))
            .unwrap_or_else(|| main.path.clone());
        if path == main.path {
            let (v, _) = diagnostic(d, main.source());
            by_path[0].1.push(v);
            continue;
        }
        let known = world
            .files
            .iter()
            .chain(world.libs.iter().flatten())
            .find(|f| f.path == path)
            .map(|f| Arc::new(SourceFile::new(f.path.clone(), f.text().to_vec())));
        let src = sources
            .entry(path.clone())
            .or_insert_with(|| known.or_else(|| text_of(&path)))
            .clone();
        let Some(src) = src else {
            // A file that cannot be read: the message still shows on the
            // document, at its start.
            let (mut v, _) = diagnostic(
                &json!({"message": d.get("message"), "severity": d.get("severity"), "code": d.get("code"), "hints": d.get("hints")}),
                main.source(),
            );
            v["range"] = proto::range(main.source(), (0, 0));
            by_path[0].1.push(v);
            continue;
        };
        let (v, _) = diagnostic(d, &src);
        if let Some(di) = directive_in_main(world, &path) {
            let dir = &main.index.directives[di];
            let loc = describe::location(&main.path, libs, &path);
            let mut summary = json!({
                "range": proto::range(main.source(), dir.span),
                "severity": v["severity"],
                "source": "neoscad",
                "message": format!("In {loc}: {}", v["message"].as_str().unwrap_or("")),
                "relatedInformation": [{
                    "location": {"uri": uri_for(&path), "range": v["range"]},
                    "message": v["message"],
                }],
            });
            if let Some(c) = v.get("code") {
                summary["code"] = c.clone();
            }
            by_path[0].1.push(summary);
        }
        match by_path.iter_mut().find(|(p, _)| *p == path) {
            Some((_, list)) => list.push(v),
            None => by_path.push((path, vec![v])),
        }
    }
    by_path
        .into_iter()
        .map(|(p, list)| (uri_for(&p), list))
        .collect()
}

/// `textDocument/codeAction`: the fixes of the diagnostics in the
/// request's context (or, when the client sent them without their data,
/// of the last published ones overlapping the range).
pub fn code_actions(uri_s: &str, src: &SourceFile, params: &Value, published: &[Value]) -> Value {
    let only = params.pointer("/context/only").and_then(Value::as_array);
    if only.is_some_and(|o| {
        !o.iter()
            .any(|k| k.as_str().is_some_and(|k| k.starts_with("quickfix")))
    }) {
        return json!([]);
    }
    let mut diags: Vec<Value> = params
        .pointer("/context/diagnostics")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !diags.iter().any(|d| d.get("data").is_some()) {
        let range = params.get("range").and_then(|r| proto::offsets(src, r));
        diags = published
            .iter()
            .filter(|d| {
                let r = d.get("range").and_then(|r| proto::offsets(src, r));
                match (range, r) {
                    (Some((a, b)), Some((c, e))) => c <= b && a <= e,
                    _ => false,
                }
            })
            .cloned()
            .collect();
    }
    let mut out = Vec::new();
    for d in &diags {
        for fix in d
            .pointer("/data/fixes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            out.push(json!({
                "title": fix["title"],
                "kind": "quickfix",
                "diagnostics": [d],
                "isPreferred": true,
                "edit": {"changes": {uri_s: fix["edits"]}},
            }));
        }
    }
    Value::Array(out)
}
