//! NeoSCAD's render-free queries (`--enable query`; `crate::query` and
//! `docs/language-extensions.md`, section 5.3) at the evaluator: anchors
//! and queries change nothing a model outputs, which is what lets a
//! `.csg` export of a model that uses them run in stock OpenSCAD.
//!
//! - An `anchor()` statement makes no node: a model's `.csg`, geometry
//!   keys and node numbering are those of the same model without it, and
//!   neither the dump nor the keys read a node's anchors.
//! - A query instantiates a child early, in a sandbox: with every query
//!   replaced by a constant, the tree, the keys and the messages are the
//!   same (but for the lines that print a query's answer), whether the
//!   call memo is on or off, whether `children()` reuses the instance or
//!   not, with `rands()` and messages in the child.
//! - Nested queries stop at the native budget with OpenSCAD's recursion
//!   error, not a crash.

use std::path::{Path, PathBuf};

use eval::{Extension, Extensions, Node, Options};

struct Lines(Vec<String>);

impl eval::Output for Lines {
    fn message(&mut self, m: &eval::Message<'_>) {
        self.0.push(format!(
            "{}: {} @{}",
            m.diag.severity.openscad_label(),
            String::from_utf8_lossy(m.text),
            m.diag.line
        ));
    }
}

/// What a model gives: its messages, `.csg`, root geometry key, the node
/// indices in tree order, and the tree's anchors (in tree order, with
/// their node's index).
#[derive(Debug, PartialEq)]
struct Out {
    lines: Vec<String>,
    csg: String,
    key: u128,
    indices: Vec<usize>,
    anchors: Vec<(usize, String)>,
    aborted: bool,
}

fn query() -> Extensions {
    Extensions::NONE
        .with(Extension::Query)
        .with(Extension::Sketch)
}

fn evaluate(text: &str, opts: &Options) -> Out {
    let path = PathBuf::from("/nonexistent/q.scad");
    let program = lang::parse_file(path, text.as_bytes().to_vec());
    assert!(!program.has_syntax_errors(), "{text}");
    let mut out = Lines(Vec::new());
    let dir = PathBuf::from("/nonexistent");
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(&program, &[], &[], dir.clone(), opts, &mut out)
    });
    let csg = eval::dump::csg(&ev.root, Path::new("/"), &lang::loader::StdFs);
    let key = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs).get(&ev.root);
    let mut indices = Vec::new();
    let mut anchors = Vec::new();
    walk(&ev.root, &mut |n| {
        indices.push(n.index);
        for a in n.anchors.iter().flat_map(|a| a.iter()) {
            anchors.push((n.index, format!("{a:?}")));
        }
    });
    Out {
        lines: out.0,
        csg,
        key,
        indices,
        anchors,
        aborted: ev.aborted,
    }
}

fn walk(n: &Node, f: &mut dyn FnMut(&Node)) {
    let mut stack = vec![n];
    while let Some(n) = stack.pop() {
        f(n);
        stack.extend(n.children.iter().rev());
    }
}

fn opts(call_memo: bool) -> Options {
    Options {
        extensions: query(),
        call_memo,
        ..Options::default()
    }
}

