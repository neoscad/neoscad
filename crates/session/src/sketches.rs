//! What the tools say about a run's constrained sketches (`--enable
//! sketch`; `docs/language-extensions.md`, section 4.8): each solved
//! sketch's summary for `check`, its entities' solved values for
//! `measure --sketch`, and, for the language server, where its entities
//! and constraints are, each constraint's state, and the "pin the
//! drawing" edit; and for `snapshot --sketch`, the solved profile.
//!
//! The facts come from the evaluated tree (`NodeKind::Sketch` carries
//! `eval::node::SketchReport`), collected once per run into
//! [`crate::Log::sketches`] as JSON: every host (the command line, MCP,
//! the apps' language server) gets them with the run's diagnostics,
//! through the same path.

use eval::node::{NodeKind, SketchEdit, SketchNode, SketchReport, SketchValues};
use lang::source::{SourceMap, Span};
use serde_json::{Value, json};

/// The most sketches a run reports. A sketch in a loop is one per
/// iteration, each with its entities; past this the rest are counted
/// (`check`'s `sketches_omitted`) but not listed.
pub const MAX_SKETCHES: usize = 100;

/// A number as the tools print it: rounded to 1e-9, with no `-0`, so a
/// solved coordinate exact to the last bit reads as written and one with
/// rounding noise does not show it.
fn r9(x: f64) -> f64 {
    let y = (x * 1e9).round() / 1e9;
    if y == 0.0 || !y.is_finite() { 0.0 } else { y }
}

fn p9(p: [f64; 2]) -> Value {
    json!([r9(p[0]), r9(p[1])])
}

/// A span as the diagnostics' JSON writes it (`diag::location_json`):
/// the file, the line, and 1-based line and byte column of each end.
fn place(sources: &SourceMap, span: Span) -> Value {
    let f = sources.get(span.file);
    let (a, b) = (f.line_col(span.start), f.line_col(span.end.max(span.start)));
    json!({
        "file": f.path.to_string_lossy(),
        "line": a.0,
        "span": {"start": {"line": a.0, "column": a.1}, "end": {"line": b.0, "column": b.1}},
    })
}

fn merge(into: &mut Value, from: Value) {
    if let (Some(o), Value::Object(m)) = (into.as_object_mut(), from) {
        o.extend(m);
    }
}

/// A sketch's state in one word: `fully-constrained`, `underconstrained`
/// (solved, with free degrees of freedom; an info, or an error with
/// `strict = true`), `conflict`, `not-converged`, or `error` (another
/// error left the profile empty).
fn status(r: &SketchReport) -> &'static str {
    let has = |c: &str| r.codes.contains(&c);
    if has("sketch-conflict") {
        "conflict"
    } else if has("sketch-no-convergence") {
        "not-converged"
    } else if r.solved && r.dof > 0 {
        "underconstrained"
    } else if r.solved && !r.failed {
        "fully-constrained"
    } else {
        "error"
    }
}

fn entity_json(e: &eval::node::SketchEntity, i: usize, sources: Option<&SourceMap>) -> Value {
    let mut v = json!({
        "id": i + 1,
        "name": e.label,
        "kind": e.kind,
    });
    if e.construction {
        v["construction"] = json!(true);
    }
    if e.free {
        v["free"] = json!(true);
    }
    if let Some(s) = sources {
        merge(&mut v, place(s, e.span));
    }
    match &e.solved {
        None => {}
        Some(SketchValues::Point(p)) => v["at"] = p9(*p),
        Some(SketchValues::Line {
            start,
            end,
            length,
            angle,
        }) => {
            v["start"] = p9(*start);
            v["end"] = p9(*end);
            v["length"] = json!(r9(*length));
            v["angle"] = json!(r9(*angle));
        }
        Some(SketchValues::Arc {
            center,
            start,
            end,
            radius,
            sweep,
            cw,
        }) => {
            v["center"] = p9(*center);
            v["start"] = p9(*start);
            v["end"] = p9(*end);
            v["radius"] = json!(r9(*radius));
            v["sweep"] = json!(r9(*sweep));
            if *cw {
                v["cw"] = json!(true);
            }
        }
        Some(SketchValues::Circle { center, radius }) => {
            v["center"] = p9(*center);
            v["radius"] = json!(r9(*radius));
        }
    }
    v
}

fn constraint_json(c: &eval::node::SketchConstraint, sources: Option<&SourceMap>) -> Value {
    let mut v = json!({
        "kind": c.kind,
        "text": c.text,
        "entities": c.entities.iter().map(|i| i + 1).collect::<Vec<_>>(),
        "status": c.status.as_str(),
    });
    if let Some(x) = c.value {
        v["value"] = json!(r9(x));
    }
    if let Some(r) = c.residual {
        v["residual"] = json!(r);
    }
    if let Some(s) = sources {
        merge(&mut v, place(s, c.span));
    }
    v
}

