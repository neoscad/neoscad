//! A project's `.scad/libraries/<Name>` entries: a symbolic link to the
//! store's checkout (or to a path dependency's directory) on Unix; on
//! Windows a directory symbolic link where the user may make one
//! (Developer Mode), else a directory junction (`mklink /J`, which needs
//! no privilege but only works on local volumes), else a copy.

use std::path::{Path, PathBuf};

use crate::error::{Context, Result};

/// How an entry was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Symlink,
    Junction,
    Copy,
}

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
    if meta.file_type().is_symlink() || is_junction(link) {
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

#[cfg(windows)]
fn is_junction(p: &Path) -> bool {
    // A junction is a reparse point that `read_link` reads.
    std::fs::read_link(p).is_ok()
}

#[cfg(not(windows))]
fn is_junction(_: &Path) -> bool {
    false
}

/// Make `link` lead to the directory `target` (replacing what is there).
pub fn link_dir(target: &Path, link: &Path) -> Result<LinkKind> {
    remove(link)?;
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
    let junction = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if junction.is_ok_and(|s| s.success()) && link.is_dir() {
        return Ok(LinkKind::Junction);
    }
    copy_tree(target, link)?;
    Ok(LinkKind::Copy)
}

#[cfg(not(any(unix, windows)))]
fn make(target: &Path, link: &Path) -> Result<LinkKind> {
    copy_tree(target, link)?;
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
