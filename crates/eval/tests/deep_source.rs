//! Source nested as deep as the parser allows (`lang::syntax::parser::
//! NESTING_LIMIT`) evaluates, dumps and is freed on an evaluator thread
//! ([`eval::DEFAULT_THREAD_STACK`]).
//!
//! The parser's limit is what bounds every recursion over the source's
//! nesting after it: the lowering, `Unit::add_scope`, the evaluation of
//! nested statements and expressions, and the trees they build. Without
//! it, 26,000 levels of `translate()` overflowed this stack in a release
//! build. If a change makes one of these stages need more stack per
//! level, this is the test that says the limit is now too high.

use std::path::{Path, PathBuf};

use eval::Options;
use lang::loader::StdFs;
use lang::syntax::parser::NESTING_LIMIT;

#[derive(Default)]
struct Lines(Vec<String>);

impl eval::Output for Lines {
    fn message(&mut self, m: &eval::Message<'_>) {
        self.0.push(format!(
            "{}: {}",
            m.diag.severity.openscad_label(),
            String::from_utf8_lossy(m.text)
        ));
    }
}

/// Evaluate `src` and dump its tree, on an evaluator thread; the errors
/// printed.
fn errors(src: String) -> Vec<String> {
    eval::with_stack(eval::DEFAULT_THREAD_STACK, move || {
        let program = lang::parse_file(PathBuf::from("/nonexistent/t.scad"), src.into_bytes());
        assert!(!program.has_syntax_errors(), "{:?}", program.diags.first());
        let mut out = Lines::default();
        let ev = eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &Options::default(),
            &mut out,
        );
        let csg = eval::dump::csg(&ev.root, Path::new("/nonexistent"), &StdFs);
        assert!(!csg.is_empty());
        drop(ev);
        drop(program);
        out.0.retain(|l| l.starts_with("ERROR"));
        out.0
    })
}

#[test]
fn source_nested_to_the_limit_evaluates() {
    // A few nodes of each program are not its levels (the root, the
    // statement, the innermost call's arguments); 10 covers them.
    let n = NESTING_LIMIT as usize - 10;
    for (what, src) in [
        ("translate", "translate([0, 0, 1]) ".repeat(n) + "cube(1);"),
        ("blocks", "{".repeat(n) + "cube(1);" + &"}".repeat(n)),
        // Two nodes a level: `IfInst > ElseClause`.
        (
            "else if",
            "x = 1;\n".to_string() + &"if (x == 0) cube(1); else ".repeat(n / 2) + "sphere(1);",
        ),
        (
            "parens",
            format!("echo({}1{});", "(".repeat(n), ")".repeat(n)),
        ),
        ("sum", format!("echo({}1);", "1 + ".repeat(n))),
        ("negations", format!("echo({}1);", "-".repeat(n))),
        ("lets", format!("echo({}1);", "let (a = 1) ".repeat(n))),
        (
            "ternaries",
            format!("echo({}1{});", "true ? ".repeat(n), " : 0".repeat(n)),
        ),
        (
            "comprehensions",
            format!("echo([{}1]);", "for (i = [0]) ".repeat(n)),
        ),
        // Three nodes a level: `CallExpr > ArgList > Arg`.
        (
            "calls",
            format!("echo({}1{});", "max(".repeat(n / 3), ")".repeat(n / 3)),
        ),
        // Nested list literals take memory with the square of their depth
        // to evaluate (a followup), about 750 MB at the limit: these stay at a
        // fifth of it.
        (
            "lists",
            format!("echo({}1{});", "[".repeat(n / 5), "]".repeat(n / 5)),
        ),
    ] {
        assert_eq!(errors(src), Vec::<String>::new(), "{what}");
    }
}
