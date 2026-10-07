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

// --- Geometry queries (`child_bounds()`, `child_measure()`) ---------------

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use eval::{Facts, GeometryOracle, OracleError};

/// What a fake oracle does when asked.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// Answer with the subtree's node count as its box.
    Count,
    /// Stop as a cancelled render does.
    Cancel,
    /// Stop as a render that passed the triangle limit does.
    Limit,
}

/// An oracle that answers from the subtree's shape alone, recording what
/// it was asked: no geometry is needed to test the evaluator's side.
#[derive(Debug)]
struct Fake {
    mode: Mode,
    calls: AtomicUsize,
    /// The `.csg` of each subtree asked about.
    seen: std::sync::Mutex<Vec<String>>,
}

impl Fake {
    fn new(mode: Mode) -> Arc<Fake> {
        Arc::new(Fake {
            mode,
            calls: AtomicUsize::new(0),
            seen: Default::default(),
        })
    }
}

impl GeometryOracle for Fake {
    fn measure(
        &self,
        subtree: &Node,
        interrupt: Option<&Arc<AtomicBool>>,
        guard: Option<&Arc<eval::limits::Guard>>,
    ) -> Result<Facts, OracleError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let mut n = 0;
        let mut indices = Vec::new();
        walk(subtree, &mut |c| {
            n += 1;
            indices.push(c.index);
        });
        // Numbered from 0 (the group standing in for `children()`'s
        // node), whatever the model's counter was: `GEOMETRY` makes a
        // thousand nodes before its first query, and no instance it
        // queries has a hundred. (Not densely: a node the instance made
        // and dropped, an empty `echo()`'s, still took an index.)
        assert_eq!(indices[0], 0);
        assert!(indices.iter().all(|&i| i < 100), "{indices:?}");
        let mut distinct = indices.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), indices.len());
        let csg = subtree
            .children
            .iter()
            .map(|c| eval::dump::csg(c, Path::new("/"), &lang::loader::StdFs))
            .collect();
        self.seen.lock().unwrap().push(csg);
        match self.mode {
            Mode::Count => Ok(if n == 1 {
                Facts::Empty
            } else {
                Facts::Solid {
                    min: [0.0; 3],
                    max: [n as f64; 3],
                    volume: n as f64,
                    surface_area: 0.0,
                }
            }),
            Mode::Cancel => {
                interrupt
                    .expect("the evaluation's flag")
                    .store(true, Ordering::Relaxed);
                Err(OracleError::Interrupted)
            }
            Mode::Limit => {
                let g = guard.expect("the evaluation's limits");
                let e = g
                    .exceeds(eval::limits::Limit::Triangles, 1e9, "sphere()")
                    .expect("over");
                g.trip(e);
                Err(OracleError::Interrupted)
            }
        }
    }
}

fn with_oracle(o: &Arc<Fake>, call_memo: bool) -> Options {
    Options {
        geometry: Some(o.clone() as Arc<dyn GeometryOracle>),
        ..opts(call_memo)
    }
}

/// A model asking geometry queries in the shapes of `MODELS`: nested,
/// repeated (for the call memo), reused and not, with `rands()` in the
/// child. Answers are printed only on `echo("Q", ...)` lines.
const GEOMETRY: &str = "for (i = [1:1000]) cube(i);\n\
     module mark(n) {\n\
       cube(n);\n\
       echo(\"mark\", n, $fn, rands(0, 1, 1, n));\n\
     }\n\
     module q(tag) {\n\
       b = child_bounds(0);\n\
       m = child_measure(0);\n\
       echo(\"Q\", tag, b, m);\n\
       children(0);\n\
     }\n\
     q(\"outer\") q(\"inner\") translate([1, 2, 3]) mark(1);\n\
     module dollar() {\n\
       b = child_bounds(0);\n\
       echo(\"Q\", b);\n\
       let($fn = 7) children(0);\n\
     }\n\
     dollar() mark(2);\n\
     module r() {\n\
       b = child_bounds();\n\
       children();\n\
     }\n\
     r() { echo(rands(0, 1, 2)); mark(3); }\n\
     module unused() {\n\
       b = child_measure(0);\n\
       echo(\"Q\", b);\n\
     }\n\
     unused() { echo(\"never\"); mark(4); }\n\
     module pegs() for (i = [0:3]) translate([i * 3, 0, 0]) q(str(\"peg \", i)) mark(6);\n\
     pegs();\n\
     pegs();\n\
     module empty() { echo(\"Q\", child_bounds(), child_measure()); children(); }\n\
     empty();\n";

fn without_geometry_queries(text: &str) -> String {
    let mut out = text
        .replace("child_bounds(", "no_query(")
        .replace("child_measure(", "no_query(");
    out.push_str("function no_query(i) = undef;\n");
    out
}

/// As `queries_change_nothing_else`, for the queries that render: the
/// tree, keys, numbering and messages are those of the model with every
/// query replaced by a constant, but for the lines printing the answers
/// and the `query-empty` warning.
#[test]
fn geometry_queries_change_nothing_else() {
    let unmarked = |out: &Out| -> Vec<String> {
        unmarked(out)
            .into_iter()
            .filter(|l| !l.contains("the children render to nothing"))
            .collect()
    };
    for call_memo in [true, false] {
        let o = Fake::new(Mode::Count);
        let with = evaluate(GEOMETRY, &with_oracle(&o, call_memo));
        let without = evaluate(&without_geometry_queries(GEOMETRY), &opts(call_memo));
        assert_eq!(with.csg, without.csg);
        assert_eq!(with.key, without.key);
        assert_eq!(with.indices, without.indices);
        assert_eq!(unmarked(&with), unmarked(&without));
        assert!(o.calls.load(Ordering::Relaxed) > 0);
    }
    let a = evaluate(GEOMETRY, &with_oracle(&Fake::new(Mode::Count), true));
    let b = evaluate(GEOMETRY, &with_oracle(&Fake::new(Mode::Count), false));
    assert_eq!(a, b, "the call memo changed the output");
}

