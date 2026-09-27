//! Hover, signature help, go to definition, find references and rename:
//! everything that starts from the name under the cursor and what it
//! resolves to ([`crate::world::World::resolve_ref`]).

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::Ctx;
use crate::context;
use crate::describe;
use crate::index::{Ns, RefKind};
use crate::proto::{self, code};
use crate::world::{Analyzed, Found, Target};

/// What the cursor is on, and the range of the name there.
pub fn target_at(ctx: &Ctx<'_>, offset: u32) -> Option<(Target, (u32, u32))> {
    let f = ctx.file();
    if let Some(di) = context::directive_at(f, offset) {
        let p = ctx.world.targets.get(&(0, di))?;
        return Some((Target::File(p.clone()), f.index.directives[di].span));
    }
    if let Some(r) = f.index.ref_at(offset) {
        let t = ctx.world.resolve_ref(f, r)?;
        return Some((t, f.index.refs[r].span));
    }
    if let Some(d) = f.index.def_at(offset) {
        let found = Found {
            file: f.clone(),
            def: d,
        };
        return Some((Target::Def(found), f.index.defs[d].name_span));
    }
    // `let`, `for`, `echo` and `assert` in expressions are keywords of
    // the grammar, and still OpenSCAD's builtins.
    let w = context::word_at(f, offset)?;
    let name = f.slice(w);
    let mut b = crate::world::builtins(&name, Ns::Module);
    b.extend(crate::world::builtins(&name, Ns::Function));
    (!b.is_empty()).then_some((Target::Builtin(b), w))
}

fn offset_of(ctx: &Ctx<'_>, params: &Value) -> Option<u32> {
    params
        .get("position")
        .and_then(|p| proto::offset(ctx.file().source(), p))
}

fn location(ctx: &Ctx<'_>, file: &Analyzed, span: (u32, u32)) -> Value {
    json!({"uri": ctx.uri_for(&file.path), "range": proto::range(file.source(), span)})
}

pub fn hover(ctx: &Ctx<'_>, params: &Value) -> Value {
    let Some(offset) = offset_of(ctx, params) else {
        return Value::Null;
    };
    let Some((target, span)) = target_at(ctx, offset) else {
        return Value::Null;
    };
    let value = match &target {
        Target::Builtin(entries) => entries
            .iter()
            .map(|e| describe::builtin_markdown(e))
            .collect::<Vec<_>>()
            .join("\n---\n"),
        Target::Def(found) => describe::def_markdown(&ctx.world, &ctx.libs, found),
        Target::File(p) => format!(
            "`{}`",
            describe::location(&ctx.world.main().path, &ctx.libs, p)
        ),
    };
    json!({
        "contents": {"kind": "markdown", "value": value},
        "range": proto::range(ctx.file().source(), span),
    })
}

pub fn definition(ctx: &Ctx<'_>, params: &Value) -> Value {
    let Some(offset) = offset_of(ctx, params) else {
        return Value::Null;
    };
    match target_at(ctx, offset) {
        Some((Target::Def(found), _)) => location(ctx, &found.file, found.def().name_span),
        Some((Target::File(p), _)) => json!({
            "uri": ctx.uri_for(&p),
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
        }),
        _ => Value::Null,
    }
}

/// Every reference to `target` in the document and the files it
/// includes (each resolved in its own place, so a shadowing local of the
/// same name is left out).
fn references_to(ctx: &Ctx<'_>, target: &Found) -> Vec<(Arc<Analyzed>, (u32, u32))> {
    let name = &target.def().name;
    let mut out = Vec::new();
    for f in &ctx.world.files {
        for (i, r) in f.index.refs.iter().enumerate() {
            if &r.name != name {
                continue;
            }
            if let Some(Target::Def(found)) = ctx.world.resolve_ref(f, i)
                && found.same(target)
            {
                out.push((f.clone(), r.span));
            }
        }
    }
    out
}

pub fn references(ctx: &Ctx<'_>, params: &Value) -> Value {
    let Some(offset) = offset_of(ctx, params) else {
        return Value::Null;
    };
    let Some((Target::Def(found), _)) = target_at(ctx, offset) else {
        return json!([]);
    };
    let mut out = Vec::new();
    let declaration = params
        .pointer("/context/includeDeclaration")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if declaration && ctx.world.file_index(&found.file).is_some() {
        out.push(location(ctx, &found.file, found.def().name_span));
    }
    for (f, span) in references_to(ctx, &found) {
        out.push(location(ctx, &f, span));
    }
    Value::Array(out)
}

