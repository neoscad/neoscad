//! NeoSCAD's fillets and chamfers (`--enable fillet`; `docs/fillets.md`,
//! the user reference `docs/fillet-edges.md`) in the editor.
//!
//! - **Selector strings.** Inside the `edges` or `except` string of a
//!   `fillet_edges()` or `chamfer_edges()` call that is the builtin (a
//!   program's own `module fillet_edges` gets nothing), completion offers
//!   the selector language (`docs/fillets.md`, section 5.2): the atoms
//!   where an operand goes, the operators after one, and the "did you
//!   mean" word when nothing matches what was typed. Hover explains the
//!   atom or operator under the cursor. The text is read here rather
//!   than parsed, because a string being typed is mostly not a selector
//!   yet; the evaluator's parser (`session::fillets::selector`) stays the
//!   one judge, and a test checks that every atom offered here parses.
//! - **The last run.** Hover on a call's name adds what the last rendered
//!   run of the same text selected (`session::fillets::line_text` and its
//!   edges), as hover on `sketch` adds its state. Selection needs the
//!   child's geometry, so this comes only from a host's rendered run
//!   (`Server::supply_log`), as "Pin count" does.

use std::path::Path;

use lang::source::SourceFile;
use lang::syntax::SyntaxKind as K;
use serde_json::{Value, json};

use crate::context;
use crate::index::Ns;
use crate::world::{Analyzed, Target};

/// The two builtins.
pub const MODULES: [&str; 2] = ["fillet_edges", "chamfer_edges"];

/// What a selector atom is, for completion and hover: the label shown,
/// the text inserted (LSP snippet syntax), and what it selects.
struct Atom {
    label: &'static str,
    insert: &'static str,
    doc: &'static str,
}

const fn atom(label: &'static str, insert: &'static str, doc: &'static str) -> Atom {
    Atom { label, insert, doc }
}

/// The atoms, in the order `docs/fillets.md` (5.2) lists them. Directions
/// are offered on z (the common case: vertical edges, the top outline);
/// `x` and `y` are the same atoms with another axis, and a vector
/// `(a, b, c)` works in place of any axis.
const ATOMS: &[Atom] = &[
    atom("all", "all", "every selectable edge (the default)"),
    atom("none", "none", "no edge (`not all`)"),
    atom(
        "convex",
        "convex",
        "convex edges (material angle under 180°): a fillet removes material",
    ),
    atom(
        "concave",
        "concave",
        "concave edges (material angle over 180°): a fillet adds material",
    ),
    atom("%line", "%line", "straight edges"),
    atom(
        "%circle",
        "%circle",
        "circles and arcs of circles (hole and boss rims, rounded corners)",
    ),
    atom("%ellipse", "%ellipse", "ellipses and their arcs"),
    atom("%bspline", "%bspline", "B-spline edges"),
    atom("|z", "|z", "lines parallel to z (CadQuery's `|Z`)"),
    atom("|x", "|x", "lines parallel to x"),
    atom("|y", "|y", "lines parallel to y"),
    atom(
        "#z",
        "#z",
        "lines perpendicular to z, and circles whose axis is z (CadQuery's `#Z`)",
    ),
    atom(
        ">z",
        ">z",
        "the edges whose centre is farthest along +z: the top (CadQuery's `>Z`)",
    ),
    atom(
        "<z",
        "<z",
        "the edges whose centre is farthest along -z: the bottom",
    ),
    atom(">x", ">x", "the edges whose centre is farthest along +x"),
    atom("<x", "<x", "the edges whose centre is farthest along -x"),
    atom(">y", ">y", "the edges whose centre is farthest along +y"),
    atom("<y", "<y", "the edges whose centre is farthest along -y"),
    atom(
        ">>z[i]",
        ">>${1:z}[${2:-2}]",
        "the i-th group of edges by centre along z, from the bottom; negative from the top (CadQuery's `>>Z[-2]`)",
    ),
    atom(
        "<<z[i]",
        "<<${1:z}[${2:1}]",
        "the i-th group of edges by centre against z, from the top",
    ),
    atom(
        "new",
        "new",
        "edges whose two faces come from different leaves: the edges booleans made",
    ),
    atom(
        "child(i)",
        "child(${1:0})",
        "edges with a face from child i of this call",
    ),
    atom(
        "child(i, j)",
        "child(${1:0}, ${2:1})",
        "edges where child i meets child j (an inner corner)",
    ),
    atom(
        "part(name)",
        "part(${1:name})",
        "edges with a face from `part(name)` (needs --enable part)",
    ),
    atom(
        "box(x0, y0, z0, x1, y1, z1)",
        "box(${1:x0}, ${2:y0}, ${3:z0}, ${4:x1}, ${5:y1}, ${6:z1})",
        "edges lying wholly in the box",
    ),
];

