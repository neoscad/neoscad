//! The update check every front end shares: is there a newer NeoSCAD
//! release for this channel and platform, according to a signed feed?
//!
//! The release workflow publishes two feeds on neoscad.org
//! (`docs/release.md`, "The update feed"): `stable.json` names the newest
//! full release, and `rc.json` the newest release of any kind, so someone
//! on the rc channel is also offered the final release that follows their
//! release candidate. Each comes with a detached minisign signature
//! (`stable.json.minisig`), made with a key whose public half is compiled
//! in here ([`RELEASE_KEYS`]).
//!
//! This module is only the decision. The host fetches the two files,
//! decides when to check, and keeps the last serial it accepted for each
//! channel; this crate has no network, clock or disk (`CLAUDE.md`,
//! "Rules"). [`check`] then
//!
//! - verifies the signature against the trusted keys, so a compromised
//!   web host or a network attacker can't point anyone at another file;
//! - checks that the feed is for the channel asked about, so a signed
//!   `rc.json` served as `stable.json` can't move stable users to an rc;
//! - refuses a serial lower than the last one accepted, so an old but
//!   validly signed feed can't be replayed to hide a newer release;
//! - offers only a strictly newer version than the running one (semver
//!   order, so `0.3.0-rc.1 < 0.3.0`), so a feed can never cause a
//!   downgrade;
//! - offers an app only a release that has its platform's file: the macOS
//!   DMG lands hours after the rest of a release (Apple's notarization),
//!   and until then a Mac app is not told about a release it can't
//!   install.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The public keys whose signatures are trusted, as the base64 line of a
/// minisign public key file (`minisign.pub`'s second line, `RW…`).
///
/// More than one is for rotation: a release adds the new key here, the
/// feed switches to signing with it once installs that trust it are
/// common, and a later release drops the old one (`docs/release.md`,
/// "Rotating the feed key"). Empty until the owner creates the release
/// key; with no key every feed is refused, so nothing is ever offered.
pub const RELEASE_KEYS: &[&str] = &[];

/// One extra trusted key, set at build time through the
/// `NEOSCAD_UPDATE_TEST_PUBLIC_KEY` environment variable: the end-to-end
/// test signs a local feed with a throwaway key and builds the CLI to
/// trust it. Compile time, not run time, so nothing on a user's machine
/// can add a key; release builds never set it.
const BUILD_TEST_KEY: Option<&str> = option_env!("NEOSCAD_UPDATE_TEST_PUBLIC_KEY");

/// The keys [`check`] trusts: [`RELEASE_KEYS`] and the build's test key.
pub fn trusted_keys() -> Vec<&'static str> {
    RELEASE_KEYS
        .iter()
        .copied()
        .chain(BUILD_TEST_KEY.filter(|k| !k.trim().is_empty()))
        .collect()
}

/// Where the feeds are published; `<base><channel>.json` and `.minisig`.
pub const FEED_BASE_URL: &str = "https://neoscad.org/updates/v1/";

/// The feed format this code reads. A change that old readers would
/// misread goes to a new directory (`/updates/v2/`), so a v1 file always
/// carries 1.
pub const FEED_SCHEMA: u32 = 1;

/// Which feed to follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// Full releases only: everyone, unless they opt in to release
    /// candidates.
    Stable,
    /// Release candidates and full releases, whichever is newer.
    Rc,
}

impl Channel {
    /// The name in the feed and in its file name (`stable`, `rc`).
    pub fn name(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Rc => "rc",
        }
    }

    /// The feed's URL under [`FEED_BASE_URL`] (or another base).
    pub fn feed_url(self, base: &str) -> String {
        format!("{base}{}.json", self.name())
    }
}

/// A platform an app is built for, as named in the feed's `artifacts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// The universal DMG.
    MacOs,
    WindowsX64,
    WindowsArm64,
    /// The Flatpak bundles.
    LinuxX86_64,
    LinuxAarch64,
}

