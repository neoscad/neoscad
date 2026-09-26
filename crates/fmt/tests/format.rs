//! The formatter's layout, comments, customizer annotations, and a pass
//! over every `.scad` file of the reference corpus: formatted output must
//! keep the program (`scadfmt::format` checks that on every file) and be
//! idempotent.

use std::path::{Path, PathBuf};

use scadfmt::{Config, Error, format};

fn fmt_with(src: &str, cfg: &Config) -> String {
    let out = format(src.as_bytes(), cfg).unwrap_or_else(|e| panic!("{e}\n{src}"));
    let out = String::from_utf8(out).unwrap();
    let again = String::from_utf8(format(out.as_bytes(), cfg).unwrap()).unwrap();
    assert_eq!(out, again, "not idempotent");
    out
}

fn fmt(src: &str) -> String {
    fmt_with(src, &Config::default())
}

#[test]
fn statements_and_blocks() {
    assert_eq!(
        fmt("module m(a,b=2){cube(a,center=true);sphere(b);}\n\n\n\nm(1);"),
        "module m(a, b=2) {\n    cube(a, center=true);\n    sphere(b);\n}\n\nm(1);\n"
    );
    assert_eq!(
        fmt("if(a)cube(1);\nelse if(b)sphere(1);else{cylinder(1);}"),
        "if (a) cube(1);\nelse if (b) sphere(1); else {\n    cylinder(1);\n}\n"
    );
    assert_eq!(
        fmt("if (a) {\ncube(1);\n} else {\n}\n"),
        "if (a) {\n    cube(1);\n} else {}\n"
    );
    // A child written on its own line stays there.
    assert_eq!(
        fmt("translate([1,0,0])\nrotate(45)\ncube(1);\n#translate([0,0,1]) %cube(1);"),
        "translate([1, 0, 0])\n    rotate(45)\n        cube(1);\n#translate([0, 0, 1]) %cube(1);\n"
    );
    assert_eq!(
        fmt("for(i=[0:1:10],j=[a:b])echo(i);let(x=1)cube(x);"),
        "for (i = [0:1:10], j = [a:b]) echo(i);\nlet (x = 1) cube(x);\n"
    );
    assert_eq!(fmt("x = [0:len(v)-1];"), "x = [0 : len(v) - 1];\n");
    assert_eq!(
        fmt("include <a.scad>\nuse <b.scad>\ncube();"),
        "include <a.scad>\nuse <b.scad>\ncube();\n"
    );
}

#[test]
fn expressions() {
    assert_eq!(fmt("x=-a+b*c^2;"), "x = -a + b * c ^ 2;\n");
    assert_eq!(fmt("x=a?b:c?d:e;"), "x = a ? b : c ? d : e;\n");
    assert_eq!(
        fmt("module q() {}\nf=function(x)x*2;y=f(3)[0].x;"),
        "module q() {}\nf = function(x) x * 2;\ny = f(3)[0].x;\n"
    );
    assert_eq!(
        fmt("v=[for(i=[0:3])if(i%2==0)i else -i,each [1,2]];"),
        "v = [for (i = [0:3]) if (i % 2 == 0) i else -i, each [1, 2]];\n"
    );
    assert_eq!(
        fmt("function f(x)=let(a=1,b=2)assert(x>0,\"pos\")a+b+x;"),
        "function f(x) = let (a = 1, b = 2) assert(x > 0, \"pos\") a + b + x;\n"
    );
}

#[test]
fn long_lines_wrap() {
    let cfg = Config {
        indent: 4,
        width: 40,
    };
    assert_eq!(
        fmt_with(
            "cube([100, 200, 300], center = true, extra = something_long);",
            &cfg
        ),
        "cube(\n    [100, 200, 300],\n    center=true,\n    extra=something_long\n);\n"
    );
    // A lone vector argument hugs the parentheses; numbers fill lines.
    assert_eq!(
        fmt_with("polygon([[0,0],[10,0],[10,10],[0,10],[5,5],[3,3]]);", &cfg),
        "polygon([\n    [0, 0],\n    [10, 0],\n    [10, 10],\n    [0, 10],\n    [5, 5],\n    [3, 3]\n]);\n"
    );
    assert_eq!(
        fmt_with(
            "x = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];",
            &cfg
        ),
        "x = [\n    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11,\n    12, 13, 14, 15\n];\n"
    );
    assert_eq!(
        fmt_with(
            "function f(x) = x > 0 ? some_long_name(x) : other_long_name(x);",
            &cfg
        ),
        "function f(x) =\n    x > 0 ? some_long_name(x)\n    : other_long_name(x);\n"
    );
    assert_eq!(
        fmt_with("y = alpha_value + beta_value + gamma_value * delta;", &cfg),
        "y =\n    alpha_value +\n        beta_value +\n        gamma_value * delta;\n"
    );
}

