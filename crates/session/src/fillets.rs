//! What the tools say about a run's `fillet_edges()`/`chamfer_edges()`
//! calls (`--enable fillet`; `docs/fillets.md`, stage F1): each call's
//! selection on its child, as diagnostics at the call and as JSON for
//! `check` (`fillets`), `measure --fillet` and `snapshot --fillet`, and
//! the "Pin count" edit the language server offers.
//!
//! Selection needs the child's geometry, so unlike a sketch's facts these
//! come after the render: [`report`] walks the rendered tree for fillet
//! nodes and asks `geom::fillet` for each one's plan (the child's facts
//! are cached on the renderer, so a warm request costs a selection per
//! call). The command line and the session call the same function, so
//! every host prints the same lines.

use std::path::Path;

use eval::Node;
use eval::node::NodeKind;
use geom::fillet::{self, EdgeFact, Facts, Plan, Status};
use geom::{RenderOptions, Renderer};
use lang::diag::{Diagnostic, Hint, PathBase};
use lang::source::{SourceMap, Span};
use serde_json::{Value, json};

/// The selector language (`docs/fillets.md`, section 5.2), for the
/// language server's completion and hover inside an `edges` string.
pub use eval::fillet::selector;

/// The most calls a run reports (a call in a loop is one per iteration);
/// past this they are counted (`fillets_omitted`) but not listed.
pub const MAX_FILLETS: usize = 100;

/// The most edges one call lists; past this, `edges_omitted`.
pub const MAX_EDGES: usize = 500;

/// A run's fillet calls: their reports as JSON and their plans (which
/// `snapshot --fillet` draws), the first [`MAX_FILLETS`] of each, and how
/// many calls there were.
#[derive(Debug, Default, Clone)]
pub struct Reports {
    pub json: Vec<Value>,
    pub count: usize,
    pub plans: Vec<Plan>,
}

/// A span as the diagnostics' JSON writes it.
fn place(sources: &SourceMap, span: Span) -> Value {
    let f = sources.get(span.file);
    let (a, b) = (f.line_col(span.start), f.line_col(span.end.max(span.start)));
    json!({
        "file": f.path.to_string_lossy(),
        "line": a.0,
        "span": {"start": {"line": a.0, "column": a.1}, "end": {"line": b.0, "column": b.1}},
    })
}

fn p4(p: [f64; 3]) -> Value {
    json!([
        fillet::round4(p[0]),
        fillet::round4(p[1]),
        fillet::round4(p[2])
    ])
}

/// The fillet nodes of a rendered tree in pre-order, skipping background
/// (`%`) subtrees, which the render leaves out.
fn calls(top: &Node) -> Vec<&Node> {
    let mut out = Vec::new();
    let mut stack = vec![top];
    while let Some(n) = stack.pop() {
        if n.origin.as_ref().is_some_and(|o| o.tag_background) {
            continue;
        }
        if matches!(n.kind, NodeKind::Fillet(_)) {
            out.push(n);
        }
        stack.extend(n.children.iter().rev());
    }
    out
}

/// Whether the tree has a fillet call at all: a run without one skips the
/// pass entirely.
pub fn any(top: &Node) -> bool {
    let mut stack = vec![top];
    while let Some(n) = stack.pop() {
        if matches!(n.kind, NodeKind::Fillet(_)) {
            return true;
        }
        stack.extend(n.children.iter());
    }
    false
}

/// The edit that writes `expect = n` into the call at `span`: the value
/// of an existing `expect` argument replaced, or `, expect = n` after the
/// last argument. `None` when the call is not in the program's text as
/// an instantiation of the module (a `.csg` round trip, a generated
/// call).
pub fn pin_edit(program: &lang::Program, span: Span, n: usize) -> Option<(Span, String)> {
    let ast = &program.ast;
    let inst = crate::orient::find_inst(&ast.root, span)?;
    let name = ast.name(inst.name);
    if name != "fillet_edges" && name != "chamfer_edges" {
        return None;
    }
    let named = inst
        .args
        .iter()
        .rev()
        .find(|a| a.name.is_some_and(|x| ast.name(x) == "expect"));
    let positional = inst.args.iter().filter(|a| a.name.is_none()).nth(3);
    if let Some(a) = named.or(positional) {
        return Some((ast.expr(a.expr).span, n.to_string()));
    }
    let last = inst.args.last()?;
    let text = &program.sources.get(last.span.file).text;
    let src =
        std::str::from_utf8(text.get(last.span.start as usize..last.span.end as usize)?).ok()?;
    Some((last.span, format!("{src}, expect = {n}")))
}