impl Platform {
    /// The key in the feed's `artifacts`.
    pub fn key(self) -> &'static str {
        match self {
            Platform::MacOs => "macos",
            Platform::WindowsX64 => "windows-x64",
            Platform::WindowsArm64 => "windows-arm64",
            Platform::LinuxX86_64 => "linux-x86_64",
            Platform::LinuxAarch64 => "linux-aarch64",
        }
    }

    /// The platform for a feed key, the inverse of [`Platform::key`].
    pub fn from_key(key: &str) -> Option<Platform> {
        [
            Platform::MacOs,
            Platform::WindowsX64,
            Platform::WindowsArm64,
            Platform::LinuxX86_64,
            Platform::LinuxAarch64,
        ]
        .into_iter()
        .find(|p| p.key() == key)
    }
}

/// A feed file, as `scripts/release/update-feed.py` writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feed {
    pub schema: u32,
    /// `stable` or `rc`: which file this is, signed with the rest.
    pub channel: Channel,
    /// Raised by one every time the file changes, never lowered.
    pub serial: u64,
    /// The release's version, without the tag's `v`.
    pub version: String,
    /// The day it was published, `YYYY-MM-DD` (UTC).
    pub date: String,
    /// The GitHub release page: notes and every download.
    pub url: String,
    /// The apps' installers by [`Platform::key`]. A platform whose file is
    /// not attached yet is missing (the macOS DMG, for hours).
    #[serde(default)]
    pub artifacts: BTreeMap<String, Artifact>,
}

/// One downloadable installer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// The file name, e.g. `NeoSCAD-0.3.0-windows-x64.msi`.
    pub name: String,
    pub url: String,
    /// Lowercase hex: what the host checks the download against.
    pub sha256: String,
    /// Bytes.
    pub size: u64,
}

/// A newer release to tell the user about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub version: String,
    pub date: String,
    /// The release page.
    pub url: String,
    /// The platform's installer; `None` when [`check`] was asked about no
    /// platform (the CLI, which package managers update).
    pub artifact: Option<Artifact>,
}

/// What [`check`] found in a feed it accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checked {
    /// The feed's serial: the host stores it, per channel, and passes it
    /// as `last_serial` next time.
    pub serial: u64,
    /// The feed's version, offered or not.
    pub version: String,
    /// A strictly newer release that this platform can install.
    pub update: Option<Update>,
}

/// Why a feed was refused. Every one means "say nothing": a host logs it
/// at most, since an offline machine or a feed between releases is not
/// the user's problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// No trusted key is configured (no release key yet).
    NoTrustedKeys,
    /// The `.minisig` file doesn't parse.
    BadSignatureFile,
    /// No trusted key made a valid signature over these bytes.
    BadSignature,
    /// The (signed) feed isn't a feed this version understands.
    BadFeed(String),
    /// The feed is for the other channel.
    WrongChannel { expected: Channel, found: Channel },
    /// A lower serial than the last one accepted: a replayed old feed.
    Replay { last: u64, found: u64 },
    /// A version that is not semver (the feed's or the running one).
    BadVersion(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::NoTrustedKeys => write!(f, "no update-feed key is configured"),
            UpdateError::BadSignatureFile => write!(f, "the feed's signature file is malformed"),
            UpdateError::BadSignature => {
                write!(f, "the feed is not signed by a trusted key")
            }
            UpdateError::BadFeed(e) => write!(f, "the feed is malformed: {e}"),
            UpdateError::WrongChannel { expected, found } => write!(
                f,
                "asked for the {} feed, got the {} feed",
                expected.name(),
                found.name()
            ),
            UpdateError::Replay { last, found } => write!(
                f,
                "the feed's serial {found} is older than the last one seen ({last})"
            ),
            UpdateError::BadVersion(v) => write!(f, "not a version: {v:?}"),
        }
    }
}

impl std::error::Error for UpdateError {}

