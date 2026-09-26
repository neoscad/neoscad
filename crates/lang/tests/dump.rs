//! End-to-end `.ast` dumps, with expected text taken from the OpenSCAD
//! nightly (2026.09.23) on the same input.

use lang::customizer::json;
use lang::customizer::{ParameterSet, Parameters};
use lang::loader::{LibraryPath, StdFs};
use lang::parse_program;

fn program(src: &str) -> lang::Program {
    let mut text = src.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    parse_program(
        "/nonexistent/t.scad".into(),
        text,
        &StdFs,
        &LibraryPath::default(),
    )
}

fn dump(src: &str) -> String {
    let p = program(src);
    assert!(!p.has_syntax_errors(), "{:?}", p.diags);
    String::from_utf8(lang::dump::dump(&p.ast)).unwrap()
}

#[test]
fn statements_and_modifiers() {
    let src = "#a() b();\n!c();\n%d() { x = 1; }\n*e();\nif (1) f(); else ;\nif (1) {} else {g(); h();}\n\
               if (1) if (2) k(); else m();\nfor(i=[1:2]) let(a=1) echo(a) assert(true) each() n();\n\
               module mm() module nn() o(); x = f(1)(2)[3].y;\ny = function(a) a + 1;\n\
               z = -(-1);w = -2^2;v=+a;\n\
               function ff() = let(a=1) [for (i=[0:1], j=1) if (i) each i else i, for (i=0;i<1;i=i+1) i, let(z=1) (for(k=1) k)];\n\
               e = echo(1) assert(2);";
    let expected = "function ff() = let(a = 1) [for(i = [0 : 1], j = 1) (if(i) (each (i)) else (i)), for(i = 0;(i < 1);i = (i + 1)) i, let(z = 1) (for(k = 1) (k))];
module mm() {
\tmodule nn() {
\t\to();
\t}
}
x = (f(1))(2)[3].y;
y = function(a) (a + 1);
z = 1;
w = -(2 ^ 2);
v = a;
e = echo(1) assert(2);
a() b();
c();
d() x = 1;
if(1) f();
else;if(1);
else {
\tg();
\th();
}
if(1) if(2) k();
else m();
for(i = [1 : 2]) let(a = 1) echo(a) assert(true) each() n();
";
    assert_eq!(dump(src), expected);
}

#[test]
fn numbers_and_parameter_annotations() {
    let src = "x = [1234565, 1e6, 1e21,1e20, 0.00001, 0.000001, -0, 0x10, 01.5, 1e400 5, \
               123456789012345678901234567890, .5e3, 5.];";
    assert_eq!(
        dump(src),
        "//Parameter(\"\")\nx = [1.23457e+6, 1e+6, 1e+21, 1e+20, 0.00001, 1e-6, 0, 16, 1.5, 5, 1.23457e+29, 500, 5];\n"
    );
    // Assignments at or after the first `{` get no annotations.
    assert_eq!(
        dump("a = 1;;\n{b=2;}\nc=3;x();"),
        "//Parameter(\"\")\na = 1;\nb = 2;\nc = 3;\nx();\n"
    );
}

#[test]
fn reassignment_keeps_first_position() {
    let p = program("// keep\na = \"test\";\nb = true;\na = assert(b);");
    let out = String::from_utf8(lang::dump::dump(&p.ast)).unwrap();
    assert_eq!(out, "a = assert(b);\n//Parameter(\"\")\nb = true;\n");
    let msgs: Vec<_> = p
        .diags
        .iter()
        .map(|d| (d.message.as_str(), d.line))
        .collect();
    assert_eq!(
        msgs,
        [("\"a\" was assigned on line 2 but was overwritten", 4)]
    );
}

#[test]
fn first_error_matches_openscad_and_recovery_continues() {
    let p = program("a = 1;\nb = (;\nc = ;\nd = 2;");
    let errors: Vec<_> = p
        .diags
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.line)
        .collect();
    assert_eq!(errors, [2, 3]);
    assert_eq!(p.openscad_diags().count(), 1);
    let p = program("a = \"abc");
    let msgs: Vec<_> = p
        .openscad_diags()
        .map(|d| (d.message.as_str(), d.line))
        .collect();
    assert_eq!(
        msgs,
        [
            ("Parser error: Unterminated string", 3),
            ("Parser error: syntax error", 3)
        ]
    );
}

#[test]
fn parameter_sets_apply_with_validation() {
    let src = "/* [G] */\nn = 2; // [0, 1, 2, 3]\ns = 34; // [10:100]\nb = true;\nv = [1, 2]; // x\nt = \"ab\"; // 3\n";
    let mut p = program(src);
    let mut warnings = Vec::new();
    let mut params = Parameters::from_ast(&p.ast, &mut warnings);
    assert!(warnings.is_empty());
    let set_json = br#"{"n": "7", "s": "340", "b": "false", "v": "[3, 4]", "t": "abcdef"}"#;
    let root = json::parse(set_json).unwrap();
    let set = ParameterSet {
        name: "x".into(),
        values: root.children.clone(),
    };
    params.import(&set);
    params.apply(&mut p.ast);
    let out = String::from_utf8(lang::dump::dump(&p.ast)).unwrap();
    assert_eq!(
        out,
        "//Group(\"G\")\n//Parameter([0, 1, 2, 3])\nn = 2;\n//Group(\"G\")\n//Parameter([10 : 100])\ns = 100;\n\
         //Group(\"G\")\n//Parameter(\"\")\nb = false;\n//Group(\"G\")\n//Parameter(\"x\")\nv = [3, 4];\n\
         //Group(\"G\")\n//Parameter(3)\nt = \"abc\";\n"
    );
}

/// Lowering and dumping a broken program (after recovery) must not panic.
#[test]
fn broken_programs_lower_and_dump() {
    let pieces = [
        "a", "=", "(", ")", "[", "]", "{", "}", ";", ",", ":", "?", "1", "\"s\"", "let", "for",
        "if", "else", "each", "module", "function", "+", "-", "*", "!", "#", "%", ".", "^", "echo",
        "assert", "\n",
    ];
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..3000 {
        let mut src = String::new();
        for _ in 0..(seed % 50) {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            src.push_str(pieces[(seed % pieces.len() as u64) as usize]);
            src.push(' ');
        }
        let p = lang::parse_file("/nonexistent/g.scad".into(), src.into_bytes());
        let _ = lang::dump::dump(&p.ast);
    }
}
