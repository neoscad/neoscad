//! NeoSCAD's constrained sketches (`--enable sketch`;
//! `docs/language-extensions.md`, section 4.8) in the editor.
//!
//! - **The vocabulary** (`point`, `line`, `horizontal`, `fillet`, ...) is
//!   bound only inside sketch bodies, as in the evaluator: resolution,
//!   hover and completion find it there ([`crate::world::World::resolve`])
//!   and nowhere else, so BOSL2's `arc()` keeps its meaning outside.
//! - **A run's sketch facts** ([`session::Log::sketches`]) travel with
//!   its diagnostics, through the same path: the server's own evaluation,
//!   or the host's run ([`crate::Server::supply_log`]). They give hover
//!   the solved values of an entity variable and the "Pin drawing" edit,
//!   offered as a code action and as a fix on the sketch's own
//!   diagnostics (the app's editor offers fixes from diagnostics alone).
//!   They are used only while the document's text is the text the run
//!   read: a span in another text would point at the wrong code.

use std::path::Path;
use std::sync::Arc;

use lang::source::SourceFile;
use serde_json::{Value, json};

use crate::index::{Ns, RefKind};
use crate::proto;
use crate::world::{Analyzed, Target};

/// Whether a builtin is part of the sketch vocabulary: bound only in
/// sketch bodies. `sketch` itself is an ordinary (extension) builtin.
pub fn is_vocabulary(e: &docs::Entry) -> bool {
    e.extension.as_deref() == Some("sketch") && e.name != "sketch"
}

pub fn ns_of(e: &docs::Entry) -> Ns {
    match e.kind {
        docs::Kind::Module => Ns::Module,
        docs::Kind::Function => Ns::Function,
        docs::Kind::Variable => Ns::Variable,
    }
}

/// The vocabulary's entries.
pub fn vocabulary_all() -> impl Iterator<Item = &'static docs::Entry> {
    docs::builtins().iter().filter(|e| is_vocabulary(e))
}

/// The vocabulary's entries called `name` in namespace `ns`: entities
/// are functions, constraints statements.
pub fn vocabulary(name: &str, ns: Ns) -> Vec<&'static docs::Entry> {
    vocabulary_all()
        .filter(|e| e.name == name && ns_of(e) == ns)
        .collect()
}

/// What accepting a vocabulary name inserts, in LSP snippet syntax: the
/// entities with their usual arguments, the statements with a
/// placeholder per handle and dimension.
const SNIPPETS: &[(&str, &str)] = &[
    ("point", "point([${1:0}, ${2:0}])"),
    ("line", "line(${1:p}, ${2:q})"),
    ("arc", "arc(${1:center}, ${2:start}, ${3:end})"),
    ("circle", "circle(${1:center}, r = ${2:5})"),
    ("coincident", "coincident(${1:a}, ${2:b});"),
    ("on", "on(${1:point}, ${2:curve});"),
    ("horizontal", "horizontal(${1:line});"),
    ("vertical", "vertical(${1:line});"),
    ("parallel", "parallel(${1:l1}, ${2:l2});"),
    ("perpendicular", "perpendicular(${1:l1}, ${2:l2});"),
    ("tangent", "tangent(${1:a}, ${2:b});"),
    ("distance", "distance(${1:a}, ${2:b}, ${3:10});"),
    ("length", "length(${1:line}, ${2:10});"),
    ("radius", "radius(${1:curve}, ${2:5});"),
    ("diameter", "diameter(${1:curve}, ${2:10});"),
    ("angle", "angle(${1:l1}, ${2:l2}, ${3:90});"),
    ("equal", "equal(${1:a}, ${2:b});"),
    ("midpoint", "midpoint(${1:point}, ${2:line});"),
    ("symmetric", "symmetric(${1:p}, ${2:q}, ${3:about});"),
    ("fix", "fix(${1:point});"),
    ("fillet", "fillet(${1:corner}, ${2:2});"),
    ("chamfer", "chamfer(${1:corner}, ${2:1});"),
];

pub fn snippet(name: &str) -> Option<&'static str> {
    SNIPPETS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// A run's sketches and the text it read.
#[derive(Debug, Clone)]
pub struct Facts {
    pub text: Arc<[u8]>,
    pub sketches: Arc<(Vec<Value>, usize)>,
}

impl Facts {
    /// The sketches, if `text` is the text the run read.
    pub fn for_text(&self, text: &[u8]) -> Option<&[Value]> {
        (*self.text == *text).then_some(&self.sketches.0[..])
    }
}

/// Whether a fact (a sketch, an entity, an edit) is in the file `path`.
fn in_file(v: &Value, path: &Path) -> bool {
    v["file"]
        .as_str()
        .is_some_and(|f| session::normal(Path::new(f)) == session::normal(path))
}

