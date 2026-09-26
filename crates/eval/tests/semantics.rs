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
