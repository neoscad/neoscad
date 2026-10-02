//! Evaluator behaviour on small programs, checked against what the
//! OpenSCAD nightly prints for the same input (the expected lines were
//! produced with `-o x.echo`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use eval::{Collect, Options};
use lang::diag::Severity;

/// Evaluate `src` as a single file and return the printed lines, with
/// locations reduced to `@line`.
fn run_with(src: &str, opts: &Options) -> (Vec<String>, eval::Evaluation) {
    let path = PathBuf::from("/nonexistent/test.scad");
    let mut text = src.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    assert!(!program.has_syntax_errors(), "syntax error in test program");
    let mut out = LineCollector::default();
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            opts,
            &mut out,
        )
    });
    (out.lines, ev)
}

fn run(src: &str) -> Vec<String> {
    run_with(src, &Options::default()).0
}

#[derive(Default)]
struct LineCollector {
    lines: Vec<String>,
}

impl eval::Output for LineCollector {
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
    }
}

#[test]
fn numbers_print_like_openscad() {
    assert_eq!(
        run("echo(1/3, 1e6, 123456789, 1e-7, -0, 0.1 + 0.2, 1/0, -1/0, 0/0);"),
        ["ECHO: 0.333333, 1e+6, 1.23457e+8, 1e-7, 0, 0.3, inf, -inf, nan"]
    );
}