/// The oracle is asked about the child as `children(i)` would make it at
/// the query: in the module's frame, with the `$` variables there; and
/// its answer reaches the program as a box and an object.
#[test]
fn the_oracle_sees_the_child_as_children_makes_it() {
    let o = Fake::new(Mode::Count);
    let text = "module m() {\n\
                  b = child_bounds(0);\n\
                  m = child_measure(0);\n\
                  echo(b);\n\
                  echo(m);\n\
                  translate([9, 9, 9]) children(0);\n\
                }\n\
                cube(5);\n\
                $fn = 6;\n\
                m() rotate(90) sphere(1);\n";
    let out = evaluate(text, &with_oracle(&o, true));
    let seen = o.seen.lock().unwrap().clone();
    // Asked twice, about the same instance (a second query reads the kept
    // instance again).
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], seen[1]);
    assert!(seen[0].contains("multmatrix"), "{}", seen[0]);
    assert!(seen[0].contains("sphere($fn = 6"), "{}", seen[0]);
    assert!(
        !seen[0].contains(" 9]"),
        "the module's own transform leaked in"
    );
    assert_eq!(
        out.lines,
        [
            "ECHO: [[0, 0, 0], [3, 3, 3]] @0",
            "ECHO: { dim = 3; empty = false; bounds = [[0, 0, 0], [3, 3, 3]]; size = [3, 3, 3]; center = [1.5, 1.5, 1.5]; volume = 3; surface_area = 0; } @0",
        ]
    );
}

/// No oracle (a host that does not render): `query-unavailable`, and the
/// answer is undef.
#[test]
fn without_an_oracle_geometry_queries_are_unavailable() {
    let text = "module m() { echo(child_bounds(0), child_measure()); children(); }\n\
                m() { echo(\"child\"); cube(1); }\n";
    let out = evaluate(text, &opts(true));
    assert_eq!(
        out.lines,
        [
            "WARNING: child_bounds(): no geometry is available here (this host does not render), so the answer is undef @1",
            "WARNING: child_measure(): no geometry is available here (this host does not render), so the answer is undef @1",
            "ECHO: undef, undef @0",
            "ECHO: \"child\" @0",
        ]
    );
}

fn run_guarded(
    text: &str,
    limits: eval::limits::Limits,
    mode: Mode,
) -> (eval::Evaluation, Vec<String>) {
    let program = lang::parse_file(
        PathBuf::from("/nonexistent/q.scad"),
        text.as_bytes().to_vec(),
    );
    let flag = Arc::new(AtomicBool::new(false));
    let guard = Arc::new(eval::limits::Guard::new(limits, flag.clone(), None));
    let o = Options {
        interrupt: Some(flag),
        guard: Some(guard),
        ..with_oracle(&Fake::new(mode), true)
    };
    let mut out = Lines(Vec::new());
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &o,
            &mut out,
        )
    });
    (ev, out.0)
}

/// A query render the request's cancellation stopped stops the
/// evaluation as a cancellation; one that passed a limit stops it with
/// the limit's error.
#[test]
fn a_stopped_query_render_stops_the_evaluation() {
    let text = "module m() { b = child_bounds(0); echo(\"after\", b); children(0); }\n\
                m() sphere(1);\n\
                echo(\"end\");\n";
    let agent = eval::limits::Limits::AGENT;
    let (ev, lines) = run_guarded(text, agent, Mode::Cancel);
    assert!(ev.interrupted, "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains("ECHO")), "{lines:?}");
    let (ev, lines) = run_guarded(text, agent, Mode::Limit);
    assert!(!ev.interrupted && ev.aborted, "{lines:?}");
    assert!(
        lines[0].starts_with(
            "ERROR: Resource limit exceeded: sphere() would make 1,000,000,000 triangles, over the triangles limit of"
        ),
        "{lines:?}"
    );
    assert!(lines.iter().all(|l| !l.contains("ECHO")), "{lines:?}");
}

/// `Limits::queries` counts every query that renders (`child_anchors()`
/// does not), and stops the evaluation past it.
#[test]
fn the_queries_limit_counts_renders() {
    let text = "module m() { a = child_anchors(0); b = child_bounds(0); echo(b); children(0); }\n\
                for (i = [1:3]) m() cube(i);\n";
    let limit = |n: u64| eval::limits::Limits {
        queries: Some(n),
        ..eval::limits::Limits::NONE
    };
    let (ev, lines) = run_guarded(text, limit(3), Mode::Count);
    assert!(!ev.aborted, "{lines:?}");
    assert_eq!(lines.len(), 3);
    let (ev, lines) = run_guarded(text, limit(2), Mode::Count);
    assert!(ev.aborted);
    assert_eq!(
        lines[..2],
        [
            "ECHO: [[0, 0, 0], [2, 2, 2]] @0",
            "ECHO: [[0, 0, 0], [2, 2, 2]] @0"
        ]
    );
    assert!(
        lines[2].starts_with(
            "ERROR: Resource limit exceeded: child_bounds() would make 3 geometry queries, over the queries limit of 2"
        ),
        "{lines:?}"
    );
}