/// The most profile points a sketch's facts carry for `snapshot
/// --sketch`; a larger profile is left out (`profile_omitted`), and the
/// snapshot draws the entities alone.
const MAX_PROFILE_POINTS: usize = 20_000;

/// The solved profile's loops, each a list of points, as the node's
/// polygon holds them (fillets and chamfers cut, arcs tessellated).
fn profile_json(s: &SketchNode) -> Value {
    if s.points.len() > MAX_PROFILE_POINTS {
        return Value::Null;
    }
    let pts = |idx: &mut dyn Iterator<Item = usize>| -> Value {
        Value::Array(
            idx.filter_map(|i| s.points.get(i))
                .map(|p| p9(*p))
                .collect(),
        )
    };
    if s.paths.is_empty() {
        json!([pts(&mut (0..s.points.len()))])
    } else {
        Value::Array(
            s.paths
                .iter()
                .map(|p| pts(&mut p.iter().copied()))
                .collect(),
        )
    }
}

fn edit_json(e: &SketchEdit, sources: &SourceMap) -> Value {
    let mut v = place(sources, e.span);
    v["text"] = json!(e.text);
    v
}

/// One sketch's facts: the summary fields of [`summary`], plus
/// `entities`, `constraints`, `profile` and `pin`.
fn sketch_json<'s>(
    node: &SketchNode,
    r: &SketchReport,
    at: Option<(&SourceMap, Span)>,
    unit: &dyn Fn(u32) -> Option<&'s SourceMap>,
) -> Value {
    let mut v = json!({
        "name": if r.name.is_empty() { Value::Null } else { json!(r.name) },
        "status": status(r),
        "dof": r.dof,
        "unknowns": r.unknowns,
        "equations": r.equations,
        "rank": r.rank,
        "iterations": r.iterations,
        "residual": r.residual,
        "continuation": r.continuation,
        "empty": r.failed,
        "codes": r.codes,
    });
    if let Some((s, span)) = at {
        merge(&mut v, place(s, span));
    }
    v["entities"] = Value::Array(
        r.entities
            .iter()
            .enumerate()
            .map(|(i, e)| entity_json(e, i, unit(e.unit)))
            .collect(),
    );
    v["constraints"] = Value::Array(
        r.constraints
            .iter()
            .map(|c| constraint_json(c, unit(c.unit)))
            .collect(),
    );
    match profile_json(node) {
        Value::Null => v["profile_omitted"] = json!(true),
        p => v["profile"] = p,
    }
    if let Some(p) = &r.pin
        && let Some(s) = unit(p.unit)
    {
        v["pin"] = edit_json(p, s);
    }
    v
}

/// Every sketch of an evaluated tree, in tree order (the first
/// [`MAX_SKETCHES`]), and how many there were in all. `unit` gives the
/// sources of a node's unit (`Origin::unit`).
pub fn collect<'s>(
    root: &eval::Node,
    unit: &dyn Fn(u32) -> Option<&'s SourceMap>,
) -> (Vec<Value>, usize) {
    let mut out = Vec::new();
    let mut count = 0;
    // A recursive module builds a tree as deep as the depth limit allows:
    // the walk keeps its own stack.
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if let NodeKind::Sketch(s) = &n.kind {
            count += 1;
            if out.len() < MAX_SKETCHES {
                let at = n
                    .origin
                    .as_ref()
                    .and_then(|o| Some((unit(o.unit)?, o.span)));
                out.push(sketch_json(s, &s.report, at, unit));
            }
        }
        stack.extend(n.children.iter().rev());
    }
    (out, count)
}

/// The fields `check` lists per sketch: everything but the entities, the
/// constraints, the profile and the edit.
pub fn summary(sketch: &Value) -> Value {
    let mut v = sketch.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("entities");
        o.remove("constraints");
        o.remove("profile");
        o.remove("profile_omitted");
        o.remove("pin");
    }
    v
}

/// The sketches named `name` among `all`; an error that lists the names
/// there are when none is.
pub fn find<'a>(all: &'a [Value], name: &str) -> Result<Vec<&'a Value>, String> {
    let found: Vec<&Value> = all.iter().filter(|s| s["name"] == json!(name)).collect();
    if !found.is_empty() {
        return Ok(found);
    }
    let mut names: Vec<&str> = all.iter().filter_map(|s| s["name"].as_str()).collect();
    names.dedup();
    Err(if all.is_empty() {
        format!("no sketch '{name}': the model has no sketches (they need `--enable sketch`)")
    } else if names.is_empty() {
        format!("no sketch '{name}': its sketches have no `name`")
    } else {
        format!("no sketch '{name}' (sketches: {})", names.join(", "))
    })
}

