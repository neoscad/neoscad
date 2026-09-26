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

/// The frame budget (`eval::recursion`), which decides on wasm32, stops a
/// recursion with OpenSCAD's messages wherever it runs out: at the
/// recursive module itself rather than at a builtin inside it, and at a
/// chain of builtins (`children()` of `children()`) that no user module
/// check sees.
#[test]
fn frame_budget_gives_the_recursion_errors() {
    let small = Options {
        frame_limit: 400,
        ..Options::default()
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
    let (lines, _) = run_with(
        "function f(n) = n == 0 ? 0 : 1 + f(n - 1);\necho(f(50));",
        &small,
    );
    assert_eq!(lines, ["ECHO: 50"]);

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

    // Printing a nested vector counts its levels against the budget too.
    let (lines, _) = run_with(
        "function nest(n, acc) = n == 0 ? acc : nest(n - 1, [acc]);\necho(nest(1000, 0));",
        &small,
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
