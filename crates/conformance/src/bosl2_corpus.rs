//! `conformance bosl2-corpus`: BOSL2's documentation examples and tests as
//! one `.scad` file each, for `conformance diff`.
//!
//! BOSL2 documents itself in comments (`// Example(2D): title` followed by
//! the code, indented `//   `), and tests itself with `tests/*.scadtest`
//! tables that its own runner feeds to OpenSCAD. Neither is a file
//! OpenSCAD can run, so this writes them out beside the library:
//!
//! - `examples_x/<file>__NNN.scad`: every `// Example` block of the
//!   library's top-level files (except `NORENDER` ones and blocks with no
//!   code), numbered per file, after the file's `// Includes:` lines and
//!   an include of the file itself. `ex__<name>.scad`: each file of
//!   `examples/`. `meta.json`: each block's tags and title.
//! - `tests_x/<file>__<test>.scad`: every `[[test]]` script, and
//!   `meta.json` with each test's name and flags.
//!
//! Includes are rewritten from `<BOSL2/...>` to `<../...>`, so the files
//! run from where they are, and `conformance diff --library-path
//! .reference` finds the same library either way.
//!
//! The rules reproduce, byte for byte, the corpus the engine-milestone
//! audit extracted by hand (no script was kept): 2,516 examples,
//! 10 `examples/` files and 976 tests at BOSL2 `9948313`. That includes
//! its quirks, since the files' names are in audit reports: the header
//! pattern also takes `// Examples:` blocks (whole, as one example) with
//! the title `s:`, a header whose tag list is never closed takes the text
//! up to the next `)` as tags, and a block indented by two spaces instead
//! of three has no code and is skipped.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::bench::read_scadtests;

/// One generated file: its directory (`examples_x` or `tests_x`), name
/// and content.
struct Out {
    dir: &'static str,
    name: String,
    text: String,
}

/// Generate the corpus from the BOSL2 checkout at `bosl2` into its
/// `examples_x/` and `tests_x/`, or with `check`, only compare. Files
/// already there that the generator does not make are reported, never
/// deleted (the audit left scratch files beside the corpus).
pub fn command(bosl2: &Path, check: bool) -> Result<u8, String> {
    if !bosl2.join("std.scad").is_file() || !bosl2.join("tests").is_dir() {
        return Err(format!(
            "{} is not a BOSL2 checkout; clone it with\n  git clone https://github.com/BelfrySCAD/BOSL2.git .reference/BOSL2",
            bosl2.display()
        ));
    }
    let mut outs = examples(bosl2)?;
    outs.extend(tests(bosl2)?);
    let (mut written, mut same, mut differ) = (0, 0, Vec::new());
    for o in &outs {
        let dir = bosl2.join(o.dir);
        let path = dir.join(&o.name);
        if fs::read(&path).is_ok_and(|b| b == o.text.as_bytes()) {
            same += 1;
            continue;
        }
        if check {
            differ.push(format!("{}/{}", o.dir, o.name));
            continue;
        }
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        fs::write(&path, &o.text).map_err(|e| format!("{}: {e}", path.display()))?;
        written += 1;
    }
    let made: HashSet<(&str, &str)> = outs.iter().map(|o| (o.dir, o.name.as_str())).collect();
    let mut extra = BTreeSet::new();
    for dir in ["examples_x", "tests_x"] {
        let Ok(rd) = fs::read_dir(bosl2.join(dir)) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !made.contains(&(dir, name.as_str())) {
                extra.insert(format!("{dir}/{name}"));
            }
        }
    }
    let count = |d: &str| outs.iter().filter(|o| o.dir == d).count();
    println!(
        "bosl2-corpus: {} files in examples_x, {} in tests_x (each with meta.json)",
        count("examples_x"),
        count("tests_x"),
    );
    if check {
        println!("{same} current, {} missing or different", differ.len());
        for d in differ.iter().take(20) {
            println!("  {d}");
        }
    } else {
        println!("{written} written, {same} already current");
    }
    if !extra.is_empty() {
        println!(
            "{} files there are not generated (left alone): {}",
            extra.len(),
            extra.iter().cloned().collect::<Vec<_>>().join(" ")
        );
    }
    Ok(u8::from(!differ.is_empty()))
}