fn num(v: &Value) -> String {
    lang::number::fmt_number(v.as_f64().unwrap_or(0.0))
}

fn point(v: &Value) -> String {
    format!("[{}, {}]", num(&v[0]), num(&v[1]))
}

/// A sketch's state in words: "fully constrained", "1 free degree of
/// freedom", "conflicting constraints", ...
pub fn state_text(s: &Value) -> String {
    match s["status"].as_str().unwrap_or("") {
        "fully-constrained" => "fully constrained".to_string(),
        "underconstrained" => {
            let n = s["dof"].as_u64().unwrap_or(0);
            format!(
                "{n} free degree{} of freedom",
                if n == 1 { "" } else { "s" }
            )
        }
        "conflict" => "conflicting constraints".to_string(),
        "not-converged" => "did not converge".to_string(),
        _ => "not solved (see its errors)".to_string(),
    }
}

/// "sketch 'slot' (line 4): fully constrained, 16 unknowns".
pub fn line_text(s: &Value) -> String {
    let name = match s["name"].as_str() {
        Some(n) => format!("sketch '{n}'"),
        None => "sketch".to_string(),
    };
    let at = match s["line"].as_u64() {
        Some(l) => format!(" (line {l})"),
        None => String::new(),
    };
    format!(
        "{name}{at}: {}, {} unknowns",
        state_text(s),
        s["unknowns"].as_u64().unwrap_or(0)
    )
}

/// One entity's solved values in words, as `measure --sketch` prints
/// them: "top line [0, 4]..[30, 4], length 30, angle 0°".
pub fn entity_text(e: &Value) -> String {
    let name = match e["name"].as_str() {
        Some(n) => n.to_string(),
        None => format!("#{}", e["id"]),
    };
    let kind = e["kind"].as_str().unwrap_or("");
    let mut t = format!("{name} {kind}");
    match kind {
        "point" if e.get("at").is_some() => t.push_str(&format!(" {}", point(&e["at"]))),
        "line" if e.get("start").is_some() => t.push_str(&format!(
            " {}..{}, length {}, angle {}°",
            point(&e["start"]),
            point(&e["end"]),
            num(&e["length"]),
            num(&e["angle"])
        )),
        "arc" if e.get("center").is_some() => t.push_str(&format!(
            " centre {}, radius {}, from {} to {}, sweep {}°{}",
            point(&e["center"]),
            num(&e["radius"]),
            point(&e["start"]),
            point(&e["end"]),
            num(&e["sweep"]),
            if e["cw"] == json!(true) {
                " clockwise"
            } else {
                ""
            }
        )),
        "circle" if e.get("center").is_some() => t.push_str(&format!(
            " centre {}, radius {}",
            point(&e["center"]),
            num(&e["radius"])
        )),
        _ => t.push_str(" (not solved)"),
    }
    if e["construction"] == json!(true) {
        t.push_str(" (construction)");
    }
    if e["free"] == json!(true) {
        t.push_str(" (free to move)");
    }
    t
}

/// One constraint's state in words, as hover shows it: "satisfied",
/// "redundant: implied by the other constraints", ...
pub fn constraint_text(c: &Value) -> String {
    let mut t = match c["status"].as_str().unwrap_or("") {
        "satisfied" => "satisfied".to_string(),
        "redundant" => "redundant: implied by the other constraints (sketch-redundant)".to_string(),
        "conflicting" => {
            "conflicting: cannot hold together with others (sketch-conflict)".to_string()
        }
        "unmet" => "not met: the solve did not converge".to_string(),
        _ => "not solved (see the sketch's errors)".to_string(),
    };
    if let Some(r) = c["residual"].as_f64() {
        t.push_str(&format!(", residual {}", lang::number::fmt_number(r)));
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_lose_their_noise() {
        assert_eq!(r9(1e-17), 0.0);
        assert_eq!(r9(-1e-12), 0.0);
        assert_eq!(r9(29.999_999_999_999_996), 30.0);
        assert_eq!(r9(f64::NAN), 0.0);
    }

    #[test]
    fn a_missing_sketch_lists_the_names() {
        let all = vec![
            json!({"name": "a"}),
            json!({"name": "b"}),
            json!({"name": null}),
        ];
        assert_eq!(find(&all, "a").unwrap().len(), 1);
        assert_eq!(
            find(&all, "c").unwrap_err(),
            "no sketch 'c' (sketches: a, b)"
        );
        assert!(find(&[], "c").unwrap_err().contains("--enable sketch"));
    }
}
