//! Lists the files under the repository's `assets/` directory as
//! `include_bytes!` tables, so adding a file there needs no code change.
//! The lists are sorted the way `text::FontDb` sorts a font directory, so
//! the bundled fonts are indexed in the same order as the reference
//! checkout's `fonts/` (the order is the last tie-break in font matching).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|e| e.expect("directory entry").path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn table(name: &str, root: &Path, keep: impl Fn(&Path) -> bool, code: &mut String) {
    let mut files = Vec::new();
    walk(root, &mut files);
    writeln!(code, "pub static {name}: &[(&str, &[u8])] = &[").unwrap();
    for f in files.iter().filter(|f| keep(f)) {
        let rel = f.strip_prefix(root).expect("under root");
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_str().expect("UTF-8 asset name"))
            .collect::<Vec<_>>()
            .join("/");
        let abs = f.to_str().expect("UTF-8 asset path");
        writeln!(code, "    ({rel:?}, include_bytes!({abs:?})),").unwrap();
    }
    writeln!(code, "];").unwrap();
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let assets = manifest.join("../../assets");
    let assets = assets
        .canonicalize()
        .expect("the repository's assets/ directory");
    println!("cargo::rerun-if-changed={}", assets.display());
    let mut code = String::new();
    let is_font = |p: &Path| {
        p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "ttf" | "otf" | "ttc" | "otc"
            )
        })
    };
    table("FONTS", &assets.join("fonts"), is_font, &mut code);
    table("LIBRARIES", &assets.join("libraries"), |_| true, &mut code);
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("assets.rs");
    std::fs::write(out, code).expect("writing assets.rs");
}
