//! Values nested as deep as the counted limit, on a small thread.
//!
//! The heap evaluator lets a recursion run 100,000 levels deep in any
//! browser (`src/heap_expr.rs`), so a program can now build a list nested
//! that deep where a WebKit worker gives the engine about 512 KiB of
//! stack: `function nest(n) = n == 0 ? 0 : [nest(n - 1)];`, or with a tail
//! call (`nest(n - 1, [acc])`) a million levels deep. Everything that
//! walks a value must then hold no native stack per level of its nesting,
//! or be bounded by a count: dropping it, printing it (which stops with
//! OpenSCAD's "Stack exhausted" error at a counted depth), comparing it,
//! hashing it for the caches, the element-wise operators, and handing it
//! to a module that turns parameters into geometry. Each case below runs
//! on a 512 KiB thread with the default options and must finish without
//! overflowing it; the debug build's frames are many times larger, so it
//! is the stricter of the two.

use std::path::PathBuf;

use eval::Options;

/// A WebKit worker's stack, about the smallest any target gives the
/// evaluator.
const SMALL_STACK: usize = 512 << 10;

const NEST: &str = "function nest(n, acc = 0) = n == 0 ? acc : nest(n - 1, [acc]);\n\
                    function nest2(n) = n == 0 ? 0 : [nest2(n - 1)];\n\
                    function nesto(n, acc = 0) = n == 0 ? acc : nesto(n - 1, object(a = acc));\n";

fn run_small(src: &str, opts: &Options) -> Vec<String> {
    let mut text = format!("{NEST}{src}").into_bytes();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(PathBuf::from("/nonexistent/t.scad"), text);
    assert!(!program.has_syntax_errors(), "syntax error in {src}");
    let opts = opts.clone();
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(move || {
            let mut out = eval::Collect::default();
            let ev = eval::evaluate(
                &program,
                &[],
                &[],
                PathBuf::from("/nonexistent"),
                &opts,
                &mut out,
            );
            let csg = eval::dump::csg(&ev.root, std::path::Path::new("/nonexistent"), &*opts.fs);
            let mut lines: Vec<String> = out.lines.into_iter().map(|l| l.2).collect();
            lines.push(format!("csg {}", csg.len()));
            // Everything the evaluation made is dropped here, on the
            // small thread.
            drop(ev);
            lines
        })
        .unwrap()
        .join()
        .unwrap()
}

/// The cases: a program, and the start of a line its output must hold
/// (every evaluation ends with a `csg` line, so "csg " only asks that it
/// finished).
const CASES: &[(&str, &str)] = &[
    // Dropping, at the end of the evaluation and in the middle of it.
    ("v = nest(999999); echo(len(v));", "1"),
    ("echo(len(nest2(99990)));", "1"),
    ("for (i = [0:2]) echo(len(nest(100000)));", "1"),
    // Printing stops at a counted depth with OpenSCAD's error.
    ("echo(nest(100000));", "Stack exhausted"),
    ("echo(str(nest(100000)));", "Stack exhausted"),
    ("echo(v = [[1], nest(100000)]);", "Stack exhausted"),
    ("echo(nest2(99990));", "Stack exhausted"),
    // Comparisons.
    ("echo(nest(100000) == nest(100000));", "true"),
    ("echo(nest(100000) < nest(100000, 1));", "true"),
    ("echo(nest(100000) != nest(100000, 1));", "true"),
    // Element-wise operators.
    ("echo(len(nest(100000) + nest(100000)));", "1"),
    ("echo(len(nest(100000) - nest(100000)));", "1"),
    ("echo(len(-nest(100000)));", "1"),
    ("echo(len(nest(100000) * 2));", "1"),
    ("echo(len(2 * nest(100000)));", "1"),
    ("echo(len(nest(100000) / 2));", "1"),
    ("echo(nest(100000) * nest(100000));", "undef"),
    // The builtins that look inside a list.
    ("echo(len(concat(nest(100000), nest(100000))));", "2"),
    (
        "echo(max(nest(100000)), min([nest(100000), 1]));",
        "max() parameter",
    ),
    ("echo(norm(nest(100000)));", "Incorrect arguments to norm()"),
    ("echo(search([nest(100000)], [nest(100000)]));", "[0]"),
    ("echo(lookup(1, nest(100000)));", "undef"),
    (
        "echo(is_list(nest(100000)), len([for (x = nest(100000)) x]));",
        "true",
    ),
    (
        "echo(chr(nest(100000, 65)), chr([nest(100000, 66), nest(100000, 67)]));",
        "\"A\", \"BC\"",
    ),
    // The caches: a module's arguments and `$` variables key the call memo.
    (
        "module m(x) cube(1);\nm(nest(100000)); m(nest(100000));",
        "csg",
    ),
    ("$v = nest(100000);\nmodule m() cube(1);\nm(); m();", "csg"),
    (
        "module m(x) children();\nm(nest(100000)) cube(1); m(nest(100000)) cube(1);",
        "csg",
    ),
    // Parameters that become geometry.
    ("translate(nest(100000)) cube(1);", "csg"),
    ("multmatrix(nest(100000)) cube(1);", "csg"),
    ("color(nest(100000)) cube(1);", "csg"),
    ("polygon(nest(100000));", "csg"),
    (
        "polyhedron(points = nest(100000), faces = [[0, 1, 2]]);",
        "csg",
    ),
    ("cube(nest(100000));", "csg"),
    ("linear_extrude(height = nest(100000)) square(1);", "csg"),
    ("text(nest(100000));", "csg"),
    // A trace prints a module's parameters.
    ("module m(x) assert(false);\nm(nest(100000));", "Assertion"),
    (
        "function f(x, n) = n == 0 ? assert(false) 0 : f(x, n - 1) + 1;\necho(f(nest(100000), 3));",
        "Assertion",
    ),
    // Objects nest as easily (`--enable object-function`).
    (
        "o = nesto(999999); echo(is_undef(o.b), is_undef(o.a));",
        "true, false",
    ),
    ("echo(nesto(100000));", "Stack exhausted"),
    ("echo(nesto(100000) == nesto(100000));", "true"),
    ("echo(object(a = nest(100000)).a == nest(100000));", "true"),
    // So do function literals, each holding the context that holds the
    // last one.
    (
        "function nf(n, acc) = n == 0 ? acc : nf(n - 1, function () acc);\nf = nf(999999, 0); echo(is_function(f));",
        "true",
    ),
    (
        "function nf(n, acc) = n == 0 ? acc : nf(n - 1, function () acc);\nfor (i = [0:2]) echo(is_function(nf(100000, 0)));",
        "true",
    ),
];

#[test]
fn deep_values_on_a_small_thread() {
    let only: Option<usize> = std::env::var("VALUE_DEPTH_CASE")
        .ok()
        .and_then(|s| s.parse().ok());
    for (k, (src, want)) in CASES.iter().enumerate() {
        if only.is_some_and(|o| o != k) {
            continue;
        }
        let opts = Options {
            features: eval::Features::from_names(&["object-function"]),
            ..Options::default()
        };
        let lines = run_small(src, &opts);
        assert!(
            lines.iter().any(|l| l.starts_with(want)),
            "case {k} ({src}): wanted {want:?} in {:?}",
            &lines[..lines.len().min(4)]
        );
    }
}