/// `@name`, offered only with `--enable query`.
const ANCHOR: Atom = atom(
    "@name",
    "@${1:name}",
    "edges through the children's anchor `name`, and along its direction if it has one (needs --enable query)",
);

/// The operators, loosest first as CadQuery binds them.
const OPERATORS: &[Atom] = &[
    atom(
        "not",
        "not",
        "every selectable edge except what follows, up to the closing parenthesis",
    ),
    atom(
        "exc",
        "exc",
        "set difference: the edges on the left, less those on the right (`except` too)",
    ),
    atom("or", "or", "the edges either side selects"),
    atom("and", "and", "the edges both sides select"),
];

/// The words of the language, for "did you mean".
const WORDS: &[&str] = &[
    "all", "none", "convex", "concave", "new", "child", "part", "box", "and", "or", "not", "exc",
    "except", "%line", "%circle", "%ellipse", "%bspline",
];

/// A selector string the cursor is in: the range of its text, without
/// the quotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectorString {
    pub content: (u32, u32),
}

/// Whether the string at `offset` is the `edges` or `except` argument
/// (named, or second and third; alone or in a list) of a call to the
/// builtin `fillet_edges` or `chamfer_edges`, with the extension on.
pub fn selector_string(ctx: &crate::Ctx<'_>, offset: u32) -> Option<SelectorString> {
    if !ctx.world.fillet {
        return None;
    }
    let f = ctx.file();
    let toks = f.program.cst.tokens();
    let i = toks.partition_point(|t| t.end() < offset);
    let (ti, t) = toks
        .iter()
        .enumerate()
        .skip(i)
        .take(2)
        .find(|(_, t)| match t.kind {
            K::String => t.start < offset && offset < t.end(),
            // An unterminated string, as while one is being typed.
            K::Error => t.start < offset && f.text().get(t.start as usize) == Some(&b'"'),
            _ => false,
        })?;
    let content = if t.kind == K::String {
        (t.start + 1, t.end() - 1)
    } else {
        (t.start + 1, t.end())
    };
    // In a list (`edges = ["|z", ">x"]`), the argument starts at its `[`.
    let mut at = t.start;
    for p in toks[..ti].iter().rev().filter(|t| !t.kind.is_trivia()) {
        match p.kind {
            K::String | K::Comma => {}
            K::LBrack => {
                at = p.start;
                break;
            }
            _ => break,
        }
    }
    let call = context::enclosing_call(f, at)?;
    if !call.statement || !MODULES.contains(&call.name.as_str()) {
        return None;
    }
    let ok = match call.named.as_deref() {
        Some(n) => n == "edges" || n == "except",
        None => call.arg == 1 || call.arg == 2,
    };
    if !ok || !is_builtin(ctx, f, &call) {
        return None;
    }
    Some(SelectorString { content })
}

/// Whether the call resolves to the builtin, not a program's own module
/// of the same name.
fn is_builtin(ctx: &crate::Ctx<'_>, f: &std::sync::Arc<Analyzed>, call: &context::Call) -> bool {
    let scope = f.index.scope_at(call.name_span.0);
    matches!(
        ctx.world
            .resolve(f, scope, call.name_span.0, &call.name, Ns::Module),
        Some(Target::Builtin(_))
    )
}

/// Characters of an atom being typed: words, sigils and an index.
fn atom_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'%' | b'|' | b'#' | b'>' | b'<' | b'@')
}

/// Whether `typed` is what an item filters on: its letters in order.
fn loose(label: &str, typed: &str) -> bool {
    crate::complete::matches(label, typed)
}