/// The fillet or chamfer call at `span` as written, and its program text.
fn call_at(program: &lang::Program, span: Span) -> Option<(&lang::ast::Instantiation, &str)> {
    let ast = &program.ast;
    let inst = crate::orient::find_inst(&ast.root, span)?;
    let name = ast.name(inst.name);
    if name != "fillet_edges" && name != "chamfer_edges" {
        return None;
    }
    let text = std::str::from_utf8(&program.sources.get(span.file).text).ok()?;
    Some((inst, text))
}

/// The span of the size argument (`r` or `d`, named or first) of the
/// call at `span`, as written.
fn size_span(program: &lang::Program, span: Span) -> Option<Span> {
    let (inst, _) = call_at(program, span)?;
    let ast = &program.ast;
    let named = inst
        .args
        .iter()
        .find(|a| a.name.is_some_and(|x| matches!(ast.name(x), "r" | "d")));
    let first = inst.args.first().filter(|a| a.name.is_none());
    let a = named.or(first)?;
    Some(ast.expr(a.expr).span)
}

/// The edit that writes `value` as the size of the call at `span`: the
/// expression of its `r` (or `d`) argument, named or first, replaced.
pub fn size_edit(program: &lang::Program, span: Span, value: &str) -> Option<(Span, String)> {
    Some((size_span(program, span)?, value.to_string()))
}

/// The span of the `edges` argument (named, or second) of the call at
/// `span`, as written.
fn edges_span(program: &lang::Program, span: Span) -> Option<Span> {
    let (inst, _) = call_at(program, span)?;
    let ast = &program.ast;
    let named = inst
        .args
        .iter()
        .find(|a| a.name.is_some_and(|x| ast.name(x) == "edges"));
    let positional = inst.args.iter().filter(|a| a.name.is_none()).nth(1);
    Some(ast.expr(named.or(positional)?.expr).span)
}

/// Where a call's diagnostic points (`docs/fillets.md`, section 15.5): a
/// problem with what the selector matched at the `edges` argument, a
/// size that does not fit at `r`/`d`, anything else at the call. An
/// editor underlines that span, so the marker sits on the text to change
/// rather than on the whole call and its children, which for a call
/// around a `difference()` is most of the model. The console's line
/// stays the call's, where OpenSCAD's messages put it.
fn diag_span(program: &lang::Program, call: Span, code: lang::diag::DiagCode) -> Span {
    use lang::diag::DiagCode as C;
    let at = match code {
        C::FilletCount | C::FilletNoEdges | C::FilletSkipped | C::FilletUnsupportedEdge => {
            edges_span(program, call)
        }
        C::FilletTooLarge | C::FilletOverlap => size_span(program, call),
        _ => None,
    };
    at.unwrap_or(call)
}