#[test]
fn strings_print_raw_in_echo_and_quoted_in_vectors() {
    assert_eq!(
        run(r#"s = "a\tb\"c"; echo(s, [s], str(s, 1));"#),
        ["ECHO: \"a\tb\"c\", [\"a\tb\"c\"], \"a\tb\"c1\""]
    );
}

#[test]
fn undefined_operations_warn_at_the_operator() {
    assert_eq!(
        run("echo(1 + \"a\");\necho([1, \"a\"] < [1, 2]);\necho(undef < undef);"),
        [
            "WARNING: undefined operation (number + string) @1",
            "ECHO: undef",
            "WARNING: undefined operation (string < number)\n\tin vector comparison at index 1 @2",
            "ECHO: undef",
            "WARNING: operation undefined (undefined < undefined) @3",
            "ECHO: undef",
        ]
    );
}

#[test]
fn element_wise_undef_is_silent() {
    // The inner `"a" + 1` produces undef inside the vector without a warning.
    assert_eq!(run("echo([1, \"a\"] + [1, 1]);"), ["ECHO: [2, undef]"]);
}

#[test]
fn matrix_products() {
    assert_eq!(
        run(
            "echo([1, 2, 3] * [[1, 0], [0, 1], [1, 1]], [[1, 2], [3, 4]] * [1, 1], [1, 2] * [3, 4]);"
        ),
        ["ECHO: [4, 5], [3, 7], 11"]
    );
}

#[test]
fn function_literals_compare_by_identity_and_print_their_source() {
    assert_eq!(
        run(
            "f = function(x, y = 2) x + y; g = f; echo(f == g, f == (function(x, y = 2) x + y), f);"
        ),
        ["ECHO: true, false, function(x, y = 2) (x + y)"]
    );
}

#[test]
fn closures_capture_their_scope() {
    assert_eq!(
        run("function adder(n) = function(x) x + n; a = adder(3); echo(a(4));"),
        ["ECHO: 7"]
    );
}

#[test]
fn special_variables_are_dynamically_scoped() {
    let src = "$a = 1;\nfunction f() = $a;\nmodule m() echo(f());\nm($a = 2);\necho(f(), let($a = 3) f());";
    assert_eq!(run(src), ["ECHO: 2", "ECHO: 1, 3"]);
}

#[test]
fn children_count_is_lexical() {
    let src =
        "module lex() { echo($children); kid(); }\nmodule kid() echo($children);\nlex() cube();";
    assert_eq!(run(src), ["ECHO: 1", "ECHO: 0"]);
}

#[test]
fn argument_binding_warnings() {
    let src = "module a(x, y) echo(x, y);\na(1, x = 2);\na(y = 1, y = 2);\na(1, 2, 3);\na(z = 1);";
    assert_eq!(
        run(src),
        [
            "WARNING: argument \"x\" overrides positional argument @2",
            "ECHO: 2, undef",
            "WARNING: argument \"y\" supplied more than once @3",
            "ECHO: undef, 2",
            "WARNING: Too many unnamed arguments supplied @4",
            "ECHO: 1, 2",
            "WARNING: variable \"z\" not specified as parameter @5",
            "ECHO: undef, undef",
        ]
    );
}

#[test]
fn list_comprehensions() {
    let src = "echo([for (i = [0 : 3]) if (i % 2) i else -i], [each [1, 2], each \"ab\", each undef],\n\
               [for (i = 0, j = 1; i < 4; i = i + 1, j = j * 2) j], [for (a = [1, 2]) for (b = [3, 4]) a * b]);";
    assert_eq!(
        run(src),
        ["ECHO: [0, 1, -2, 3], [1, 2, \"a\", \"b\"], [1, 2, 4, 8], [3, 4, 6, 8]"]
    );
}

#[test]
fn tail_recursion_runs_in_constant_stack() {
    let src = "function count(n, acc = 0) = n == 0 ? acc : let(m = n - 1) count(m, acc + n);\necho(count(500000));";
    assert_eq!(run(src), ["ECHO: 1.25e+11"]);
}

#[test]
fn accumulators_moved_into_a_tail_call_keep_their_values() {
    // `concat(acc, [x])` and `[each acc, x]` in a tail call take `acc` out
    // of the frame the call replaces, so the list can grow in place (see
    // `Evaluator::move_accumulators`). Each case here is one where that
    // must not happen, or must not be visible: a list shared with a global,
    // a closure or a later read, a module parameter the callee reads, a `$`
    // variable, a named argument, a nested call of the same function.
    let src = r#"function b(n, acc=[]) = n==0 ? acc : b(n-1, concat(acc,[n]));
echo(b(5));
x = [1,2];
echo(b(3, x), x);
function c(n, acc) = let(g = function() acc) n==0 ? [acc, g()] : c(n-1, concat(acc,[n]));
echo(c(3, [0]));
function d(n, acc) = n==0 ? acc : d(n-1, concat(acc,[len(acc)]));
echo(d(4, [7]));
function s(n, acc) = n==0 ? acc : s(n-1, [each acc, n]);
echo(s(3, "ab"), s(3, [0:2]), s(2, 5), s(2, undef));
f = function(n, acc) n==0 ? acc : f(n-1, [each acc, n]);
echo(f(4, [0]));
function e(n, acc) = let(a2 = concat(acc,[0])) n==0 ? a2 : e(n-1, concat(a2, [n]));
echo(e(3, []));
module m(acc) {
  function h(n, a) = n==0 ? [a, acc] : h(n-1, a);
  echo(h(2, concat(acc,[1])));
  function g(n, a) = n==0 ? [a, acc] : g(n-1, concat(a, [n]));
  echo(g(3, acc), acc);
}
m([9]);
function k(n, $acc) = n==0 ? $acc : k(n-1, concat($acc,[n]));
echo(k(3, []));
function q(n, acc) = n==0 ? acc : q(n-1, concat(acc, [len(q(n-1, []))]));
echo(q(3, []));
function r(n, acc) = n==0 ? acc : r(n-1, acc=concat(acc,[n]));
echo(r(3, [1]));
function u(n, acc) = n==0 ? acc : u(n-1, [each acc, n, each acc]);
echo(u(2, [1]));
function v(n, acc) = n==0 ? acc : v(n-1, [each acc, for (i=[0:1]) n*10+i]);
echo(v(3, []));
function w(n, acc) = n==0 ? acc : w(n-1, concat(acc, [n], 7, "s"));
echo(w(2, []));
function z(n, acc, keep) = n==0 ? [acc, keep] : z(n-1, concat(acc, [n]), n==2 ? acc : keep);
echo(z(4, [], undef));
function bb(n, acc) = n == 0 ? acc : let(p = acc) bb(n-1, concat(p, [n, len(acc)]));
echo(bb(3, [5]));
echo([for (a = [[1],[2]]) b(2, a)]);"#;
    assert_eq!(
        run(src),
        [
            "ECHO: [5, 4, 3, 2, 1]",
            "ECHO: [1, 2, 3, 2, 1], [1, 2]",
            "ECHO: [[0, 3, 2, 1], [0, 3, 2, 1]]",
            "ECHO: [7, 1, 2, 3, 4]",
            "ECHO: [\"a\", \"b\", 3, 2, 1], [0, 1, 2, 3, 2, 1], [5, 2, 1], [2, 1]",
            "ECHO: [0, 4, 3, 2, 1]",
            "ECHO: [0, 3, 0, 2, 0, 1, 0]",
            "ECHO: [[9, 1], [9]]",
            "ECHO: [[9, 3, 2, 1], [9]], [9]",
            "ECHO: [3, 2, 1]",
            "ECHO: [2, 1, 0]",
            "ECHO: [1, 3, 2, 1]",
            "ECHO: [1, 2, 1, 1, 1, 2, 1]",
            "ECHO: [30, 31, 20, 21, 10, 11]",
            "ECHO: [2, 7, \"s\", 1, 7, \"s\"]",
            "ECHO: [[4, 3, 2, 1], [4, 3]]",
            "ECHO: [5, 3, 1, 2, 3, 1, 5]",
            "ECHO: [[1, 2, 1], [2, 2, 1]]",
        ]
    );
}

#[test]
fn register_variables_and_pure_frames_keep_the_scoping_rules() {
    // `let` and comprehension variables, and the parameters of calls that
    // bind only positional arguments, live in registers rather than
    // contexts (`Evaluator::regs`). These are the rules that could tell
    // the difference: a binding that reads the outer one of its own name
    // (its register is unset yet), a duplicate, the same `let` live in
    // two activations at once, a variable that is not a function letting
    // the function search go on, `$` variables seen through a pure frame
    // (from a tail call, a non-tail call and a tail `let`), one function
    // called both with a pure frame and with named arguments, `is_undef`
    // of a register, accumulators in registers, an unnamed `let`
    // argument. Expected lines are the nightly's.
    let src = r#"x = 10;
echo(let(x = x + 1, y = x * 2, x = 5) [x, y]);
echo([for (i = [0 : 2]) let(i = i * 2) i], [for (i = [1 : 2], j = [0 : i]) [i, j]]);
function f(n) = n == 0 ? 0 : let(a = n) a + f(n - 1) * 10 + a;
echo(f(3));
function sq(v) = v * v;
h = function(v) v + 100;
echo(let(g = h) g(2), let(sq = 3) sq(4), let(sq = h) sq(4));
function ap(fn, v) = fn(v);
echo(ap(function(y) y * 3, 2), ap(h, 1));
function dy() = $v;
function p(v) = dy();
function t($v) = p(1);
function t2($v) = 1 + p(1);
function t3($v) = let(k = 2) p(k);
echo(t(7), t2(7), t3(7));
function g(a, b = 1) = a <= 0 ? b : g(a - 1, b * 2) + g(b = b, a = a - 1);
echo(g(3));
echo(let(u = undef) is_undef(u), let(a = 1, b = is_undef(c) ? a : c, c = 2) [b, c]);
function m(n, acc) = let(k = n * 2) n == 0 ? acc : m(n - 1, [each acc, k]);
echo(m(3, [0]));
function m2(n, acc) = let(t = acc) n == 0 ? [t, acc] : m2(n - 1, concat(t, [n]));
echo(m2(2, [9]));
echo(let(1) 2);
function lc(n) = [for (i = [0 : n]) let(j = i) if (j % 2 == 0) lc2(j)];
function lc2(k) = let(j = k + 1) [j, k];
echo(lc(3));"#;
    assert_eq!(
        run(src),
        [
            "WARNING: Ignoring duplicate variable assignment \"x\" = 5 @2",
            "ECHO: [11, 22]",
            "ECHO: [0, 2, 4], [[1, 0], [1, 1], [2, 0], [2, 1], [2, 2]]",
            "ECHO: 246",
            "ECHO: 102, 16, 104",
            "ECHO: 6, 101",
            "ECHO: 7, 8, 7",
            "ECHO: 27",
            "ECHO: true, [1, 2]",
            "ECHO: [0, 6, 4, 2]",
            "ECHO: [[9, 2, 1], [9, 2, 1]]",
            "WARNING: Assignment without variable name 1 @24",
            "ECHO: 2",
            "ECHO: [[1, 0], [3, 2]]",
        ]
    );
}

#[test]
fn a_non_tail_call_never_moves_its_callers_accumulator() {
    // `len1(concat(acc, [0]))` is not a tail call: it is evaluated in the
    // context of `t`'s tail-call loop, which reads `acc` again afterwards.
    // The loop borrows its caller's context for a first call instead of
    // holding a reference of its own (`Evaluator::eval_call`), so the
    // reference count that proves a context private has to allow for
    // that, or this call would take `acc` from its caller. Expected lines
    // are the nightly's.
    let src = r#"function len1(a) = len(a);
function t(n, acc) = n == 0 ? [len1(concat(acc, [0])), acc] : t(n - 1, concat(acc, [n]));
echo(t(2, [9]));
function t2(n, acc) = n == 0 ? len1(concat(acc, [0])) + len(acc) : t2(n - 1, concat(acc, [n]));
echo(t2(2, [9]));
function t3(n, acc) = n == 0 ? let(x = len1([each acc, 0])) [x, acc] : t3(n - 1, [each acc, n]);
echo(t3(1, [4]));"#;
    assert_eq!(
        run(src),
        ["ECHO: [4, [9, 2, 1]]", "ECHO: 7", "ECHO: [3, [4, 1]]",]
    );
}

#[test]
fn infinite_recursion_is_an_error_not_a_crash() {
    let (lines, ev) = run_with(
        "function f(n) = 1 + f(n + 1);\necho(f(0));",
        &Options::default(),
    );
    assert!(ev.aborted);
    assert_eq!(
        lines[0],
        "ERROR: Recursion detected calling function 'f' @1"
    );
    assert_eq!(
        lines.last().map(String::as_str),
        Some("TRACE: called by 'echo' @2")
    );
    let (lines, _) = run_with("module m() m();\nm();", &Options::default());
    assert_eq!(lines[0], "ERROR: Recursion detected calling module 'm' @1");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("TRACE:   *** Excluding"))
    );
}