/// [`check_with_keys`] against [`trusted_keys`]: what the apps and the
/// CLI call.
pub fn check(
    feed: &[u8],
    signature: &[u8],
    current: &str,
    channel: Channel,
    platform: Option<Platform>,
    last_serial: Option<u64>,
) -> Result<Checked, UpdateError> {
    check_with_keys(
        &trusted_keys(),
        feed,
        signature,
        current,
        channel,
        platform,
        last_serial,
    )
}

/// The whole check (see the module documentation): verify `signature`
/// over `feed` with one of `keys`, parse it, check its channel and serial,
/// and offer its release if it is newer than `current` and, with a
/// `platform`, has that platform's installer.
pub fn check_with_keys(
    keys: &[&str],
    feed: &[u8],
    signature: &[u8],
    current: &str,
    channel: Channel,
    platform: Option<Platform>,
    last_serial: Option<u64>,
) -> Result<Checked, UpdateError> {
    verify(keys, feed, signature)?;
    // Parsed only after the signature holds, so unsigned bytes never reach
    // the JSON parser.
    let parsed: Feed =
        serde_json::from_slice(feed).map_err(|e| UpdateError::BadFeed(e.to_string()))?;
    if parsed.schema != FEED_SCHEMA {
        return Err(UpdateError::BadFeed(format!(
            "schema {} (this version reads {FEED_SCHEMA})",
            parsed.schema
        )));
    }
    if parsed.channel != channel {
        return Err(UpdateError::WrongChannel {
            expected: channel,
            found: parsed.channel,
        });
    }
    if let Some(last) = last_serial
        && parsed.serial < last
    {
        return Err(UpdateError::Replay {
            last,
            found: parsed.serial,
        });
    }
    let offered = parse_version(&parsed.version)?;
    let running = parse_version(current)?;
    // An rc file can name a full release and a stable file never names an
    // rc; but a hand-edited or buggy stable feed naming one must still not
    // move stable users onto a prerelease.
    let allowed = channel == Channel::Rc || offered.pre.is_empty();
    let artifact = platform.map(|p| parsed.artifacts.get(p.key()).cloned());
    let update = (allowed && offered > running && artifact.as_ref().is_none_or(Option::is_some))
        .then(|| Update {
            version: parsed.version.clone(),
            date: parsed.date.clone(),
            url: parsed.url.clone(),
            artifact: artifact.flatten(),
        });
    Ok(Checked {
        serial: parsed.serial,
        version: parsed.version,
        update,
    })
}

/// `Ok` when one of `keys` signed `feed`. Legacy (non-prehashed) minisign
/// signatures are refused: `minisign -S` has made prehashed ones by
/// default since 0.11, and the workflow uses it.
fn verify(keys: &[&str], feed: &[u8], signature: &[u8]) -> Result<(), UpdateError> {
    if keys.is_empty() {
        return Err(UpdateError::NoTrustedKeys);
    }
    let text = std::str::from_utf8(signature).map_err(|_| UpdateError::BadSignatureFile)?;
    let sig =
        minisign_verify::Signature::decode(text).map_err(|_| UpdateError::BadSignatureFile)?;
    let trusted = keys
        .iter()
        .filter_map(|k| minisign_verify::PublicKey::from_base64(k.trim()).ok())
        .any(|k| k.verify(feed, &sig, false).is_ok());
    if trusted {
        Ok(())
    } else {
        Err(UpdateError::BadSignature)
    }
}

/// Whether `offered` is a strictly newer version than `current`, in
/// semver order: for a host that stored an [`Update`] and must tell
/// whether it still applies (the user may have upgraded since).
pub fn is_newer(offered: &str, current: &str) -> Result<bool, UpdateError> {
    Ok(parse_version(offered)? > parse_version(current)?)
}

/// A release version, with or without the tag's leading `v`.
fn parse_version(v: &str) -> Result<semver::Version, UpdateError> {
    let bare = v.trim().strip_prefix('v').unwrap_or(v.trim());
    semver::Version::parse(bare).map_err(|_| UpdateError::BadVersion(v.to_string()))
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
