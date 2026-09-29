//! `neoscad fmt`, `neoscad test`, `neoscad docs` and `neoscad generate` as
//! commands: exit codes, what they write, and their JSON.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_neoscad");

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nsc-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

fn neoscad(dir: &Path, args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .env("NEOSCAD_NO_SERVER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut i = child.stdin.take().unwrap();
    if let Some(s) = stdin {
        i.write_all(s.as_bytes()).unwrap();
    }
    drop(i);
    child.wait_with_output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn fmt_checks_writes_and_refuses() {
    let d = scratch("fmt");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::create_dir_all(d.join(".hidden")).unwrap();
    std::fs::write(d.join("a.scad"), "cube( 1 );\n").unwrap();
    std::fs::write(d.join("sub/b.scad"), "sphere(2);\n").unwrap();
    std::fs::write(d.join("sub/bad.scad"), "cube(;\n").unwrap();
    std::fs::write(d.join(".hidden/c.scad"), "cube( 3 );\n").unwrap();
    let o = neoscad(&d, &["fmt", "--check"], None);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(text(&o.stdout), "would reformat a.scad\n");
    assert_eq!(
        text(&o.stderr),
        "neoscad fmt: sub/bad.scad: line 1: Parser error: syntax error\n"
    );
    let o = neoscad(&d, &["fmt", "a.scad", "--format", "json"], None);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["counts"]["changed"], 1);
    assert_eq!(
        std::fs::read_to_string(d.join("a.scad")).unwrap(),
        "cube(1);\n"
    );
    assert_eq!(
        std::fs::read_to_string(d.join(".hidden/c.scad")).unwrap(),
        "cube( 3 );\n"
    );
    assert_eq!(
        std::fs::read_to_string(d.join("sub/bad.scad")).unwrap(),
        "cube(;\n"
    );
    // A config file above the file sets the indent.
    std::fs::write(d.join(".neoscad-fmt.toml"), "indent = 2\n").unwrap();
    let o = neoscad(
        &d,
        &["fmt", "--stdin", "sub/x.scad"],
        Some("module m(){cube();}"),
    );
    assert_eq!(text(&o.stdout), "module m() {\n  cube();\n}\n");
    let o = neoscad(&d, &["fmt", "--diff", "sub/b.scad"], None);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(text(&o.stdout), "");
}

#[test]
fn test_runs_the_example_suite() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let o = neoscad(&repo, &["test", "examples/tests"], None);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
    assert!(
        text(&o.stdout).ends_with("test result: ok. 7 passed; 0 failed; 3 files\n"),
        "{}",
        text(&o.stdout)
    );
    let d = scratch("test");
    std::fs::write(d.join("x_test.scad"), "module test_f() assert(false);\n").unwrap();
    let o = neoscad(&d, &["test", "--format", "json"], None);
    assert_eq!(o.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["tests"][0]["failures"][0]["kind"], "error");
}

#[test]
fn docs_answers_and_suggests() {
    let d = scratch("docs");
    let o = neoscad(&d, &["docs", "linear_extrude"], None);
    assert_eq!(o.status.code(), Some(0));
    assert!(text(&o.stdout).starts_with("module linear_extrude("));
    let o = neoscad(&d, &["docs", "translat"], None);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        text(&o.stderr),
        "neoscad docs: no builtin named 'translat'; did you mean 'translate'?\n"
    );
    let o = neoscad(&d, &["docs", "$fn", "--format", "json"], None);
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["entries"][0]["kind"], "variable");
    // Nothing close: the hint names the command line's flag.
    let o = neoscad(&d, &["docs", "thread"], None);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        text(&o.stderr),
        "neoscad docs: no builtin named 'thread'; for your own or a library's code add --in FILE\n"
    );
}

/// The packages run exactly these commands (scripts/release/man-completions.sh,
/// packaging/aur/PKGBUILD); what they print is checked by `generate`'s
/// unit tests.
#[test]
fn generate_prints_the_man_page_and_completions() {
    let d = scratch("generate");
    let o = neoscad(&d, &["generate", "man"], None);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(text(&o.stdout).contains("\n.TH NEOSCAD 1 "));
    for (shell, start) in [
        ("bash", "_neoscad() {"),
        ("zsh", "#compdef neoscad"),
        ("fish", "# Print an optspec"),
    ] {
        let o = neoscad(&d, &["generate", "completions", shell], None);
        assert_eq!(o.status.code(), Some(0), "{shell}: {}", text(&o.stderr));
        let out = text(&o.stdout);
        assert!(out.starts_with(start), "{shell}");
        assert!(out.contains("generate"), "{shell}");
    }
    // An unknown shell is a usage error, with OpenSCAD's status 1.
    let o = neoscad(&d, &["generate", "completions", "tcsh"], None);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("possible values"));
}
