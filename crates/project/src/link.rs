//! A project's `.scad/libraries/<Name>` entries: a symbolic link to the
//! store's checkout (or to a path dependency's directory) on Unix; on
//! Windows a directory symbolic link where the user may make one
//! (Developer Mode), else a copy. (A junction would need no privilege,
//! but making one takes either `cmd /C mklink /J`, whose command line a
//! folder name could add commands to, or unsafe Win32 calls; a copy
//! needs neither.)
//!
//! A copy holds a marker file, [`COPY_MARKER`], so that a fetch can tell
//! the copies it made from directories it did not: it replaces and
//! removes links and its own copies only ([`remove_made`]), never a real
//! directory someone else put in `.scad/libraries`.

use std::path::{Path, PathBuf};

use crate::error::{Context, Result};

/// How an entry was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Symlink,
    Copy,
}

/// The file at the top of every copy [`link_dir`] makes.
pub const COPY_MARKER: &str = ".scad-deps-copy";

/// Whether `link` already leads to `target` (both existing).
pub fn points_to(link: &Path, target: &Path) -> bool {
    if std::fs::read_link(link).is_ok_and(|t| t == target) {
        return true;
    }
    match (std::fs::canonicalize(link), std::fs::canonicalize(target)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Remove the entry at `link`, whatever it is (a link, a junction, a
/// copied directory, a stray file); nothing if there is none.
pub fn remove(link: &Path) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(link) else {
        return Ok(());
    };
    if is_link(link) {
        // A directory link on Windows is removed as a directory, a Unix
        // one as a file.
        return std::fs::remove_file(link)
            .or_else(|_| std::fs::remove_dir(link))
            .at("remove", link);
    }
    if meta.is_dir() {
        crate::store::make_writable(link);
        return std::fs::remove_dir_all(link).at("remove", link);
    }
    std::fs::remove_file(link).at("remove", link)
}

/// Whether `p` is a symbolic link or (on Windows) a junction, without
/// following it.
pub fn is_link(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) || is_junction(p)
}

/// Remove the entry at `link` if it is one this module makes: a link or
/// a junction (not what it leads to), or a directory holding
/// [`COPY_MARKER`]. Ok(false), and nothing removed, for anything else
/// (a real directory, a file), and for nothing at all.
pub fn remove_made(link: &Path) -> Result<bool> {
    let Ok(meta) = std::fs::symlink_metadata(link) else {
        return Ok(false);
    };
    let ours = is_link(link)
        || (meta.is_dir()
            && std::fs::symlink_metadata(link.join(COPY_MARKER)).is_ok_and(|m| m.is_file()));
    if ours {
        remove(link)?;
    }
    Ok(ours)
}

#[cfg(windows)]
fn is_junction(p: &Path) -> bool {
    // A junction is a reparse point that `read_link` reads.
    std::fs::read_link(p).is_ok()
}

#[cfg(not(windows))]
fn is_junction(_: &Path) -> bool {
    false
}

/// Make `link` lead to the directory `target`, replacing a link or a
/// copy this module made there; anything else there is an error, and is
/// left as it is.
pub fn link_dir(target: &Path, link: &Path) -> Result<LinkKind> {
    if !remove_made(link)? && std::fs::symlink_metadata(link).is_ok() {
        return Err(crate::error::Error(format!(
            "{} is not a link or a copy that a fetch made, so it is left alone; move it out of the way and fetch again",
            link.display()
        )));
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).at("create", parent)?;
    }
    make(target, link)
}

#[cfg(unix)]
fn make(target: &Path, link: &Path) -> Result<LinkKind> {
    std::os::unix::fs::symlink(target, link)
        .ctx(|| format!("cannot link {} to {}", link.display(), target.display()))?;
    Ok(LinkKind::Symlink)
}

#[cfg(windows)]
fn make(target: &Path, link: &Path) -> Result<LinkKind> {
    if std::os::windows::fs::symlink_dir(target, link).is_ok() {
        return Ok(LinkKind::Symlink);
    }
    make_copy(target, link)
}

#[cfg(not(any(unix, windows)))]
fn make(target: &Path, link: &Path) -> Result<LinkKind> {
    make_copy(target, link)
}

/// A copy of `target` at `link`, marked as made here.
#[cfg_attr(unix, allow(dead_code))]
fn make_copy(target: &Path, link: &Path) -> Result<LinkKind> {
    copy_tree(target, link)?;
    let marker = link.join(COPY_MARKER);
    std::fs::write(
        &marker,
        "A copy that scad-deps made of a dependency, for a system without links; a fetch replaces it.\n",
    )
    .at("write", &marker)?;
    Ok(LinkKind::Copy)
}

/// Copy the directory `from` to `to` (which must not exist), files and
/// links as they are; the copy is writable whatever `from` was.
pub fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).at("create", to)?;
    for e in std::fs::read_dir(from).at("read", from)? {
        let e = e.at("read", from)?;
        let src = e.path();
        let dst: PathBuf = to.join(e.file_name());
        let ft = e.file_type().at("read", &src)?;
        if ft.is_dir() {
            copy_tree(&src, &dst)?;
        } else if ft.is_symlink() {
            let t = std::fs::read_link(&src).at("read", &src)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&t, &dst).at("create", &dst)?;
            #[cfg(not(unix))]
            {
                // Windows: the file git would have written for the link.
                let _ = std::fs::write(&dst, t.to_string_lossy().as_bytes());
            }
        } else {
            std::fs::copy(&src, &dst).at("copy", &src)?;
            crate::store::set_writable(&dst, true);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fetch replaces and removes only links and its own copies: a real
    /// directory in `.scad/libraries` (or a file) stays, contents and all.
    #[test]
    fn removes_only_what_it_made() {
        let base = std::env::temp_dir().join(format!("scad-deps-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let target = base.join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("x.scad"), "x = 1;\n").unwrap();
        let real = base.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("keep.scad"), "k = 1;\n").unwrap();
        assert!(!remove_made(&real).unwrap());
        assert!(real.join("keep.scad").is_file());
        assert!(link_dir(&target, &real).is_err());
        assert!(real.join("keep.scad").is_file());
        let file = base.join("file");
        std::fs::write(&file, "f").unwrap();
        assert!(!remove_made(&file).unwrap());
        assert!(file.is_file());
        // A copy is marked, and so is removable.
        let copy = base.join("copy");
        assert_eq!(make_copy(&target, &copy).unwrap(), LinkKind::Copy);
        assert!(copy.join(COPY_MARKER).is_file());
        assert!(remove_made(&copy).unwrap());
        assert!(!copy.exists());
        // A link goes, and what it leads to stays.
        let link = base.join("link");
        link_dir(&target, &link).unwrap();
        assert!(is_link(&link) || link.join(COPY_MARKER).is_file());
        assert!(remove_made(&link).unwrap());
        assert!(target.join("x.scad").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }
}