#[test]
fn scoping_rules_survive_static_resolution() {
    // Names are resolved ahead of time (the evaluator's `resolve`), and
    // each of these is a rule a static resolution could get wrong: a scope
    // assignment that reads a name assigned later in it (the outer one, or
    // a named argument), a function reading a global before it is set, a
    // named argument that is not a parameter, a C-style `for` whose
    // increment makes a closure, `for` variables seen by later ones, a
    // `let` binding that reads a later one, a user module named like a
    // builtin that binds, `$children` passed by name, a variable holding a
    // function shadowing a function, a parameter default naming another
    // parameter (it sees the defining scope), `$` variables through calls,
    // children in the caller's scope, and duplicate parameters. The
    // expected lines are the nightly's.
    let src = r#"a = 5;
module m1() { b = a; a = 1; echo(m1b = b, a = a); }
m1();
function early() = late;
e1 = early();
late = 9;
echo(e1 = e1, e2 = early());
function fz() = zz;
echo(fz = fz(zz = 5));
module mq() echo(q = q);
mq(q = 3);
module mx() { x = 1; echo(mx = x); }
mx(x = 5);
module my() { y = x2 + 1; x2 = 10; echo(my = y); }
my(x2 = 4);
echo([for (i = 0, f = function() i; i < 3; i = i + 1, f = function() i * 10) f()]);
echo([for (i = [0:2], j = [0:i]) [i, j]]);
echo(let(p = q0, q0 = 1) p);
q0 = 100;
module intersection_for(i) { echo(uif = i); children(); }
intersection_for(i = [1:2]) echo(child_i = i);
module mc() echo(mc = $children);
mc($children = 4);
function f10() = 1;
echo(f10 = let(f10 = function() 2) f10());
function fd(a, b = a) = b;
echo(fd = fd(1));
module rec(n) { function g() = n; if (n > 0) { echo(rec = g()); rec(n - 1); } }
rec(2);
$fn = 3;
function dyn() = $fn;
module md() echo(md = dyn());
md($fn = 7);
x3 = 1;
echo([for (x3 = [x3 + 1]) x3]);
module ch() { v = 2; children(); }
v = 1;
ch() echo(ch_v = v);
function g5(n) = let(n = n + 1) n;
echo(g5 = g5(1));
function cl(k) = function(x) x + k;
h = cl(10);
echo(cl = h(1));
echo(is_undef(nope), is_undef(a));
function dup(a, a) = a;
echo(dup = dup(1, 2));"#;
    assert_eq!(
        run(src),
        [
            "WARNING: Ignoring unknown variable \"late\" @4",
            "ECHO: m1b = 5, a = 1",
            "ECHO: e1 = undef, e2 = 9",
            "WARNING: variable \"zz\" not specified as parameter @9",
            "ECHO: fz = 5",
            "WARNING: variable \"q\" not specified as parameter @11",
            "ECHO: q = 3",
            "WARNING: variable \"x\" not specified as parameter @13",
            "WARNING: Parameter \"x\" is overwritten with a literal @12",
            "ECHO: mx = 1",
            "WARNING: variable \"x2\" not specified as parameter @15",
            "WARNING: Parameter \"x2\" is overwritten with a literal @14",
            "ECHO: my = 5",
            "ECHO: [0, 10, 20]",
            "ECHO: [[0, 0], [1, 0], [1, 1], [2, 0], [2, 1], [2, 2]]",
            "ECHO: 100",
            "ECHO: uif = [1 : 1 : 2]",
            "WARNING: Ignoring unknown variable \"i\" @21",
            "ECHO: child_i = undef",
            "WARNING: variable \"$children\" not specified as parameter @23",
            "ECHO: mc = 4",
            "ECHO: f10 = 2",
            "ECHO: fd = 5",
            "ECHO: rec = 2",
            "ECHO: rec = 1",
            "ECHO: md = 7",
            "ECHO: [2]",
            "ECHO: ch_v = 1",
            "ECHO: g5 = 2",
            "ECHO: cl = 11",
            "ECHO: true, false",
            "ECHO: dup = 2",
        ]
    );
}

