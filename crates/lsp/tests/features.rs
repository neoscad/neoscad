//! Each feature through the server's message API, over in-memory files.

mod common;

use common::{Client, labels, uri};
use serde_json::{Value, json};

const MAIN: &str = "/p/main.scad";

#[test]
fn diagnostics_with_codes_hints_fixes_and_includes() {
    let parts = "module part() cubee(1);\n";
    let main = "include <parts.scad>\ncub(10);\nx = zz + 1;\n";
    let mut c = Client::mem(&[("/p/parts.scad", parts), (MAIN, main)]);
    c.open(MAIN, main);
    assert!(c.server.diagnostics_pending());
    let pubs = c.diagnostics();
    assert!(!c.server.diagnostics_pending());
    let mine = pubs.iter().find(|p| p["uri"] == uri(MAIN)).unwrap();
    assert_eq!(mine["version"], 1);
    let list = mine["diagnostics"].as_array().unwrap();
    let cub = list
        .iter()
        .find(|d| d["code"] == "unknown-module" && d["message"].as_str().unwrap().contains("'cub'"))
        .unwrap();
    assert_eq!(cub["range"]["start"], json!({"line": 1, "character": 0}));
    assert_eq!(cub["severity"], 2);
    assert!(
        cub["message"]
            .as_str()
            .unwrap()
            .contains("did you mean 'cube'?")
    );
    let fix = &cub["data"]["fixes"][0];
    assert_eq!(fix["edits"][0]["newText"], "cube");
    assert_eq!(
        fix["edits"][0]["range"]["end"],
        json!({"line": 1, "character": 3})
    );
    assert!(list.iter().any(|d| d["code"] == "unknown-variable"));
    // `part()` is never called, so nothing about `cubee` comes from the
    // evaluation; make the included file's own problem a syntax-free
    // warning at the top level instead.
    let parts2 = "cubee(1);\n";
    c.open("/p/parts.scad", parts2);
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri(MAIN), "version": 2}, "contentChanges": [{"text": main}]}),
    );
    let pubs = c.diagnostics();
    let inc = pubs
        .iter()
        .find(|p| p["uri"] == uri("/p/parts.scad"))
        .expect("the included file's own list");
    let d = &inc["diagnostics"][0];
    assert_eq!(d["code"], "unknown-module");
    assert_eq!(d["range"]["start"], json!({"line": 0, "character": 0}));
    // ... and a marker on the include line of the document, pointing there.
    let mine = pubs.iter().find(|p| p["uri"] == uri(MAIN)).unwrap();
    let summary = mine["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["relatedInformation"].is_array())
        .unwrap();
    assert_eq!(
        summary["range"]["start"],
        json!({"line": 0, "character": 0})
    );
    assert!(
        summary["message"]
            .as_str()
            .unwrap()
            .starts_with("In parts.scad: ")
    );
    assert_eq!(
        summary["relatedInformation"][0]["location"]["uri"],
        uri("/p/parts.scad")
    );

    // Fixed: the include's list is cleared.
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri("/p/parts.scad"), "version": 2}, "contentChanges": [{"text": "cube(1);\n"}]}),
    );
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri(MAIN), "version": 3}, "contentChanges": [{"text": "include <parts.scad>\ncube(10);\n"}]}),
    );
    let pubs = c.diagnostics();
    for p in &pubs {
        assert_eq!(p["diagnostics"], json!([]), "{p}");
    }
}

#[test]
fn code_actions_apply_the_fix() {
    let main = "sphre(r = 2);\n";
    let mut c = Client::mem(&[(MAIN, main)]);
    c.open(MAIN, main);
    let pubs = c.diagnostics();
    let d = pubs[0]["diagnostics"][0].clone();
    let actions = c.request(
        "textDocument/codeAction",
        json!({"textDocument": {"uri": uri(MAIN)}, "range": d["range"], "context": {"diagnostics": [d]}}),
    );
    assert_eq!(actions[0]["kind"], "quickfix");
    assert_eq!(actions[0]["title"], "Change 'sphre' to 'sphere'");
    assert_eq!(
        actions[0]["edit"]["changes"][uri(MAIN)][0]["newText"],
        "sphere"
    );
    // Without the diagnostics' data, from the last published ones.
    let actions = c.request(
        "textDocument/codeAction",
        json!({"textDocument": {"uri": uri(MAIN)}, "range": d["range"], "context": {"diagnostics": []}}),
    );
    assert_eq!(actions.as_array().unwrap().len(), 1);
}

