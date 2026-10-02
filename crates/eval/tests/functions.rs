//! Function calls, comprehensions and `let`/`assert`/`echo` expressions,
//! compared between the two evaluators.
//!
//! Under `--features heap-eval`, an expression that can reach a user call
//! runs on the heap evaluator's stack (`src/heap_expr.rs`), and its output
//! must be byte-identical to the recursive evaluator's. As in
//! `statements.rs`, the expected files under `tests/functions/` were
//! written by the recursive evaluator (`NEOSCAD_BLESS=1 cargo test --test
//! functions`), and both builds must match them. The cases aim at where
//! the two could drift: evaluation order and side effects (`echo`,
//! short-circuits), closures and function literals, `$` variables across
//! calls, tail calls and their accumulators, recursion inside
//! comprehensions, errors and their traces from every position, `assert`
//! failing deep in a recursion, `--hardwarnings`, the list limit, the call
//! memo's replays of modules that call functions, and an interrupt in the
//! middle of a function recursion.

mod support;

use eval::Options;

fn check(name: &str, src: &str, opts: &Options, stop: Option<usize>) {
    support::check_in("functions", name, src, opts, stop);
}

#[test]
fn calls_in_every_position() {
    check(
        "positions",
        r#"
function f(n) = n == 0 ? 0 : 1 + f(n - 1);
function v(n) = [n, n + 1, n + 2];
function e(x) = echo("e", x) x;
echo(f(5), -f(3), !f(0), f(2) * f(3), f(4) / f(2), f(3) % 2, f(2) ^ 3);
echo(v(f(2))[f(1)], v(1).y, v(2)[0] < f(3), [f(1), f(2)] == [1, 2]);
echo(e(1) && e(0) && e(2), e(0) || e(3) || e(4), e(false) ? e("a") : e("b"));
echo([f(0) : f(2) : f(6)], [for (i = [f(1) : f(3)]) i]);
echo(max(0, f(4)), concat(v(f(1)), [f(2)]), len(v(f(3))), str("s", f(2)));
echo(is_undef(f(1)), is_undef(undef_function(1)));
a = f(7);
b = let(x = f(2), y = x + f(1)) [x, y];
echo(a, b);
module m(p = f(3), q) echo(p, q);
m(q = f(1));
"#,
        &Options::default(),
        None,
    );
}

#[test]
fn closures_and_literals() {
    check(
        "closures",
        r#"
fact = function(n) n <= 1 ? 1 : n * fact(n - 1);
echo(fact(10));
function adder(k) = function(x) x + k;
add3 = adder(3);
echo(add3(4), adder(5)(6), (function(x) x * 2)(21));
fs = [for (i = [0:3]) function(x) x + i];
echo([for (g = fs) g(10)]);
function compose(f, g) = function(x) f(g(x));
echo(compose(add3, fact)(4));
function twice(g, x) = g(g(x));
echo(twice(function(y) [y, y], 1));
k = 7;
lit = function(x, d = k + fact(3)) x + d;
echo(lit(1), lit(1, 2), lit(d = 1, x = 2));
function rec_lit(n) = let(r = function(m) m <= 0 ? [] : concat(r(m - 1), [m])) r(n);
echo(rec_lit(5));
echo(5(1), undef(2), "s"(3));
"#,
        &Options::default(),
        None,
    );
}

#[test]
fn special_variables_across_calls() {
    check(
        "dollar",
        r#"
function d() = $v;
function through(n) = n == 0 ? d() : 1 * through(n - 1);
echo(let($v = 3) d(), let($v = 4) through(5));
function setv(n) = let($v = n) through(2);
echo(setv(9), [for ($v = [1, 2]) d() + through(1)]);
function fnd() = $fn;
module m() echo(fnd(), let($fn = 5) fnd());
m($fn = 12);
function deflt(x = $fn) = x;
echo(deflt(), let($fn = 3) deflt(), [let($fn = 4) for (i = [0:1]) deflt() + i]);
"#,
        &Options::default(),
        None,
    );
}

#[test]
fn tail_calls_and_accumulators() {
    check(
        "tail",
        r#"
function cat(n, acc = []) = n == 0 ? acc : cat(n - 1, concat(acc, [n]));
function grow(n, acc = []) = n == 0 ? acc : grow(n - 1, [each acc, n]);
function lets(n, acc = 0) = n == 0 ? acc : let(a = acc + n) lets(n - 1, a);
function both(n, acc = []) = n == 0 ? acc : let(x = cat(2)) both(n - 1, concat(acc, [len(x) + n]));
function nested(n) = n == 0 ? [] : [each nested(n - 1), n];
function viaassert(n) = n == 0 ? "done" : assert(n > 0) echo("at", n) viaassert(n - 1);
echo(len(cat(2000)), cat(5), len(grow(2000)), grow(4), lets(1000));
echo(both(4), len(nested(1500)), nested(4), viaassert(3));
function forever(n) = forever(n + 1);
echo(forever(0));
echo("after");
"#,
        &Options::default(),
        None,
    );
}

