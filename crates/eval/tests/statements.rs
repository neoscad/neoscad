//! Statement execution, compared between the two evaluators.
//!
//! The heap evaluator (`--features heap-eval`, `src/heap.rs`) re-implements
//! statement execution as frames on a heap stack, and its output must be
//! byte-identical to the recursive evaluator's. Each case here writes its
//! messages, its `.csg` dump and every node's index and kind; the expected
//! files under `tests/statements/` were written by the recursive evaluator
//! (`NEOSCAD_BLESS=1 cargo test --test statements`) and both builds must
//! match them. The cases aim at the paths where the two could drift: `$`
//! variables through `children()`, `for` and `let`, children chains and
//! indices, every kind of `for` value, errors and their traces at each
//! point of a module call, `--hardwarnings`, the call memo's replays, the
//! counted depth limit and an interrupt in the middle of a recursion.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eval::{Node, Options};

struct Lines {
    lines: Vec<String>,
    /// Raised after this many messages, when set.
    stop: Option<(usize, Arc<AtomicBool>)>,
}

impl eval::Output for Lines {
    fn message(&mut self, m: &eval::Message<'_>) {
        let mut s = format!(
            "{}: {}",
            m.diag.severity.openscad_label(),
            String::from_utf8_lossy(m.text)
        );
        if m.diag.span.is_some() {
            s.push_str(&format!(" @{}", m.diag.line));
        }
        self.lines.push(s);
        if let Some((n, flag)) = &self.stop
            && self.lines.len() >= *n
        {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

fn indices(n: &Node, depth: usize, out: &mut String) {
    // Iterative: the deep cases' trees are thousands of levels deep.
    let mut todo = vec![(n, depth)];
    while let Some((n, d)) = todo.pop() {
        let kind = format!("{:?}", n.kind);
        let kind = kind.split(['(', ' ', '{']).next().unwrap_or("");
        out.push_str(&format!("{d} {} {kind}\n", n.index));
        for c in n.children.iter().rev() {
            todo.push((c, d + 1));
        }
    }
}

fn run(src: &str, opts: &Options, stop: Option<usize>) -> (String, eval::Evaluation) {
    let path = PathBuf::from("/nonexistent/test.scad");
    let mut text = src.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    assert!(!program.has_syntax_errors(), "syntax error in test program");
    let mut opts = opts.clone();
    let mut out = Lines {
        lines: Vec::new(),
        stop: None,
    };
    if let Some(n) = stop {
        let flag = Arc::new(AtomicBool::new(false));
        opts.interrupt = Some(flag.clone());
        out.stop = Some((n, flag));
    }
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &opts,
            &mut out,
        )
    });
    let mut s = out.lines.join("\n");
    s.push_str(&format!(
        "\n-- aborted {} interrupted {} hard {}\n-- csg\n",
        ev.aborted, ev.interrupted, ev.hard_warning
    ));
    s.push_str(&eval::dump::csg(
        &ev.root,
        std::path::Path::new("/nonexistent"),
        &*opts.fs,
    ));
    s.push_str("\n-- nodes\n");
    indices(&ev.root, 0, &mut s);
    (s, ev)
}

const HEAP: bool = cfg!(feature = "heap-eval");

fn with_depth(depth: u64, opts: Options) -> Options {
    let limits = eval::limits::Limits {
        depth: Some(depth),
        ..eval::limits::Limits::NONE
    };
    let flag = Arc::new(AtomicBool::new(false));
    Options {
        guard: Some(Arc::new(eval::limits::Guard::new(
            limits,
            flag.clone(),
            None,
        ))),
        interrupt: Some(flag),
        ..opts
    }
}

fn check(name: &str, src: &str, opts: &Options, stop: Option<usize>) {
    let (got, _) = run(src, opts, stop);
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/statements")
        .join(format!("{name}.expected"));
    // Blessed by the recursive evaluator, the reference.
    if !HEAP && std::env::var_os("NEOSCAD_BLESS").is_some() {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file:?}: {e}"));
    if got != want {
        let (g, w): (Vec<_>, Vec<_>) = (got.lines().collect(), want.lines().collect());
        let at = g
            .iter()
            .zip(&w)
            .position(|(a, b)| a != b)
            .unwrap_or(g.len().min(w.len()));
        panic!(
            "{name}: differs at line {at}:\n got: {:?}\nwant: {:?}",
            g.get(at),
            w.get(at)
        );
    }
}

