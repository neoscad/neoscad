//! Completion: the names visible at the cursor (local, the document's,
//! its includes', its `use`d libraries', the builtins, `$` variables and
//! keywords), the parameters of the call being typed as `name=`, and
//! snippets for the common builtins.
//!
//! Statement positions offer modules and expression positions functions
//! and variables. The list is filtered on the server by the name typed
//! so far (loosely: its letters in order, so the editor's own fuzzy
//! filtering finds everything it would), because a BOSL2 model sees two
//! thousand names and most requests are for a letter or two.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::Ctx;
use crate::context::{self, Call};
use crate::describe;
use crate::index::{DefKind, Ns};
use crate::proto;
use crate::world::{Candidate, Origin, Target};

/// Past this many items the list is cut and marked incomplete, so the
/// editor asks again as the name grows.
const MAX_ITEMS: usize = 400;

// The protocol's `CompletionItemKind`s.
const KIND_FUNCTION: u8 = 3;
const KIND_FIELD: u8 = 5;
const KIND_VARIABLE: u8 = 6;
const KIND_MODULE: u8 = 9;
const KIND_KEYWORD: u8 = 14;
const KIND_SNIPPET: u8 = 15;
const KIND_CONSTANT: u8 = 21;

const STATEMENT_KEYWORDS: &[&str] = &[
    "module",
    "function",
    "if",
    "else",
    "for",
    "let",
    "include",
    "use",
    "intersection_for",
];
const EXPRESSION_KEYWORDS: &[&str] = &[
    "true", "false", "undef", "let", "for", "each", "if", "function",
];

/// Snippets for the common builtins, in LSP snippet syntax: what typing
/// the name and accepting inserts at a statement.
const SNIPPETS: &[(&str, &str)] = &[
    ("cube", "cube([${1:10}, ${2:10}, ${3:10}]);"),
    ("sphere", "sphere(r=${1:5});"),
    ("cylinder", "cylinder(h=${1:10}, r=${2:5});"),
    ("square", "square([${1:10}, ${2:10}]);"),
    ("circle", "circle(r=${1:5});"),
    ("polygon", "polygon(points=[${1}]);"),
    ("polyhedron", "polyhedron(points=[${1}], faces=[${2}]);"),
    ("text", "text(\"${1}\", size=${2:10});"),
    ("translate", "translate([${1:0}, ${2:0}, ${3:0}]) ${0}"),
    ("rotate", "rotate([${1:0}, ${2:0}, ${3:0}]) ${0}"),
    ("scale", "scale([${1:1}, ${2:1}, ${3:1}]) ${0}"),
    ("mirror", "mirror([${1:1}, ${2:0}, ${3:0}]) ${0}"),
    ("resize", "resize([${1:10}, ${2:10}, ${3:10}]) ${0}"),
    ("color", "color(\"${1:red}\") ${0}"),
    ("union", "union() {\n\t${0}\n}"),
    ("difference", "difference() {\n\t${0}\n}"),
    ("intersection", "intersection() {\n\t${0}\n}"),
    ("hull", "hull() {\n\t${0}\n}"),
    ("minkowski", "minkowski() {\n\t${0}\n}"),
    ("linear_extrude", "linear_extrude(height=${1:10}) ${0}"),
    ("rotate_extrude", "rotate_extrude(angle=${1:360}) ${0}"),
    ("offset", "offset(r=${1:1}) ${0}"),
    ("projection", "projection(cut=${1:false}) ${0}"),
    ("import", "import(\"${1}\");"),
];

const KEYWORD_SNIPPETS: &[(&str, &str)] = &[
    ("module", "module ${1:name}(${2}) {\n\t${0}\n}"),
    ("function", "function ${1:name}(${2}) = ${0};"),
    ("for", "for (${1:i} = [${2:0}:${3:9}]) ${0}"),
    ("if", "if (${1:condition}) {\n\t${0}\n}"),
    ("include", "include <${1}>"),
    ("use", "use <${1}>"),
];

