//! The Linux app's update check, apart from the GTK glue
//! (`src/app/update.rs`): the settings and when a check is due, where the
//! feed is, what a fetched feed means for this install, and what the
//! notice tells the user to do (docs/linux-app.md, "Updates";
//! docs/audits/auto-update.md has the design and the owner's decisions).
//!
//! The decision itself, whether a feed is genuine and offers something
//! newer, is `client::update::check`, the code the CLI and the other apps
//! share. This module adds only what is particular to this app: the two
//! settings ("Check for updates automatically", on by default; "Receive
//! release candidates", off), the last serial accepted per channel (so an
//! old, validly signed feed can't be replayed), and the advice for each
//! way the app can be installed.
//!
//! The app ships only as a Flatpak bundle on each release (the `.deb`,
//! `.rpm` and tarballs carry the command line, which has its own notice).
//! A bundle carries no repository (`flatpak build-bundle` without
//! `--repo-url`), so `flatpak update` never reaches the next release: the
//! notice offers the new bundle to download and install. A copy that is
//! not a Flatpak (built from source) is pointed at the release page.

use std::path::{Path, PathBuf};

use client::update::{self, Artifact, Channel, Platform, UpdateError};
use serde_json::{Value, json};

/// How often an automatic check runs: once a day at most.
pub const INTERVAL_SECS: u64 = 24 * 60 * 60;

/// A feed and its signature are a few hundred bytes; anything larger is
/// not one, and is not read past this.
pub const MAX_FEED_BYTES: usize = 64 * 1024;

/// Another directory to fetch the feed from, ending in `/`, as for the
/// CLI (`crates/cli/src/update.rs`): for testing against a local feed.
/// The signature is still checked against the keys compiled in, so this
/// can't make the app believe an unsigned file.
pub const FEED_URL_ENV: &str = "NEOSCAD_UPDATE_FEED_URL";

/// Set to anything to stop the automatic check, as for the CLI; CI (`CI`
/// set) never checks either, so test runs make no request. The menu's
/// "Check for Updates" still works.
pub const NO_CHECK_ENV: &str = "NEOSCAD_NO_UPDATE_CHECK";

/// How this copy of the app was installed, which decides what the notice
/// tells the user to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    /// Inside a Flatpak sandbox: installed from a release's bundle.
    Flatpak,
    /// Anything else: built from source, or packaged by someone else.
    Other,
}

impl Install {
    /// `/.flatpak-info` exists inside every Flatpak sandbox, and nowhere
    /// else (`flatpak-run(1)`); the caller says whether it does.
    pub fn detect(flatpak_info_exists: bool) -> Install {
        if flatpak_info_exists {
            Install::Flatpak
        } else {
            Install::Other
        }
    }

    /// The feed's installer to ask for, by the machine's architecture
    /// (`std::env::consts::ARCH`). A Flatpak needs its architecture's
    /// bundle, and a release is offered only once that bundle is attached
    /// (the aarch64 one may be missing: its build is allowed to fail).
    /// Another install asks for none, since it is not updated from the
    /// bundle.
    pub fn platform(self, arch: &str) -> Option<Platform> {
        match self {
            Install::Flatpak => match arch {
                "x86_64" => Some(Platform::LinuxX86_64),
                "aarch64" => Some(Platform::LinuxAarch64),
                _ => None,
            },
            Install::Other => None,
        }
    }
}

/// What the app keeps between runs, in `updates.json` under the user's
/// configuration directory ([`settings_path`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Check about once a day (owner decision: on by default).
    pub automatic: bool,
    /// Follow the rc feed (owner decision: opt-in).
    pub rc: bool,
    /// When the last automatic check started, in seconds since the Unix
    /// epoch. Recorded before the fetch, so a check that fails (offline)
    /// also waits a day rather than retrying all day.
    pub checked: u64,
    /// The serial of the last feed accepted on each channel.
    pub stable_serial: Option<u64>,
    pub rc_serial: Option<u64>,
    /// A version the user chose "Later" for: automatic checks don't show
    /// it again (a newer one, or the menu's check, does).
    pub dismissed: Option<String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            automatic: true,
            rc: false,
            checked: 0,
            stable_serial: None,
            rc_serial: None,
            dismissed: None,
        }
    }
}

impl Settings {
    /// The settings in `bytes`, with the default for anything missing or
    /// malformed: a damaged file must not turn the check off, or on.
    pub fn from_json(bytes: &[u8]) -> Settings {
        let v: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
        let d = Settings::default();
        Settings {
            automatic: v["automatic"].as_bool().unwrap_or(d.automatic),
            rc: v["rc"].as_bool().unwrap_or(d.rc),
            checked: v["checked"].as_u64().unwrap_or(d.checked),
            stable_serial: v["serial"]["stable"].as_u64(),
            rc_serial: v["serial"]["rc"].as_u64(),
            dismissed: v["dismissed"].as_str().map(str::to_string),
        }
    }

    pub fn to_json(&self) -> String {
        let v = json!({
            "automatic": self.automatic,
            "rc": self.rc,
            "checked": self.checked,
            "serial": {"stable": self.stable_serial, "rc": self.rc_serial},
            "dismissed": self.dismissed,
        });
        serde_json::to_string_pretty(&v).unwrap_or_default()
    }

    /// The file at `path`, or the defaults when it is missing or unreadable.
    pub fn load(path: &Path) -> Settings {
        std::fs::read(path)
            .map(|b| Settings::from_json(&b))
            .unwrap_or_default()
    }