#[test]
fn hover_builtins_user_code_and_constants() {
    let main = "// Wall thickness.\nwall = 2 * 1.5;\nsize = [wall, wall * 2, 10];\n// A box.\n// With a lid.\nmodule box(s = 1) { cube(s); }\nbox(s = size);\nlet (q = wall) echo(q);\n";
    let mut c = Client::mem(&[(MAIN, main)]);
    c.open(MAIN, main);
    let h = c.at("textDocument/hover", MAIN, main, "cube(s)", 1);
    let v = h["contents"]["value"].as_str().unwrap();
    assert!(v.contains("module cube(size=1, center=false)"), "{v}");
    let h = c.at("textDocument/hover", MAIN, main, "box(s = size)", 0);
    let v = h["contents"]["value"].as_str().unwrap();
    assert!(
        v.contains("module box(s=1)") && v.contains("A box.") && v.contains("With a lid."),
        "{v}"
    );
    assert!(v.contains("main.scad:6"), "{v}");
    let h = c.at("textDocument/hover", MAIN, main, "size);", 0);
    let v = h["contents"]["value"].as_str().unwrap();
    assert!(
        v.contains("size = [wall, wall * 2, 10]") && v.contains("Value: `[3, 6, 10]`"),
        "{v}"
    );
    let h = c.at("textDocument/hover", MAIN, main, "wall * 2", 0);
    let v = h["contents"]["value"].as_str().unwrap();
    assert!(
        v.contains("Wall thickness.") && v.contains("Value: `3`"),
        "{v}"
    );
    // A named argument names the parameter.
    let h = c.at("textDocument/hover", MAIN, main, "s = size", 0);
    let v = h["contents"]["value"].as_str().unwrap();
    assert!(v.contains("Parameter of `module box(s=1)`"), "{v}");
    // `let` in a statement is the builtin module.
    let h = c.at("textDocument/hover", MAIN, main, "let (q", 0);
    assert!(h["contents"]["value"].as_str().unwrap().contains("let"));
    // Nothing on a number.
    assert!(c.at("textDocument/hover", MAIN, main, "1.5", 0).is_null());
}

/// A NeoSCAD extension's builtin says so in its hover and its completion
/// detail, with the same label `neoscad docs` prints; an OpenSCAD
/// builtin's does not.
#[test]
fn extension_builtins_are_labelled() {
    const LABEL: &str = "NeoSCAD extension (`--enable part`); not in OpenSCAD";
    let main = "part(\"lid\") cube(1);\n";
    let mut c = Client::mem(&[(MAIN, main)]);
    c.open(MAIN, main);
    let h = c.at("textDocument/hover", MAIN, main, "part(", 0);
    let v = h["contents"]["value"].as_str().unwrap();
    assert!(v.contains(LABEL), "{v}");
    let h = c.at("textDocument/hover", MAIN, main, "cube(", 0);
    let v = h["contents"]["value"].as_str().unwrap();
    assert!(!v.contains("NeoSCAD extension"), "{v}");

    let text = format!("{main}par");
    let mut c = Client::mem(&[(MAIN, text.as_str())]);
    c.open(MAIN, &text);
    let r = c.request(
        "textDocument/completion",
        json!({"textDocument": {"uri": uri(MAIN)}, "position": {"line": 1, "character": 3}}),
    );
    let items = r["items"].as_array().unwrap();
    let part = items.iter().find(|i| i["label"] == "part").unwrap();
    assert!(part["detail"].as_str().unwrap().ends_with(LABEL), "{part}");
}

