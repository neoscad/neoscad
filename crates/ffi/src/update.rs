//! The update check for the apps: `client::update::check` across the
//! bridge. The app fetches `<base><channel>.json` and its `.minisig` (a
//! plain GET, nothing identifying: `docs/privacy.md`), keeps the serial
//! of the last feed it accepted for each channel, and decides when to
//! look and what to show; this says whether the feed is genuine and
//! whether it offers something newer for this platform.
//! `docs/audits/auto-update.md` lists what each app still has to build
//! on top.

use crate::{CoreError, guarded};
use client::update;

/// Which feed to follow: everyone gets stable; "Receive release
/// candidates" switches to rc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum UpdateChannel {
    Stable,
    Rc,
}

impl From<UpdateChannel> for update::Channel {
    fn from(c: UpdateChannel) -> Self {
        match c {
            UpdateChannel::Stable => update::Channel::Stable,
            UpdateChannel::Rc => update::Channel::Rc,
        }
    }
}

/// The installer an app wants from the feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum UpdatePlatform {
    /// The universal DMG.
    MacOs,
    WindowsX64,
    WindowsArm64,
    LinuxX86_64,
    LinuxAarch64,
}

impl From<UpdatePlatform> for update::Platform {
    fn from(p: UpdatePlatform) -> Self {
        match p {
            UpdatePlatform::MacOs => update::Platform::MacOs,
            UpdatePlatform::WindowsX64 => update::Platform::WindowsX64,
            UpdatePlatform::WindowsArm64 => update::Platform::WindowsArm64,
            UpdatePlatform::LinuxX86_64 => update::Platform::LinuxX86_64,
            UpdatePlatform::LinuxAarch64 => update::Platform::LinuxAarch64,
        }
    }
}

/// An installer to download; check its SHA-256 before running it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct UpdateArtifact {
    pub name: String,
    pub url: String,
    /// Lowercase hex.
    pub sha256: String,
    pub size: u64,
}

/// A newer release this platform can install.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct UpdateOffer {
    pub version: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// The release page (notes and downloads).
    pub url: String,
    /// Always present when the check named a platform.
    pub artifact: Option<UpdateArtifact>,
}

/// What a genuine feed said.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct UpdateCheck {
    /// Store it for this channel and pass it back as `last_serial`.
    pub serial: u64,
    /// The feed's version, offered or not.
    pub version: String,
    pub offer: Option<UpdateOffer>,
}

/// Checks a fetched feed (`client::update::check`): its signature against
/// the keys compiled into the core, its channel, that its serial is not
/// below `last_serial` (a replayed old feed), and whether it offers a
/// newer version than `current` with this `platform`'s installer. A
/// refused feed is `InvalidArgument` with the reason; the app should log
/// it and say nothing to the user.
#[uniffi::export]
pub fn check_for_update(
    feed: Vec<u8>,
    signature: Vec<u8>,
    current: String,
    channel: UpdateChannel,
    platform: Option<UpdatePlatform>,
    last_serial: Option<u64>,
) -> Result<UpdateCheck, CoreError> {
    guarded(|| {
        let c = update::check(
            &feed,
            &signature,
            &current,
            channel.into(),
            platform.map(Into::into),
            last_serial,
        )
        .map_err(|e| CoreError::InvalidArgument {
            message: e.to_string(),
        })?;
        Ok(UpdateCheck {
            serial: c.serial,
            version: c.version,
            offer: c.update.map(|u| UpdateOffer {
                version: u.version,
                date: u.date,
                url: u.url,
                artifact: u.artifact.map(|a| UpdateArtifact {
                    name: a.name,
                    url: a.url,
                    sha256: a.sha256,
                    size: a.size,
                }),
            }),
        })
    })
}

/// The URL of a channel's feed; its signature is the same URL plus
/// `.minisig`.
#[uniffi::export]
pub fn update_feed_url(channel: UpdateChannel) -> String {
    update::Channel::from(channel).feed_url(update::FEED_BASE_URL)
}

/// Whether this build trusts any feed key. Before the release key exists
/// (and in any build without one) no feed can verify, so an app should
/// make no update request at all rather than fetch a feed it must refuse.
#[uniffi::export]
pub fn update_check_available() -> bool {
    !update::trusted_keys().is_empty()
}
