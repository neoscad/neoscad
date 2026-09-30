//! Whether the running `neoscad` is an official release binary.
//!
//! Community results are accepted only from official release binaries
//! (owner decision): a self-built binary may use other flags, another
//! toolchain or local patches, and its times would be filed under a version
//! whose release they do not describe. So every release carries
//! [`SUMS_ASSET`], the SHA-256 of the `neoscad` executable inside each
//! target's archive (written by `.github/workflows/publish-packages.yml`),
//! and a run compares its own executable's hash with the line for its
//! target. This is a client-side courtesy that saves a user from running
//! an hour of benchmarks only to have the submission refused; the
//! benchmarks repository repeats the check on every submission, since a
//! client can be made to say anything.
//!
//! The archive checksums cargo-dist publishes (`*.tar.xz.sha256`,
//! `sha256.sum`) cannot serve: they hash the archives, not the executable
//! inside, which is all a running binary can read of itself.

use serde::{Deserialize, Serialize};

/// The release asset listing each target's executable hash, in
/// `sha256sum` format: `<hex>  <target-triple>/neoscad[.exe]`.
pub const SUMS_ASSET: &str = "neoscad-executables.sha256sums";

/// The repository whose releases are official.
pub const RELEASE_REPO: &str = "neoscad/neoscad";

/// The URL of a release asset of version `version` (without the `v`).
pub fn release_asset_url(version: &str, asset: &str) -> String {
    format!("https://github.com/{RELEASE_REPO}/releases/download/v{version}/{asset}")
}

/// The outcome of the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Check {
    /// The executable's hash is the release's for this target.
    Matched,
    /// The release lists this target with another hash: a self-built,
    /// patched or re-signed binary.
    Mismatch,
    /// The release exists but has no executable for this target.
    TargetNotListed,
    /// The release's list could not be fetched (offline, or no such
    /// release: a development version).
    Unavailable,
}

impl Check {
    pub fn official(self) -> bool {
        self == Check::Matched
    }

    /// Why a result is (not) official, for the user.
    pub fn explain(self, version: &str, target: &str) -> String {
        match self {
            Check::Matched => {
                format!("official release: matches v{version}'s published {target} executable")
            }
            Check::Mismatch => format!(
                "not an official release binary: its SHA-256 differs from v{version}'s published \
                 {target} executable (a self-built or modified binary)"
            ),
            Check::TargetNotListed => format!(
                "not an official release binary: v{version} publishes no {target} executable"
            ),
            Check::Unavailable => format!(
                "unverified: could not fetch v{version}'s {SUMS_ASSET} from GitHub \
                 (offline, or not a released version)"
            ),
        }
    }
}

/// Parse `sha256sum` lines into (hash, name) pairs; malformed lines are
/// skipped. A leading `*` (binary mode) on the name is dropped.
pub fn parse_sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let hash = it.next()?;
            let name = it.next()?;
            (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| {
                (
                    hash.to_ascii_lowercase(),
                    name.trim_start_matches('*').into(),
                )
            })
        })
        .collect()
}

/// Check `sha256` (this executable's) against the release's `sums` for
/// `target`; `sums` is `None` when it could not be fetched.
pub fn check(sums: Option<&str>, target: &str, sha256: &str) -> Check {
    let Some(sums) = sums else {
        return Check::Unavailable;
    };
    let prefix = format!("{target}/");
    let listed: Vec<String> = parse_sums(sums)
        .into_iter()
        .filter(|(_, name)| {
            name.strip_prefix(&prefix)
                .is_some_and(|exe| exe == "neoscad" || exe == "neoscad.exe")
        })
        .map(|(hash, _)| hash)
        .collect();
    if listed.is_empty() {
        Check::TargetNotListed
    } else if listed.iter().any(|h| h.eq_ignore_ascii_case(sha256)) {
        Check::Matched
    } else {
        Check::Mismatch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aa00000000000000000000000000000000000000000000000000000000000001";
    const B: &str = "bb00000000000000000000000000000000000000000000000000000000000002";

    fn sums() -> String {
        format!(
            "{A}  aarch64-apple-darwin/neoscad\n\
             {B} *x86_64-pc-windows-msvc/neoscad.exe\n\
             not a line\n"
        )
    }

    #[test]
    fn matching_hash_is_official() {
        let c = check(Some(&sums()), "aarch64-apple-darwin", A);
        assert_eq!(c, Check::Matched);
        assert!(c.official());
        assert_eq!(
            check(Some(&sums()), "x86_64-pc-windows-msvc", &B.to_uppercase()),
            Check::Matched
        );
    }

    #[test]
    fn anything_else_is_not() {
        assert_eq!(
            check(Some(&sums()), "aarch64-apple-darwin", B),
            Check::Mismatch
        );
        assert_eq!(
            check(Some(&sums()), "x86_64-unknown-linux-gnu", A),
            Check::TargetNotListed
        );
        // Another target's hash never vouches for this one.
        assert_eq!(
            check(Some(&sums()), "aarch64-apple-darwin", B),
            Check::Mismatch
        );
        assert_eq!(check(None, "aarch64-apple-darwin", A), Check::Unavailable);
        assert!(!Check::Unavailable.official());
        // A prefix of the target is not the target.
        let tricky = format!("{A}  aarch64-apple-darwin-evil/neoscad\n");
        assert_eq!(
            check(Some(&tricky), "aarch64-apple-darwin", A),
            Check::TargetNotListed
        );
    }

    #[test]
    fn serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&Check::TargetNotListed).unwrap(),
            "\"target-not-listed\""
        );
    }
}
