//! One window's document, without GTK: where it lives, its text (the
//! host's copy that Save writes), whether it has unsaved changes, and the
//! title the header bar shows.

use std::path::{Path, PathBuf};

use client::{DiskAction, DiskTracker, EditorText, ReloadEdit, SaveCheck};

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
    /// The file as last read or written, to tell another program's change
    /// from this window's own save (`client::DiskTracker`).
    disk: DiskTracker,
    /// Reads of the file that failed in a row (other than its absence).
    failed_reads: u32,
}

/// Failed reads of a changed file before the change is let go: a read
/// can fail while another program holds the file, which passes; a file
/// this app may not read does not. (The macOS and Windows apps' core
/// counts the same, `crates/ffi/src/disk.rs`.)
const MAX_FAILED_READS: u32 = 4;

/// A file's bytes; `Ok(None)` if there is none.
pub fn read_disk(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
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
            disk: DiskTracker::new(),
            failed_reads: 0,
        }
    }

    /// A document read from `path`. `text` is the file's bytes exactly
    /// (`decode` refuses rather than alters them), so it is also what the
    /// file is known to hold.
    pub fn from_file(path: PathBuf, text: String) -> Document {
        let mut disk = DiskTracker::new();
        disk.loaded(text.as_bytes());
        Document {
            untitled: path.to_string_lossy().into_owned(),
            file: Some(path),
            text: EditorText::new(text),
            changes: 0,
            saved: Some(0),
            disk,
            failed_reads: 0,
        }
    }

    /// The watcher saw the file change: what that means for this document
    /// (see `client::DiskAction`).
    pub fn disk_check(&mut self) -> DiskAction {
        let Some(path) = &self.file else {
            return DiskAction::None;
        };
        let disk = match read_disk(path) {
            Ok(d) => d,
            Err(_) if self.failed_reads < MAX_FAILED_READS => {
                self.failed_reads += 1;
                return DiskAction::ReadAgain {
                    ms: client::DISK_READ_AGAIN_MS,
                };
            }
            Err(_) => {
                self.failed_reads = 0;
                return DiskAction::None;
            }
        };
        self.failed_reads = 0;
        let dirty = self.is_dirty();
        let action = self.disk.check(disk.as_deref(), &self.text.text(), dirty);
        if action == DiskAction::Saved {
            self.mark_saved();
        }
        action
    }

    /// The notice's Reload: the edits that make the text the file's, or
    /// `None` if it is gone or not UTF-8.
    pub fn disk_reload(&mut self) -> Option<Vec<ReloadEdit>> {
        let disk = read_disk(self.file.as_deref()?).ok()?;
        self.disk.reload(disk.as_deref(), &self.text.text())
    }

    /// The notice's Keep mine.
    pub fn keep_mine(&mut self) {
        self.disk.keep_mine();
    }

    /// Whether a notice about the file is showing.
    pub fn disk_notice(&self) -> bool {
        self.disk.is_reporting()
    }

    /// The editor applied a change: if it was a reload landing, the text
    /// is the file's again and the document is saved. True then.
    pub fn reload_landed(&mut self) -> bool {
        if self.disk.is_pending() && self.disk.editor_changed(self.text.bytes()) {
            self.mark_saved();
            return true;
        }
        false
    }

    /// A reload with no editor to apply it (the page not up yet): the
    /// text changes here, and the editor shows it when it loads.
    pub fn reload_without_editor(&mut self, edits: &[ReloadEdit]) {
        let text = client::apply_reload_edits(&self.text.text(), edits);
        self.text.replace(text);
        self.reload_landed();
    }

    /// Before Save writes to the document's own file: whether it may.
    pub fn save_check(&self) -> SaveCheck {
        match &self.file {
            Some(path) => self
                .disk
                .save_check(read_disk(path).ok().flatten().as_deref()),
            None => SaveCheck::Write,
        }
    }

    /// The text matches the file again (a reload landed; another program
    /// saved this very text).
    fn mark_saved(&mut self) {
        self.saved = Some(self.changes);
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

    /// Written to `path` (Save, Save As): the text is now the file's.
    pub fn saved_to(&mut self, path: PathBuf) {
        self.file = Some(path);
        self.saved = Some(self.changes);
        self.disk.saved(self.text.bytes());
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
    fn another_programs_change_reloads_a_clean_document_and_conflicts_with_a_dirty_one() {
        let dir =
            std::env::temp_dir().join(format!("neoscad-linux-app-disk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("model.scad");
        std::fs::write(&path, "cube(1);\n").unwrap();
        let text = decode(std::fs::read(&path).unwrap()).unwrap();
        let mut d = Document::from_file(path.clone(), text);
        // The watcher's echo of nothing new.
        assert_eq!(d.disk_check(), DiskAction::None);

        // Clean: reloaded in place (here without an editor), still clean.
        std::fs::write(&path, "cube(2);\n").unwrap();
        let DiskAction::Reload { edits } = d.disk_check() else {
            panic!("no reload")
        };
        d.record(EditKind::Edit); // the editor's report of the reload
        d.reload_without_editor(&edits);
        assert_eq!(d.text.text(), "cube(2);\n");
        assert!(!d.is_dirty());
        assert_eq!(d.save_check(), SaveCheck::Write);
        // Undoing the reload is an edit away from the file.
        d.record(EditKind::Undo);
        assert!(d.is_dirty());
        d.record(EditKind::Redo);

        // Dirty: a conflict, and Save asks.
        d.record(EditKind::Edit);
        d.text.replace("sphere(1);\n".into());
        std::fs::write(&path, "cube(3);\n").unwrap();
        assert_eq!(d.disk_check(), DiskAction::Conflict { reloadable: true });
        assert!(d.disk_notice());
        d.keep_mine();
        assert_eq!(d.disk_check(), DiskAction::None);
        assert_eq!(d.save_check(), SaveCheck::Changed);
        // Reload from the notice takes theirs.
        let edits = d.disk_reload().unwrap();
        d.record(EditKind::Edit);
        d.reload_without_editor(&edits);
        assert_eq!(d.text.text(), "cube(3);\n");
        assert!(!d.is_dirty());

        // Saved over: the save is the file, and its echo is nothing.
        d.record(EditKind::Edit);
        d.text.replace("cube(4);\n".into());
        std::fs::write(&path, d.text.bytes()).unwrap();
        d.saved_to(path.clone());
        assert_eq!(d.disk_check(), DiskAction::None);

        // Deleted: reported after a second look; Save may write it again.
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(d.disk_check(), DiskAction::ReadAgain { .. }));
        assert_eq!(d.disk_check(), DiskAction::Missing);
        assert_eq!(d.save_check(), SaveCheck::Write);
        std::fs::remove_dir_all(&dir).unwrap();

        // An untitled document has no file to watch.
        let mut u = Document::untitled("/nonexistent/Untitled.scad".into(), String::new());
        assert_eq!(u.disk_check(), DiskAction::None);
        assert_eq!(u.save_check(), SaveCheck::Write);
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