/// Whether `name` has the letters of `typed` in order, ignoring case.
fn matches(name: &str, typed: &str) -> bool {
    let mut it = name.chars().flat_map(char::to_lowercase);
    typed
        .chars()
        .flat_map(char::to_lowercase)
        .all(|c| it.any(|n| n == c))
}

pub fn completion(ctx: &Ctx<'_>, params: &Value) -> Value {
    let f = ctx.file();
    let Some(offset) = params
        .get("position")
        .and_then(|p| proto::offset(f.source(), p))
    else {
        return Value::Null;
    };
    if context::in_comment_or_string(f, offset) || context::directive_at(f, offset).is_some() {
        return json!({"isIncomplete": false, "items": []});
    }
    let start = context::prefix_start(f, offset);
    let typed = f.slice((start, offset));
    let empty = json!({"isIncomplete": false, "items": []});
    if typed.starts_with(|c: char| c.is_ascii_digit()) {
        return empty;
    }
    if context::previous(f, start).is_some_and(|t| t.kind == lang::syntax::SyntaxKind::Dot) {
        return empty;
    }
    // A statement starts here, unless the cursor is in a call's arguments.
    let call = context::enclosing_call(f, start);
    let statement = call.is_none() && context::statement_position(f, start);
    let private = typed.starts_with('_');
    let mut items: Vec<(String, Value)> = Vec::new();
    let mut labels: HashSet<String> = HashSet::new();
    if let Some(c) = &call
        && c.arg_start
        && c.named.is_none()
    {
        for (name, detail) in call_params(ctx, c) {
            if matches(&name, &typed) && labels.insert(format!("{name}=")) {
                let mut item = json!({
                    "label": format!("{name}="),
                    "kind": KIND_FIELD,
                    "sortText": format!("0{name}"),
                });
                if let Some(d) = detail {
                    item["detail"] = json!(d);
                }
                items.push((format!("0{name}"), item));
            }
        }
    }
    let scope = f.index.scope_at(offset);
    for c in ctx.world.visible(f, scope, offset) {
        let wanted = match c.ns {
            Ns::Module => statement,
            Ns::Function => !statement,
            Ns::Variable => true,
        };
        if !wanted || !matches(&c.name, &typed) {
            continue;
        }
        // Library helpers named `_x` are private by BOSL2's (and common)
        // convention: offered only when asked for.
        if c.name.starts_with('_') && !private && c.origin >= Origin::Include {
            continue;
        }
        let (sort, item) = candidate_item(ctx, &c, statement);
        if labels.insert(c.name.clone()) {
            items.push((sort, item));
        }
    }
    let keywords = if statement {
        STATEMENT_KEYWORDS
    } else {
        EXPRESSION_KEYWORDS
    };
    for k in keywords {
        if matches(k, &typed) && labels.insert((*k).to_string()) {
            let mut item = json!({"label": k, "kind": KIND_KEYWORD, "sortText": format!("5{k}")});
            if statement && let Some((_, s)) = KEYWORD_SNIPPETS.iter().find(|(n, _)| n == k) {
                item["insertText"] = json!(s);
                item["insertTextFormat"] = json!(2);
                item["kind"] = json!(KIND_SNIPPET);
            }
            items.push((format!("5{k}"), item));
        }
    }
    items.sort_by(|a, b| a.0.cmp(&b.0));
    let incomplete = items.len() > MAX_ITEMS;
    items.truncate(MAX_ITEMS);
    json!({
        "isIncomplete": incomplete,
        "itemDefaults": {"editRange": proto::range(f.source(), (start, offset))},
        "items": items.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
    })
}