#[test]
fn completion_in_statements_expressions_and_calls() {
    let lib = "module lib_mod(a) {}\nfunction lib_fn(x) = x;\nlib_var = 3;\n";
    let inc = "module inc_mod() {}\ninc_var = 1;\nmodule _private() {}\n";
    let main = "use <lib.scad>\ninclude <inc.scad>\nwidth = 3;\nmodule box(size = 1, center = false) { cube(size); }\n";
    let mut c = Client::mem(&[("/p/lib.scad", lib), ("/p/inc.scad", inc), (MAIN, main)]);
    let text = format!("{main}\nb");
    c.open(MAIN, &text);
    let at_end = |t: &str| {
        let lines: Vec<&str> = t.split('\n').collect();
        json!({"line": lines.len() - 1, "character": lines.last().unwrap().len()})
    };
    let r = c.request(
        "textDocument/completion",
        json!({"textDocument": {"uri": uri(MAIN)}, "position": at_end(&text)}),
    );
    let l = labels(&r);
    assert!(l.contains(&"box".to_string()), "{l:?}");
    // Statement position: modules, not functions.
    assert!(!l.iter().any(|x| x == "lib_fn"));
    // The edit replaces the typed "b".
    assert_eq!(r["itemDefaults"]["editRange"]["start"]["character"], 0);

    let text = format!("{main}\n");
    let cases: &[(&str, &[&str], &[&str])] = &[
        // Modules from the use'd library and the include, but not private
        // library helpers, and not the library's variables.
        ("l", &["lib_mod"], &["lib_var", "lib_fn"]),
        ("i", &["inc_mod", "intersection"], &[]),
        ("_", &["_private"], &[]),
        // Expressions: functions and variables (the included file's too).
        ("x = l", &["lib_fn", "len"], &["lib_mod"]),
        ("x = inc", &["inc_var"], &[]),
        ("x = wi", &["width"], &[]),
        // Named parameters inside a call, then the scope's names.
        ("box(ce", &["center="], &[]),
        ("box(1, s", &["size=", "sin"], &[]),
        ("cylinder(h = 1, r", &["r=", "r1=", "r2="], &[]),
        // After `name =` only values.
        ("box(center = t", &["true"], &["size="]),
        // `$` variables.
        ("sphere(1, $f", &["$fn", "$fa", "$fs"], &[]),
    ];
    for (typed, want, not) in cases {
        let t = format!("{text}{typed}");
        c.notify(
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri(MAIN), "version": 9}, "contentChanges": [{"text": t}]}),
        );
        let r = c.request(
            "textDocument/completion",
            json!({"textDocument": {"uri": uri(MAIN)}, "position": at_end(&t)}),
        );
        let l = labels(&r);
        for w in *want {
            assert!(
                l.iter().any(|x| x == w),
                "{typed:?}: {w} missing from {l:?}"
            );
        }
        for n in *not {
            assert!(!l.iter().any(|x| x == n), "{typed:?}: {n} in {l:?}");
        }
    }
    // A snippet for a builtin at a statement.
    let t = format!("{text}trans");
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri(MAIN), "version": 10}, "contentChanges": [{"text": t}]}),
    );
    let r = c.request(
        "textDocument/completion",
        json!({"textDocument": {"uri": uri(MAIN)}, "position": at_end(&t)}),
    );
    let item = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == "translate")
        .unwrap()
        .clone();
    assert_eq!(item["insertTextFormat"], 2);
    assert!(
        item["insertText"]
            .as_str()
            .unwrap()
            .starts_with("translate([${1:0}")
    );
    // Nothing inside comments or strings.
    let t = format!("{text}// b");
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri(MAIN), "version": 11}, "contentChanges": [{"text": t}]}),
    );
    let r = c.request(
        "textDocument/completion",
        json!({"textDocument": {"uri": uri(MAIN)}, "position": at_end(&t)}),
    );
    assert!(labels(&r).is_empty());
}

