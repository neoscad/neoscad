//! The hint of a syntax error names the offending token and where it is,
//! and a program that stops short says it ran out: the end marker
//! OpenSCAD appends (`"\n\x03\n"`) is not a token to show anyone.

use std::path::PathBuf;

use lang::diag::DiagCode;

fn hint(src: &str) -> String {
    let p = lang::parse_file(PathBuf::from("/t.scad"), src.as_bytes().to_vec());
    let d = p
        .diags
        .iter()
        .find(|d| d.code == DiagCode::SyntaxError)
        .expect("a syntax error");
    d.hints[0].message.clone()
}

#[test]
fn a_program_that_stops_short_reads_end_of_input_where_it_stops() {
    // As the command line and the session parse a file: with OpenSCAD's
    // end marker appended. It used to read ``unexpected `\u0003` at line 2,
    // column 1``.
    for (src, at) in [
        ("cube(1\n\x03\n", "line 1, column 7"),
        ("cube(1\n\n\n\x03\n", "line 1, column 7"),
        (
            "sphere(2);\ntranslate([1, 0, 0]\n\x03\n",
            "line 2, column 20",
        ),
        // Without the marker the end of the text is the end.
        ("cube(1", "line 1, column 7"),
    ] {
        let h = hint(src);
        assert!(
            h.starts_with(&format!("unexpected end of input at {at}:")),
            "{src:?}: {h}"
        );
        assert!(!h.contains('\u{3}'), "{h}");
    }
}

#[test]
fn a_token_in_the_wrong_place_is_named() {
    let h = hint("cube(1 sphere(2);\n\x03\n");
    assert!(
        h.starts_with("unexpected `sphere` at line 1, column 8:"),
        "{h}"
    );
}