#[test]
fn special_variables_through_children_for_and_let() {
    check(
        "dollar",
        r#"
module a() { $x = 1; children(); echo("a", $x, $children); }
module b() { echo("b", $x, $parent_modules, parent_module(1)); children(); }
a() { b() echo("c", $x); let($x = 2) b() echo("d", $x); for ($x = [3, 4]) b(); }
module outer() { $y = "o"; inner() echo("in children", $y); }
module inner() { $y = "i"; echo("inner", $y); children(); }
outer();
translate([1, 0, 0], $fn = 7) { echo($fn); sphere(1); }
"#,
        &Options::default(),
        None,
    );
}

#[test]
fn children_indices_and_chains() {
    check(
        "children",
        r#"
module m() { children(1); children([0, 2]); children(5); children([0:1]); children("x"); children([0, "y"]); echo($children); }
m() { echo(0); echo(1); echo(2); }
module pass() children();
module deep(n) { if (n > 0) deep(n - 1) children(); else children(); }
deep(5) pass() { echo("bottom", $parent_modules); cube(1); }
module noch() children(0);
noch();
children();
module sel(k) children(k);
sel(1) { cube(1); sphere(2); }
"#,
        &Options::default(),
        None,
    );
}

#[test]
fn for_values_and_variables() {
    check(
        "for",
        r#"
for (c = "héllo") echo(c);
for (i = [0:2:5], j = [i:i+1]) echo(i, j);
for ($v = [1, 2]) echo($v);
for (u = undef) echo("never");
for (n = 7) echo(n);
for (k = [[1, 2], [3]]) echo(k);
intersection_for (i = [0:1]) translate([i, 0, 0]) cube(2);
for (i = [1:0]) echo("empty");
for (i = [0:1:2e6]) echo("too many");
for () echo("no variables");
for (i = [0:2]) { x = i * 2; echo(x); cube(x); }
module f(n) for (i = [0:n]) let(k = i) translate([k, 0, 0]) cube(k + 1);
f(3);
for (i = [0:1], $j = [5, 6], k = "ab") echo(i, $j, k);
"#,
        &Options::default(),
        None,
    );
}

#[test]
fn errors_and_their_traces() {
    check(
        "assert",
        r#"
module leaf(v) { assert(v < 3, str("too big ", v)); cube(v); }
module mid(n) { for (i = [0:n]) let(k = i) translate([k, 0, 0]) leaf(k); }
module top() { mid(5); }
top();
echo("not reached");
"#,
        &Options::default(),
        None,
    );
    check(
        "init_scope",
        r#"
module m(x) { y = x + 1; z = assert(false, "in the body"); echo(y); }
module n() { m(1); }
n();
"#,
        &Options::default(),
        None,
    );
    check(
        "children_scope",
        r#"
module c() { echo("before"); children(); echo("after"); }
c() { z = assert(false, "in children scope"); echo(z); }
"#,
        &Options::default(),
        None,
    );
    check(
        "for_range",
        r#"
module m() { for (i = [0:1], j = assert(i < 1, "second")) echo(i, j); }
translate([1, 0, 0]) m();
"#,
        &Options::default(),
        None,
    );
    check(
        "args",
        r#"
module m(a) cube(a);
module n() m(assert(false, "argument"));
if (true) n();
"#,
        &Options::default(),
        None,
    );
    check(
        "if_condition",
        r#"
module m() if (assert(false, "condition")) cube(1);
let (q = 1) m();
"#,
        &Options::default(),
        None,
    );
    check(
        "echo_let",
        r#"
module m() { echo("e") let(a = 1, b = assert(false, "let")) cube(a); }
m();
"#,
        &Options::default(),
        None,
    );
    check(
        "unknown",
        r#"
module m() { nosuch(1) cube(1); echo($children); cube(2); }
m() { nosuch2(); }
"#,
        &Options::default(),
        None,
    );
}