/// Models whose anchors and queries can be taken out line by line: every
/// `anchor()` statement is on a line of its own, and every query's answer
/// is printed only on lines marked `// Q`.
const MODELS: &[&str] = &[
    // Anchors in transforms, loops, conditionals and module bodies, and a
    // module whose only child is an anchor.
    "module peg(h) {\n\
       cylinder(d = 2, h = h);\n\
       anchor(\"top\", [0, 0, h], [0, 0, 1]);\n\
     }\n\
     module only() {\n\
       anchor(\"alone\", [1, 2, 3]);\n\
     }\n\
     module show() {\n\
       a = child_anchors();\n\
       echo(\"Q\", a);\n\
       children();\n\
     }\n\
     show() translate([1, 0, 0]) peg(3);\n\
     show() for (i = [0:2]) rotate(i * 30) {\n\
       anchor(str(\"p\", i), [i, 0]);\n\
       square(1);\n\
     }\n\
     show() if (true) {\n\
       anchor(\"if\", [0, 0, 0]);\n\
     }\n\
     only();\n\
     union() { only(); square(1); }\n\
     echo(\"after\");\n",
    // The sandbox: nested queries, `$` variables read by the child, messages
    // and `rands()` in the child, a child never instantiated, a child
    // instantiated twice, a call repeated for the call memo.
    "module mark(n) {\n\
       cube(1);\n\
       anchor(str(\"m\", n), [n, 0, 0]);\n\
       echo(\"mark\", n, $fn, rands(0, 1, 1, n));\n\
     }\n\
     module q(tag) {\n\
       a = child_anchors(0);\n\
       echo(\"Q\", tag, a);\n\
       children(0);\n\
     }\n\
     q(\"outer\") q(\"inner\") translate([1, 2, 3]) mark(1);\n\
     module dollar() {\n\
       a = child_anchors(0);\n\
       echo(\"Q\", a);\n\
       let($fn = 7) children(0);\n\
     }\n\
     dollar() mark(2);\n\
     module r() {\n\
       a = child_anchors();\n\
       children();\n\
     }\n\
     r() { echo(rands(0, 1, 2)); mark(3); cube(undefined_size); }\n\
     echo(after = rands(0, 1, 1));\n\
     module unused() {\n\
       a = child_anchors(0);\n\
       echo(\"Q\", a);\n\
     }\n\
     unused() { echo(\"never\"); mark(4); }\n\
     module twice() {\n\
       a = child_anchors(0);\n\
       children(0);\n\
       translate([5, 0, 0]) children(0);\n\
     }\n\
     twice() mark(5);\n\
     module pegs() for (i = [0:3]) translate([i * 3, 0, 0]) q(str(\"peg \", i)) mark(6);\n\
     pegs();\n\
     pegs();\n",
    // A sketch child: solved once, its messages printed once.
    "module q() {\n\
       a = child_anchors(0);\n\
       echo(\"Q\", a);\n\
       children(0);\n\
     }\n\
     q() translate([2, 0]) sketch(name = \"s\") {\n\
       p = point([1, 2]);\n\
       c = circle(p, 3);\n\
       anchor(\"centre\", p);\n\
       anchor(\"loose\", [0, 0]);\n\
     }\n",
];

/// `text` without its `anchor()` lines.
fn without_anchors(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with("anchor("))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// `text` with every query replaced by a function of the same arguments
/// that returns `undef` (the `echo("Q", ...)` lines that print an answer
/// stay, so that the echo nodes are numbered alike).
fn without_queries(text: &str) -> String {
    let mut out = text.replace("child_anchors(", "no_query(");
    out.push_str("function no_query(i) = undef;\n");
    out
}

/// The messages, less the answers printed by `echo("Q", ...)`.
fn unmarked(out: &Out) -> Vec<String> {
    out.lines
        .iter()
        .filter(|l| !l.starts_with("ECHO: \"Q\""))
        .cloned()
        .collect()
}

#[test]
fn anchors_change_no_csg_no_key_and_no_numbering() {
    for text in MODELS {
        let with = evaluate(text, &opts(true));
        assert!(!with.anchors.is_empty(), "{text}");
        let without = evaluate(&without_anchors(text), &opts(true));
        assert_eq!(with.csg, without.csg, "{text}");
        assert_eq!(with.key, without.key, "{text}");
        assert_eq!(with.indices, without.indices, "{text}");
        assert!(
            without.anchors.is_empty() || text.contains("sketch("),
            "{text}"
        );
    }
}

