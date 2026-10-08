//! Geometry queries in the language server (`--enable query`;
//! `docs/language-extensions.md`, section 5): completion offers `anchor`
//! and the `child_*` functions only with the extension on, and the
//! `child_*` ones only inside module bodies, where there are children to
//! ask about.

mod common;

use common::{Client, labels};

const MAIN: &str = "/w/main.scad";

const MODEL: &str = "module plate() {
  b = child_bounds(0);
  anchor(\"a\", [0, 0]);
  children(0);
}
c = child_measure(0);
anchor(\"top\", [0, 0, 1]);
plate() cube(1);
";

const CHILD: [&str; 4] = [
    "child_anchors",
    "child_bounds",
    "child_distance",
    "child_measure",
];

/// The labels offered with the cursor `delta` bytes into `needle`.
fn offered(c: &mut Client, needle: &str, delta: usize) -> Vec<String> {
    labels(&c.at("textDocument/completion", MAIN, MODEL, needle, delta))
}

#[test]
fn queries_are_offered_only_with_the_extension_and_children_only_in_modules() {
    let mut c = Client::mem_config(&[(MAIN, MODEL)], |c: &mut session::Config| {
        c.extensions = session::Extensions::NONE.with(session::Extension::Query);
    });
    c.open(MAIN, MODEL);
    // In a module body: every `child_*` query, and `anchor` as a statement.
    let l = offered(&mut c, "child_bounds(0)", 6);
    for name in CHILD {
        assert!(l.contains(&name.to_string()), "{name} in {l:?}");
    }
    let l = offered(&mut c, "anchor(\"a\"", 3);
    assert!(l.contains(&"anchor".to_string()), "{l:?}");
    // At the top level: no children to ask about, but `anchor` still
    // names a point there.
    let l = offered(&mut c, "child_measure(0)", 6);
    for name in CHILD {
        assert!(!l.contains(&name.to_string()), "{name} in {l:?}");
    }
    let l = offered(&mut c, "anchor(\"top\"", 3);
    assert!(l.contains(&"anchor".to_string()), "{l:?}");
}

#[test]
fn queries_are_not_offered_without_the_extension() {
    let mut c = Client::mem_config(&[(MAIN, MODEL)], |_: &mut session::Config| {});
    c.open(MAIN, MODEL);
    for (needle, delta) in [
        ("child_bounds(0)", 6),
        ("child_measure(0)", 6),
        ("anchor(\"a\"", 3),
        ("anchor(\"top\"", 3),
    ] {
        let l = offered(&mut c, needle, delta);
        for name in CHILD.iter().chain(&["anchor"]) {
            assert!(!l.contains(&name.to_string()), "{name} at {needle}: {l:?}");
        }
    }
}
