//! Whether to offer to make NeoSCAD the system's default app for OpenSCAD
//! files, apart from the GTK glue (`src/app/default_app.rs`;
//! docs/linux-app.md, "Default app for OpenSCAD files").
//!
//! The offer is a dialog shortly after launch, and a row in Preferences.
//! Both go through `g_app_info_set_as_default_for_type`, which writes the
//! user's `mimeapps.list`; this module decides when that is worth asking
//! and when it can work at all:
//!
//! - Inside a Flatpak or Snap, `~/.config` is the sandbox's own, so the
//!   write would land in a file the desktop never reads and the dialog
//!   would promise something that doesn't happen. Never ask there (the
//!   Flatpak's exported desktop entry already claims `.scad` files, and
//!   the desktop's own Open With settings can pick it).
//! - Without our desktop entry installed (running from `target/`), there
//!   is no app id for the desktop to remember, so nothing to make default.
//! - "Don't ask again" is kept in `default-app.json` beside the other
//!   settings files; "Not Now" alone keeps nothing, so the next launch
//!   asks again.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The type the desktop entry claims (`linux/data/org.neoscad.NeoSCAD.desktop`)
/// and the MIME definition declares, OpenSCAD's own name.
pub const MIME_TYPE: &str = "application/x-openscad";

/// The desktop entry's id, as gio names installed apps.
pub const DESKTOP_ID: &str = "org.neoscad.NeoSCAD.desktop";

/// Set to anything to stop the launch dialog for this run (the smoke test
/// and anyone scripting the app); CI (`CI` set) never asks either.
/// Preferences still shows the row and its button.
pub const NO_PROMPT_ENV: &str = "NEOSCAD_NO_DEFAULT_APP_PROMPT";

/// Whether the launch dialog should be shown. Every input is a fact the
/// GTK side looked up, so the rule itself is tested without a desktop.
/// `suppressed` is the user's "Don't ask again", or the environment's off
/// switch ([`suppressed`]).
pub fn should_ask(
    sandboxed: bool,
    entry_found: bool,
    already_default: bool,
    suppressed: bool,
) -> bool {
    can_make_default(sandboxed, entry_found) && !already_default && !suppressed
}

/// Whether "Make Default" can work: outside a sandbox, with our desktop
/// entry installed. Preferences shows its button only then.
pub fn can_make_default(sandboxed: bool, entry_found: bool) -> bool {
    !sandboxed && entry_found
}

/// The launch dialog is off: the user said "Don't ask again", or this run
/// has [`NO_PROMPT_ENV`] or `CI` set (so tests and CI's smoke run never
/// stop at a dialog).
pub fn suppressed(dont_ask: bool, no_prompt_env: bool, ci: bool) -> bool {
    dont_ask || no_prompt_env || ci
}

/// Inside a sandbox whose `~/.config` is not the user's: a Flatpak
/// (`/.flatpak-info` exists in every Flatpak sandbox, and `FLATPAK_ID` is
/// set there) or a Snap (`SNAP` is set in every snap's environment).
pub fn sandboxed(flatpak_info_exists: bool, flatpak_id: bool, snap: bool) -> bool {
    flatpak_info_exists || flatpak_id || snap
}

/// Whether the system's default app for [`MIME_TYPE`], by gio id, is
/// NeoSCAD.
pub fn is_default(default_id: Option<&str>) -> bool {
    default_id == Some(DESKTOP_ID)
}

/// What Preferences says about the default app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// NeoSCAD opens OpenSCAD files.
    Default,
    /// Another app does (its name), or none; "Make Default" can work.
    Other(Option<String>),
    /// In a Flatpak or Snap: the desktop's settings choose.
    Sandboxed,
    /// No desktop entry installed: nothing to make default.
    NotInstalled,
}

impl Status {
    /// From the same facts as [`should_ask`], plus the current default's
    /// name for the message.
    pub fn new(
        sandboxed: bool,
        entry_found: bool,
        already_default: bool,
        default_name: Option<String>,
    ) -> Status {
        if already_default {
            Status::Default
        } else if sandboxed {
            Status::Sandboxed
        } else if !entry_found {
            Status::NotInstalled
        } else {
            Status::Other(default_name)
        }
    }

