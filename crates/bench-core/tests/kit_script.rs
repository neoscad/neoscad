//! `scripts/release/bench-kit.sh`: the same inputs give the same archive
//! bytes, and the archive is a kit `neoscad bench` can read, holding every
//! model of `conformance/bench.json`.
//!
//! BOSL2 and OpenSCAD's examples are stand-ins made here (the files
//! bench.json names, with stub contents), so the test needs neither
//! checkout nor the network; `--unpinned` lets the script take them.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bench-kit-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().is_ok()
}

/// Stand-in BOSL2 and OpenSCAD trees with every file bench.json names.
fn stand_ins(dir: &Path) -> (PathBuf, PathBuf) {
    let bosl2 = dir.join("BOSL2");
    let openscad = dir.join("openscad");
    std::fs::create_dir_all(&bosl2).unwrap();
    std::fs::create_dir_all(openscad.join("examples")).unwrap();
    std::fs::write(bosl2.join("std.scad"), "// std\n").unwrap();
    std::fs::write(bosl2.join("LICENSE"), "BSD 2-Clause License\n").unwrap();
    std::fs::write(openscad.join("examples/COPYING-CC0.txt"), "CC0\n").unwrap();
    let cfg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo().join("conformance/bench.json")).unwrap(),
    )
    .unwrap();
    for m in cfg["models"].as_object().unwrap().values() {
        if let Some(f) = m["file"].as_str() {
            let p = PathBuf::from(
                f.replace("{REF}", &openscad.to_string_lossy())
                    .replace("{BOSL2}", &bosl2.to_string_lossy()),
            );
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "cube(1);\n").unwrap();
        }
    }
    (bosl2, openscad)
}

fn build(out: &Path, bosl2: &Path, openscad: &Path) -> Vec<u8> {
    let o = Command::new("bash")
        .arg(repo().join("scripts/release/bench-kit.sh"))
        .args(["--unpinned", "--out"])
        .arg(out)
        .arg("--bosl2")
        .arg(bosl2)
        .arg("--openscad")
        .arg(openscad)
        .env("SOURCE_DATE_EPOCH", "1790000000")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let version = std::fs::read_to_string(repo().join("Cargo.toml"))
        .unwrap()
        .lines()
        .find_map(|l| {
            l.strip_prefix("version = \"")
                .map(|v| v.trim_end_matches('"').to_string())
        })
        .unwrap();
    let name = bench_core::kit::asset_name(&version);
    let bytes = std::fs::read(out.join(&name)).unwrap();
    let sums = std::fs::read_to_string(out.join(format!("{name}.sha256"))).unwrap();
    assert_eq!(
        sums.split_whitespace().next().unwrap(),
        bench_core::sha256_hex(&bytes)
    );
    bytes
}

#[test]
fn kit_script_is_reproducible_and_readable() {
    if !have("jq") || !have("bash") {
        eprintln!("skipped: needs bash and jq");
        return;
    }
    let dir = scratch("repro");
    let (bosl2, openscad) = stand_ins(&dir);
    let a = build(&dir.join("a"), &bosl2, &openscad);
    // Touch the inputs: new mtimes must not change the archive.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(bosl2.join("std.scad"), "// std\n").unwrap();
    let b = build(&dir.join("b"), &bosl2, &openscad);
    assert!(a == b, "two builds of the same inputs differ");

    let unpacked = dir.join("unpacked");
    std::fs::create_dir_all(&unpacked).unwrap();
    bench_core::kit::extract_tar_gz(&a, &unpacked).unwrap();
    let kit = bench_core::kit::load_dir(&unpacked).unwrap();
    let cfg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo().join("conformance/bench.json")).unwrap(),
    )
    .unwrap();
    let want: Vec<&String> = cfg["models"].as_object().unwrap().keys().collect();
    let got: Vec<&String> = kit.kit.models.keys().collect();
    assert_eq!(got, want);
    assert_eq!(kit.kit.sources.bosl2_commit, "unpinned");
    assert_eq!(kit.kit.runs, cfg["runs"].as_u64().unwrap() as u32);
    assert!(kit.kit.models.values().any(|m| m.quick));
    assert!(kit.root.join("libraries/BOSL2/std.scad").is_file());
    assert!(kit.root.join("licenses/BOSL2-BSD-2-Clause.txt").is_file());
    // Inline sources are written out exactly.
    let src = std::fs::read_to_string(kit.root.join("models/mink_convex.scad")).unwrap();
    assert_eq!(
        src,
        cfg["models"]["mink_convex"]["source"].as_str().unwrap()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