/// The edit that splits the call at `span` into two nested calls, the
/// concave edges first (the inner one), then the convex ones of its
/// result (`docs/fillets.md`, section 7.4): the call's head written
/// twice, with `except = "concave"` and `except = "convex"` under the
/// default `edges = "all"` and no `except`, otherwise its `edges` string
/// narrowed with `and convex` and `and concave`. `None` unless `edges`
/// is absent or a plain string literal.
pub fn nested_edit(program: &lang::Program, span: Span) -> Option<(Span, String)> {
    let (inst, text) = call_at(program, span)?;
    let ast = &program.ast;
    let bytes = text.as_bytes();
    let name = ast.name(inst.name);
    let start = inst.span.start as usize;
    let at = start + text.get(start..)?.find(name)?;
    // The argument list's closing parenthesis, strings skipped.
    let open = at + text.get(at..)?.find('(')?;
    let mut depth = 0i32;
    let mut close = None;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let close = close?;
    let named = inst
        .args
        .iter()
        .find(|a| a.name.is_some_and(|x| ast.name(x) == "edges"));
    let positional = inst.args.iter().filter(|a| a.name.is_none()).nth(1);
    // Under the default `edges = "all"` (absent or written), each call
    // keeps it and leaves the other sense out with `except`. Narrowing
    // `edges` instead (`"(all) and convex"`) made the edges the inner
    // call creates and v1 cannot round (the ellipses where two mitred
    // concave blends meet) named ones, so errors, and the rewritten
    // model of a block on a plate failed where the original only warned
    // about them (section 15.5).
    let except = inst
        .args
        .iter()
        .find(|a| a.name.is_some_and(|x| ast.name(x) == "except"))
        .or_else(|| inst.args.iter().filter(|a| a.name.is_none()).nth(2));
    let all = match named.or(positional) {
        None => true,
        Some(a) => {
            let s = ast.expr(a.expr).span;
            text.get(s.start as usize..s.end as usize)
                .and_then(|l| l.strip_prefix('"')?.strip_suffix('"'))
                .is_some_and(|inner| inner.trim().eq_ignore_ascii_case("all"))
        }
    };
    if all && except.is_none() {
        let sep = if inst.args.is_empty() { "" } else { ", " };
        let head = text.get(at..close)?.trim_end();
        return Some((
            Span::new(span.file, at as u32, close as u32 + 1),
            format!("{head}{sep}except = \"concave\") {head}{sep}except = \"convex\")"),
        ));
    }
    let head = |sense: &str| -> Option<String> {
        match named.or(positional) {
            Some(a) => {
                let s = ast.expr(a.expr).span;
                let (s0, s1) = (s.start as usize, s.end as usize);
                let lit = text.get(s0..s1)?;
                let inner = lit.strip_prefix('"')?.strip_suffix('"')?;
                if inner.contains('\\') || inner.contains('"') {
                    return None;
                }
                Some(format!(
                    "{}\"({inner}) and {sense}\"{}",
                    text.get(at..s0)?,
                    text.get(s1..=close)?
                ))
            }
            None => {
                let sep = if inst.args.is_empty() { "" } else { ", " };
                Some(format!(
                    "{}{sep}edges = \"{sense}\")",
                    text.get(at..close)?.trim_end()
                ))
            }
        }
    };
    Some((
        Span::new(span.file, at as u32, close as u32 + 1),
        format!("{} {}", head("convex")?, head("concave")?),
    ))
}

fn edge_json(e: &EdgeFact, index: usize, status: &str) -> Value {
    let mut v = json!({
        "index": index,
        "curve": e.curve.name(),
        "sense": e.sense.name(),
        "angle": fillet::round4(e.angle),
        "class": e.class.name(),
        "length": fillet::round4(e.length),
        "center": p4(e.center),
        "from": p4(e.from),
        "to": p4(e.to),
        "faces": e.faces,
        "children": [e.children[0], e.children[1]],
        "status": status,
    });
    if e.closed {
        v["closed"] = json!(true);
    }
    if let Some(d) = e.direction {
        v["direction"] = p4(d);
    }
    if let Some(a) = e.axis {
        v["axis"] = p4(a);
    }
    if let Some(r) = e.radius {
        v["radius"] = json!(fillet::round4(r));
    }
    if e.parts.iter().any(|p| !p.is_empty()) {
        v["parts"] = json!([e.parts[0], e.parts[1]]);
    }
    v
}

