//! Source nested deeper than the parser's limit (`syntax::parser::
//! NESTING_LIMIT`) ends in OpenSCAD's "memory exhausted" parser error, and
//! source nested up to it parses and lowers, both within the stack of a
//! process's main thread.
//!
//! Before the limit, each program here overflowed that stack: the parser
//! and the lowering recurse once per level. With the plain node count the
//! limit used to be, an optimised build's 5,000 levels of `translate()`
//! took 3.9 MB; a test build's frames are several times larger (17 MB for
//! 5,000 levels), and its limit is half.

use std::path::PathBuf;

use lang::Program;
use lang::diag::DiagCode;
use lang::source::FileId;
use lang::syntax::SyntaxKind::{
    self, Arg, ArgList, Assignment, BinaryExpr, BlockStmt, CallExpr, ElseClause, IfInst, LcFor,
    LetExpr, Literal, ModuleDef, ModuleInst, ParenExpr, SourceFile, TernaryExpr, UnaryExpr,
    VectorExpr,
};
use lang::syntax::parser::{NESTING_LIMIT, WASM32_NESTING_LIMIT, nesting_weight, parse_with_limit};

/// The main thread's stack on macOS and Linux (8 MiB), which `neoscad fmt`
/// parses on, in an optimised build; four times that in a test build.
const STACK: usize = if cfg!(debug_assertions) {
    32 << 20
} else {
    8 << 20
};

/// Far past the limit for every kind (100,000 levels; the cheapest kinds
/// reach 5,000 in an optimised build).
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

fn weight(path: &[SyntaxKind]) -> u32 {
    path.iter().map(|&k| nesting_weight(k)).sum()
}

/// Each kind of nesting, `levels(w)` levels of it, where `w` is the
/// weight of one level (the nodes a level adds).
fn programs(levels: impl Fn(u32) -> usize) -> Vec<(&'static str, String)> {
    let n = |kinds: &[SyntaxKind]| levels(weight(kinds));
    vec![
        (
            "translate",
            "translate() ".repeat(n(&[ModuleInst])) + "cube();",
        ),
        ("blocks", {
            let n = n(&[BlockStmt]);
            "{".repeat(n) + "cube();" + &"}".repeat(n)
        }),
        ("modules", "module m() ".repeat(n(&[ModuleDef])) + "cube();"),
        ("parens", {
            let n = n(&[ParenExpr]);
            format!("x = {}1{};", "(".repeat(n), ")".repeat(n))
        }),
        ("lists", {
            let n = n(&[VectorExpr]);
            format!("x = {}1{};", "[".repeat(n), "]".repeat(n))
        }),
        (
            "negations",
            format!("x = {}1;", "-".repeat(n(&[UnaryExpr]))),
        ),
        ("calls", {
            let n = n(&[CallExpr, ArgList, Arg]);
            format!("x = {}1{};", "f(".repeat(n), ")".repeat(n))
        }),
        ("sum", format!("x = {}1;", "1 + ".repeat(n(&[BinaryExpr])))),
        (
            "powers",
            format!("x = {}1;", "2 ^ ".repeat(n(&[BinaryExpr]))),
        ),
        ("ternaries", {
            let n = n(&[TernaryExpr]);
            format!("x = {}1{};", "a ? ".repeat(n), " : 0".repeat(n))
        }),
        (
            "lets",
            format!("x = {}1;", "let (a = 1) ".repeat(n(&[LetExpr]))),
        ),
        (
            "comprehensions",
            format!("x = [{}1];", "for (i = [0]) ".repeat(n(&[LcFor]))),
        ),
        (
            "else if",
            "if (a) b(); else ".repeat(n(&[IfInst, ElseClause])) + "c();",
        ),
    ]
}

#[test]
fn nesting_past_the_limit_is_a_parser_error() {
    for (what, src) in programs(|_| DEEP) {
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
    // SourceFile > ModuleInst x (n + 1) > the last ModuleInst's ArgList.
    let at = |n: usize| "translate() ".repeat(n) + "cube();";
    let fixed = weight(&[SourceFile, ArgList]);
    let n = ((NESTING_LIMIT - fixed) / nesting_weight(ModuleInst)) as usize - 1;
    let ok = on_stack(|| parse(&at(n)));
    assert!(!ok.has_syntax_errors(), "{:?}", ok.diags.first());
    on_stack(move || drop(ok));
    assert!(on_stack(|| parse(&at(n + 1)).has_syntax_errors()));

    // SourceFile > Assignment > ParenExpr x n > Literal.
    let at = |n: usize| format!("x = {}1{};", "(".repeat(n), ")".repeat(n));
    let fixed = weight(&[SourceFile, Assignment, Literal]);
    let n = ((NESTING_LIMIT - fixed) / nesting_weight(ParenExpr)) as usize;
    let ok = on_stack(|| parse(&at(n)));
    assert!(!ok.has_syntax_errors(), "{:?}", ok.diags.first());
    on_stack(move || drop(ok));
    assert!(on_stack(|| parse(&at(n + 1)).has_syntax_errors()));

    // Every other kind, a little under the limit (the root, the statement
    // and the innermost operand are not levels).
    for (what, src) in programs(|w| ((NESTING_LIMIT - 200) / w) as usize) {
        let p = on_stack(|| parse(&src));
        assert!(!p.has_syntax_errors(), "{what}: {:?}", p.diags.first());
        on_stack(move || drop(p));
    }
}

/// The smallest nesting limit `src` parses under: its weighted depth.
fn weighted_depth(src: &[u8]) -> u32 {
    let parses = |limit: u32| {
        parse_with_limit(lang::syntax::lexer::lex(src, FileId(0)).tokens, limit)
            .errors
            .iter()
            .all(|e| !e.too_deep)
    };
    let (mut lo, mut hi) = (0, NESTING_LIMIT);
    assert!(parses(hi));
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if parses(mid) { hi = mid } else { lo = mid }
    }
    hi
}

/// The deepest library files by weight parse in a browser: their weighted
/// depth is under the wasm32 limit. MCAD's `bitmap.scad` (an `else if`
/// chain 186 nodes deep) is vendored; BOSL2's `nurbs.scad` (66 `assert`s
/// chained in one expression, the deepest by weight in BOSL2, MCAD and
/// OpenSCAD's tests) is checked when the reference checkout is there.
#[test]
fn the_deepest_library_files_parse_in_a_browser() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let bitmap = std::fs::read(format!("{root}/assets/libraries/MCAD/bitmap/bitmap.scad"))
        .expect("the vendored MCAD");
    let depth = weighted_depth(&bitmap);
    assert!(
        depth < WASM32_NESTING_LIMIT * 8 / 10,
        "bitmap.scad: {depth}"
    );
    match std::fs::read(format!("{root}/.reference/BOSL2/nurbs.scad")) {
        Ok(nurbs) => {
            let depth = weighted_depth(&nurbs);
            assert!(depth < WASM32_NESTING_LIMIT * 9 / 10, "nurbs.scad: {depth}");
        }
        Err(_) => eprintln!("skipped nurbs.scad: no .reference/BOSL2"),
    }
}
