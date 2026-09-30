//! `neoscad bench` end to end on a tiny kit: the result it writes, the
//! official-release check, and that the payload names no person, host or
//! path. The kit is two cubes, so a run takes well under a second.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use bench_core::official::Check;
use bench_core::result::{BenchResult, Source};

const BIN: &str = env!("CARGO_BIN_EXE_neoscad");

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nsc-bench-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// A kit with a cold start and one model that imports a generated STL.
fn tiny_kit(dir: &Path) -> PathBuf {
    let kit = dir.join("kit");
    std::fs::create_dir_all(kit.join("models")).unwrap();
    std::fs::create_dir_all(kit.join("inputs")).unwrap();
    std::fs::create_dir_all(kit.join("libraries")).unwrap();
    std::fs::write(kit.join("models/cold_start.scad"), "cube(1);\n").unwrap();
    std::fs::write(
        kit.join("models/tiny.scad"),
        "difference() { import(\"tiny.stl\"); cube(1); }\n",
    )
    .unwrap();
    std::fs::write(kit.join("models/slow.scad"), "cube(3);\n").unwrap();
    std::fs::write(kit.join("inputs/tiny.stl.scad"), "cube(2);\n").unwrap();
    let kit_json = serde_json::json!({
        "kit_schema": 1,
        "version": "0.0.0-test",
        "sources": {"neoscad_commit": "0".repeat(40), "bosl2_commit": "unpinned", "openscad_commit": "unpinned"},
        "runs": 3,
        "single_run_over_s": 60,
        "timeout_s": 60,
        "quick": {"runs": 1, "cold_start_runs": 2},
        "library_path": "libraries",
        "cold_start": {"description": "cube", "file": "models/cold_start.scad", "runs": 20},
        "models": {
            "tiny": {"description": "imported cube", "file": "models/tiny.scad",
                     "inputs": {"tiny.stl": "inputs/tiny.stl.scad"}, "quick": true},
            "slow": {"description": "not quick", "file": "models/slow.scad", "quick": false}
        }
    });
    std::fs::write(kit.join("kit.json"), kit_json.to_string()).unwrap();
    kit
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("bench")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// This binary's line in a release-sums file, or another hash.
fn sums_file(dir: &Path, official: bool) -> PathBuf {
    let sha = bench_core::sha256_file(Path::new(BIN)).unwrap();
    let hash = if official { sha } else { "0".repeat(64) };
    // The triple as build.rs saw it is what the binary looks up; the test
    // lists every target so it does not have to know which one it is.
    let mut text = String::new();
    for t in [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-gnu",
        "aarch64-pc-windows-msvc",
        "x86_64-pc-windows-msvc",
    ] {
        let exe = if t.contains("windows") {
            "neoscad.exe"
        } else {
            "neoscad"
        };
        text.push_str(&format!("{hash}  {t}/{exe}\n"));
    }
    let p = dir.join("sums.txt");
    std::fs::write(&p, text).unwrap();
    p
}

#[test]
fn quick_run_writes_a_schema_1_result() {
    let dir = scratch("quick");
    let kit = tiny_kit(&dir);
    let sums = sums_file(&dir, true);
    let out = dir.join("result.json");
    let o = run(
        &dir,
        &[
            "--quick",
            "--kit",
            kit.to_str().unwrap(),
            "--no-openscad",
            "--json",
            out.to_str().unwrap(),
            "--release-sums",
            sums.to_str().unwrap(),
        ],
    );
    let stdout = String::from_utf8_lossy(&o.stdout);
    assert!(
        o.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(stdout.contains("Summary"), "{stdout}");
    assert!(stdout.contains("tiny"), "{stdout}");

    let text = std::fs::read_to_string(&out).unwrap();
    let r: BenchResult = serde_json::from_str(&text).unwrap();
    assert_eq!(r.schema, 1);
    assert_eq!(r.source, Source::User);
    assert!(r.neoscad.official);
    assert_eq!(r.neoscad.official_check, Check::Matched);
    assert_eq!(
        r.neoscad.sha256,
        bench_core::sha256_file(Path::new(BIN)).unwrap()
    );
    assert!(r.method.quick);
    assert_eq!((r.method.runs, r.method.cold_start_runs), (1, 2));
    assert!(r.openscad.is_none());
    // Only the quick model ran, and it succeeded.
    assert_eq!(r.models.keys().collect::<Vec<_>>(), ["tiny"]);
    let tiny = &r.models["tiny"];
    assert!(tiny.neoscad.rc.ok(), "{tiny:?}");
    assert!(tiny.neoscad.best_s.is_some());
    assert!(tiny.openscad.is_none());
    assert_eq!(r.cold_start.neoscad.runs_s.len(), 2);
    assert_eq!(r.kit.archive_sha256, None);
    assert_eq!(r.kit.content_sha256.len(), 64);
    assert_eq!(r.method.version, bench_core::timing::METHOD_VERSION);

    // Nothing that identifies the user, the host or the files.
    let hostname = Command::new("hostname")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    let temp = std::env::temp_dir().to_string_lossy().into_owned();
    for (what, s) in [
        ("hostname", hostname.as_str()),
        ("hostname (short)", hostname.split('.').next().unwrap_or("")),
        ("user name", user.as_str()),
        ("home", home.as_str()),
        ("temp dir", temp.trim_end_matches(['/', '\\'])),
        ("kit path", kit.to_str().unwrap()),
        ("binary path", BIN),
    ] {
        if s.len() >= 3 {
            assert!(
                !text.contains(s),
                "the result contains the {what} '{s}':\n{text}"
            );
        }
    }
    // No field of the result is a path.
    assert!(!text.contains("/Users/") && !text.contains("/home/") && !text.contains(":\\\\"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn submit_refuses_a_binary_that_is_not_the_release() {
    let dir = scratch("unofficial");
    let kit = tiny_kit(&dir);
    let sums = sums_file(&dir, false);
    let out = dir.join("result.json");
    let o = run(
        &dir,
        &[
            "--quick",
            "--kit",
            kit.to_str().unwrap(),
            "--no-openscad",
            "--json",
            out.to_str().unwrap(),
            "--release-sums",
            sums.to_str().unwrap(),
            "--submit",
        ],
    );
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("official release binaries only"),
        "{stderr}"
    );
    // Refused before anything ran.
    assert!(!out.exists());

    // Without --submit it runs, and says it is not official.
    let o = run(
        &dir,
        &[
            "--quick",
            "--kit",
            kit.to_str().unwrap(),
            "--no-openscad",
            "--json",
            out.to_str().unwrap(),
            "--release-sums",
            sums.to_str().unwrap(),
        ],
    );
    assert!(o.status.success());
    let r: BenchResult = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert!(!r.neoscad.official);
    assert_eq!(r.neoscad.official_check, Check::Mismatch);
    let _ = std::fs::remove_dir_all(&dir);
}