    /// The row's subtitle.
    pub fn message(&self) -> String {
        match self {
            Status::Default => "NeoSCAD opens OpenSCAD files".into(),
            Status::Other(Some(name)) => format!("OpenSCAD files open in {name}"),
            Status::Other(None) => "No app is the default for OpenSCAD files".into(),
            Status::Sandboxed => "Choose the app for OpenSCAD files in your desktop's settings: \
                 an app in a sandbox can't change it"
                .into(),
            Status::NotInstalled => {
                "NeoSCAD's desktop entry isn't installed, so it can't be the default app".into()
            }
        }
    }

    /// Whether the row offers "Make Default".
    pub fn offers_button(&self) -> bool {
        matches!(self, Status::Other(_))
    }
}

/// What is kept between runs, in `default-app.json` under the user's
/// configuration directory ([`settings_path`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settings {
    /// The user ticked "Don't ask again" in the launch dialog.
    pub dont_ask: bool,
}

impl Settings {
    /// The settings in `bytes`, asking for anything missing or malformed:
    /// a damaged file only costs one more question.
    pub fn from_json(bytes: &[u8]) -> Settings {
        let v: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
        Settings {
            dont_ask: v["dontAsk"].as_bool().unwrap_or(false),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&json!({"dontAsk": self.dont_ask})).unwrap_or_default()
    }

    /// The file at `path`, or the defaults when it is missing.
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
        let tmp = dir.join(format!("default-app.{}.tmp", std::process::id()));
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }
}

/// Where the settings live: `<config>/neoscad/default-app.json`, beside
/// `updates.json`, `language.json` and `agents.json`.
pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("neoscad").join("default-app.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_only_when_it_can_work_and_is_wanted() {
        // Installed, not sandboxed, not default, not suppressed: ask.
        assert!(should_ask(false, true, false, false));
        // A sandbox's mimeapps.list is not the desktop's.
        assert!(!should_ask(true, true, false, false));
        // No desktop entry: nothing to make default.
        assert!(!should_ask(false, false, false, false));
        // Already the default.
        assert!(!should_ask(false, true, true, false));
        // "Don't ask again", or the environment's off switch.
        assert!(!should_ask(false, true, false, true));
        // Every other combination is a no as well.
        for bits in 0..16u8 {
            let [s, e, d, x] = [0, 1, 2, 3].map(|i| bits & (1 << i) != 0);
            assert_eq!(should_ask(s, e, d, x), !s && e && !d && !x, "{bits:04b}");
        }
    }

    #[test]
    fn tests_and_ci_never_ask() {
        assert!(!suppressed(false, false, false));
        assert!(suppressed(true, false, false));
        assert!(suppressed(false, true, false));
        assert!(suppressed(false, false, true));
        assert!(!should_ask(
            false,
            true,
            false,
            suppressed(false, false, true)
        ));
    }

    #[test]
    fn sandbox_and_default_detection() {
        assert!(!sandboxed(false, false, false));
        assert!(sandboxed(true, false, false));
        assert!(sandboxed(false, true, false));
        assert!(sandboxed(false, false, true));
        assert!(is_default(Some("org.neoscad.NeoSCAD.desktop")));
        assert!(!is_default(Some("org.openscad.OpenSCAD.desktop")));
        assert!(!is_default(None));
    }

    #[test]
    fn preferences_status() {
        assert_eq!(Status::new(false, true, true, None), Status::Default);
        // Default wins over the other facts: it is simply true.
        assert_eq!(Status::new(true, false, true, None), Status::Default);
        assert_eq!(Status::new(true, true, false, None), Status::Sandboxed);
        assert_eq!(Status::new(false, false, false, None), Status::NotInstalled);
        let other = Status::new(false, true, false, Some("OpenSCAD".into()));
        assert_eq!(other, Status::Other(Some("OpenSCAD".into())));
        assert!(other.offers_button());
        assert_eq!(other.message(), "OpenSCAD files open in OpenSCAD");
        assert!(Status::Other(None).offers_button());
        for s in [Status::Default, Status::Sandboxed, Status::NotInstalled] {
            assert!(!s.offers_button(), "{s:?}");
        }
    }

    #[test]
    fn settings_round_trip_and_default_to_asking() {
        let s = Settings { dont_ask: true };
        assert_eq!(Settings::from_json(s.to_json().as_bytes()), s);
        assert_eq!(Settings::from_json(b"{\"dontAsk\": 1"), Settings::default());
        assert!(!Settings::from_json(b"[]").dont_ask);
        let dir = std::env::temp_dir().join(format!("neoscad-default-app-{}", std::process::id()));
        let path = settings_path(&dir);
        assert!(path.ends_with("neoscad/default-app.json"));
        assert_eq!(Settings::load(&path), Settings::default());
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