#[test]
fn duplicate_let_bindings_warn() {
    // The first binding of a name in a `let` wins; a `$` name is checked
    // apart from copies of the caller's `$` variables in the same frame.
    // The expected lines are the nightly's.
    let src = r#"echo(let(a = 1, a = 2) a);
function f() = let($x = 1, $x = 2, b = 3, b = 4) [$x, b];
function g() = f();
$x = 9;
echo(f(), g());
echo([for (i = 0, i = 5; i < 2; i = i + 1, i = 7) i]);
let (q = 1, q = 2) echo(q);
module mm() { let ($fn = 1, $fn = 2) echo($fn); }
mm($fn = 5);"#;
    assert_eq!(
        run(src),
        [
            "WARNING: Ignoring duplicate variable assignment \"a\" = 2 @1",
            "ECHO: 1",
            "WARNING: Ignoring duplicate variable assignment \"$x\" = 2 @2",
            "WARNING: Ignoring duplicate variable assignment \"b\" = 4 @2",
            "WARNING: Ignoring duplicate variable assignment \"$x\" = 2 @2",
            "WARNING: Ignoring duplicate variable assignment \"b\" = 4 @2",
            "ECHO: [1, 3], [1, 3]",
            "WARNING: Ignoring duplicate variable assignment \"i\" = 5 @6",
            "WARNING: Ignoring duplicate variable assignment \"i\" = 7 @6",
            "WARNING: Ignoring duplicate variable assignment \"i\" = 7 @6",
            "ECHO: [0, 1]",
            "WARNING: Ignoring duplicate variable assignment \"q\" = 2 @7",
            "ECHO: 1",
            "WARNING: Ignoring duplicate variable assignment \"$fn\" = 2 @8",
            "ECHO: 1",
        ]
    );
}

