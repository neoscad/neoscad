//! `neoscad test`: the example suite in `examples/tests` passes, results
//! do not depend on the thread count, and failures say what failed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lang::loader::{LibraryPath, StdFs};
use lang::vfs::MemFs;
use serde_json::Value;
use session::modeltest::TestRequest;
use session::{Config, Session};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn disk_session() -> Session {
    let mut cfg = Config::new(Arc::new(StdFs), LibraryPath(Vec::new()));
    cfg.work_dir = repo();
    Session::new(cfg)
}

fn mem_session(files: &[(&str, &str)]) -> Session {
    let fs = Arc::new(MemFs::new());
    for (p, t) in files {
        fs.insert(format!("/doc/{p}"), t.as_bytes().to_vec());
    }
    let mut cfg = Config::new(fs, LibraryPath(Vec::new()));
    cfg.work_dir = PathBuf::from("/doc");
    Session::new(cfg)
}

/// The report without its timings, which are the only part that may
/// differ between runs.
fn without_timings(mut v: Value) -> Value {
    fn strip(v: &mut Value) {
        match v {
            Value::Object(o) => {
                o.remove("timings_ms");
                o.values_mut().for_each(strip);
            }
            Value::Array(a) => a.iter_mut().for_each(strip),
            _ => {}
        }
    }
    strip(&mut v);
    v
}

fn run(s: &Session, paths: &[&str], jobs: usize) -> Value {
    let req = TestRequest {
        paths: paths.iter().map(|p| p.to_string()).collect(),
        jobs,
        ..TestRequest::default()
    };
    s.test(&req).unwrap().json
}

#[test]
fn example_suite_passes() {
    let s = disk_session();
    let v = run(&s, &["examples/tests"], 4);
    assert_eq!(v["exit_code"], 0, "{}", session::modeltest::text(&v));
    assert_eq!(v["counts"]["tests"], 7, "{v}");
    assert_eq!(v["counts"]["files"], 3);
}

#[test]
fn output_is_the_same_at_any_thread_count() {
    let s = disk_session();
    let one = without_timings(run(&s, &["examples/tests"], 1));
    let many = without_timings(run(&disk_session(), &["examples/tests"], 8));
    assert_eq!(one, many);
    // And warm: the same session again.
    let again = without_timings(run(&s, &["examples/tests"], 3));
    assert_eq!(one, again);
}

#[test]
fn failures_name_what_failed() {
    let s = mem_session(&[(
        "a_test.scad",
        "cube(100); // not part of any test\n\
         // @expect volume 8±0.001\n\
         // @expect components 1\n\
         module test_cube() cube(2);\n\
         // @expect volume 9\n\
         module test_wrong() cube(2);\n\
         module test_assert() assert(1 + 1 == 3, \"arithmetic\");\n\
         // @expect volum 3\n\
         module test_typo() cube(1);\n\
         // @expect parts a,b\n\
         module test_parts() { part(\"a\") cube(1); part(\"b\") translate([5, 0, 0]) cube(1); }\n",
    )]);
    let v = run(&s, &[], 2);
    let t = |name: &str| -> Value {
        v["tests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(t("test_cube")["ok"], true, "{v}");
    assert_eq!(t("test_parts")["ok"], true, "{v}");
    let wrong = t("test_wrong");
    assert_eq!(wrong["ok"], false);
    assert_eq!(wrong["failures"][0]["kind"], "expect");
    assert_eq!(wrong["failures"][0]["expected"]["value"], 9.0);
    assert_eq!(wrong["failures"][0]["actual"], 8.0);
    assert_eq!(
        wrong["failures"][0]["message"],
        "@expect volume 9: expected 9, got 8"
    );
    let a = t("test_assert");
    assert_eq!(a["failures"][0]["kind"], "error");
    assert_eq!(
        a["failures"][0]["message"],
        "ERROR: Assertion '((1 + 1) == 3)' failed: \"arithmetic\" in file a_test.scad, line 7"
    );
    assert_eq!(t("test_typo")["failures"][0]["kind"], "expectation-syntax");
    assert_eq!(v["counts"]["passed"], 2);
    assert_eq!(v["exit_code"], 1);
    let filtered = s
        .test(&TestRequest {
            filter: Some("cube".into()),
            ..TestRequest::default()
        })
        .unwrap();
    assert_eq!(filtered.json["counts"]["tests"], 1);
    assert_eq!(filtered.exit_code, 0);
}

#[test]
fn files_that_do_not_parse_fail() {
    let s = mem_session(&[("test_x.scad", "module test_a() cube(;\n")]);
    let v = run(&s, &[], 1);
    assert_eq!(v["exit_code"], 1);
    assert_eq!(v["tests"][0]["failures"][0]["kind"], "file");
}