#[test]
fn local_scopes_in_completion_and_definition() {
    let main = "module m(p) {\n  q = p;\n  \n}\nfor (i = [0:2]) translate([i, 0, 0]) cube(1);\nf = function(z) z + 1;\n";
    let mut c = Client::mem(&[(MAIN, main)]);
    c.open(MAIN, main);
    // Inside the module: its parameter and assignment.
    let r = c.request(
        "textDocument/completion",
        json!({"textDocument": {"uri": uri(MAIN)}, "position": {"line": 2, "character": 2}}),
    );
    let l = labels(&r);
    assert!(l.contains(&"p".into()) && l.contains(&"q".into()), "{l:?}");
    // Outside it, not.
    let r = c.request(
        "textDocument/completion",
        json!({"textDocument": {"uri": uri(MAIN)}, "position": {"line": 6, "character": 0}}),
    );
    let l = labels(&r);
    assert!(
        !l.contains(&"p".into()) && !l.contains(&"q".into()),
        "{l:?}"
    );
    // The loop variable from its use.
    let d = c.at("textDocument/definition", MAIN, main, "i, 0, 0", 0);
    assert_eq!(d["range"]["start"], json!({"line": 4, "character": 5}));
    let d = c.at("textDocument/definition", MAIN, main, "z + 1", 0);
    assert_eq!(d["range"]["start"], json!({"line": 5, "character": 13}));
}

#[test]
fn signature_help_for_builtins_and_user_modules() {
    let main = "module box(size, center = false) {}\nbox(1, \ncylinder(h = 2, \nx = max(";
    let mut c = Client::mem(&[(MAIN, main)]);
    c.open(MAIN, main);
    let s = c.at("textDocument/signatureHelp", MAIN, main, "box(1, ", 7);
    assert_eq!(s["signatures"][0]["label"], "box(size, center=false)");
    assert_eq!(s["activeParameter"], 1);
    let p = &s["signatures"][0]["parameters"][1]["label"];
    assert_eq!(p, &json!([10, 22]));
    let s = c.at(
        "textDocument/signatureHelp",
        MAIN,
        main,
        "cylinder(h = 2, ",
        16,
    );
    let labels: Vec<&str> = s["signatures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels.len(), 2, "{labels:?}");
    assert!(labels[0].starts_with("cylinder(h=1"));
    let s = c.at("textDocument/signatureHelp", MAIN, main, "max(", 4);
    assert!(
        s["signatures"][0]["label"]
            .as_str()
            .unwrap()
            .starts_with("max(")
    );
    assert!(
        c.at("textDocument/signatureHelp", MAIN, main, "module box(", 11)
            .is_null()
    );
}

#[test]
fn definition_and_references_across_includes_and_uses() {
    let parts = "module bolt(d) cylinder(d = d, h = 10);\nhole = 3;\nbolt(hole);\n";
    let lib = "module nut() {}\n";
    let main = "include <parts.scad>\nuse <lib/nuts.scad>\nbolt(hole);\nbolt(d = hole);\nnut();\n";
    let mut c = Client::mem(&[
        ("/p/parts.scad", parts),
        ("/lib/lib/nuts.scad", lib),
        (MAIN, main),
    ]);
    c.open(MAIN, main);
    let d = c.at("textDocument/definition", MAIN, main, "bolt(hole)", 0);
    assert_eq!(d["uri"], uri("/p/parts.scad"));
    assert_eq!(d["range"]["start"], json!({"line": 0, "character": 7}));
    let d = c.at("textDocument/definition", MAIN, main, "nut()", 0);
    assert_eq!(d["uri"], uri("/lib/lib/nuts.scad"));
    // The directive itself goes to its file (found through the library path).
    let d = c.at("textDocument/definition", MAIN, main, "lib/nuts", 0);
    assert_eq!(d["uri"], uri("/lib/lib/nuts.scad"));
    // A named argument goes to the parameter.
    let d = c.at("textDocument/definition", MAIN, main, "d = hole", 0);
    assert_eq!(d["range"]["start"], json!({"line": 0, "character": 12}));
    let d = c.at("textDocument/definition", MAIN, main, "include", 0);
    assert_eq!(d["uri"], uri("/p/parts.scad"));
    // Punctuation has no definition.
    assert!(
        c.at("textDocument/definition", MAIN, main, "nut();", 4)
            .is_null()
    );
    let refs = c.request(
        "textDocument/references",
        json!({"textDocument": {"uri": uri(MAIN)}, "position": Client::pos(main, "hole);", 0), "context": {"includeDeclaration": true}}),
    );
    let mut got: Vec<(String, u64)> = refs
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["uri"]
                    .as_str()
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_string(),
                l["range"]["start"]["line"].as_u64().unwrap(),
            )
        })
        .collect();
    got.sort();
    assert_eq!(
        got,
        [
            ("main.scad".into(), 2),
            ("main.scad".into(), 3),
            ("parts.scad".into(), 1),
            ("parts.scad".into(), 2)
        ]
    );
}