#[test]
fn tail_call_limit() {
    let (lines, _) = run_with(
        "function crash() = crash();\necho(crash());",
        &Options::default(),
    );
    assert_eq!(
        lines,
        [
            "ERROR: Recursion detected calling function 'crash' @1",
            "TRACE: called by 'crash' @1",
            "TRACE: called by 'echo' @2"
        ]
    );
}

#[test]
fn assertions() {
    let (lines, ev) = run_with(
        "function g(n) = assert(n < 2, str(\"big \", n)) n;\necho(g(1));\necho(g(5));",
        &Options::default(),
    );
    assert!(ev.aborted);
    assert_eq!(
        lines,
        [
            "ECHO: 1",
            "ERROR: Assertion '(n < 2)' failed: \"big 5\" @1",
            "TRACE: called by 'g' @3",
            "TRACE: called by 'echo' @3"
        ]
    );
}

#[test]
fn seeded_rands_match_openscad() {
    // rands.scad: echo(rands(1, 2, 3, 4.1)) prints [1.96977, 1.55343, 1.99383].
    assert_eq!(
        run("echo(rands(1, 2, 3, 4.1), rands(1, 2, 3, -4.1));"),
        ["ECHO: [1.96977, 1.55343, 1.99383], [1.19758, 1.92189, 1.67397]"]
    );
}