/// The skipped edges, by reason and the leaf they belong to.
fn skipped_json(p: &Plan, f: &Facts) -> Value {
    let mut groups: std::collections::BTreeMap<(&'static str, Option<u32>), usize> =
        Default::default();
    for &i in &p.skipped {
        let e = &f.edges[i];
        let reason = e.skip.map_or("tangent", |s| s.name());
        *groups.entry((reason, e.origin)).or_default() += 1;
    }
    Value::Array(
        groups
            .into_iter()
            .map(|((reason, origin), count)| {
                let mut v = json!({"reason": reason, "count": count});
                if let Some((module, loc)) = origin.and_then(|o| f.origins.get(o as usize)) {
                    v["module"] = json!(module);
                    if let Some(l) = loc {
                        v["line"] = json!(l.line);
                    }
                }
                v
            })
            .collect(),
    )
}

/// One call's report: its place, arguments, status and selected edges.
pub fn plan_json(p: &Plan, index: usize, at: Option<Value>, pin: Option<Value>) -> Value {
    let mut v = json!({
        "index": index,
        "module": p.kind.module(),
        p.kind.size_name(): p.size,
        "selector": p.edges_text,
        "except": p.except_text,
        "expect": p.expect,
        "status": p.status.name(),
        "matched": p.selected.len(),
    });
    if let Some(Value::Object(m)) = at {
        v.as_object_mut().expect("an object").extend(m);
    }
    if let Some(f) = &p.facts {
        let edges: Vec<Value> = p
            .selected
            .iter()
            .take(MAX_EDGES)
            .enumerate()
            .map(|(k, &i)| {
                let status = if p.unsupported.contains(&i) {
                    "unsupported"
                } else if p.build.as_ref().is_some_and(|b| b.edges.contains(&i)) {
                    "built"
                } else {
                    "selected"
                };
                edge_json(&f.edges[i], k + 1, status)
            })
            .collect();
        v["edges"] = Value::Array(edges);
        if p.selected.len() > MAX_EDGES {
            v["edges_omitted"] = json!(p.selected.len() - MAX_EDGES);
        }
        v["unsupported"] = json!(p.unsupported.len());
        v["skipped"] = skipped_json(p, f);
        v["selectable"] = json!(f.edges.iter().filter(|e| e.skip.is_none()).count());
        if let Some((lo, hi)) = f.bbox {
            v["bbox"] = json!({"min": p4(lo), "max": p4(hi)});
        }
    }
    let codes: Vec<&str> = p.diags.iter().map(|d| d.code.as_str()).collect();
    v["codes"] = json!(codes);
    if let Some(pin) = pin {
        v["pin"] = pin;
    }
    v
}

/// The fillet plans of a rendered tree: each call's diagnostics printed
/// on `con` at the call, and its report as JSON (the first
/// [`MAX_FILLETS`], and how many calls there were). `program` gives each
/// evaluation unit's program (0 the main one, `1 + i` the i-th library).
#[allow(clippy::too_many_arguments)]
pub fn report<'a, W: std::io::Write>(
    con: &mut eval::Console<W>,
    top: &Node,
    renderer: &Renderer,
    keys: &eval::dump::Keys,
    opts: &RenderOptions,
    program: &dyn Fn(u32) -> Option<&'a lang::Program>,
    cwd: &Path,
) -> Reports {
    let nodes = calls(top);
    let mut out = Reports {
        count: nodes.len(),
        ..Reports::default()
    };
    for (k, n) in nodes.iter().enumerate() {
        let Some(mut p) = fillet::plan(renderer, n, keys, opts) else {
            continue;
        };
        if p.status == Status::Interrupted {
            // The host is stopping; what it reports now is discarded.
            break;
        }
        // What the boolean kept of each blend.
        let after = fillet::blend_diags(renderer, n, keys, opts, &p);
        if after
            .iter()
            .any(|d| d.severity == lang::diag::Severity::Error)
        {
            p.status = Status::Failed;
        }
        p.diags.extend(after);
        let origin = n.origin.as_ref();
        let prog = origin.and_then(|o| program(o.unit));
        let pin = match (prog, origin) {
            (Some(prog), Some(o))
                if p.facts.is_some()
                    && (p.expect.is_some() || !p.selected.is_empty())
                    && p.expect != Some(p.selected.len() as u32) =>
            {
                pin_edit(prog, o.span, p.selected.len())
            }
            _ => None,
        };
        for d in &p.diags {
            let mut diag = Diagnostic::new(d.code, d.severity, d.message.clone());
            if let Some(o) = origin {
                let span = prog.map_or(o.span, |p| diag_span(p, o.span, d.code));
                diag = diag.at(span, o.line).with_base(PathBase::MainFileDir);
            }
            for (i, h) in d.hints.iter().enumerate() {
                // The count's first hint carries the edit that pins it;
                // a size or vertex problem's, the edit that fixes it.
                let fix = match (d.fix, prog, origin) {
                    (Some(fillet::Fix::Size(x)), Some(prog), Some(o)) => {
                        size_edit(prog, o.span, &fillet::number_text(x))
                    }
                    (Some(fillet::Fix::Nested), Some(prog), Some(o)) => nested_edit(prog, o.span),
                    _ => None,
                };
                let replacement = if i != 0 {
                    None
                } else if d.code == lang::diag::DiagCode::FilletCount {
                    pin.clone()
                } else {
                    fix
                };
                diag.hints.push(Hint {
                    message: h.clone(),
                    replacement,
                });
            }
            match prog {
                Some(prog) => con.diagnostic(&diag, &prog.sources, cwd),
                None => con.diagnostic(&diag, &lang::source::SourceMap::default(), cwd),
            }
        }
        if out.json.len() < MAX_FILLETS {
            let at = match (prog, origin) {
                (Some(prog), Some(o)) => Some(place(&prog.sources, o.span)),
                _ => None,
            };
            let pin_json = match (prog, &pin) {
                (Some(prog), Some((span, text))) => {
                    let mut v = place(&prog.sources, *span);
                    v["text"] = json!(text);
                    v["count"] = json!(p.selected.len());
                    Some(v)
                }
                _ => None,
            };
            out.json.push(plan_json(&p, k + 1, at, pin_json));
            out.plans.push(p);
        }
    }
    out
}

