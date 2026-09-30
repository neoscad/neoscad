//! One window's document, without GTK: where it lives, its text (the
//! host's copy that Save writes), whether it has unsaved changes, and the
//! title the header bar shows.

use std::path::{Path, PathBuf};

use client::EditorText;

use crate::bridge::EditKind;

/// A document: a file, or untitled (new, or opened from an example).
#[derive(Debug, Clone)]
pub struct Document {
    /// The file it was read from or last saved to.
    file: Option<PathBuf>,
    /// The path the core knows an untitled document by: a file name in a
    /// real folder that no file has (`Client::untitled_path`), so its
    /// includes resolve beside it as a saved file's would.
    untitled: String,
    /// The text, kept in step with the editor's (`bridge.rs`).
    pub text: EditorText,
    /// Changes made (an undo takes one back), and the count at the last
    /// save; `None` after a change that no undo can take back (a resync),
    /// so the document stays edited until it is saved.
    changes: i64,
    saved: Option<i64>,
}

impl Document {
    /// An untitled document with `text`, known to the core as `core_path`.
    /// An example's text starts clean: closing it unchanged loses nothing.
    pub fn untitled(core_path: String, text: String) -> Document {
        Document {
            file: None,
            untitled: core_path,
            text: EditorText::new(text),
            changes: 0,
            saved: Some(0),
        }
    }

    /// A document read from `path`.
    pub fn from_file(path: PathBuf, text: String) -> Document {
        Document {
            untitled: path.to_string_lossy().into_owned(),
            file: Some(path),
            text: EditorText::new(text),
            changes: 0,
            saved: Some(0),
        }
    }

    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// The path the core's session knows the document by.
    pub fn core_path(&self) -> String {
        match &self.file {
            Some(f) => f.to_string_lossy().into_owned(),
            None => self.untitled.clone(),
        }
    }

    /// The name shown: the file's, or the untitled path's.
    pub fn name(&self) -> String {
        let p = self
            .file
            .clone()
            .unwrap_or_else(|| PathBuf::from(&self.untitled));
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled.scad".into())
    }

    /// A transaction was applied.
    pub fn record(&mut self, kind: EditKind) {
        self.changes += match kind {
            EditKind::Undo => -1,
            EditKind::Edit | EditKind::Redo => 1,
        };
    }

    /// The editor's text replaced the copy after they disagreed: the
    /// editor had changes the copy missed, so the document is edited, and
    /// no undo count can say when it is clean again.
    pub fn record_resync(&mut self, text: String) {
        self.text.replace(text);
        self.saved = None;
    }

    /// Written to `path` (Save, Save As).
    pub fn saved_to(&mut self, path: PathBuf) {
        self.file = Some(path);
        self.saved = Some(self.changes);
    }

    pub fn is_dirty(&self) -> bool {
        self.saved != Some(self.changes)
    }

    /// An untitled document nobody has typed into: File > Open and
    /// Examples replace it rather than opening another window, as GNOME
    /// Text Editor reuses an empty tab.
    pub fn is_replaceable(&self) -> bool {
        self.file.is_none() && !self.is_dirty() && self.text.byte_length() == 0
    }

    /// The header bar's title and subtitle, and the window's title (what
    /// the shell's overview and alt-tab show).
    pub fn titles(&self) -> Titles {
        let name = self.name();
        let dirty = if self.is_dirty() { "• " } else { "" };
        let subtitle = match &self.file {
            Some(f) => f
                .parent()
                .map(|d| display_dir(d, std::env::var_os("HOME").map(PathBuf::from)))
                .unwrap_or_default(),
            None => "Unsaved".into(),
        };
        Titles {
            title: format!("{dirty}{name}"),
            subtitle,
            window: format!("{dirty}{name} — NeoSCAD"),
        }
    }
}

/// See [`Document::titles`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Titles {
    pub title: String,
    pub subtitle: String,
    pub window: String,
}

/// A folder as GNOME apps show it: under the home folder as `~/...`.
pub fn display_dir(dir: &Path, home: Option<PathBuf>) -> String {
    match home.as_deref().and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => dir.display().to_string(),
    }
}

/// A file's bytes as text. Not UTF-8 is refused rather than decoded
/// lossily, as the macOS app does: saving it back would silently replace
/// the bytes that did not decode.
pub fn decode(bytes: Vec<u8>) -> Result<String, String> {
    String::from_utf8(bytes).map_err(|e| {
        format!(
            "The file is not UTF-8 text (byte {} is not valid), which is how OpenSCAD reads files.",
            e.utf8_error().valid_up_to()
        )
    })
}

/// A file name for an export of the document named `name`: its stem with
/// `extension` (`CSG.scad` -> `CSG.stl`).
pub fn export_name(name: &str, extension: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".into());
    format!("{stem}.{extension}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_and_undos_track_the_saved_state() {
        let mut d = Document::untitled("/home/u/Untitled.scad".into(), String::new());
        assert!(!d.is_dirty());
        assert!(d.is_replaceable());
        d.record(EditKind::Edit);
        assert!(d.is_dirty());
        assert_eq!(d.titles().title, "• Untitled.scad");
        assert_eq!(d.titles().window, "• Untitled.scad — NeoSCAD");
        assert_eq!(d.titles().subtitle, "Unsaved");
        d.record(EditKind::Undo);
        assert!(!d.is_dirty(), "undone to the saved text");
        d.record(EditKind::Redo);
        d.saved_to("/home/u/a.scad".into());
        assert!(!d.is_dirty());
        assert_eq!(d.core_path(), "/home/u/a.scad");
        assert_eq!(d.name(), "a.scad");
        d.record(EditKind::Undo);
        assert!(d.is_dirty(), "undone past the save");
    }

    #[test]
    fn a_resync_stays_edited_until_saved() {
        let mut d = Document::from_file("/p/x.scad".into(), "a".into());
        d.record_resync("b".into());
        assert!(d.is_dirty());
        d.record(EditKind::Undo);
        assert!(d.is_dirty());
        assert_eq!(d.text.text(), "b");
        d.saved_to("/p/x.scad".into());
        assert!(!d.is_dirty());
        assert!(!d.is_replaceable());
    }

    #[test]
    fn folders_under_home_are_abbreviated() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(display_dir(Path::new("/home/u/cad"), home.clone()), "~/cad");
        assert_eq!(display_dir(Path::new("/home/u"), home.clone()), "~");
        assert_eq!(display_dir(Path::new("/srv/x"), home), "/srv/x");
    }

    #[test]
    fn text_must_be_utf8_and_exports_take_the_stem() {
        assert_eq!(decode(b"cube(1);".to_vec()), Ok("cube(1);".into()));
        assert!(decode(vec![b'a', 0xff]).unwrap_err().contains("byte 1"));
        assert_eq!(export_name("CSG.scad", "stl"), "CSG.stl");
        assert_eq!(export_name("", "png"), "Untitled.png");
    }
}