#[test]
fn builtin_edge_cases() {
    assert_eq!(
        run(
            "echo(sin(30), cos(90), tan(45), asin(0.5), atan2(1, 1), chr([65, 66], [67 : 68]), ord(\"\u{e4}\"),\n\
             len(\"a\u{e4}\"), search(\"a\", \"abca\", 0), lookup(1.5, [[1, 10], [2, 20]]), norm([3, 4]), cross([1, 0, 0], [0, 1, 0]));"
        ),
        ["ECHO: 0.5, 0, 1, 30, 45, \"ABCD\", 228, 2, [[0, 3]], 15, 5, [0, 0, 1]"]
    );
    assert_eq!(
        run("echo(min([]), max(\"a\"));"),
        [
            "WARNING: min() number of parameters does not match: expected at least 1 vector element, found 0 @1",
            "WARNING: max() parameter could not be converted: argument 0: expected number, found string (\"a\") @1",
            "ECHO: undef, undef"
        ]
    );
}

#[test]
fn ranges() {
    assert_eq!(
        run("r = [0 : 2 : 10]; echo(r, r[1], r.end, [for (x = [1 : 0.5 : 2]) x]);\necho([5 : 1]);"),
        [
            "ECHO: [0 : 2 : 10], 2, 10, [1, 1.5, 2]",
            "WARNING: begin is greater than the end, but step is positive @2",
            "ECHO: [5 : 1 : 1]"
        ]
    );
}

#[test]
fn repeated_messages_are_suppressed_by_the_console() {
    let mut buf = Vec::new();
    {
        let mut con = eval::Console::new(&mut buf, PathBuf::from("/"), false);
        for _ in 0..8 {
            con.print(Some(Severity::Warning), b"WARNING: same");
        }
        con.print(Some(Severity::Echo), b"ECHO: x");
    }
    assert_eq!(String::from_utf8(buf).unwrap().lines().count(), 6);
}

#[test]
fn interrupt_stops_evaluation() {
    let flag = Arc::new(AtomicBool::new(true));
    let opts = Options {
        interrupt: Some(flag),
        ..Default::default()
    };
    let (lines, ev) = run_with(
        "function f(n) = n == 0 ? 0 : f(n - 1);\necho(f(10));\necho(\"after\");",
        &opts,
    );
    assert!(ev.interrupted);
    assert!(lines.is_empty(), "{lines:?}");
}

#[test]
fn node_tree_carries_parameters() {
    let (_, ev) = run_with(
        "translate([1, 2]) cube(3, center = true);\nsphere(d = 4, $fn = 12);",
        &Options::default(),
    );
    let kids = &ev.root.children;
    assert_eq!(kids.len(), 2);
    match &kids[0].kind {
        eval::node::NodeKind::Transform { matrix, .. } => {
            assert_eq!([matrix[0][3], matrix[1][3], matrix[2][3]], [1.0, 2.0, 0.0])
        }
        k => panic!("unexpected {k:?}"),
    }
    assert_eq!(
        kids[0].children[0].kind,
        eval::node::NodeKind::Cube {
            size: [3.0; 3],
            center: true
        }
    );
    match &kids[1].kind {
        eval::node::NodeKind::Sphere { r, disc } => {
            assert_eq!(*r, 2.0);
            assert_eq!(disc.fn_, 12.0);
        }
        k => panic!("unexpected {k:?}"),
    }
}

#[test]
fn collect_output_keeps_codes() {
    let path = PathBuf::from("/nonexistent/t.scad");
    let program = lang::parse_file(path, b"echo(x);\n\x03\n".to_vec());
    let mut out = Collect::default();
    eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &Options::default(),
            &mut out,
        )
    });
    assert_eq!(out.lines[0].1, lang::diag::DiagCode::UnknownVariable);
    assert_eq!(out.lines[1].1, lang::diag::DiagCode::Echo);
}