#[test]
fn formatting_whole_and_range() {
    let main = "module m(){cube(1);}\nx=[1,2,3];\ny  =  2;\n";
    let mut c = Client::mem(&[(MAIN, main)]);
    c.open(MAIN, main);
    let edits = c.request("textDocument/formatting", json!({"textDocument": {"uri": uri(MAIN)}, "options": {"tabSize": 4, "insertSpaces": true}}));
    let edits = edits.as_array().unwrap();
    assert!(!edits.is_empty());
    let applied = apply(main, edits);
    assert_eq!(
        applied,
        "module m() {\n    cube(1);\n}\nx = [1, 2, 3];\ny = 2;\n"
    );
    // Only the third line.
    let edits = c.request(
        "textDocument/rangeFormatting",
        json!({"textDocument": {"uri": uri(MAIN)}, "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 3}}, "options": {}}),
    );
    let applied = apply(main, edits.as_array().unwrap());
    assert_eq!(applied, "module m(){cube(1);}\nx=[1,2,3];\ny = 2;\n");
    // A syntax error: refused with a reason.
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri(MAIN), "version": 2}, "contentChanges": [{"text": "cube(;\n"}]}),
    );
    let r = c.call(
        "textDocument/formatting",
        json!({"textDocument": {"uri": uri(MAIN)}, "options": {}}),
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("syntax error"),
        "{r}"
    );
}