/// A call's one-line summary: "fillet_edges at line 4: 4 edges (4 line,
/// convex, 90°), r 2".
pub fn line_text(v: &Value) -> String {
    let module = v["module"].as_str().unwrap_or("fillet_edges");
    let line = v["line"]
        .as_u64()
        .map(|l| format!(" at line {l}"))
        .unwrap_or_default();
    let size_name = if module == "chamfer_edges" { "d" } else { "r" };
    let size = render::snapshot::number(v[size_name].as_f64().unwrap_or(0.0));
    let matched = v["matched"].as_u64().unwrap_or(0);
    let status = v["status"].as_str().unwrap_or("");
    let what = match status {
        "2d" => "2D children (no edges)".to_string(),
        "empty" => "empty children".to_string(),
        "no-brep" => "no B-rep to select on".to_string(),
        _ => {
            let edges = v["edges"].as_array().map(Vec::as_slice).unwrap_or_default();
            // "4 line, convex, 90°" when they agree, else the kinds counted.
            let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
            for e in edges {
                let k = format!(
                    "{}, {}, {}°",
                    e["curve"].as_str().unwrap_or(""),
                    e["sense"].as_str().unwrap_or(""),
                    e["angle"].as_f64().unwrap_or(0.0).round()
                );
                *kinds.entry(k).or_default() += 1;
            }
            let detail = kinds
                .iter()
                .map(|(k, n)| format!("{n} {k}"))
                .collect::<Vec<_>>()
                .join("; ");
            let s = if matched == 1 { "" } else { "s" };
            if detail.is_empty() {
                format!("{matched} edge{s}")
            } else {
                format!("{matched} edge{s} ({detail})")
            }
        }
    };
    let mut out = format!("{module}{line}: {what}, {size_name} {size}");
    if let Some(sel) = v["selector"].as_str() {
        out.push_str(&format!(", edges = {sel}"));
    }
    match status {
        "selected" | "2d" | "empty" | "no-brep" => {}
        s => out.push_str(&format!(" [{s}]")),
    }
    out
}