/// `--hardwarnings`: the first warning stops evaluation with the traces an
/// error would get, and nothing printed after it survives (the nightly,
/// `-o x.echo --hardwarnings`).
#[test]
fn hardwarnings_stop_at_the_first_warning() {
    let opts = Options {
        hardwarnings: true,
        ..Default::default()
    };
    let (lines, ev) = run_with(
        "module m(){ echo(1); circle(r=1,d=4); echo(2);}\nm();\necho(3);",
        &opts,
    );
    assert!(ev.hard_warning);
    assert_eq!(
        lines,
        [
            "ECHO: 1",
            "WARNING: Ignoring radius variable \"r\" as diameter \"d\" is defined too. @1",
            "TRACE: called by 'circle' @1",
            "TRACE: call of 'm()' @1",
            "TRACE: called by 'm' @2",
        ]
    );
    let (lines, ev) = run_with(
        "function f(a) = a + undefvar;\nb = f(1);\nc = test();",
        &opts,
    );
    assert!(ev.hard_warning);
    assert_eq!(
        lines,
        [
            "WARNING: Ignoring unknown variable \"undefvar\" @1",
            "TRACE: called by 'f' @2",
            "TRACE: assignment to \"b\" @2",
        ]
    );
    // An unknown module is reported before the instantiation's try block.
    let (lines, _) = run_with("hello();\necho(1);", &opts);
    assert_eq!(lines, ["WARNING: Ignoring unknown module 'hello' @1"]);
    // Without the flag, evaluation carries on.
    let (lines, ev) = run_with("hello();\necho(1);", &Options::default());
    assert!(!ev.hard_warning);
    assert_eq!(lines.len(), 2);
}

/// Small recursion limits stop a recursion with OpenSCAD's messages
/// wherever they run out. The counted depth limit stops function and
/// module recursion (at the recursive module itself, and at a chain of
/// `children()` of `children()`), and the frame budget (`eval::recursion`,
/// the wasm32 guard) stops what still recurses natively: a recursion
/// through a range's bounds, and printing a deeply nested vector.
#[test]
fn small_limits_give_the_recursion_errors() {
    let budget = Options {
        frame_limit: 400,
        ..Options::default()
    };
    // Statements and calls take no native stack (past a few native
    // calls), so the counted depth limit is what stops them.
    let small = {
        let limits = eval::limits::Limits {
            depth: Some(40),
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
            ..budget.clone()
        }
    };
    let (lines, ev) = run_with(
        "function f(n) = n == 0 ? 0 : 1 + f(n - 1);\necho(f(1000));",
        &small,
    );
    assert!(ev.aborted);
    assert_eq!(
        lines[0],
        "ERROR: Recursion detected calling function 'f' @1"
    );
    let within = 30;
    let (lines, _) = run_with(
        &format!("function f(n) = n == 0 ? 0 : 1 + f(n - 1);\necho(f({within}));"),
        &small,
    );
    assert_eq!(lines, [format!("ECHO: {within}")]);

    let (lines, _) = run_with(
        "module m(n) { if (n > 0) translate([1, 0, 0]) m(n - 1); }\nm(1000);",
        &small,
    );
    assert_eq!(lines[0], "ERROR: Recursion detected calling module 'm' @1");

    // Each level nests `children()` once more; the innermost resolves the
    // whole chain without instantiating a user module.
    let chain = "module c(n) { if (n > 0) c(n - 1) children(); else children(); }\nc(80) cube(1);";
    let (lines, ev) = run_with(chain, &small);
    assert!(ev.aborted);
    assert!(
        lines[0].starts_with("ERROR: Recursion detected calling module '"),
        "{lines:?}"
    );

    // A C-style `for`'s initialiser is evaluated natively, so a recursion
    // through it holds native stack per level, and the frame budget stops
    // it at its call, far short of the depth limit. Each level starts a
    // heap loop (`recursion::HEAP_LOOP_FRAMES`), so this uses the wasm32
    // release budget, under which browsers reached 30-37 levels of the
    // shapes that recursed this way. (A range's bounds were one, and are
    // on the heap now: `functions.rs` runs one to the depth limit.)
    let wasm = Options {
        frame_limit: 2_000,
        ..Options::default()
    };
    let cfor = "function f(n) = n == 0 ? 0 : [for (i = f(n - 1); i < n; i = n) i][0] + 1;";
    let (lines, _) = run_with(&format!("{cfor}\necho(f(20));"), &wasm);
    assert_eq!(lines, ["ECHO: 20"]);
    let (lines, ev) = run_with(&format!("{cfor}\necho(f(1000));"), &wasm);
    assert!(ev.aborted);
    assert_eq!(
        lines[0],
        "ERROR: Recursion detected calling function 'f' @1"
    );

    // Printing a nested vector counts its levels against the budget too.
    let (lines, _) = run_with(
        "function nest(n, acc) = n == 0 ? acc : nest(n - 1, [acc]);\necho(nest(1000, 0));",
        &budget,
    );
    assert_eq!(
        lines[0],
        "ERROR: Stack exhausted while trying to convert a vector to EchoString"
    );
}

