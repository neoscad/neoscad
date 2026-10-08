//! Preferences > Language: which of NeoSCAD's language extensions every
//! window runs its document with (docs/language-extensions.md, section
//! 2), the Linux port of the macOS app's `LanguageSettings.swift`.
//! Constrained sketches (`--enable sketch`), geometry queries (`--enable
//! query`) and STEP export with exact surfaces (`--enable exact`, which
//! adds STEP to File > Export), all off by default as on the command line:
//! off, a file means exactly what it means in OpenSCAD. `part()` keeps its
//! per-window toggle in the check and measure panels.
//!
//! The names go to each window's document loop (`DocumentLoop::set_enable`,
//! so every run, check, measure and export takes them) and to its language
//! server (`Language::set_enable`, for the sketch vocabulary in
//! completion, hover and navigation); changing one runs open documents
//! again.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The extensions on, kept in `language.json` under the user's
/// configuration directory ([`settings_path`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settings {
    /// Constrained sketches: `sketch() { ... }`.
    pub sketch: bool,
    /// Geometry queries: `anchor()`, `child_anchors()`, `child_bounds()`,
    /// `child_measure()` and `child_distance()`.
    pub query: bool,
    /// STEP export with exact surfaces (File > Export > STEP).
    pub exact: bool,
}

impl Settings {
    /// The settings in `bytes`, off for anything missing or malformed: a
    /// damaged file must not turn an extension on.
    pub fn from_json(bytes: &[u8]) -> Settings {
        let v: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
        Settings {
            sketch: v["sketch"].as_bool().unwrap_or(false),
            query: v["query"].as_bool().unwrap_or(false),
            exact: v["exact"].as_bool().unwrap_or(false),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(
            &json!({"sketch": self.sketch, "query": self.query, "exact": self.exact}),
        )
        .unwrap_or_default()
    }

    /// The file at `path`, or everything off when it is missing.
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
        let tmp = dir.join(format!("language.{}.tmp", std::process::id()));
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    /// The `--enable` names runs and language servers take.
    pub fn names(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.sketch {
            out.push("sketch".to_string());
        }
        if self.query {
            out.push("query".to_string());
        }
        if self.exact {
            out.push("exact".to_string());
        }
        out
    }
}

/// Where the settings live: `<config>/neoscad/language.json`, beside the
/// update check's `updates.json`.
pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("neoscad").join("language.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_default_to_off() {
        let s = Settings {
            sketch: true,
            query: false,
            exact: false,
        };
        assert_eq!(Settings::from_json(s.to_json().as_bytes()), s);
        assert_eq!(s.names(), ["sketch"]);
        let all = Settings {
            sketch: true,
            query: true,
            exact: true,
        };
        assert_eq!(Settings::from_json(all.to_json().as_bytes()), all);
        assert_eq!(all.names(), ["sketch", "query", "exact"]);
        // Damage turns nothing on.
        assert_eq!(Settings::from_json(b"{\"sketch\": 1"), Settings::default());
        assert_eq!(Settings::from_json(b"[]").names(), Vec::<String>::new());
    }

    #[test]
    fn settings_save_and_load() {
        let dir = std::env::temp_dir().join(format!("neoscad-language-{}", std::process::id()));
        let path = settings_path(&dir);
        assert_eq!(Settings::load(&path), Settings::default());
        let s = Settings {
            sketch: false,
            query: true,
            exact: true,
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
