//! Locations the harness works with, and small process helpers.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Repository layout, resolved once per invocation.
#[derive(Debug, Clone)]
pub struct Ctx {
    /// NeoSCAD repository root.
    pub repo: PathBuf,
    /// OpenSCAD reference checkout (absolute, canonical).
    pub ref_root: PathBuf,
}

/// Where the reference checkout lives, relative to the repository root.
pub const REF_REL: &str = ".reference/openscad";

impl Ctx {
    pub fn discover() -> Result<Self, String> {
        // The harness is a development tool that always runs from this
        // checkout, so the compile-time crate location is the reliable anchor
        // (the working directory may be anywhere, e.g. under target/).
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo = crate_dir
            .join("../..")
            .canonicalize()
            .map_err(|e| format!("cannot resolve repository root: {e}"))?;
        let ref_root = repo.join(REF_REL);
        if !ref_root.join("tests/CMakeLists.txt").is_file() {
            return Err(format!(
                "no OpenSCAD reference checkout at {}; create it with\n  git clone --depth 1 https://github.com/openscad/openscad.git {REF_REL}",
                ref_root.display()
            ));
        }
        let ref_root = ref_root.canonicalize().map_err(|e| e.to_string())?;
        Ok(Self { repo, ref_root })
    }

    pub fn ref_str(&self) -> String {
        self.ref_root.to_string_lossy().into_owned()
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.repo.join("conformance/manifest.json")
    }

    pub fn baseline_path(&self) -> PathBuf {
        self.repo.join("conformance/baseline.json")
    }

    pub fn showcase_path(&self) -> PathBuf {
        self.repo.join("conformance/showcase.json")
    }

    pub fn progress_dir(&self) -> PathBuf {
        self.repo.join("progress")
    }

    /// The directory tests run in. OpenSCAD's goldens were generated with
    /// ctest running in `<build>/tests`, and diagnostics print input paths
    /// relative to the working directory (`in file ../../tests/...`). Running
    /// in the reference's own `build/tests` (gitignored by OpenSCAD) keeps
    /// those paths identical without relying on the rewrite step.
    pub fn work_dir(&self) -> PathBuf {
        self.ref_root.join("build/tests")
    }

    /// Where actual outputs are written, like ctest's `output/<test>/`.
    pub fn actual_dir(&self) -> PathBuf {
        self.repo.join("target/conformance/actual")
    }

    pub fn default_binary(&self) -> PathBuf {
        self.repo.join("target/release/neoscad")
    }

    pub fn reference_commit(&self) -> String {
        git(&self.ref_root, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into())
    }
}

/// Run git in `dir` and return trimmed stdout, or None on any failure.
pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `os.path.relpath(path, start)` for absolute, normalised paths.
pub fn relpath(path: &Path, start: &Path) -> String {
    let p: Vec<Component> = path.components().collect();
    let s: Vec<Component> = start.components().collect();
    let common = p.iter().zip(&s).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".into(); s.len() - common];
    parts.extend(p[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    if parts.is_empty() { ".".into() } else { parts.join("/") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relpath_like_python() {
        assert_eq!(relpath(Path::new("/r/tests"), Path::new("/r/build/tests")), "../../tests");
        assert_eq!(relpath(Path::new("/a/b"), Path::new("/a/b")), ".");
        assert_eq!(relpath(Path::new("/a/b/c"), Path::new("/a")), "b/c");
    }
}