/// Unseeded `rands()` starts from the seed the host passes, so a host that
/// passes the same seed gets the same numbers (the command line passes
/// OpenSCAD's time-and-process seed).
#[test]
fn unseeded_rands_follow_the_host_seed() {
    let with = |seed| {
        let opts = Options {
            rng_seed: seed,
            ..Options::default()
        };
        run_with("echo(rands(0, 1, 3));", &opts).0
    };
    assert_eq!(with(7), with(7));
    assert_ne!(with(7), with(8));
    // Seeded calls ignore it.
    let seeded = |seed| {
        let opts = Options {
            rng_seed: seed,
            ..Options::default()
        };
        run_with("echo(rands(0, 1, 2, 42));", &opts).0
    };
    assert_eq!(seeded(1), seeded(2));
}

/// `dxf_dim()` reads through `Options::fs`, so an in-memory file system
/// serves it (the WASM build has no other).
#[test]
fn dxf_dim_reads_through_the_file_system() {
    let Ok(dxf) = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.reference/openscad/examples/Old/example009.dxf"),
    ) else {
        eprintln!("skipped: no reference checkout");
        return;
    };
    let fs = lang::vfs::MemFs::new();
    fs.insert("/mem/parts.dxf", dxf);
    let opts = Options {
        fs: Arc::new(fs),
        ..Options::default()
    };
    let (lines, _) = run_with(
        "echo(dxf_dim(file = \"/mem/parts.dxf\", name = \"bodywidth\"));",
        &opts,
    );
    assert_eq!(lines, ["ECHO: 22"]);
}

/// neoscad's `part()` extension: off by default, when it is exactly
/// OpenSCAD's unknown module; on, a node with a dotted name.
#[test]
fn part_is_opt_in() {
    let src = "part(\"lid\") { cube(1); part(\"hinge\") cube(2); }\npart(\"lid\") sphere(1);";
    assert_eq!(
        run(src),
        [
            "WARNING: Ignoring unknown module 'part' @1",
            "WARNING: Ignoring unknown module 'part' @2",
        ]
    );
    let on = Options {
        parts: true,
        ..Options::default()
    };
    let (lines, ev) = run_with(src, &on);
    assert_eq!(lines, ["WARNING: Duplicate part name 'lid' @2"]);
    let csg = eval::dump::csg(
        &ev.root,
        std::path::Path::new("/nonexistent"),
        &lang::loader::StdFs,
    );
    assert_eq!(
        csg,
        "part(name = \"lid\") {\n\tcube(size = [1, 1, 1], center = false);\n\tpart(name = \"lid.hinge\") {\n\t\tcube(size = [2, 2, 2], center = false);\n\t}\n}\npart(name = \"lid\") {\n\tsphere($fn = 0, $fa = 12, $fs = 2, r = 1);\n}\n\n"
    );
    // A program's own `part` module wins, as any user module over a
    // builtin; a bad name warns and keeps the children as a group.
    let (lines, _) = run_with("module part(n) echo(n); part(\"x\");\npart2 = 1;", &on);
    assert_eq!(lines, ["ECHO: \"x\""]);
    let (lines, ev) = run_with("part(3) cube(1);", &on);
    assert_eq!(
        lines,
        ["WARNING: part(name=3) needs a non-empty string name; treating it as a group @1"]
    );
    assert!(matches!(
        ev.root.children[0].kind,
        eval::node::NodeKind::Group { .. }
    ));
}