/// Completion inside a selector string: `{isIncomplete, itemDefaults,
/// items}`, the edit range being the atom typed so far.
pub fn completion(ctx: &crate::Ctx<'_>, s: SelectorString, offset: u32) -> Value {
    let f = ctx.file();
    let text = f.text();
    let lo = s.content.0 as usize;
    let mut start = (offset as usize).min(text.len());
    while start > lo && atom_char(text[start - 1]) {
        start -= 1;
    }
    let typed = String::from_utf8_lossy(&text[start..offset as usize]).into_owned();
    let before = String::from_utf8_lossy(&text[lo..start]).into_owned();
    let empty = json!({"isIncomplete": false, "items": []});
    // Inside `child(`, `part(` or `box(`, numbers and names go, not atoms.
    if in_atom_arguments(&before) {
        return empty;
    }
    let operand = expects_operand(&before);
    let mut items: Vec<Value> = Vec::new();
    let mut push = |a: &Atom, kind: u8, rank: usize| {
        let mut item = json!({
            "label": a.label,
            "kind": kind,
            "detail": a.doc,
            "sortText": format!("{rank:03}"),
            "filterText": a.label,
        });
        if a.insert != a.label {
            item["insertText"] = json!(a.insert);
            item["insertTextFormat"] = json!(2);
        }
        items.push(item);
    };
    const KEYWORD: u8 = 14;
    const VALUE: u8 = 12;
    let anchor = ctx.world.query.then_some(&ANCHOR);
    let atoms = ATOMS.iter().chain(anchor);
    if operand {
        for (i, a) in atoms.enumerate() {
            if loose(a.label, &typed) {
                push(a, VALUE, i);
            }
        }
        if loose("not", &typed) {
            push(&OPERATORS[0], KEYWORD, 900);
        }
    } else {
        for (i, a) in OPERATORS.iter().enumerate().skip(1).rev() {
            if loose(a.label, &typed) {
                push(a, KEYWORD, i);
            }
        }
    }
    // Nothing for what was typed: the word it is likely a slip for, kept
    // through the editor's own filter by filtering on the typed text.
    if items.is_empty() && typed.len() >= 2 {
        let lower = typed.to_ascii_lowercase();
        if let Some(w) = session::diag::did_you_mean(&lower, WORDS.iter().copied()) {
            let a = ATOMS
                .iter()
                .chain(OPERATORS)
                .find(|a| a.label == w || a.label.starts_with(&format!("{w}(")));
            let doc = a.map_or("", |a| a.doc);
            let insert = a.map_or(w, |a| a.insert);
            let mut item = json!({
                "label": w,
                "kind": VALUE,
                "detail": format!("did you mean '{w}'? {doc}"),
                "filterText": typed,
                "sortText": "000",
            });
            if insert != w {
                item["insertText"] = json!(insert);
                item["insertTextFormat"] = json!(2);
            }
            items.push(item);
        }
    }
    json!({
        "isIncomplete": false,
        "itemDefaults": {"editRange": crate::proto::range(f.source(), (start as u32, offset))},
        "items": items,
    })
}

/// Whether the text before the cursor ends inside the parentheses of
/// `child(`, `part(` or `box(`.
fn in_atom_arguments(before: &str) -> bool {
    let b = before.as_bytes();
    let mut depth = 0i32;
    let mut i = b.len();
    while i > 0 {
        i -= 1;
        match b[i] {
            b')' => depth += 1,
            b'(' if depth > 0 => depth -= 1,
            b'(' => {
                let word: String = before[..i]
                    .trim_end()
                    .chars()
                    .rev()
                    .take_while(|c| c.is_ascii_alphabetic())
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                return matches!(word.to_ascii_lowercase().as_str(), "child" | "part" | "box");
            }
            _ => {}
        }
    }
    false
}

/// Whether an operand (an atom, `not` or a parenthesis) goes after
/// `before`: at the start, after `(` or after an operator; otherwise an
/// operator does.
fn expects_operand(before: &str) -> bool {
    let t = before.trim_end();
    if t.is_empty() || t.ends_with('(') {
        return true;
    }
    let word: String = t
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    // A word that is the whole last token, not the tail of `%line` or
    // `|z`.
    let whole = t.len() == word.len() || {
        let c = t.as_bytes()[t.len() - word.len() - 1];
        c.is_ascii_whitespace() || c == b'('
    };
    whole
        && matches!(
            word.to_ascii_lowercase().as_str(),
            "and" | "or" | "not" | "exc" | "except"
        )
}

