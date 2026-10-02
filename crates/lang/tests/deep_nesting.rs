//! Source nested deeper than the parser's limit (`syntax::parser::
//! NESTING_LIMIT`) ends in OpenSCAD's "memory exhausted" parser error, and
//! source nested up to it parses and lowers, both within the stack of a
//! process's main thread.
//!
//! Before the limit, each program here overflowed that stack: the parser
//! and the lowering recurse once per level. At the limit of an optimised
//! build (5,000 levels of `translate()`) they take 3.9 MB; a test build's
//! frames are several times larger (17 MB for 5,000 levels), and its limit
//! is half.

use std::path::PathBuf;

use lang::Program;
use lang::diag::DiagCode;
use lang::syntax::parser::NESTING_LIMIT;

/// The main thread's stack on macOS and Linux (8 MiB), which `neoscad fmt`
/// parses on, in an optimised build; four times that in a test build.
const STACK: usize = if cfg!(debug_assertions) {
    32 << 20
} else {
    8 << 20
};

/// Far past the limit (20 times it in an optimised build).
const DEEP: usize = 100_000;

fn on_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn_scoped(s, f)
            .expect("a thread")
            .join()
            .expect("no overflow or panic")
    })
}

fn parse(src: &str) -> Program {
    lang::parse_file(PathBuf::from("/t.scad"), src.as_bytes().to_vec())
}

/// `n` levels of each kind of nesting, with how many syntax tree nodes
/// one level is.
fn programs(n: usize) -> Vec<(&'static str, String)> {
    vec![
        ("translate", "translate() ".repeat(n) + "cube();"),
        ("blocks", "{".repeat(n) + "cube();" + &"}".repeat(n)),
        ("modules", "module m() ".repeat(n) + "cube();"),
        (
            "parens",
            format!("x = {}1{};", "(".repeat(n), ")".repeat(n)),
        ),
        ("lists", format!("x = {}1{};", "[".repeat(n), "]".repeat(n))),
        ("negations", format!("x = {}1;", "-".repeat(n))),
        (
            "calls",
            format!("x = {}1{};", "f(".repeat(n), ")".repeat(n)),
        ),
        ("sum", format!("x = {}1;", "1 + ".repeat(n))),
        ("powers", format!("x = {}1;", "2 ^ ".repeat(n))),
        (
            "ternaries",
            format!("x = {}1{};", "a ? ".repeat(n), " : 0".repeat(n)),
        ),
        ("lets", format!("x = {}1;", "let (a = 1) ".repeat(n))),
        (
            "comprehensions",
            format!("x = [{}1];", "for (i = [0]) ".repeat(n)),
        ),
        ("else if", "if (a) b(); else ".repeat(n) + "c();"),
    ]
}

#[test]
fn nesting_past_the_limit_is_a_parser_error() {
    for (what, src) in programs(DEEP) {
        let p = on_stack(|| parse(&src));
        let errors: Vec<_> = p.diags.iter().filter(|d| d.is_error()).collect();
        assert_eq!(errors.len(), 1, "{what}: {errors:?}");
        assert_eq!(errors[0].code, DiagCode::SyntaxError, "{what}");
        assert_eq!(
            errors[0].message, "Parser error: memory exhausted",
            "{what}"
        );
        assert!(p.has_syntax_errors());
        // The unparsed rest is kept, so the tree still holds every byte.
        assert_eq!(p.cst.text(&p.sources), src.as_bytes(), "{what}");
        on_stack(move || drop(p));
    }
}

#[test]
fn nesting_up_to_the_limit_parses() {
    let limit = NESTING_LIMIT as usize;
    // SourceFile > ModuleInst x n > the last ModuleInst > ArgList.
    let at = |n: usize| "translate() ".repeat(n) + "cube();";
    let ok = on_stack(|| parse(&at(limit - 3)));
    assert!(!ok.has_syntax_errors(), "{:?}", ok.diags.first());
    on_stack(move || drop(ok));
    assert!(on_stack(|| parse(&at(limit - 2)).has_syntax_errors()));

    // SourceFile > Assignment > ParenExpr x n > Literal.
    let at = |n: usize| format!("x = {}1{};", "(".repeat(n), ")".repeat(n));
    let ok = on_stack(|| parse(&at(limit - 3)));
    assert!(!ok.has_syntax_errors(), "{:?}", ok.diags.first());
    on_stack(move || drop(ok));
    assert!(on_stack(|| parse(&at(limit - 2)).has_syntax_errors()));

    // Every other kind, a little under the limit.
    for (what, src) in programs(limit / 4) {
        let p = on_stack(|| parse(&src));
        assert!(!p.has_syntax_errors(), "{what}: {:?}", p.diags.first());
        on_stack(move || drop(p));
    }
}