/// The documentation examples and `examples/` files.
fn examples(bosl2: &Path) -> Result<Vec<Out>, String> {
    // Exactly one space after `//`: an indented `//   Examples:` is prose
    // in a description (rounding.scad), not a block.
    let header = Regex::new(r"^// Example(?:\(([^)]*)\))?:?\s*(.*)$").unwrap();
    let mut outs = Vec::new();
    let mut meta = Vec::new();
    for path in scad_files(bosl2)? {
        let name = stem(&path);
        let text = read(&path)?;
        let lines: Vec<&str> = text.split('\n').collect();
        let mut includes: Vec<String> = Vec::new();
        if let Some(i) = lines.iter().position(|l| l.starts_with("// Includes:")) {
            for l in lines[i + 1..].iter().take_while(|l| l.starts_with("//   ")) {
                includes.push(l[5..].trim().replace("<BOSL2/", "<../"));
            }
        }
        let me = format!("include <../{name}.scad>");
        if !includes.contains(&me) {
            includes.push(me);
        }
        let mut n = 0;
        let mut i = 0;
        while i < lines.len() {
            let Some(c) = header.captures(lines[i]) else {
                i += 1;
                continue;
            };
            let tags = c.get(1).map_or("", |m| m.as_str());
            let title = c[2].trim();
            let body: Vec<&str> = lines[i + 1..]
                .iter()
                .take_while(|l| l.starts_with("//   "))
                .map(|l| &l[5..])
                .collect();
            i += 1 + body.len();
            if tags.contains("NORENDER") || body.is_empty() {
                continue;
            }
            n += 1;
            let file = format!("{name}__{n:03}.scad");
            let mut src = includes.join("\n");
            for b in body {
                src.push('\n');
                src.push_str(b);
            }
            src.push('\n');
            meta.push((
                file.clone(),
                vec![("tags", tags.to_string()), ("title", title.to_string())],
            ));
            outs.push(Out {
                dir: "examples_x",
                name: file,
                text: relative_includes(&src),
            });
        }
    }
    outs.push(Out {
        dir: "examples_x",
        name: "meta.json".into(),
        text: meta_json(meta.iter().map(|(f, kv)| {
            (
                f.as_str(),
                kv.iter()
                    .map(|(k, v)| (*k, Json::Str(v.as_str())))
                    .collect(),
            )
        })),
    });
    for path in scad_files(&bosl2.join("examples"))? {
        outs.push(Out {
            dir: "examples_x",
            name: format!("ex__{}.scad", stem(&path)),
            text: relative_includes(&read(&path)?),
        });
    }
    Ok(outs)
}

/// The `[[test]]` scripts of `tests/*.scadtest`, as they are (they
/// already include `<../std.scad>`).
fn tests(bosl2: &Path) -> Result<Vec<Out>, String> {
    let tests = read_scadtests(&bosl2.join("tests"))?;
    let mut outs: Vec<Out> = Vec::new();
    let meta = meta_json(tests.iter().map(|(file, t)| {
        let keys = t
            .keys
            .iter()
            .map(|(k, v)| {
                let v = match v.as_str() {
                    "true" => Json::Bool(true),
                    "false" => Json::Bool(false),
                    v => Json::Str(
                        v.strip_prefix('"')
                            .and_then(|v| v.strip_suffix('"'))
                            .unwrap_or(v),
                    ),
                };
                (k.as_str(), v)
            })
            .collect();
        (file.as_str(), keys)
    }));
    for (file, t) in &tests {
        outs.push(Out {
            dir: "tests_x",
            name: file.clone(),
            text: t.script.clone(),
        });
    }
    outs.push(Out {
        dir: "tests_x",
        name: "meta.json".into(),
        text: meta,
    });
    Ok(outs)
}

/// The `.scad` files directly in `dir`, sorted by name.
fn scad_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "scad"))
        .filter(|p| !stem(p).starts_with('.'))
        .collect();
    v.sort();
    Ok(v)
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read(p: &Path) -> Result<String, String> {
    fs::read_to_string(p)
        .map(|s| s.replace("\r\n", "\n"))
        .map_err(|e| format!("{}: {e}", p.display()))
}

fn relative_includes(src: &str) -> String {
    src.replace("include <BOSL2/", "include <../")
        .replace("use <BOSL2/", "use <../")
}

enum Json<'a> {
    Str(&'a str),
    Bool(bool),
}

/// An object of objects, laid out as the audit's `meta.json` files are
/// (Python's `json.dumps(indent=0)`: a line per member, no indentation,
/// non-ASCII escaped), so a regenerated one compares byte for byte.
fn meta_json<'a>(entries: impl Iterator<Item = (&'a str, Vec<(&'a str, Json<'a>)>)>) -> String {
    let mut out = String::from("{");
    let mut first = true;
    for (k, members) in entries {
        out.push_str(if first { "\n" } else { ",\n" });
        first = false;
        json_str(&mut out, k);
        out.push_str(": {");
        for (i, (mk, v)) in members.iter().enumerate() {
            out.push_str(if i == 0 { "\n" } else { ",\n" });
            json_str(&mut out, mk);
            out.push_str(": ");
            match v {
                Json::Str(s) => json_str(&mut out, s),
                Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            }
        }
        out.push_str(if members.is_empty() { "}" } else { "\n}" });
    }
    out.push_str(if first { "}" } else { "\n}" });
    out
}

/// A JSON string as Python writes it with `ensure_ascii`: printable ASCII
/// as is, everything else escaped (UTF-16 units past the BMP).
fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_json_is_laid_out_as_python_writes_it() {
        let m = meta_json(
            [
                (
                    "a.scad",
                    vec![
                        ("tags", Json::Str("2D")),
                        ("title", Json::Str("90\u{b0} \"x\"")),
                    ],
                ),
                ("b.scad", vec![("expect_success", Json::Bool(false))]),
            ]
            .into_iter(),
        );
        assert_eq!(
            m,
            "{\n\"a.scad\": {\n\"tags\": \"2D\",\n\"title\": \"90\\u00b0 \\\"x\\\"\"\n},\n\"b.scad\": {\n\"expect_success\": false\n}\n}"
        );
        assert_eq!(meta_json(std::iter::empty()), "{}");
        let mut s = String::new();
        json_str(&mut s, "\u{1F600}\u{7f}");
        assert_eq!(s, "\"\\ud83d\\ude00\\u007f\"");
    }
}