/// Apply LSP edits (line/character, ASCII) to `text`.
fn apply(text: &str, edits: &[Value]) -> String {
    let offset = |p: &Value| {
        let (l, ch) = (
            p["line"].as_u64().unwrap() as usize,
            p["character"].as_u64().unwrap() as usize,
        );
        text.split_inclusive('\n')
            .take(l)
            .map(str::len)
            .sum::<usize>()
            + ch
    };
    let mut spans: Vec<(usize, usize, String)> = edits
        .iter()
        .map(|e| {
            (
                offset(&e["range"]["start"]),
                offset(&e["range"]["end"]),
                e["newText"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    spans.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut out = text.to_string();
    for (a, b, t) in spans {
        out.replace_range(a..b, &t);
    }
    out
}

#[test]
fn outline_and_folding() {
    let main = "// a\n// b\nwidth = 3;\nmodule box(s) {\n    inner = s;\n    module nested() {}\n}\nfunction f(x) =\n    x + 1;\npts = [\n    1,\n    2\n];\n";
    let mut c = Client::mem(&[(MAIN, main)]);
    c.open(MAIN, main);
    let s = c.request(
        "textDocument/documentSymbol",
        json!({"textDocument": {"uri": uri(MAIN)}}),
    );
    let names: Vec<&str> = s
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["width", "box", "f", "pts"]);
    let bx = &s[1];
    assert_eq!(bx["kind"], 2);
    assert_eq!(bx["detail"], "(s)");
    let kids: Vec<&str> = bx["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert_eq!(kids, ["inner", "nested"]);
    let f = c.request(
        "textDocument/foldingRange",
        json!({"textDocument": {"uri": uri(MAIN)}}),
    );
    let got: Vec<(u64, u64, Option<&str>)> = f
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["startLine"].as_u64().unwrap(),
                r["endLine"].as_u64().unwrap(),
                r["kind"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (0, 1, Some("comment")),
            (3, 5, None),
            (7, 8, None),
            (9, 11, None)
        ]
    );
}

#[test]
fn rename_when_safe() {
    let parts = "module shared() {}\nuses_main = main_var;\n";
    let main = "include <parts.scad>\nmain_var = 1;\nr = 2;\nmodule ring(r = 1) { cylinder(r = r); }\nring(r = r);\necho(r);\nshared();\n";
    let mut c = Client::mem(&[("/p/parts.scad", parts), (MAIN, main)]);
    c.open(MAIN, main);
    let rename = |c: &mut Client, needle: &str, delta: usize, new: &str| {
        c.call(
            "textDocument/rename",
            json!({"textDocument": {"uri": uri(MAIN)}, "position": Client::pos(main, needle, delta), "newName": new}),
        )
    };
    // The top-level `r` (not the parameter of the same name).
    let r = rename(&mut c, "echo(r)", 5, "radius");
    let edits = r["result"]["changes"][uri(MAIN)]
        .as_array()
        .unwrap()
        .clone();
    let got = apply(main, &edits);
    assert_eq!(
        got,
        main.replace("r = 2;", "radius = 2;")
            .replace("ring(r = r);", "ring(r = radius);")
            .replace("echo(r);", "echo(radius);")
    );
    // The parameter: its uses in the body and the named argument.
    let r = rename(&mut c, "ring(r = 1)", 5, "rad");
    let got = apply(main, r["result"]["changes"][uri(MAIN)].as_array().unwrap());
    assert!(
        got.contains("module ring(rad = 1) { cylinder(r = rad); }")
            && got.contains("ring(rad = r);"),
        "{got}"
    );
    let p = c.request(
        "textDocument/prepareRename",
        json!({"textDocument": {"uri": uri(MAIN)}, "position": Client::pos(main, "ring(r = r)", 0)}),
    );
    assert_eq!(p["placeholder"], "ring");
    // Refused: used from an included file, defined in one, builtin,
    // taken, not a name.
    for (needle, new, why) in [
        ("main_var", "mv", "used from the included file"),
        ("shared()", "sh", "defined in another file"),
        ("cylinder", "cyl", "builtins"),
        ("r = 2", "main_var", "already defined"),
        ("ring(r = r)", "cube", "builtin"),
        ("r = 2", "2x", "not a valid"),
    ] {
        let r = rename(&mut c, needle, 0, new);
        let m = r["error"]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("{needle} -> {new}: {r}"));
        assert!(m.contains(why), "{needle} -> {new}: {m}");
    }
}

#[test]
fn utf16_positions_past_non_ascii_text() {
    let main = "s = \"héllo 😀\"; cube(s);\n";
    let mut c = Client::mem(&[(MAIN, main)]);
    c.open(MAIN, main);
    // `cube` starts at UTF-16 column 16 ("😀" is two units, "é" one).
    let h = c.request(
        "textDocument/hover",
        json!({"textDocument": {"uri": uri(MAIN)}, "position": {"line": 0, "character": 17}}),
    );
    assert_eq!(h["range"]["start"]["character"], 16);
    assert!(
        h["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("module cube")
    );
    // Incremental edits count in UTF-16 too: replace "cube" by "sphere".
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri(MAIN), "version": 2}, "contentChanges": [
            {"range": {"start": {"line": 0, "character": 16}, "end": {"line": 0, "character": 20}}, "text": "sphere"}
        ]}),
    );
    let h = c.request(
        "textDocument/hover",
        json!({"textDocument": {"uri": uri(MAIN)}, "position": {"line": 0, "character": 18}}),
    );
    assert!(
        h["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("module sphere"),
        "{h}"
    );
}

#[test]
fn protocol_errors_and_lifecycle() {
    let mut c = Client::mem(&[]);
    let r = c.call(
        "textDocument/unknownThing",
        json!({"textDocument": {"uri": uri(MAIN)}}),
    );
    assert!(r["error"].is_object());
    let r = c.call("workspace/whatever", json!({}));
    assert_eq!(r["error"]["code"], -32601);
    let out = c.server.handle(&c.session, "{not json");
    assert!(out[0].contains("-32700"));
    assert_eq!(c.request("shutdown", Value::Null), Value::Null);
    let r = c.call(
        "textDocument/hover",
        json!({"textDocument": {"uri": uri(MAIN)}, "position": {"line": 0, "character": 0}}),
    );
    assert_eq!(r["error"]["code"], -32600);
    c.notify("exit", Value::Null);
    assert!(c.server.exited());
    assert_eq!(c.server.exit_code(), 0);
}