/// The item for a visible name, with its sort key: the call's parameters
/// first, then local names, the document's, its includes', its
/// libraries', the builtins; variables after modules at a statement.
fn candidate_item(ctx: &Ctx<'_>, c: &Candidate, statement: bool) -> (String, Value) {
    let rank = match c.origin {
        Origin::Local => 1,
        Origin::File => 2,
        Origin::Include | Origin::Library => 3,
        Origin::Builtin => 4,
    };
    let rank = if statement && c.ns == Ns::Variable {
        6
    } else {
        rank
    };
    let sort = format!("{rank}{}", c.name);
    let mut item = json!({"label": c.name, "sortText": sort});
    if let Some(e) = c.builtin {
        item["kind"] = json!(match c.ns {
            Ns::Module => KIND_MODULE,
            Ns::Function => KIND_FUNCTION,
            Ns::Variable => KIND_CONSTANT,
        });
        // An extension's label goes in the detail, which editors show
        // beside the name in the list: a reader picking `part` should see
        // it is NeoSCAD's before choosing it, not only in the hover.
        item["detail"] = json!(match e.extension_label() {
            Some(l) => format!("{}  {l}", e.signature),
            None => e.signature.clone(),
        });
        item["documentation"] = json!({"kind": "markdown", "value": e.summary});
        if crate::sketch::is_vocabulary(e) {
            // Offered only inside a sketch body (`World::visible`): a
            // statement with its `;`, an entity as an expression.
            if let Some(s) = crate::sketch::snippet(&c.name) {
                item["insertText"] = json!(s);
                item["insertTextFormat"] = json!(2);
            }
        } else if statement
            && c.ns == Ns::Module
            && let Some((_, s)) = SNIPPETS.iter().find(|(n, _)| *n == c.name)
        {
            item["insertText"] = json!(s);
            item["insertTextFormat"] = json!(2);
        }
    } else if let Some(found) = &c.found {
        let d = found.def();
        item["kind"] = json!(match d.kind {
            DefKind::Module => KIND_MODULE,
            DefKind::Function => KIND_FUNCTION,
            _ => KIND_VARIABLE,
        });
        let detail = match d.kind {
            DefKind::Module | DefKind::Function => describe::signature(d),
            _ => match d.value {
                Some(v) => format!("= {}", describe::one_line(&found.file.slice(v), 60)),
                None => "parameter".to_string(),
            },
        };
        item["detail"] = json!(detail);
        if c.origin >= Origin::Include {
            let loc = describe::location(&ctx.world.main().path, &ctx.libs, &found.file.path);
            let doc = match describe::summary(found) {
                Some(s) => format!("{s}\n\n*{loc}*"),
                None => format!("*{loc}*"),
            };
            item["documentation"] = json!({"kind": "markdown", "value": doc});
        } else if let Some(s) = describe::summary(found) {
            item["documentation"] = json!({"kind": "markdown", "value": s});
        }
    }
    (sort, item)
}

/// The parameters of the called module or function, with a note each.
fn call_params(ctx: &Ctx<'_>, c: &Call) -> Vec<(String, Option<String>)> {
    let f = ctx.file();
    let scope = f.index.scope_at(c.name_span.0);
    let order = if c.statement {
        [Ns::Module, Ns::Function]
    } else {
        [Ns::Function, Ns::Module]
    };
    let target = order
        .iter()
        .find_map(|ns| ctx.world.resolve(f, scope, c.name_span.0, &c.name, *ns));
    match target {
        Some(Target::Def(found)) => found
            .def()
            .params
            .iter()
            .map(|p| {
                let note = describe::argument_doc(&found, &p.name).or_else(|| {
                    p.default
                        .as_ref()
                        .map(|d| format!("= {}", describe::one_line(d, 40)))
                });
                (p.name.clone(), note)
            })
            .collect(),
        Some(Target::Builtin(entries)) => {
            let mut out: Vec<(String, Option<String>)> = Vec::new();
            for e in entries {
                for p in &e.params {
                    for name in p.name.split(',').map(str::trim) {
                        if !name.is_empty() && !out.iter().any(|(n, _)| n == name) {
                            let note = (!p.doc.is_empty()).then(|| p.doc.clone());
                            out.push((name.to_string(), note));
                        }
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    }
}
