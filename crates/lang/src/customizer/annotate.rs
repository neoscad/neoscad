//! Attaching customizer annotations to top-level assignments.
//!
//! A port of `CommentParser::collectParameters` (src/core/customizer/
//! CommentParser.cc). OpenSCAD does not use the syntax tree for this: it
//! rescans the raw program text line by line, with its own simplified idea
//! of strings and comments. That scan has quirks (a `{` inside a string
//! still ends the parameter section; a line comment needs `//` in column 1
//! to count as a description) and the goldens depend on them, so the scan
//! is ported byte for byte rather than derived from the CST.

use crate::ast::{Annotation, Ast, ExprKind};
use crate::customizer::comment;
use crate::source::{FileId, Span};

struct Group {
    name: Vec<u8>,
    line: u32,
}

fn starts(text: &[u8], i: usize, pat: &[u8]) -> bool {
    text.get(i..).is_some_and(|t| t.starts_with(pat))
}

/// Line of the first `{` outside comments (strings are not respected), or
/// the last line: assignments at or after it are not parameters.
fn line_to_stop(text: &[u8]) -> u32 {
    let n = text.len();
    let mut line = 1u32;
    let mut in_string = false;
    let mut i = 0usize;
    while i < n {
        if text[i] == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        if in_string && starts(text, i, b"\\\"") {
            i += 2;
            continue;
        }
        if text[i] == b'"' {
            in_string = !in_string;
            i += 1;
            continue;
        }
        if !in_string && starts(text, i, b"//") {
            i += 1;
            while i < n && text[i] != b'\n' {
                i += 1;
            }
            line += 1;
            i += 1;
            continue;
        }
        if !in_string && starts(text, i, b"/*") {
            i += 1;
            if i < n {
                i += 1;
            } else {
                i += 1;
                continue;
            }
            while i < n && !starts(text, i, b"*/") {
                if text[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
        }
        if i < n && text[i] == b'{' {
            return line;
        }
        i += 1;
    }
    line
}

/// Single-line `/* ... */` comments, each naming a group by its `[...]`
/// parts joined with `-`.
fn collect_groups(text: &[u8]) -> Vec<Group> {
    let n = text.len();
    let mut groups = Vec::new();
    let mut line = 1u32;
    let mut in_string = false;
    let mut i = 0usize;
    while i < n {
        if text[i] == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        if in_string && starts(text, i, b"\\\"") {
            i += 2;
            continue;
        }
        if text[i] == b'"' {
            in_string = !in_string;
            i += 1;
            continue;
        }
        if !in_string && starts(text, i, b"//") {
            i += 1;
            while i < n && text[i] != b'\n' {
                i += 1;
            }
            line += 1;
            i += 1;
            continue;
        }
        if !in_string && starts(text, i, b"/*") {
            i += 1;
            if i < n {
                i += 1;
            } else {
                i += 1;
                continue;
            }
            let mut is_group = true;
            let from = i;
            while i < n && !starts(text, i, b"*/") {
                if text[i] == b'\n' {
                    line += 1;
                    is_group = false;
                }
                i += 1;
            }
            if is_group {
                groups.push(Group {
                    name: group_name(&text[from..i]),
                    line,
                });
            }
        }
        i += 1;
    }
    groups
}

/// `createGroup`: every `\[(.*?)\]` match, joined with `-`.
fn group_name(mut comment: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut first = true;
    while let Some(open) = comment.iter().position(|&b| b == b'[') {
        let Some(close) = comment[open + 1..].iter().position(|&b| b == b']') else {
            break;
        };
        if !first {
            out.push(b'-');
        }
        first = false;
        out.extend_from_slice(&comment[open + 1..open + 1 + close]);
        comment = &comment[open + 1 + close + 1..];
    }
    out
}

/// Byte offset where 1-based `line` starts (the loop in `getComment`).
fn line_start(text: &[u8], mut line: u32) -> usize {
    let mut start = 0;
    while start < text.len() {
        if line <= 1 {
            break;
        }
        if text[start] == b'\n' {
            line -= 1;
        }
        start += 1;
    }
    start
}

/// `getComment`: the text after `//` on `line`, unless the line holds more
/// than one statement.
fn get_comment(text: &[u8], line: u32) -> Vec<u8> {
    if line < 1 {
        return Vec::new();
    }
    let start = line_start(text, line);
    let mut end = start + 1;
    while end < text.len() && text[end] != b'\n' {
        end += 1;
    }
    let comment = &text[start.min(text.len())..end.min(text.len())];
    if comment.is_empty() {
        return Vec::new();
    }
    let mut k = 0usize;
    let mut semicolons = 0;
    let mut in_string = false;
    while k < comment.len() - 1 {
        if in_string && starts(comment, k, b"\\\"") {
            k += 2;
            continue;
        }
        if comment[k] == b'"' {
            in_string = !in_string;
        }
        if !in_string {
            if starts(comment, k, b"//") {
                break;
            }
            if comment[k] == b';' {
                if semicolons > 0 {
                    return Vec::new();
                }
                semicolons += 1;
            }
        }
        k += 1;
    }
    if k + 2 > comment.len() {
        return Vec::new();
    }
    comment[k + 2..].to_vec()
}

/// `getDescription`: a `//` comment starting in column 1 of `line`, with
/// leading blanks removed and any further `//` turned into a space.
fn get_description(text: &[u8], line: u32) -> Vec<u8> {
    if line < 1 {
        return Vec::new();
    }
    let mut start = line_start(text, line);
    if !starts(text, start, b"//") {
        return Vec::new();
    }
    start += 2;
    while start < text.len() && (text[start] == b' ' || text[start] == b'\t') {
        start += 1;
    }
    let mut out = Vec::new();
    while start < text.len() && text[start] != b'\n' {
        if starts(text, start, b"//") {
            out.push(b' ');
            start += 1;
        } else {
            out.push(text[start]);
        }
        start += 1;
    }
    out
}

/// Annotate the literal top-level assignments of the main file. `fulltext`
/// is the main file's text as parsed (with the `-D` suffix), and `is_main`
/// tells whether a file is the main file.
pub fn collect_parameters(ast: &mut Ast, fulltext: &[u8], is_main: impl Fn(FileId) -> bool) {
    let groups = collect_groups(fulltext);
    let parse_till = line_to_stop(fulltext);
    let mut root = std::mem::take(&mut ast.root);
    for a in &mut root.assignments {
        if !ast.is_literal(a.expr) {
            continue;
        }
        let loc = a.overwrite.unwrap_or(a.loc);
        if loc.line >= parse_till || !is_main(loc.span.file) {
            continue;
        }
        let mut list = Vec::new();
        let c = get_comment(fulltext, loc.line);
        let param = if c.is_empty() {
            None
        } else {
            comment::parse(&c, ast)
        };
        let param =
            param.unwrap_or_else(|| ast.add(ExprKind::String(Box::default()), Span::default()));
        list.push(Annotation {
            name: "Parameter",
            expr: param,
        });

        let descr = get_description(fulltext, loc.line - 1);
        if !descr.is_empty() {
            let e = ast.add(ExprKind::String(descr.into()), Span::default());
            list.push(Annotation {
                name: "Description",
                expr: e,
            });
        }
        if let Some(g) = groups.iter().rev().find(|g| g.line < loc.line) {
            let e = ast.add(ExprKind::String(g.name.as_slice().into()), Span::default());
            list.push(Annotation {
                name: "Group",
                expr: e,
            });
        }
        // `addAnnotations` inserts into a map, so an existing annotation of
        // the same name is kept.
        for an in list {
            if a.annotation(an.name).is_none() {
                a.annotations.push(an);
            }
        }
    }
    ast.root = root;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_line_is_first_brace_even_in_strings() {
        assert_eq!(line_to_stop(b"a=1;\n// {\nb=\"{\";\nc(){}"), 3);
        assert_eq!(line_to_stop(b"a=1;\n/* {\n */ x{"), 3);
        assert_eq!(line_to_stop(b"a=1;\nb=2;\n"), 3);
    }

    #[test]
    fn groups_are_single_line_block_comments() {
        let g = collect_groups(b"/* [A] */\nx=1;\n/*[B][C]*/\n/* multi\nline [D] */\n/* none */");
        let v: Vec<_> = g.iter().map(|g| (g.name.as_slice(), g.line)).collect();
        assert_eq!(v, [(&b"A"[..], 1), (b"B-C", 3), (b"", 6)]);
    }

    #[test]
    fn comments_and_descriptions() {
        let t = b"// the answer\nx = 42; // [0:100]\ny = 1; z = 2; // no\n  // indented\nw = \"a//b\"; //c\n";
        assert_eq!(get_comment(t, 2), b" [0:100]");
        assert_eq!(get_comment(t, 3), b"");
        assert_eq!(get_comment(t, 5), b"c");
        assert_eq!(get_description(t, 1), b"the answer");
        assert_eq!(get_description(t, 4), b"");
        assert_eq!(get_description(b"// a // b\n", 1), b"a   b");
    }
}