/// Hover on the atom or operator under the cursor in a selector string:
/// what it selects, and the range of the text explained.
pub fn hover_atom(f: &Analyzed, s: SelectorString, offset: u32) -> Option<(String, (u32, u32))> {
    let text = f.text();
    let (lo, hi) = (s.content.0 as usize, s.content.1 as usize);
    let at = offset as usize;
    if at < lo || at > hi {
        return None;
    }
    // The run of atom characters around the cursor, then the
    // parentheses of a vector or of `child(...)` after it.
    let mut a = at;
    while a > lo && atom_char(text[a - 1]) {
        a -= 1;
    }
    let mut b = at;
    while b < hi && atom_char(text[b]) {
        b += 1;
    }
    if a == b {
        return None;
    }
    let word = String::from_utf8_lossy(&text[a..b]).to_ascii_lowercase();
    let doc = describe_atom(&word)?;
    Some((
        format!(
            "`{}`: {doc}\n\n*Edge selector (`docs/fillets.md`, section 5.2)*",
            word
        ),
        (a as u32, b as u32),
    ))
}

/// What a written atom or operator means.
fn describe_atom(word: &str) -> Option<String> {
    let axis = |s: &str| -> String {
        match s {
            "" => "the direction".to_string(),
            "x" | "y" | "z" => s.to_string(),
            _ => "the direction".to_string(),
        }
    };
    let find = |label: &str| {
        ATOMS
            .iter()
            .chain(OPERATORS)
            .chain(std::iter::once(&ANCHOR))
            .find(|a| a.label == label)
            .map(|a| a.doc.to_string())
    };
    if let Some(d) = find(word) {
        return Some(d);
    }
    if word == "except" {
        return find("exc");
    }
    if matches!(word, "x" | "y" | "z") {
        return Some(format!("lines parallel to {word} (`|{word}`)"));
    }
    for (w, label) in [
        ("child", "child(i, j)"),
        ("part", "part(name)"),
        ("box", "box(x0, y0, z0, x1, y1, z1)"),
    ] {
        if word == w {
            let all = find(label)?;
            return Some(if w == "child" {
                format!("`child(i)`: {}; `child(i, j)`: {all}", find("child(i)")?)
            } else {
                all
            });
        }
    }
    if let Some(rest) = word.strip_prefix('@') {
        return Some(format!(
            "edges through the anchor '{rest}' of the children, and along its direction if it has one (needs --enable query)"
        ));
    }
    if let Some(rest) = word.strip_prefix("%") {
        return find(&format!("%{rest}"));
    }
    for (sigil, what) in [
        (
            ">>",
            "the i-th group of edges by centre along {}, counted from the lowest (`>>{}[0]`); negative indices count down from the highest (`>>{}[-1]` is `>{}`)",
        ),
        (
            "<<",
            "the i-th group of edges by centre against {}, counted from the highest (`<<{}[0]`)",
        ),
        (">", "the edges whose centre is farthest along {}"),
        ("<", "the edges whose centre is farthest against {}"),
        ("|", "lines parallel to {}"),
        (
            "#",
            "lines perpendicular to {}, and circles whose axis is along it",
        ),
    ] {
        if let Some(rest) = word.strip_prefix(sigil) {
            return Some(what.replace("{}", &axis(rest)));
        }
    }
    None
}

/// Hover's last run of the fillet call whose name is at `name`: its
/// summary line and selected edges (the first eight), from the last
/// rendered run of this text. A call that ran more than once (in a loop)
/// shows its first run and how many there were.
pub fn hover_call(
    src: &SourceFile,
    path: &Path,
    fillets: &[Value],
    name: (u32, u32),
) -> Option<String> {
    // The innermost call around the name: an outer call's span holds the
    // inner call's name too.
    let mut best: Option<(&Value, (u32, u32))> = None;
    let mut runs = 0;
    for f in fillets {
        if !crate::sketch::in_file(f, path) {
            continue;
        }
        let Some((a, b)) = crate::sketch::span_of(src, f) else {
            continue;
        };
        if !(a <= name.0 && name.1 <= b) {
            continue;
        }
        match best {
            Some((_, (x, y))) if (a, b) == (x, y) => runs += 1,
            Some((_, (x, y))) if b - a >= y - x => {}
            _ => {
                best = Some((f, (a, b)));
                runs = 1;
            }
        }
    }
    let (f, _) = best?;
    let mut out = format!(
        "Last run: `{}`",
        session::fillets::line_text(f).replace('`', "'")
    );
    if runs > 1 {
        out.push_str(&format!(" (the first of {runs} runs)"));
    }
    let edges = f["edges"].as_array().map(Vec::as_slice).unwrap_or_default();
    for e in edges.iter().take(8) {
        out.push_str(&format!("\n- {}", session::fillets::edge_text(e)));
    }
    if edges.len() > 8 {
        out.push_str(&format!("\n- ... {} more", edges.len() - 8));
    }
    Some(out)
}

