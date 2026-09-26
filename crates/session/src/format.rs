//! `neoscad fmt` as a session operation: a file's text (an open
//! document's unsaved buffer when there is one), its nearest
//! `.neoscad-fmt.toml`, and the formatter (`scadfmt`). The session never
//! writes files: the command line writes what it is given back, and a
//! server's client applies the text.

use std::path::{Path, PathBuf};

use lang::loader::FileSystem;
use serde_json::{Value, json};

use crate::Session;

/// What to format.
#[derive(Debug, Clone, Default)]
pub struct FormatRequest {
    /// The file, absolute or relative to `cwd`: read through the session
    /// (unless `text` is given), where the configuration lookup starts
    /// and how messages name it. `None` with `text`: standard input,
    /// configured from `cwd`.
    pub input: Option<String>,
    /// The text to format instead of the file's.
    pub text: Option<Vec<u8>>,
    pub cwd: Option<PathBuf>,
    /// Override the configuration file's settings.
    pub indent: Option<usize>,
    pub width: Option<usize>,
}

/// Why a file was not formatted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatFailure {
    Read(String),
    Config(String),
    Format(scadfmt::Error),
}

impl FormatFailure {
    /// `read`, `config`, `syntax`, `unsupported` or `internal`.
    pub fn kind(&self) -> &'static str {
        match self {
            FormatFailure::Read(_) => "read",
            FormatFailure::Config(_) => "config",
            FormatFailure::Format(scadfmt::Error::Syntax(_)) => "syntax",
            FormatFailure::Format(scadfmt::Error::Unsupported(_)) => "unsupported",
            FormatFailure::Format(scadfmt::Error::Internal(_)) => "internal",
        }
    }
}

impl std::fmt::Display for FormatFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatFailure::Read(m) | FormatFailure::Config(m) => f.write_str(m),
            FormatFailure::Format(e) => write!(f, "{e}"),
        }
    }
}

/// One file's result.
#[derive(Debug, Clone)]
pub struct Formatted {
    /// As the request named it (`<stdin>` for text without a name).
    pub display: String,
    /// The absolute path, when there is a file.
    pub path: Option<PathBuf>,
    pub original: Vec<u8>,
    /// The formatted text, or why there is none (the file is to be left
    /// as it is).
    pub result: Result<Vec<u8>, FormatFailure>,
    pub config: scadfmt::Config,
    pub config_file: Option<PathBuf>,
}

impl Formatted {
    pub fn changed(&self) -> bool {
        self.result.as_ref().is_ok_and(|t| *t != self.original)
    }

    /// A unified diff of the change (empty when unchanged or failed).
    pub fn diff(&self) -> String {
        match &self.result {
            Ok(t) => scadfmt::unified_diff(
                &String::from_utf8_lossy(&self.original),
                &String::from_utf8_lossy(t),
                &format!("{} (original)", self.display),
                &format!("{} (formatted)", self.display),
            ),
            Err(_) => String::new(),
        }
    }

    /// `{"path", "changed", "error", "config"}`, plus the formatted
    /// `text` and the `diff` when asked for.
    pub fn json(&self, text: bool, diff: bool) -> Value {
        let error = match &self.result {
            Ok(_) => Value::Null,
            Err(e) => {
                let errors: Vec<Value> = match e {
                    FormatFailure::Format(scadfmt::Error::Syntax(v)) => v
                        .iter()
                        .map(|s| json!({"line": s.line, "message": s.message}))
                        .collect(),
                    _ => Vec::new(),
                };
                json!({"kind": e.kind(), "message": e.to_string(), "errors": errors})
            }
        };
        let mut v = json!({
            "path": self.display,
            "changed": self.changed(),
            "error": error,
            "config": self.config_file.as_ref().map(|p| p.display().to_string()),
        });
        if text {
            v["text"] = match &self.result {
                Ok(t) => json!(String::from_utf8_lossy(t)),
                Err(_) => Value::Null,
            };
        }
        if diff {
            v["diff"] = json!(self.diff());
        }
        v
    }
}

impl Session {
    /// Format one file or text (see [`FormatRequest`]).
    pub fn format(&self, req: &FormatRequest) -> Formatted {
        let cwd = req.cwd.clone().unwrap_or_else(|| self.cfg.work_dir.clone());
        let path = req.input.as_ref().map(|i| crate::normal(&cwd.join(i)));
        let display = req.input.clone().unwrap_or_else(|| "<stdin>".into());
        let mut out = Formatted {
            display,
            path: path.clone(),
            original: Vec::new(),
            result: Err(FormatFailure::Read(String::new())),
            config: scadfmt::Config::default(),
            config_file: None,
        };
        let text = match (&req.text, &path) {
            (Some(t), _) => t.clone(),
            (None, Some(p)) => match self.fs.read(p) {
                Ok(t) => t,
                Err(e) => {
                    out.result = Err(FormatFailure::Read(format!(
                        "cannot read '{}': {e}",
                        out.display
                    )));
                    return out;
                }
            },
            (None, None) => {
                out.result = Err(FormatFailure::Read("nothing to format".into()));
                return out;
            }
        };
        out.original = text;
        let dir = path
            .as_deref()
            .and_then(Path::parent)
            .map_or(cwd.clone(), Path::to_path_buf);
        match scadfmt::find_config(&*self.fs, &dir) {
            Ok(Some((p, c))) => {
                out.config = c;
                out.config_file = Some(p);
            }
            Ok(None) => {}
            Err(e) => {
                out.result = Err(FormatFailure::Config(e));
                return out;
            }
        }
        if let Some(i) = req.indent {
            out.config.indent = i;
        }
        if let Some(w) = req.width {
            out.config.width = w;
        }
        out.result = scadfmt::format(&out.original, &out.config).map_err(FormatFailure::Format);
        out
    }

    /// The `.scad` files named by `paths` (files as given; directories
    /// searched recursively, skipping hidden ones), relative to `cwd`,
    /// sorted within each directory. `keep` selects files by name in
    /// directories (files named explicitly are always kept).
    pub fn find_files(
        &self,
        paths: &[String],
        cwd: &Path,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Vec<(String, PathBuf)>, String> {
        let mut out = Vec::new();
        for p in paths {
            let abs = crate::normal(&cwd.join(p));
            if self.fs.is_dir(&abs) {
                self.walk(p, &abs, keep, &mut out)?;
            } else if self.fs.exists(&abs) {
                out.push((p.clone(), abs));
            } else {
                return Err(format!("'{p}' does not exist"));
            }
        }
        Ok(out)
    }

    fn walk(
        &self,
        shown: &str,
        dir: &Path,
        keep: &dyn Fn(&str) -> bool,
        out: &mut Vec<(String, PathBuf)>,
    ) -> Result<(), String> {
        let mut entries = self
            .fs
            .read_dir(dir)
            .map_err(|e| format!("cannot list '{shown}': {e}"))?;
        entries.sort();
        for e in entries {
            let Some(name) = e.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let child = if shown == "." {
                name.to_string()
            } else {
                format!("{}/{name}", shown.trim_end_matches('/'))
            };
            if self.fs.is_dir(&e) {
                self.walk(&child, &e, keep, out)?;
            } else if keep(name) {
                out.push((child, e));
            }
        }
        Ok(())
    }
}

/// `.scad` files.
pub fn is_scad(name: &str) -> bool {
    name.ends_with(".scad")
}