/// The byte range of a fact's `span` in `src`.
fn span_of(src: &SourceFile, v: &Value) -> Option<(u32, u32)> {
    let at = |k: &str| -> Option<u32> {
        let p = &v["span"][k];
        Some(proto::line_col_offset(
            src,
            p["line"].as_u64()? as u32,
            p["column"].as_u64()? as u32,
        ))
    };
    Some((at("start")?, at("end")?))
}

/// The title of the "Pin drawing" action.
pub const PIN_TITLE: &str = "Pin drawing: rewrite the guesses to the solved coordinates";

/// A sketch's "Pin drawing" as a fix (`{title, edits}`), when it has one
/// in `src`.
fn pin_fix(src: &SourceFile, path: &Path, sketch: &Value) -> Option<Value> {
    let pin = sketch.get("pin")?;
    if !in_file(pin, path) {
        return None;
    }
    let span = span_of(src, pin)?;
    let name = match sketch["name"].as_str() {
        Some(n) => format!(" ('{n}')"),
        None => String::new(),
    };
    Some(json!({
        "title": format!("{PIN_TITLE}{name}"),
        "edits": [{"range": proto::range(src, span), "newText": pin["text"]}],
    }))
}

/// The sketches of `path` whose call covers `(a, b)`, innermost last.
fn around<'a>(
    src: &SourceFile,
    path: &Path,
    sketches: &'a [Value],
    (a, b): (u32, u32),
) -> Vec<&'a Value> {
    sketches
        .iter()
        .filter(|s| in_file(s, path))
        .filter(|s| span_of(src, s).is_some_and(|(x, y)| x <= a && b <= y))
        .collect()
}

/// `textDocument/codeAction`'s refactorings: "Pin drawing" for the
/// sketch the range is in.
pub fn pin_actions(
    uri: &str,
    src: &SourceFile,
    path: &Path,
    sketches: &[Value],
    range: (u32, u32),
) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for s in around(src, path, sketches, range) {
        let Some(fix) = pin_fix(src, path, s) else {
            continue;
        };
        // A sketch in a module called twice is two facts with one edit
        // each, the same or not; the first stands.
        if out.iter().any(|a| a["title"] == fix["title"]) {
            continue;
        }
        out.push(json!({
            "title": fix["title"],
            "kind": "refactor.rewrite",
            "edit": {"changes": {uri: fix["edits"]}},
        }));
    }
    out
}

/// Add "Pin drawing" to the fixes of each sketch diagnostic (a code
/// starting `sketch-`) inside a sketch that has the edit, unless the
/// diagnostic carries that edit already (a flip's hint is this edit).
/// The app's editor offers fixes only from diagnostics.
pub fn attach_pins(list: &mut [Value], src: &SourceFile, path: &Path, sketches: &[Value]) {
    for d in list.iter_mut() {
        if !d["code"].as_str().is_some_and(|c| c.starts_with("sketch-")) {
            continue;
        }
        let Some(r) = d.get("range").and_then(|r| proto::offsets(src, r)) else {
            continue;
        };
        let Some(fix) = around(src, path, sketches, r)
            .last()
            .and_then(|s| pin_fix(src, path, s))
        else {
            continue;
        };
        let text = &fix["edits"][0]["newText"];
        let fixes = &mut d["data"]["fixes"];
        if !fixes.is_array() {
            *fixes = json!([]);
        }
        let Some(list) = fixes.as_array_mut() else {
            continue;
        };
        if list.iter().any(|f| f["edits"][0]["newText"] == *text) {
            continue;
        }
        list.push(fix);
    }
}

/// An entity's solved values as hover text: "line, solved: [0, 4] to
/// [30, 4], length 30, angle 0°".
fn entity_markdown(e: &Value) -> String {
    let text = session::sketches::entity_text(e);
    // `entity_text` starts with the name, which the hover already shows.
    let rest = match e["name"].as_str() {
        Some(n) => text.strip_prefix(n).unwrap_or(&text).trim_start(),
        None => &text,
    };
    format!("Solved: `{rest}`")
}

/// Hover's solved values for the variable whose value is at `value`: the
/// entity a call there made, in the last run of this text. The same
/// sketch solved more than once (a helper called twice) shows the first.
pub fn hover_values(
    src: &SourceFile,
    path: &Path,
    sketches: &[Value],
    value: (u32, u32),
) -> Option<String> {
    for s in sketches {
        for e in s["entities"].as_array().into_iter().flatten() {
            if in_file(e, path) && span_of(src, e) == Some(value) {
                let mut out = entity_markdown(e);
                let state = session::sketches::state_text(s);
                let sk = match s["name"].as_str() {
                    Some(n) => format!("sketch '{n}'"),
                    None => "the sketch".to_string(),
                };
                out.push_str(&format!(" ({sk}: {state})"));
                return Some(out);
            }
        }
    }
    None
}

