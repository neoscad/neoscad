//! The library crates never touch the disk, the environment or the clock
//! (CLAUDE.md, "Rules"; docs/architecture.md): files come through
//! `FileSystem`, paths and seeds through options. Only `lang/src/host.rs`,
//! behind the `host` feature, reaches the machine, and only hosts and tests
//! turn that feature on.
//!
//! A slip compiles everywhere the workspace builds (another member turns
//! the feature on) and fails quietly on wasm32 (`current_dir` and
//! `canonicalize` return errors there), so it is checked here: the
//! manifests, and the non-test source of every library crate.

use std::path::{Path, PathBuf};

/// The crates that may use the machine directly (CLAUDE.md, "Rules"). Any
/// other crate is a library crate, including one added later.
const HOSTS: &[&str] = &[
    "agent-link",
    "bench-core",
    "cli",
    "conformance",
    "ffi",
    "linux-app",
    "uniffi-bindgen",
    "wasm-check",
    "web",
    "web-view",
];

/// What library code must not call. Comments are ignored, so docs may
/// still name them.
const FORBIDDEN: &[&str] = &[
    "std::fs",
    "env::var",
    "env::current_dir",
    "env::args",
    "env::home_dir",
    "env::temp_dir",
    "env::set_var",
    "current_exe",
    "StdFs",
    "LibraryPath::from_env",
    "LibraryPath::user_dir",
    ".canonicalize()",
    "File::open",
    "File::create",
    "Instant::now",
    "SystemTime::now",
];

/// Attributes whose item is not library code: tests, and what only exists
/// with the `host` feature (`lang`'s re-export of `StdFs`).
const GUARDS: &[&str] = &["#[cfg(test)]", "#[cfg(feature = \"host\")]"];

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn library_crates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(crates_dir())
        .expect("crates/")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.join("Cargo.toml").is_file())
        .filter(|p| !HOSTS.contains(&p.file_name().unwrap().to_str().unwrap()))
        .collect();
    out.sort();
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries {
        let p = e.expect("entry").path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if p.is_dir() {
            // A test module's own directory (`mod tests;` as `tests/mod.rs`).
            if name != "tests" {
                rust_files(&p, out);
            }
        } else if name.ends_with(".rs") && !name.ends_with("tests.rs") {
            out.push(p);
        }
    }
}

/// The lines of `text` outside items under one of [`GUARDS`], without
/// comments.
fn unguarded_code(text: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if GUARDS.contains(&line.trim()) {
            // Skip the item it guards: through its closing brace, or one
            // line for an item without a body (`mod tests;`, `use`).
            i += 1;
            while i < lines.len() && lines[i].trim_start().starts_with("#[") {
                i += 1;
            }
            let mut depth = 0i32;
            let mut opened = false;
            while i < lines.len() {
                let code = strip_comment(lines[i]);
                for c in code.chars() {
                    match c {
                        '{' => {
                            depth += 1;
                            opened = true;
                        }
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                i += 1;
                if (opened && depth <= 0) || (!opened && code.trim_end().ends_with(';')) {
                    break;
                }
            }
            continue;
        }
        out.push((i + 1, strip_comment(line).to_string()));
        i += 1;
    }
    out
}

fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(at) => &line[..at],
        None => line,
    }
}

#[test]
fn library_code_does_not_reach_the_machine() {
    let mut found = Vec::new();
    for krate in library_crates() {
        let mut files = Vec::new();
        rust_files(&krate.join("src"), &mut files);
        files.sort();
        for f in files {
            // The one module allowed to, behind the `host` feature.
            if f.ends_with("lang/src/host.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&f).expect("source");
            for (n, code) in unguarded_code(&text) {
                for pat in FORBIDDEN {
                    if code.contains(pat) {
                        found.push(format!("{}:{n}: {pat}", f.display()));
                    }
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "library code reaching the disk, environment or clock (take it \
         through FileSystem or Options; see lang/src/host.rs):\n{}",
        found.join("\n")
    );
}

#[test]
fn no_library_crate_turns_on_the_host_feature() {
    let mut found = Vec::new();
    for krate in library_crates() {
        let manifest = krate.join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest).expect("manifest");
        let mut section = String::new();
        for (n, line) in text.lines().enumerate() {
            let t = line.trim();
            if t.starts_with('[') {
                section = t.to_string();
                continue;
            }
            // Tests may use the disk; the library may not.
            let library_dep =
                section.ends_with("dependencies]") && !section.ends_with("dev-dependencies]");
            let code = t.split('#').next().unwrap_or("");
            if library_dep && code.contains("neoscad-lang") && code.contains("\"host\"") {
                found.push(format!("{}:{}: {t}", manifest.display(), n + 1));
            }
        }
    }
    assert!(
        found.is_empty(),
        "a library crate depends on lang's `host` feature:\n{}",
        found.join("\n")
    );
}

/// The scanner itself: test items are skipped, other code and later items
/// are not.
#[test]
fn test_items_are_skipped() {
    let src = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn b() { std::fs::read(\"x\"); }\n}\n#[cfg(test)]\nuse std::fs;\nfn c() { std::fs::read(\"y\"); } // std::env::var\n";
    let code = unguarded_code(src);
    let hits: Vec<usize> = code
        .iter()
        .filter(|(_, l)| l.contains("std::fs") || l.contains("env::var"))
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(hits, vec![8]);
}