/// Why the name at `offset` cannot be renamed, or what renaming it
/// changes (the definition and its references, all in the document).
/// A rename's target, the range of the name at the cursor, and every
/// range to change.
type Plan = (Found, (u32, u32), Vec<(u32, u32)>);

fn rename_plan(ctx: &Ctx<'_>, offset: u32) -> Result<Plan, String> {
    let f = ctx.file();
    let (target, span) = target_at(ctx, offset).ok_or("there is no name here")?;
    let found = match target {
        Target::Def(found) => found,
        Target::Builtin(_) => return Err("OpenSCAD's builtins cannot be renamed".into()),
        Target::File(_) => return Err("an include or use cannot be renamed here".into()),
    };
    let d = found.def();
    if !Arc::ptr_eq(&found.file, f) {
        return Err(format!(
            "'{}' is defined in another file ({})",
            d.name,
            describe::location(&f.path, &ctx.libs, &found.file.path)
        ));
    }
    if d.name.starts_with('$') {
        return Err("special variables are dynamically scoped; renaming one is not safe".into());
    }
    let ns = d.kind.ns();
    // A top-level name that an included file also defines is one name to
    // OpenSCAD (includes are textual); renaming half of it changes which
    // definition wins.
    if d.scope == 0 {
        for other in ctx.world.files.iter().skip(1) {
            if other
                .index
                .defs_in(0)
                .any(|(_, x)| x.name == d.name && x.kind.ns() == ns)
            {
                return Err(format!(
                    "'{}' is also defined in the included file {}",
                    d.name,
                    describe::location(&f.path, &ctx.libs, &other.path)
                ));
            }
        }
    }
    let refs = references_to(ctx, &found);
    if let Some((other, _)) = refs.iter().find(|(file, _)| !Arc::ptr_eq(file, f)) {
        return Err(format!(
            "'{}' is used from the included file {}",
            d.name,
            describe::location(&f.path, &ctx.libs, &other.path)
        ));
    }
    let mut spans = vec![d.name_span];
    spans.extend(refs.into_iter().map(|(_, s)| s));
    Ok((found, span, spans))
}

pub fn prepare_rename(ctx: &Ctx<'_>, params: &Value) -> Result<Value, (i64, String)> {
    let offset = offset_of(ctx, params).ok_or((code::INVALID_PARAMS, "no position".to_string()))?;
    let (found, span, _) = rename_plan(ctx, offset).map_err(|e| (code::REQUEST_FAILED, e))?;
    Ok(json!({
        "range": proto::range(ctx.file().source(), span),
        "placeholder": found.def().name,
    }))
}

fn valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && lang::syntax::SyntaxKind::keyword(s.as_bytes()).is_none()
}

pub fn rename(ctx: &Ctx<'_>, params: &Value) -> Result<Value, (i64, String)> {
    let failed = |m: String| (code::REQUEST_FAILED, m);
    let offset = offset_of(ctx, params).ok_or((code::INVALID_PARAMS, "no position".to_string()))?;
    let new = params
        .get("newName")
        .and_then(Value::as_str)
        .ok_or((code::INVALID_PARAMS, "no newName".to_string()))?;
    if !valid_name(new) {
        return Err(failed(format!("'{new}' is not a valid OpenSCAD name")));
    }
    let (found, _, spans) = rename_plan(ctx, offset).map_err(failed)?;
    let f = ctx.file();
    let d = found.def();
    let ns = d.kind.ns();
    if new == d.name {
        return Ok(json!({"changes": {}}));
    }
    // The new name must not already mean something where the old one is
    // written (it would be shadowed or shadow), and no existing use of
    // the new name may fall into the renamed definition's reach.
    let scope_of = |at: u32| f.index.scope_at(at);
    for &(a, _) in &spans {
        if let Some(t) = ctx.world.resolve(f, scope_of(a), a, new, ns) {
            let what = match t {
                Target::Builtin(_) => "an OpenSCAD builtin".to_string(),
                _ => "already defined here".to_string(),
            };
            return Err(failed(format!(
                "'{new}' is {what}; renaming would change which definition is used"
            )));
        }
    }
    for file in &ctx.world.files {
        for r in &file.index.refs {
            let same_ns = match r.kind {
                RefKind::Module => ns == Ns::Module,
                RefKind::Function => ns != Ns::Module,
                RefKind::Variable => ns == Ns::Variable,
                RefKind::NamedArg => false,
            };
            let reach =
                d.scope == 0 || (Arc::ptr_eq(file, f) && f.index.encloses(d.scope, r.scope));
            if r.name == new && same_ns && reach {
                return Err(failed(format!(
                    "'{new}' is already used where the renamed name would capture it"
                )));
            }
        }
    }
    let edits: Vec<Value> = spans
        .iter()
        .map(|&s| json!({"range": proto::range(f.source(), s), "newText": new}))
        .collect();
    let mut changes = HashMap::new();
    changes.insert(ctx.uri.clone(), edits);
    Ok(json!({"changes": changes}))
}

