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

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        delay_gpu_frameworks();
    }
}

/// The system frameworks the renderer links (through wgpu's Metal backend),
/// which every `neoscad` process loads, drawing or not.
const GPU_FRAMEWORKS: &[&str] = &[
    "Metal",
    "QuartzCore",
    "CoreGraphics",
    "Foundation",
    "CoreFoundation",
];

/// The oldest deployment target for which the linker accepts delay-init;
/// below it the linker warns that it ignores the flag.
const DELAY_INIT_MIN_MACOS: u32 = 15;

/// Marks the GPU frameworks delay-init: dyld still maps them at launch, but
/// runs their initializers, and those of everything they pull in (about
/// 400, in SkyLight, CoreDisplay, Network and the Swift runtime among
/// others), only when the process first calls into them. Linking the
/// renderer (`9336b1c`) made every run pay for those initializers, 1.3 ms
/// of a cold start that had been 2.8 ms (performance audit, R2), although
/// only PNG export and snapshots draw. With delay-init, `neoscad
/// --version` runs only libSystem's, the C++ runtime's and its own
/// initializers; a PNG export takes as long as before (about 52 ms for a
/// cube) and draws the same bytes. Unlike a helper binary or a `dlopen`ed
/// renderer, this changes no code and no packaging.
///
/// delay-init needs a deployment target of macOS 15, which the app and the
/// release build (`scripts/apple/release.sh`) already use, so a plain
/// `cargo build` links `neoscad` for 15 too unless `MACOSX_DEPLOYMENT_TARGET`
/// says otherwise. An older explicit target skips the flags rather than
/// making the linker warn about them.
///
/// For CoreGraphics the linker warns that it ignores delay-init because
/// the framework exports weak definitions, yet it records the flag and
/// dyld honours it; leaving CoreGraphics out loses the whole saving, since
/// initializing it initializes nearly all of the 400. Its weak exports are
/// templated preference lookups (`CGDefaultsCheck<...>`), which neoscad
/// does not bind to. The test `gpu_frameworks_are_not_initialized_at_launch`
/// (`tests/flags.rs`) fails if a linker or dyld stops honouring the flag.
fn delay_gpu_frameworks() {
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    let target = match std::env::var("MACOSX_DEPLOYMENT_TARGET") {
        Ok(v) => v,
        Err(_) => {
            let v = format!("{DELAY_INIT_MIN_MACOS}.0");
            // For rustc's own invocations, which link the binary.
            println!("cargo:rustc-env=MACOSX_DEPLOYMENT_TARGET={v}");
            v
        }
    };
    let major = target.split('.').next().and_then(|m| m.parse::<u32>().ok());
    if major.is_none_or(|m| m < DELAY_INIT_MIN_MACOS) {
        return;
    }
    for f in GPU_FRAMEWORKS {
        println!("cargo:rustc-link-arg-bins=-Wl,-delay_framework,{f}");
    }
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