#[test]
fn hardwarnings_stop_inside_modules() {
    check(
        "hardwarnings",
        r#"
module m(n) { if (n > 0) translate([n, 0, 0]) m(n - 1); else echo(undefined_var); }
m(3);
echo("after");
"#,
        &Options {
            hardwarnings: true,
            ..Options::default()
        },
        None,
    );
    check(
        "hardwarnings_builtin",
        r#"
module m() { cube(-1); sphere(1); }
group() m();
"#,
        &Options {
            hardwarnings: true,
            check_parameter_ranges: true,
            ..Options::default()
        },
        None,
    );
}

#[test]
fn the_depth_limit_counts_module_calls() {
    let src = r#"
module r(n, s = "x") { translate([n, 0, 0]) r(n + 1, s); }
r(0);
"#;
    check("depth", src, &with_depth(50, Options::default()), None);
    check(
        "depth_no_params",
        src,
        &with_depth(
            50,
            Options {
                trace_usermodule_parameters: false,
                ..Options::default()
            },
        ),
        None,
    );
    // Within the limit, through `if`, `children()` and `for`.
    check(
        "depth_within",
        r#"
module m(n) { if (n > 0) for (i = [0]) m(n - 1) children(); else children(); }
m(49) cube(1);
"#,
        &with_depth(50, Options::default()),
        None,
    );
}

#[test]
fn call_memo_replays() {
    let src = r#"
module w(s) { for (i = [0:3]) translate([i, 0, 0]) cube(s + $fn / 100); echo("w", s, $fn); }
w(1); w(1); $fn = 8; w(1); translate([1, 0, 0]) w(1); w(1) cube(1);
module k() { children(); echo("k", $children); }
k() w(2); k() w(2); k() { w(2); }
module rec(n) { if (n > 0) { rec(n - 1); rec(n - 1); } else w(3); }
rec(4);
"#;
    check("memo", src, &Options::default(), None);
    check(
        "memo_off",
        src,
        &Options {
            call_memo: false,
            ..Options::default()
        },
        None,
    );
}

#[test]
fn parts() {
    check(
        "parts",
        r#"
part("a") { part("b") cube(1); part("b") cube(2); part(3) cube(3); }
module p() part("m") children();
p() part("n") sphere(1);
"#,
        &Options {
            parts: true,
            ..Options::default()
        },
        None,
    );
}

#[test]
fn an_interrupt_unwinds_a_recursion() {
    check(
        "interrupt",
        r#"
module m(n) { echo(n); if (n > 0) translate([1, 0, 0]) for (i = [0]) let(k = n) m(k - 1); }
m(100);
echo("after");
"#,
        &Options::default(),
        Some(30),
    );
}

/// The heap evaluator's point: a module recursion through statements
/// takes no native stack, so it reaches the counted limit on a thread of
/// 128 KiB, through `translate`, `children()` and `for` alike.
#[cfg(feature = "heap-eval")]
#[test]
fn deep_module_recursion_on_a_small_thread() {
    let depth = eval::limits::DEFAULT_DEPTH as usize;
    let cases = [
        "module m(n) { if (n > 0) m(n - 1); }",
        "module m(n) { if (n > 0) translate([1, 0, 0]) m(n - 1); else cube(1); }",
        "module m(n) { if (n > 0) m(n - 1) children(); else children(); }",
        "module m(n) { for (i = [0]) if (n > 0) let(k = n - 1) m(k); }",
    ];
    for (k, case) in cases.into_iter().enumerate() {
        for (n, ok) in [(depth - 1, true), (depth + 10, false)] {
            let src = format!("{case}\nm({n}) cube(1);\n\x03\n");
            let program = lang::parse_file(PathBuf::from("/nonexistent/t.scad"), src.into_bytes());
            let mut out = eval::Collect::default();
            let ev = std::thread::Builder::new()
                .stack_size(128 << 10)
                .spawn(move || {
                    let ev = eval::evaluate(
                        &program,
                        &[],
                        &[],
                        PathBuf::from("/nonexistent"),
                        &Options::default(),
                        &mut out,
                    );
                    (ev.aborted, out)
                })
                .unwrap()
                .join()
                .unwrap();
            let (aborted, out) = ev;
            let text: Vec<String> = out.lines.into_iter().map(|l| l.2).collect();
            assert_eq!(
                aborted,
                !ok,
                "case {k}, m({n}): {:?}",
                &text[..text.len().min(3)]
            );
            if !ok {
                assert_eq!(text[0], "Recursion detected calling module 'm'", "case {k}");
            }
        }
    }
}