/// Hover on a named argument of a builtin call (`r` in `fillet_edges(r =
/// 2)`, `scale` in `linear_extrude(scale = 2)`): the parameter's line of
/// the reference. `None` where the word is not an argument name.
pub fn hover_argument(ctx: &crate::Ctx<'_>, offset: u32) -> Option<(String, (u32, u32))> {
    let f = ctx.file();
    let w = context::word_at(f, offset)?;
    let next = f
        .program
        .cst
        .tokens()
        .iter()
        .filter(|t| !t.kind.is_trivia())
        .find(|t| t.start >= w.1)?;
    if next.kind != K::Eq {
        return None;
    }
    let call = context::enclosing_call(f, w.0)?;
    let scope = f.index.scope_at(call.name_span.0);
    let order = if call.statement {
        [Ns::Module, Ns::Function]
    } else {
        [Ns::Function, Ns::Module]
    };
    let Some(Target::Builtin(entries)) = order.iter().find_map(|ns| {
        ctx.world
            .resolve(f, scope, call.name_span.0, &call.name, *ns)
    }) else {
        return None;
    };
    let name = f.slice(w);
    for e in entries {
        for p in &e.params {
            if p.name.split(',').any(|n| n.trim() == name) {
                let mut s = format!("```openscad\n{name}\n```\nArgument of `{}`", e.signature);
                if !p.ty.is_empty() {
                    s.push_str(&format!(": *{}*", p.ty));
                }
                if let Some(d) = &p.default {
                    s.push_str(&format!(" = `{d}`"));
                }
                if !p.doc.is_empty() {
                    s.push_str(&format!("\n\n{}", p.doc));
                }
                if let Some(l) = e.extension_label() {
                    s.push_str(&format!("\n\n*{l}*"));
                }
                return Some((s, w));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every atom and operator offered is the evaluator's language: each
    /// parses, with its placeholders filled in, so completion never
    /// writes a selector the call then rejects.
    #[test]
    fn every_offered_atom_parses() {
        let allowed = session::fillets::selector::Allowed {
            part: true,
            anchor: true,
        };
        let fill = |s: &str| -> String {
            // `${1:z}` to `z`.
            let mut out = String::new();
            let mut rest = s;
            while let Some(i) = rest.find("${") {
                out.push_str(&rest[..i]);
                let body = &rest[i + 2..];
                let end = body.find('}').unwrap();
                out.push_str(body[..end].split_once(':').map_or("", |(_, d)| d));
                rest = &body[end + 1..];
            }
            out.push_str(rest);
            out.replace("x0", "0")
                .replace("y0", "0")
                .replace("z0", "0")
                .replace("x1", "1")
                .replace("y1", "1")
                .replace("z1", "1")
        };
        for a in ATOMS.iter().chain(std::iter::once(&ANCHOR)) {
            let text = fill(a.insert);
            session::fillets::selector::parse(&text, allowed)
                .unwrap_or_else(|e| panic!("{text}: {e:?}"));
        }
        for o in OPERATORS {
            let text = if o.label == "not" {
                "not convex".to_string()
            } else {
                format!("convex {} |z", o.label)
            };
            session::fillets::selector::parse(&text, allowed)
                .unwrap_or_else(|e| panic!("{text}: {e:?}"));
        }
        for w in WORDS {
            assert!(
                describe_atom(w).is_some(),
                "{w} has no description for hover"
            );
        }
    }

    #[test]
    fn operands_and_operators() {
        assert!(expects_operand(""));
        assert!(expects_operand("|z and "));
        assert!(expects_operand("(convex or "));
        assert!(expects_operand("not "));
        assert!(!expects_operand("|z "));
        assert!(!expects_operand("%line "));
        assert!(!expects_operand("child(0, 1) "));
        // `%and` would not be an operator.
        assert!(!expects_operand("x"));
        assert!(in_atom_arguments("child("));
        assert!(in_atom_arguments("convex and box(0, 0, "));
        assert!(!in_atom_arguments("(convex or "));
        assert!(!in_atom_arguments("child(0) and "));
    }
}
