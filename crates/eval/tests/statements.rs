//! Statement execution, held to the recursive evaluator's output.
//!
//! The heap evaluator (`src/heap.rs`) re-implemented statement execution
//! as frames on a heap stack, and its output had to be byte-identical to
//! the recursive evaluator's, which it has since replaced. Each case here
//! writes its messages, its `.csg` dump and every node's index and kind;
//! the expected files under `tests/statements/` were written by the
//! recursive evaluator (`NEOSCAD_BLESS=1 cargo test --test statements`)
//! and still hold the heap one to them. The cases aim at the paths where
//! the two could drift: `$`
//! variables through `children()`, `for` and `let`, children chains and
//! indices, every kind of `for` value, errors and their traces at each
//! point of a module call, `--hardwarnings`, the call memo's replays, the
//! counted depth limit and an interrupt in the middle of a recursion.

mod support;

use eval::Options;
use support::with_depth;

fn check(name: &str, src: &str, opts: &Options, stop: Option<usize>) {
    support::check_in("statements", name, src, opts, stop);
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
            let program = lang::parse_file(
                std::path::PathBuf::from("/nonexistent/t.scad"),
                src.into_bytes(),
            );
            let mut out = eval::Collect::default();
            let ev = std::thread::Builder::new()
                .stack_size(128 << 10)
                .spawn(move || {
                    let ev = eval::evaluate(
                        &program,
                        &[],
                        &[],
                        std::path::PathBuf::from("/nonexistent"),
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