#[test]
fn comprehensions_with_calls() {
    check(
        "comprehensions",
        r#"
function sq(x) = x * x;
function g(n) = n == 0 ? [] : [for (i = [0:0]) each g(n - 1)];
function tree(n) = n == 0 ? [1] : [for (i = [0:1]) each tree(n - 1)];
echo([for (i = [0:4]) sq(i)], [for (i = [0:2], j = [i:sq(i)]) [i, j]]);
echo([for (c = "abc") str(c, sq(2))], [for (k = undef) sq(k)], [for (n = 5) sq(n)]);
echo([for (x = [sq(1), sq(2)]) if (x > sq(1)) x else -x]);
echo([for (x = [1:4]) let(y = sq(x)) if (y % 2 == 0) y]);
echo([each [for (i = [0:2]) sq(i)], each sq(3), for (i = [0:1]) each [i, sq(i)]]);
echo([for (i = 0, j = sq(1); i < 4; i = i + 1, j = j + sq(i)) [i, j]]);
echo(len(g(300)), len(tree(8)));
echo([for (x = [for (i = [0:3]) sq(i)]) each [x, x]]);
echo([for (i = [0:1:2e6]) sq(i)]);
o = [for (i = [0:2]) [i, sq(i)]];
echo([for (p = o) p[1] + sq(p[0])]);
"#,
        &Options::default(),
        None,
    );
}

#[test]
fn errors_and_their_traces() {
    check(
        "assert_deep",
        r#"
function f(n) = n == 0 ? assert(false, "at the bottom") 0 : 1 + f(n - 1);
echo(f(6));
echo("not reached");
"#,
        &Options::default(),
        None,
    );
    check(
        "assert_in_lc",
        r#"
function g(n) = [for (i = [0:n]) assert(i < 3, str("i is ", i)) i];
module m() echo(g(5));
m();
"#,
        &Options::default(),
        None,
    );
    check(
        "assert_in_let",
        r#"
function h(n) = let(a = n + 1, b = assert(a < 3, "a too big") a) b;
function k(n) = n == 0 ? 0 : h(n) + k(n - 1);
x = k(4);
echo(x);
"#,
        &Options::default(),
        None,
    );
    check(
        "assert_in_arg",
        r#"
function one(x) = x;
function t(n) = n == 0 ? one(assert(false, "in an argument")) : 2 * t(n - 1);
echo(t(3));
"#,
        &Options::default(),
        None,
    );
    check(
        "unknown",
        r#"
function u(n) = n == 0 ? nosuch(1) + nosuchvar : [u(n - 1)];
echo(u(2), [for (i = [0:1]) nosuch2(i)]);
"#,
        &Options::default(),
        None,
    );
    check(
        "recursion",
        r#"
function add_up_to(n) = n == 0 ? 0 : n + add_up_to(n - 1);
echo(add_up_to(10));
function crash() = crash();
echo(crash());
"#,
        &Options::default(),
        None,
    );
}

#[test]
fn hardwarnings_stop_inside_functions() {
    check(
        "hardwarnings",
        r#"
function f(n) = n == 0 ? undefined_var : [for (i = [0]) 1 + f(n - 1)];
echo("before");
echo(f(3));
echo("after");
"#,
        &Options {
            hardwarnings: true,
            ..Options::default()
        },
        None,
    );
}

#[test]
fn list_limit_in_a_comprehension() {
    let limits = eval::limits::Limits {
        list: Some(100),
        ..eval::limits::Limits::NONE
    };
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let opts = Options {
        guard: Some(std::sync::Arc::new(eval::limits::Guard::new(
            limits,
            flag.clone(),
            None,
        ))),
        interrupt: Some(flag),
        ..Options::default()
    };
    check(
        "list_limit",
        r#"
function r(n) = n == 0 ? [] : [each r(n - 1), n, n];
function s(x) = x;
echo(len(r(40)));
echo([for (i = [0:60]) each [s(i), s(i)]]);
"#,
        &opts,
        None,
    );
}