/// The DOF summary hover shows on a `sketch(` call.
pub fn hover_sketch(
    src: &SourceFile,
    path: &Path,
    sketches: &[Value],
    name: (u32, u32),
) -> Option<String> {
    let s = sketches
        .iter()
        .find(|s| in_file(s, path) && span_of(src, s).is_some_and(|(a, _)| a == name.0))?;
    Some(format!("Last run: {}", session::sketches::line_text(s)))
}

/// The arguments of the call that is the whole of `span` (an entity's
/// assignment, `line([0, 4], q)`): its name, and per argument its name
/// when given by name and the range of its expression.
type Args = (String, Vec<(Option<String>, (u32, u32))>);

fn call_args(f: &Analyzed, (a, b): (u32, u32)) -> Option<Args> {
    use lang::syntax::SyntaxKind as K;
    let toks: Vec<_> = f
        .program
        .cst
        .tokens()
        .iter()
        .filter(|t| !t.kind.is_trivia() && t.start >= a && t.end() <= b)
        .collect();
    let (name, open) = (toks.first()?, toks.get(1)?);
    if name.kind != K::Ident || open.kind != K::LParen {
        return None;
    }
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start: Option<usize> = None;
    let mut close = false;
    for (i, t) in toks.iter().enumerate().skip(2) {
        match t.kind {
            K::RParen | K::Comma if depth == 0 => {
                if let Some(s) = start.take() {
                    let arg = &toks[s..i];
                    let named = arg.len() > 2 && arg[0].kind == K::Ident && arg[1].kind == K::Eq;
                    let expr = if named { &arg[2..] } else { arg };
                    if let (Some(first), Some(last)) = (expr.first(), expr.last()) {
                        let n = named.then(|| f.slice((arg[0].start, arg[0].end())));
                        out.push((n, (first.start, last.end())));
                    }
                }
                if t.kind == K::RParen {
                    close = true;
                    break;
                }
                continue;
            }
            K::LParen | K::LBrack | K::LBrace => depth += 1,
            K::RParen | K::RBrack | K::RBrace => depth -= 1,
            _ => {}
        }
        if start.is_none() {
            start = Some(i);
        }
    }
    close.then(|| (f.slice((name.start, name.end())), out))
}

/// Which argument of an entity call a member names: `.start` of a line
/// is its first point, of an arc its second, and so on.
fn member_param(call: &str, member: &str) -> Option<(usize, &'static str)> {
    Some(match (call, member) {
        ("line", "start") => (0, "p"),
        ("line", "end") => (1, "q"),
        ("arc", "center") => (0, "center"),
        ("arc", "start") => (1, "start"),
        ("arc", "end") => (2, "end"),
        ("circle", "center") => (0, "center"),
        _ => return None,
    })
}

/// Go to definition on a handle's member (`top.start`, `e1.center`) in a
/// sketch body: the point it is, where the entity was made. A point given
/// by a variable goes to that variable's definition; one given as another
/// handle's member (`top.start`) goes on to that member; one written as
/// `[x, y]` goes to that literal.
pub fn member_definition(ctx: &crate::Ctx<'_>, offset: u32) -> Option<(Arc<Analyzed>, (u32, u32))> {
    use lang::syntax::SyntaxKind as K;
    let mut file = ctx.file().clone();
    let mut at = offset;
    // `a.start` whose point is written `b.end`: a few steps at most.
    for _ in 0..4 {
        let f = file.clone();
        let w = crate::context::word_at(&f, at)?;
        let member = f.slice(w);
        let dot = crate::context::previous(&f, w.0)?;
        if dot.kind != K::Dot {
            return None;
        }
        let r = f
            .index
            .ref_at(crate::context::previous(&f, dot.start)?.start)?;
        if f.index.refs[r].kind != RefKind::Variable {
            return None;
        }
        ctx.world.sketch_body(&f, f.index.refs[r].scope)?;
        let Target::Def(found) = ctx.world.resolve_ref(&f, r)? else {
            return None;
        };
        let value = found.def().value?;
        let (call, args) = call_args(&found.file, value)?;
        let (pos, param) = member_param(&call, &member)?;
        let arg = args
            .iter()
            .find(|(n, _)| n.as_deref() == Some(param))
            .or_else(|| args.iter().filter(|(n, _)| n.is_none()).nth(pos))?
            .1;
        let text = found.file.slice(arg);
        let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        if !text.is_empty() && text.bytes().all(word) {
            let ar = found.file.index.ref_at(arg.0)?;
            let Target::Def(d) = ctx.world.resolve_ref(&found.file, ar)? else {
                return None;
            };
            let span = d.def().name_span;
            return Some((d.file.clone(), span));
        }
        if !(text.contains('.') && text.bytes().all(|c| word(c) || c == b'.' || c == b' ')) {
            return Some((found.file.clone(), arg));
        }
        file = found.file.clone();
        at = arg.1;
    }
    None
}
