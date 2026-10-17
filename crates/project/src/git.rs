//! The system `git`, run as a child process (as the conformance harness
//! does for its reference checkout): no git library in the build, and the
//! user's credentials, proxies and URL rewrites apply to clones and
//! fetches as they do for any other git command.
//!
//! Only [`Git::clone_bare`], [`Git::fetch`] and [`Git::fetch_commit`]
//! reach the network; `--offline` never calls them. Materialising a tree
//! ([`Git::checkout`]) runs with the user's and the system's git
//! configuration turned off, so line-ending conversion and filters cannot
//! make the same commit check out to different bytes on two machines.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{Error, Result, bail};

/// The environment variable naming the git executable (default `git` on
/// the `PATH`).
pub const GIT_ENV: &str = "NEOSCAD_GIT";

/// How to run git.
#[derive(Debug, Clone)]
pub struct Git {
    exe: PathBuf,
    /// An empty file, the "global" configuration of checkouts.
    empty_config: PathBuf,
}

impl Git {
    /// `NEOSCAD_GIT`, else `git`. `empty_config` is an empty file that
    /// stands in for the user's configuration while a tree is written.
    pub fn new(empty_config: PathBuf) -> Git {
        let exe = std::env::var_os(GIT_ENV)
            .filter(|v| !v.is_empty())
            .map_or_else(|| PathBuf::from("git"), PathBuf::from);
        Git { exe, empty_config }
    }

    fn command(&self, git_dir: Option<&Path>) -> Command {
        let mut c = Command::new(&self.exe);
        if let Some(d) = git_dir {
            c.arg("--git-dir").arg(d);
        }
        c.stdin(Stdio::null());
        c
    }

    fn run<I, S>(&self, git_dir: Option<&Path>, args: I, what: &str) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut c = self.command(git_dir);
        c.args(args);
        output(c, &self.exe, what)
    }

    /// `git clone --bare URL DEST`.
    pub fn clone_bare(&self, url: &str, dest: &Path) -> Result<()> {
        let mut c = self.command(None);
        c.args(["clone", "--bare", "--quiet", "--", url]).arg(dest);
        output(c, &self.exe, &format!("cloning {url}")).map(drop)
    }

    /// Update every branch and tag of `mirror` from its remote (moved
    /// tags too; deleted ones are pruned).
    pub fn fetch(&self, mirror: &Path, url: &str) -> Result<()> {
        self.run(
            Some(mirror),
            [
                "fetch",
                "--quiet",
                "--prune",
                "--force",
                "origin",
                "+refs/heads/*:refs/heads/*",
                "+refs/tags/*:refs/tags/*",
            ],
            &format!("fetching {url}"),
        )
        .map(drop)
    }

    /// Fetch one commit by id, for a `rev` no branch or tag reaches
    /// (servers that allow it, as GitHub does).
    pub fn fetch_commit(&self, mirror: &Path, url: &str, commit: &str) -> Result<()> {
        self.run(
            Some(mirror),
            ["fetch", "--quiet", "origin", commit],
            &format!("fetching {commit} from {url}"),
        )
        .map(drop)
    }

    /// The commit `spec` names in `mirror` (a ref or an abbreviated id),
    /// if it has it.
    pub fn commit_of(&self, mirror: &Path, spec: &str) -> Option<String> {
        let out = self
            .run(
                Some(mirror),
                [
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "--end-of-options",
                    &format!("{spec}^{{commit}}"),
                ],
                "rev-parse",
            )
            .ok()?;
        let s = String::from_utf8_lossy(&out).trim().to_string();
        (s.len() == 40 || s.len() == 64).then_some(s)
    }

    /// The bytes of `path` at `commit`, if it has that file.
    pub fn show(&self, mirror: &Path, commit: &str, path: &str) -> Option<Vec<u8>> {
        let spec = format!("{commit}:{path}");
        let t = self
            .run(Some(mirror), ["cat-file", "-t", &spec], "cat-file")
            .ok()?;
        if String::from_utf8_lossy(&t).trim() != "blob" {
            return None;
        }
        self.run(Some(mirror), ["cat-file", "blob", &spec], "cat-file")
            .ok()
    }

    /// Whether `commit` has a directory at `path`.
    pub fn has_dir(&self, mirror: &Path, commit: &str, path: &str) -> bool {
        self.run(
            Some(mirror),
            ["cat-file", "-t", &format!("{commit}:{path}")],
            "cat-file",
        )
        .is_ok_and(|t| String::from_utf8_lossy(&t).trim() == "tree")
    }

    /// Write the tree of `commit` (or of its `include_root` folder) into
    /// the empty directory `dest`, with no `.git`: a throwaway index in
    /// `scratch` is read from the mirror's objects and checked out, so
    /// the mirror itself is not changed. Submodules are not checked out.
    pub fn checkout(
        &self,
        mirror: &Path,
        commit: &str,
        include_root: Option<&str>,
        dest: &Path,
        scratch: &Path,
    ) -> Result<()> {
        let tree = match include_root {
            Some(r) => format!("{commit}:{r}"),
            None => commit.to_string(),
        };
        let index = scratch.join("index");
        let env = |c: &mut Command| {
            c.env("GIT_INDEX_FILE", &index)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", &self.empty_config)
                .env_remove("GIT_WORK_TREE");
        };
        let mut c = self.command(Some(mirror));
        env(&mut c);
        c.args(["read-tree", "--end-of-options", &tree]);
        output(c, &self.exe, &format!("reading {tree}"))?;
        let mut c = self.command(Some(mirror));
        env(&mut c);
        c.arg("--work-tree").arg(dest).args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.eol=lf",
            "-c",
            "core.symlinks=true",
            "-c",
            "core.bare=false",
            "checkout-index",
            "--all",
            "--force",
        ]);
        output(c, &self.exe, &format!("checking out {tree}"))?;
        let _ = std::fs::remove_file(&index);
        Ok(())
    }
}

fn output(mut c: Command, exe: &Path, what: &str) -> Result<Vec<u8>> {
    let out = c.output().map_err(|e| {
        Error(format!(
            "cannot run git ({}: {e}); install git, or set {GIT_ENV} to its path",
            exe.display()
        ))
    })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        if err.is_empty() {
            bail!("git failed {what} ({})", out.status);
        }
        bail!("git failed {what}: {err}");
    }
    Ok(out.stdout)
}