/// One selected edge as a line: "3. line (convex, 90°) at [0, 0, 10], 20
/// long, plane | plane".
pub fn edge_text(e: &Value) -> String {
    let n = |v: &Value| render::snapshot::number(v.as_f64().unwrap_or(0.0));
    let pt = |v: &Value| {
        v.as_array()
            .into_iter()
            .flatten()
            .map(n)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut s = format!(
        "{}. {} ({}, {}°, {}) at [{}], {} long, {} | {}",
        e["index"],
        e["curve"].as_str().unwrap_or(""),
        e["sense"].as_str().unwrap_or(""),
        n(&e["angle"]),
        e["class"].as_str().unwrap_or(""),
        pt(&e["center"]),
        n(&e["length"]),
        e["faces"][0].as_str().unwrap_or(""),
        e["faces"][1].as_str().unwrap_or(""),
    );
    if e["status"] == json!("unsupported") {
        s.push_str(" (unsupported)");
    }
    s
}

/// The call `which` names: a number is its index (from 1, as reports
/// number calls); anything else a selector as reports print it (with or
/// without its quotes). `Err` says which calls there are.
pub fn find<'v>(fillets: &'v [Value], which: &str) -> Result<&'v Value, String> {
    let w = which.trim();
    let found = match w.parse::<u64>() {
        Ok(i) => fillets.iter().find(|f| f["index"].as_u64() == Some(i)),
        Err(_) => {
            let bare = w.trim_matches('"').to_lowercase();
            fillets.iter().find(|f| {
                f["selector"]
                    .as_str()
                    .is_some_and(|s| s.trim_matches('"').to_lowercase() == bare)
            })
        }
    };
    found.ok_or_else(|| {
        if fillets.is_empty() {
            "the model has no fillet_edges() or chamfer_edges() call (they need --enable fillet)"
                .to_string()
        } else {
            let list: Vec<String> = fillets
                .iter()
                .map(|f| format!("{} ({})", f["index"], line_text(f)))
                .collect();
            format!(
                "no fillet call '{which}'; the calls are: {}",
                list.join("; ")
            )
        }
    })
}

/// Selected edges, bold and numbered.
const SELECTED: [u8; 3] = [20, 100, 200];
/// Selected edges this version does not blend.
const UNSUPPORTED: [u8; 3] = [200, 30, 30];
/// Edges the selector named that are never filleted, dashed.
const SKIPPED: [u8; 3] = [225, 115, 0];
/// Every other selectable edge, thin.
const OTHER: [u8; 3] = [110, 110, 110];

fn rgba(c: [u8; 3]) -> [f32; 4] {
    [
        f32::from(c[0]) / 255.0,
        f32::from(c[1]) / 255.0,
        f32::from(c[2]) / 255.0,
        1.0,
    ]
}

/// What `snapshot --fillet` draws over the model (`docs/fillets.md`,
/// section 11): every selectable edge of the call's child thin, the
/// selected ones bold and numbered as the reports number them (red when
/// of a kind not blended yet), and the edges the selector named but that
/// are never filleted dashed. The header line and the legend go with it.
pub fn overlay(
    p: &Plan,
) -> (
    render::snapshot::SketchOverlay,
    String,
    Vec<([f32; 4], String)>,
) {
    use render::snapshot::{Label, SketchOverlay, Stroke};
    let mut o = SketchOverlay::default();
    let header = line_text(&plan_json(p, 0, None, None));
    let Some(f) = &p.facts else {
        return (o, header, Vec::new());
    };
    for (i, e) in f.edges.iter().enumerate() {
        if e.skip.is_none() && !p.selected.contains(&i) {
            o.strokes.push(Stroke {
                points: e.path.clone(),
                color: OTHER,
                dashed: false,
                bold: false,
            });
        }
    }
    for &i in &p.skipped {
        o.strokes.push(Stroke {
            points: f.edges[i].path.clone(),
            color: SKIPPED,
            dashed: true,
            bold: false,
        });
    }
    for (k, &i) in p.selected.iter().enumerate() {
        let e = &f.edges[i];
        let color = if p.unsupported.contains(&i) {
            UNSUPPORTED
        } else {
            SELECTED
        };
        o.strokes.push(Stroke {
            points: e.path.clone(),
            color,
            dashed: false,
            bold: true,
        });
        // On the edge itself (a circle's centre is off it).
        let at = e.path[e.path.len() / 2];
        o.labels.push(Label {
            point: at,
            text: (k + 1).to_string(),
            color,
            boxed: true,
        });
    }
    let mut legend = vec![(rgba(SELECTED), "selected".to_string())];
    if !p.unsupported.is_empty() {
        legend.push((rgba(UNSUPPORTED), "not blended yet".to_string()));
    }
    if !p.skipped.is_empty() {
        legend.push((rgba(SKIPPED), "skipped".to_string()));
    }
    legend.push((rgba(OTHER), "other edges".to_string()));
    (o, header, legend)
}