#[test]
fn comments_stay_where_they_were() {
    let src = "// header\n\n/* block */\nmodule m() { // after brace\n  // inside\n  cube(1); // trailing\n\n\n  // before close\n}\nx = [1, // one\n  2];\ny = 1; /* inline */ z = 2;\n// end\n";
    assert_eq!(
        fmt(src),
        "// header\n\n/* block */\nmodule m() { // after brace\n    // inside\n    cube(1); // trailing\n\n    // before close\n}\nx = [\n    1, // one\n    2\n];\ny = 1; /* inline */\nz = 2;\n// end\n"
    );
    // A comment between a call and its block keeps its line.
    assert_eq!(
        fmt("for (i = [1:3])\n// note\n{\ncube(i);\n}\n"),
        "for (i = [1:3])\n// note\n{\n    cube(i);\n}\n"
    );
    // Trailing blanks of comments go, except where the customizer reads
    // them (see below).
    assert_eq!(fmt("module m() {} // x  \n"), "module m() {} // x\n");
}

#[test]
fn customizer_annotations_are_kept() {
    // Parameters: the comment after an assignment, the description on
    // the line before (only in column 1), groups, and statements sharing
    // a line (OpenSCAD ignores the comment of a line with two).
    let src = "/* [Size] */\n// width of the box  \nwidth=10; // [5:50]\n  // not a description\ndepth=20;\na=1; b=2; // [1:3]\nlong=[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32]; // list\nmodule m() {}\n";
    let out = fmt(src);
    assert_eq!(
        out,
        "/* [Size] */\n// width of the box  \nwidth = 10; // [5:50]\n  // not a description\ndepth = 20;\na = 1; b = 2; // [1:3]\nlong = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32]; // list\nmodule m() {}\n"
    );
    assert_eq!(
        scadfmt::ast_dump(src.as_bytes()),
        scadfmt::ast_dump(out.as_bytes())
    );
}

#[test]
fn files_that_do_not_parse_are_refused() {
    match format(b"cube(1;\n", &Config::default()) {
        Err(Error::Syntax(e)) => assert_eq!(e[0].line, 1),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        format(b"x = \"open\n", &Config::default()),
        Err(Error::Syntax(_))
    ));
}

#[test]
fn line_endings_and_encodings() {
    assert_eq!(fmt("a=1;\r\nb=2; // c\r\n"), "a = 1;\r\nb = 2; // c\r\n");
    // Latin-1 bytes in a string come through unchanged.
    let out = format(b"s=\"caf\xe9\";", &Config::default()).unwrap();
    assert_eq!(out, b"s = \"caf\xe9\";\n");
    assert_eq!(fmt(""), "");
    assert_eq!(fmt("\n\n// only\n\n"), "// only\n");
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|e| e == "scad") {
            out.push(p);
        }
    }
}

/// Every file of OpenSCAD's tests and examples, MCAD and BOSL2 (those
/// present under `.reference`): formatted with its program unchanged, or
/// refused for a syntax error; and formatting the output again changes
/// nothing.
#[test]
fn reference_corpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.reference");
    let dirs = [
        "openscad/tests/data/scad",
        "openscad/examples",
        "openscad/libraries/MCAD",
        "BOSL2",
    ];
    let cfg = Config::default();
    let mut problems = Vec::new();
    let (mut formatted, mut syntax) = (0, 0);
    for d in dirs {
        let mut files = Vec::new();
        walk(&root.join(d), &mut files);
        for f in files {
            let text = std::fs::read(&f).unwrap();
            match format(&text, &cfg) {
                Ok(out) => {
                    formatted += 1;
                    match format(&out, &cfg) {
                        Ok(again) if again == out => {}
                        _ => problems.push(format!("not idempotent: {}", f.display())),
                    }
                }
                Err(Error::Syntax(_)) => syntax += 1,
                Err(e) => problems.push(format!("{}: {e}", f.display())),
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    eprintln!("reference corpus: {formatted} formatted, {syntax} with syntax errors");
}