#[test]
fn documents_are_synced_into_the_session() {
    let mut c = Client::mem(&[]);
    c.open(MAIN, "cube(1);\n");
    let docs = c.session.documents();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].path.to_str(), Some(MAIN));
    c.notify(
        "textDocument/didClose",
        json!({"textDocument": {"uri": uri(MAIN)}}),
    );
    assert!(c.session.documents().is_empty());
}

#[test]
fn answers_use_the_clients_spelling_of_its_uris() {
    // A client may leave characters unescaped that we would escape.
    let spelled = "file:///p/part(1).scad";
    let text = "w = 1;\ncube(w);\nfoo();\n";
    let mut c = Client::mem(&[]);
    c.notify(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": spelled, "languageId": "openscad", "version": 1, "text": text}}),
    );
    let d = c.request(
        "textDocument/definition",
        json!({"textDocument": {"uri": spelled}, "position": {"line": 1, "character": 5}}),
    );
    assert_eq!(d["uri"], spelled);
    let pubs = c.diagnostics();
    assert_eq!(pubs[0]["uri"], spelled);
}

/// What the app does after each pause in typing: one render of the text,
/// whose diagnostics (the geometry stage's among them) the server
/// publishes for the client's version of that text.
#[test]
fn a_hosts_run_publishes_for_the_version_with_its_text() {
    use std::sync::Arc;
    let v1 = "cub(1);\n";
    let v2 = "union() { cube(1); square(1); }\n";
    let mut c = Client::host_run(&[(MAIN, v1)]);
    c.open(MAIN, v1);
    // The server evaluates nothing itself.
    assert!(!c.server.diagnostics_pending());
    assert!(c.diagnostics().is_empty());
    let run = |c: &Client, text: &str| {
        let mut r = session::Run::new(MAIN);
        r.text = Some(Arc::from(text.as_bytes()));
        let out = c
            .session
            .render(&r, session::Mode::Render, &render::ColorScheme::cornfield())
            .unwrap();
        out.log.diagnostics_json()
    };
    let parse = |out: Vec<String>| -> Vec<Value> {
        out.iter()
            .map(|m| serde_json::from_str::<Value>(m).unwrap()["params"].clone())
            .collect()
    };
    // The client's version has the run's text: published at once.
    let d = run(&c, v1);
    let pubs = parse(
        c.server
            .supply(&c.session, MAIN.as_ref(), Arc::from(v1.as_bytes()), d),
    );
    assert_eq!(pubs.len(), 1);
    assert_eq!(pubs[0]["version"], 1);
    assert_eq!(pubs[0]["diagnostics"][0]["code"], "unknown-module");
    // A run of text the client has not sent yet waits for it...
    let d = run(&c, v2);
    let pubs = c
        .server
        .supply(&c.session, MAIN.as_ref(), Arc::from(v2.as_bytes()), d);
    assert!(pubs.is_empty());
    // ... and goes out with the change that brings it: the geometry
    // stage's warning, located at the 2D child.
    let pubs = c.notify_all(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri(MAIN), "version": 2}, "contentChanges": [{"text": v2}]}),
    );
    assert_eq!(pubs.len(), 1, "{pubs:?}");
    let p = &pubs[0]["params"];
    assert_eq!(p["version"], 2);
    let w = &p["diagnostics"][0];
    assert!(
        w["message"].as_str().unwrap().contains("Mixing 2D and 3D"),
        "{w}"
    );
    assert_eq!(w["range"]["start"], json!({"line": 0, "character": 19}));
    // A version no run has read yet publishes nothing.
    let pubs = c.notify_all(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri(MAIN), "version": 3}, "contentChanges": [{"text": "cube(2);\n"}]}),
    );
    assert!(pubs.is_empty());
}