    /// Written to a temporary file and renamed over `path`, so a crash
    /// mid-write leaves the old settings rather than half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("updates.{}.tmp", std::process::id()));
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    /// The feed this copy follows.
    pub fn channel(&self) -> Channel {
        if self.rc {
            Channel::Rc
        } else {
            Channel::Stable
        }
    }

    /// Whether an automatic check should run at `now` (seconds since the
    /// epoch). A last check "in the future" means the clock went back:
    /// check, rather than wait for the clock to catch up.
    pub fn due(&self, now: u64) -> bool {
        self.automatic && (now < self.checked || now - self.checked >= INTERVAL_SECS)
    }

    fn serial(&self, channel: Channel) -> Option<u64> {
        match channel {
            Channel::Stable => self.stable_serial,
            Channel::Rc => self.rc_serial,
        }
    }

    fn set_serial(&mut self, channel: Channel, serial: u64) {
        match channel {
            Channel::Stable => self.stable_serial = Some(serial),
            Channel::Rc => self.rc_serial = Some(serial),
        }
    }
}

/// Where the settings live: `<config>/neoscad/updates.json`, with
/// `config` the user's configuration directory (`~/.config`, or inside a
/// Flatpak `~/.var/app/org.neoscad.NeoSCAD/config`).
pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("neoscad").join("updates.json")
}

/// The directory the feeds are fetched from: [`update::FEED_BASE_URL`],
/// or `custom` ([`FEED_URL_ENV`]) when that is https, or plain http to
/// the loopback address (a test feed on this machine). Anything else is
/// refused, so a stray setting can't send the request in the clear.
pub fn feed_base(custom: Option<&str>) -> Result<String, String> {
    let Some(base) = custom.map(str::trim).filter(|b| !b.is_empty()) else {
        return Ok(update::FEED_BASE_URL.to_string());
    };
    let loopback = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
        .iter()
        .any(|p| base.starts_with(p));
    if !(base.starts_with("https://") || loopback) {
        return Err(format!(
            "{FEED_URL_ENV} must be https, or http to the loopback address: {base}"
        ));
    }
    Ok(if base.ends_with('/') {
        base.to_string()
    } else {
        format!("{base}/")
    })
}

/// The feed's URL and its signature's for `channel` under `base`.
pub fn feed_urls(base: &str, channel: Channel) -> (String, String) {
    let feed = channel.feed_url(base);
    let sig = format!("{feed}.minisig");
    (feed, sig)
}

/// A newer release, and what this install should do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub version: String,
    /// The running version.
    pub current: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// The release page: notes and every download.
    pub release_url: String,
    /// The Flatpak bundle for this architecture, for a Flatpak install.
    pub bundle: Option<Artifact>,
    pub install: Install,
}

impl Notice {
    /// The banner's text.
    pub fn title(&self) -> String {
        format!("NeoSCAD {} is available", self.version)
    }

    /// The details dialog's text: how to update this install.
    pub fn body(&self) -> String {
        let head = format!(
            "You have NeoSCAD {}. Version {} was released on {}.",
            self.current, self.version, self.date
        );
        match (&self.install, &self.bundle) {
            (Install::Flatpak, Some(b)) => format!(
                "{head}\n\nDownload the new Flatpak bundle and open it with Software, or install \
                 it from a terminal:\n\nflatpak install --user --reinstall {}\n\nThis copy came \
                 from a bundle, which flatpak update does not update.",
                b.name
            ),
            _ => format!(
                "{head}\n\nNeoSCAD for Linux is released as a Flatpak bundle. This copy was not \
                 installed from one, so update it the way it was installed: for a source \
                 checkout, pull and build again. The release page has the notes and the \
                 bundles."
            ),
        }
    }

    /// What the dialog's main button opens: the bundle for a Flatpak,
    /// else the release page.
    pub fn download_url(&self) -> &str {
        match (&self.install, &self.bundle) {
            (Install::Flatpak, Some(b)) => &b.url,
            _ => &self.release_url,
        }
    }

    /// The main button's label.
    pub fn download_label(&self) -> &'static str {
        match (&self.install, &self.bundle) {
            (Install::Flatpak, Some(_)) => "Download Bundle",
            _ => "Open Release Page",
        }
    }
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The feed's version is not newer than this one (or has no bundle
    /// for this Flatpak's architecture yet).
    UpToDate {
        latest: String,
    },
    Available(Notice),
}

/// A fetched feed and signature checked with `client::update::check_with_keys`
/// (the app passes `client::update::trusted_keys()`) on the channel the
/// settings choose, against that channel's last serial. An accepted feed
/// records its serial in `settings`, which the caller then saves; a
/// refused one changes nothing and is reported only by a manual check.
pub fn evaluate(
    settings: &mut Settings,
    keys: &[&str],
    feed: &[u8],
    signature: &[u8],
    current: &str,
    install: Install,
    arch: &str,
) -> Result<Outcome, UpdateError> {
    let channel = settings.channel();
    let checked = update::check_with_keys(
        keys,
        feed,
        signature,
        current,
        channel,
        install.platform(arch),
        settings.serial(channel),
    )?;
    settings.set_serial(channel, checked.serial);
    Ok(match checked.update {
        Some(u) => Outcome::Available(Notice {
            version: u.version,
            current: current.to_string(),
            date: u.date,
            release_url: u.url,
            bundle: u.artifact,
            install,
        }),
        None => Outcome::UpToDate {
            latest: checked.version,
        },
    })
}

/// Whether to show `notice`: always after the menu's check, and after an
/// automatic one unless the user put this version off with "Later".
pub fn should_show(notice: &Notice, settings: &Settings, manual: bool) -> bool {
    manual || settings.dismissed.as_deref() != Some(notice.version.as_str())
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