#[test]
fn call_memo_replays_modules_that_call_functions() {
    let src = r#"
function f(n) = n == 0 ? $fn : 1 + f(n - 1);
function lc(n) = [for (i = [0:n]) f(i)];
module w(s) { echo("w", s, f(s), lc(s)); cube(f(s)); }
w(2); w(2); $fn = 3; w(2); translate([1, 0, 0]) w(2);
module rec(n) { if (n > 0) { rec(n - 1); rec(n - 1); } else w(f(1)); }
rec(3);
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
fn an_interrupt_unwinds_a_function_recursion() {
    check(
        "interrupt",
        r#"
function f(n) = n == 0 ? 0 : echo(n) let(a = [for (i = [0]) f(n - 1)]) a[0] + 1;
echo(f(100));
echo("after");
"#,
        &Options::default(),
        Some(30),
    );
}

/// The heap evaluator's point for functions: a recursion through calls,
/// comprehensions or both, and through modules and functions together,
/// takes no native stack, so it reaches the counted limit on a thread of
/// 128 KiB and stops there with OpenSCAD's error.
#[cfg(feature = "heap-eval")]
#[test]
fn deep_function_recursion_on_a_small_thread() {
    let depth = eval::limits::DEFAULT_DEPTH as usize;
    // Each case recurses `n` levels; `[f]` is the call that stops it.
    let cases = [
        (
            "function f(n) = n == 0 ? 0 : 1 + f(n - 1);\necho(f(N));",
            "f",
        ),
        (
            "function f(n) = n == 0 ? [] : [for (i = [0:0]) each f(n - 1)];\necho(len(f(N)));",
            "f",
        ),
        (
            "function f(n) = n == 0 ? 0 : let(a = max(0, f(n - 1))) a + 1;\necho(f(N));",
            "f",
        ),
        (
            "function f(n) = n == 0 ? 0 : [f(n - 1)][0] + 1;\necho(f(N));",
            "f",
        ),
        (
            "g = function(n) n == 0 ? 0 : 1 + g(n - 1);\necho(g(N));",
            "g",
        ),
        // Modules and functions together: half each.
        (
            "function f(n) = n == 0 ? 0 : 1 + f(n - 1);\nmodule m(n) { if (n > 0) m(n - 1); else echo(f(H)); }\nm(H);",
            "f",
        ),
    ];
    for (k, (case, name)) in cases.into_iter().enumerate() {
        for (n, ok) in [(depth - 10, true), (depth + 10, false)] {
            let src = case
                .replace('N', &n.to_string())
                .replace('H', &(n / 2).to_string());
            let src = format!("{src}\n\x03\n");
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
            let (_, out) = ev;
            let text: Vec<String> = out.lines.into_iter().map(|l| l.2).collect();
            let error = text.iter().find(|l| l.starts_with("Recursion detected"));
            if ok {
                assert!(
                    error.is_none(),
                    "case {k}, n {n}: {:?}",
                    &text[..text.len().min(3)]
                );
            } else {
                assert_eq!(
                    error.map(String::as_str),
                    Some(format!("Recursion detected calling function '{name}'").as_str()),
                    "case {k}, n {n}"
                );
            }
        }
    }
}

/// The counted limit with a small value: within it a recursion runs, past
/// it the call that reaches it fails with every level traced, and module
/// levels count towards it too.
#[cfg(feature = "heap-eval")]
#[test]
fn the_depth_limit_counts_function_calls() {
    let opts = support::with_depth(50, Options::default());
    let run = |src: &str| support::run(src, &opts, None).0;
    let f = "function f(n) = n == 0 ? 0 : 1 + f(n - 1);\n";
    assert!(run(&format!("{f}echo(f(40));")).starts_with("ECHO: 40\n"));
    let got = run(&format!("{f}echo(f(60));"));
    assert!(
        got.starts_with("ERROR: Recursion detected calling function 'f'"),
        "{got}"
    );
    // The trace keeps its ends and counts what it leaves out.
    let shown = got.lines().filter(|l| l.contains("called by 'f'")).count();
    let excluded: usize = got
        .lines()
        .find_map(|l| {
            l.split("Excluding ")
                .nth(1)?
                .split(' ')
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or(0);
    let traces = shown + excluded;
    assert!((45..=50).contains(&traces), "{traces} traces: {got}");
    let m = "module m(n) { if (n > 0) m(n - 1); else echo(f(30)); }\n";
    assert!(run(&format!("{f}{m}m(10);")).starts_with("ECHO: 30\n"));
    let got = run(&format!("{f}{m}m(30);"));
    assert!(
        got.starts_with("ERROR: Recursion detected calling function 'f'"),
        "{got}"
    );
}