/// A signature's label and its parameters' labels as UTF-16 offsets into
/// it (the client highlights the active one).
fn signature_info(
    label: String,
    params: &[String],
    docs: &[Option<String>],
    doc: Option<String>,
) -> Value {
    let mut out_params = Vec::new();
    let open = label.find('(').map_or(0, |i| i + 1);
    let mut from = open;
    for (i, p) in params.iter().enumerate() {
        let Some(at) = label[from..].find(p.as_str()).map(|x| x + from) else {
            continue;
        };
        let a = label[..at].encode_utf16().count();
        let b = a + p.encode_utf16().count();
        let mut pi = json!({"label": [a, b]});
        if let Some(Some(d)) = docs.get(i) {
            pi["documentation"] = json!(d);
        }
        out_params.push(pi);
        from = at + p.len();
    }
    let mut v = json!({"label": label, "parameters": out_params});
    if let Some(d) = doc {
        v["documentation"] = json!({"kind": "markdown", "value": d});
    }
    v
}

/// The top-level comma-separated pieces of `s`.
fn split_params(s: &str) -> Vec<String> {
    let (mut depth, mut cur, mut out) = (0i32, String::new(), Vec::new());
    for c in s.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

pub fn signature_help(ctx: &Ctx<'_>, params: &Value) -> Value {
    let Some(offset) = offset_of(ctx, params) else {
        return Value::Null;
    };
    let f = ctx.file();
    let Some(call) = context::enclosing_call(f, offset) else {
        return Value::Null;
    };
    let scope = f.index.scope_at(call.name_span.0);
    let order = if call.statement {
        [Ns::Module, Ns::Function]
    } else {
        [Ns::Function, Ns::Module]
    };
    let Some(target) = order.iter().find_map(|ns| {
        ctx.world
            .resolve(f, scope, call.name_span.0, &call.name, *ns)
    }) else {
        return Value::Null;
    };
    // Per signature: its parameter names, to find the active one.
    let mut sigs: Vec<(Value, Vec<String>)> = Vec::new();
    match target {
        Target::Builtin(entries) => {
            for e in entries {
                for alt in e.signature.split(" | ") {
                    let inner = alt
                        .find('(')
                        .and_then(|a| alt.rfind(')').map(|b| &alt[a + 1..b]))
                        .unwrap_or("");
                    let ps = split_params(inner);
                    let names: Vec<String> = ps
                        .iter()
                        .map(|p| p.split('=').next().unwrap_or(p).trim().to_string())
                        .collect();
                    let docs: Vec<Option<String>> = names
                        .iter()
                        .map(|n| {
                            e.params
                                .iter()
                                .find(|p| p.name.split(',').any(|x| x.trim() == n))
                                .map(|p| p.doc.clone())
                        })
                        .collect();
                    sigs.push((
                        signature_info(alt.to_string(), &ps, &docs, Some(e.summary.clone())),
                        names,
                    ));
                }
            }
        }
        Target::Def(found) => {
            let d = found.def();
            let ps: Vec<String> = d
                .params
                .iter()
                .map(|p| match &p.default {
                    Some(v) => format!("{}={}", p.name, describe::one_line(v, 40)),
                    None => p.name.clone(),
                })
                .collect();
            let docs: Vec<Option<String>> = d
                .params
                .iter()
                .map(|p| describe::argument_doc(&found, &p.name))
                .collect();
            let label = format!("{}({})", d.name, ps.join(", "));
            let names = d.params.iter().map(|p| p.name.clone()).collect();
            sigs.push((
                signature_info(label, &ps, &docs, describe::summary(&found)),
                names,
            ));
        }
        Target::File(_) => return Value::Null,
    }
    let active = |names: &[String]| -> usize {
        match &call.named {
            Some(n) => names.iter().position(|x| x == n).unwrap_or(names.len()),
            None => call.arg,
        }
    };
    // The first signature with room for the argument the cursor is in.
    let best = sigs
        .iter()
        .position(|(_, names)| active(names) < names.len())
        .unwrap_or(0);
    let signatures: Vec<Value> = sigs
        .iter()
        .map(|(v, names)| {
            let mut v = v.clone();
            v["activeParameter"] = json!(active(names));
            v
        })
        .collect();
    json!({
        "signatures": signatures,
        "activeSignature": best,
        "activeParameter": sigs.get(best).map_or(0, |(_, n)| active(n)),
    })
}