#[test]
fn neither_the_dump_nor_the_keys_read_anchors() {
    for text in MODELS {
        let path = PathBuf::from("/nonexistent/q.scad");
        let program = lang::parse_file(path, text.as_bytes().to_vec());
        let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
            eval::evaluate(
                &program,
                &[],
                &[],
                PathBuf::from("/nonexistent"),
                &opts(true),
                &mut Lines(Vec::new()),
            )
        });
        let mut bare = ev.root.clone();
        let mut stack = vec![&mut bare];
        let mut cleared = 0;
        while let Some(n) = stack.pop() {
            cleared += usize::from(n.anchors.take().is_some());
            stack.extend(n.children.iter_mut());
        }
        assert!(cleared > 0);
        let fs = lang::loader::StdFs;
        let (k1, k2) = (
            eval::dump::Keys::new(&ev.root, &fs),
            eval::dump::Keys::new(&bare, &fs),
        );
        let mut s1 = vec![&ev.root];
        let mut s2 = vec![&bare];
        while let (Some(a), Some(b)) = (s1.pop(), s2.pop()) {
            assert_eq!(k1.get(a), k2.get(b));
            s1.extend(a.children.iter());
            s2.extend(b.children.iter());
        }
        assert_eq!(
            eval::dump::csg(&ev.root, Path::new("/"), &fs),
            eval::dump::csg(&bare, Path::new("/"), &fs)
        );
    }
}

#[test]
fn queries_change_nothing_else() {
    for text in MODELS {
        for call_memo in [true, false] {
            let with = evaluate(text, &opts(call_memo));
            let without = evaluate(&without_queries(text), &opts(call_memo));
            assert_eq!(with.csg, without.csg, "{text}");
            assert_eq!(with.key, without.key, "{text}");
            assert_eq!(with.indices, without.indices, "{text}");
            assert_eq!(with.anchors, without.anchors, "{text}");
            assert_eq!(unmarked(&with), unmarked(&without), "{text}");
            assert!(with.lines.len() > unmarked(&with).len(), "{text}");
        }
        // The call memo replays repeated calls, queries inside them
        // included, to the same output.
        assert_eq!(evaluate(text, &opts(true)), evaluate(text, &opts(false)));
    }
}

/// Flag off, the names are OpenSCAD's: unknown, with its warnings, and a
/// program's own definitions of them work as they always have, flag on or
/// off.
#[test]
fn off_the_names_are_unknown_and_own_definitions_win() {
    let calls = "module m() { a = child_anchors(0); echo(a); children(); }\n\
                 m() { cube(1); anchor(\"x\", [0, 0, 0]); }\n";
    let off = evaluate(calls, &Options::default());
    assert_eq!(
        off.lines,
        [
            "WARNING: Ignoring unknown function 'child_anchors' @1",
            "ECHO: undef @0",
            "WARNING: Ignoring unknown module 'anchor' @2",
        ]
    );
    let own = format!(
        "function child_anchors(i) = [\"own\", i];\n\
         module anchor(name, point) echo(\"own anchor\", name);\n{calls}"
    );
    let off = evaluate(&own, &Options::default());
    let on = evaluate(&own, &opts(true));
    assert_eq!(off, on);
    assert_eq!(
        on.lines,
        ["ECHO: [\"own\", 0] @0", "ECHO: \"own anchor\", \"x\" @0"]
    );
}

/// Each level of queries inside queried children holds native stack, so
/// a deep nest stops with OpenSCAD's recursion error at the frame budget
/// (a wasm32 build's guard) rather than overflowing.
#[test]
fn nested_queries_stop_at_the_native_budget() {
    let text = "module q() { a = child_anchors(0); children(0); }\n\
                module rec(n) { if (n > 0) q() rec(n - 1); else cube(1); }\n\
                rec(200);\n";
    let o = Options {
        frame_limit: 2_000,
        ..opts(true)
    };
    let out = evaluate(text, &o);
    assert!(out.aborted);
    assert!(
        out.lines[0].starts_with("ERROR: Recursion detected calling function 'child_anchors'"),
        "{:?}",
        out.lines
    );
    // Within the budget, the same model evaluates.
    let shallow = evaluate(&text.replace("rec(200)", "rec(10)"), &o);
    assert!(!shallow.aborted, "{:?}", shallow.lines);
}
