//! Records what `neoscad --info` reports about the build: the compiler and
//! the versions of the libraries that decide the output (the geometry
//! kernels, text shaping, file formats). They come from the workspace's
//! `Cargo.lock`, so the list cannot drift from what was actually linked.

use std::path::Path;
use std::process::Command;

/// Crates `--info` names, in the order it prints them.
const REPORTED: &[&str] = &[
    "manifold-rust",
    "clipper2-rust",
    "harfrust",
    "skrifa",
    "zip",
    "flate2",
    "quick-xml",
    "png",
    "wgpu",
];

fn main() {
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());
    let text = std::fs::read_to_string(&lock).unwrap_or_default();
    let mut found = Vec::new();
    for name in REPORTED {
        if let Some(v) = locked_version(&text, name) {
            found.push(format!("{name}={v}"));
        }
    }
    println!("cargo:rustc-env=NEOSCAD_LIB_VERSIONS={}", found.join(";"));

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "rustc (unknown version)".into());
    println!("cargo:rustc-env=NEOSCAD_RUSTC_VERSION={version}");
}

/// The version of the `[[package]]` named `name`: the line after its
/// `name = "..."` line.
fn locked_version(lock: &str, name: &str) -> Option<String> {
    let key = format!("name = \"{name}\"");
    let mut lines = lock.lines();
    while let Some(l) = lines.next() {
        if l == key {
            let v = lines.next()?.strip_prefix("version = \"")?;
            return Some(v.trim_end_matches('"').to_string());
        }
    }
    None
}
