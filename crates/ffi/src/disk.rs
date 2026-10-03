//! The open document's own file for the macOS and Windows apps:
//! `client::DiskTracker` across the bridge, with the reads it needs.
//!
//! The tracker decides what a change on disk means (reload in place, a
//! conflict to show, a missing file) and whether Save may write; see
//! `crates/client/src/disk.rs`. This crate may touch the disk, so the
//! reads are done here rather than in each host: both apps then read the
//! file the same way, as bytes (a decoding read would hide a byte-order
//! mark or text that is not UTF-8, and the stamps would never match).

use std::io::ErrorKind;
use std::sync::{Arc, Mutex, PoisonError};

use crate::{CoreError, EditorText, guarded};

pub use client::{DiskAction, ReloadEdit, SaveCheck};

/// One replacement for the editor's `agentEdit` (0-based lines, UTF-16
/// columns, in the text before any edit of the batch applies).
#[uniffi::remote(Record)]
pub struct ReloadEdit {
    pub start_line: u64,
    pub start_character: u64,
    pub end_line: u64,
    pub end_character: u64,
    pub insert: String,
}

/// What to do about the file as it is now (`client::DiskAction`).
#[uniffi::remote(Enum)]
pub enum DiskAction {
    /// Nothing (the app's own save, seen by its watcher; a change already
    /// reported).
    None,
    /// The file may be half written: call `check` again after `ms`.
    ReadAgain { ms: u64 },
    /// The file holds the document's text: mark it saved, hide the notice.
    Saved,
    /// Clean document: apply the edits with `agentEdit`
    /// (`reload_edits_json`); `editor_changed` then says when it is clean.
    Reload { edits: Vec<ReloadEdit> },
    /// Unsaved changes, and the file changed: show the notice.
    Conflict { reloadable: bool },
    /// The file is gone (deleted or moved away): show the notice.
    Missing,
    /// The file is back as it was: hide the notice.
    Resolved,
}

/// Whether Save may write (`client::SaveCheck`).
#[uniffi::remote(Enum)]
pub enum SaveCheck {
    Write,
    Changed,
}

/// Reads of a file that failed (other than its absence) before `check`
/// gives up on the change: Windows refuses a read while another program
/// holds the file open for writing, which passes; a file the app may not
/// read does not.
const MAX_FAILED_READS: u32 = 4;

/// A document's file on disk, as the app last read or wrote it.
#[derive(Debug, uniffi::Object)]
pub struct DocumentFile {
    inner: Mutex<(client::DiskTracker, u32)>,
}

impl DocumentFile {
    fn lock(&self) -> std::sync::MutexGuard<'_, (client::DiskTracker, u32)> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The file's bytes; `Ok(None)` if there is none.
fn read(path: &str) -> Result<Option<Vec<u8>>, std::io::Error> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[uniffi::export]
impl DocumentFile {
    /// Not a `Result`, as `EditorText::new` is not: it allocates nothing
    /// that could fail.
    #[uniffi::constructor]
    pub fn new() -> Arc<DocumentFile> {
        Arc::new(DocumentFile {
            inner: Mutex::new((client::DiskTracker::new(), 0)),
        })
    }

    /// The file was read (`bytes`, as read): that is the file as the
    /// document knows it.
    pub fn loaded(&self, bytes: Vec<u8>) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().0.loaded(&bytes);
            Ok(())
        })
    }

    /// The file was written with `bytes` (Save, Save As).
    pub fn saved(&self, bytes: Vec<u8>) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().0.saved(&bytes);
            Ok(())
        })
    }

    /// The document has no file any more.
    pub fn forget(&self) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().0.forget();
            Ok(())
        })
    }

    /// The watcher saw `path` change: what to do. `text` is the document's
    /// copy, `dirty` whether it has unsaved changes.
    pub fn check(
        &self,
        path: String,
        text: Arc<EditorText>,
        dirty: bool,
    ) -> Result<DiskAction, CoreError> {
        guarded(|| {
            let mut me = self.lock();
            let disk = match read(&path) {
                Ok(d) => {
                    me.1 = 0;
                    d
                }
                Err(_) if me.1 < MAX_FAILED_READS => {
                    me.1 += 1;
                    return Ok(DiskAction::ReadAgain {
                        ms: client::DISK_READ_AGAIN_MS,
                    });
                }
                Err(_) => {
                    me.1 = 0;
                    return Ok(DiskAction::None);
                }
            };
            let buffer = text.lock().text();
            Ok(me.0.check(disk.as_deref(), &buffer, dirty))
        })
    }

    /// The user chose Reload: the edits that make `text` the file's
    /// text, or nothing if it is gone or not UTF-8.
    pub fn reload(
        &self,
        path: String,
        text: Arc<EditorText>,
    ) -> Result<Option<Vec<ReloadEdit>>, CoreError> {
        guarded(|| {
            let disk = read(&path).map_err(|e| CoreError::Failed {
                message: e.to_string(),
            })?;
            let buffer = text.lock().text();
            Ok(self.lock().0.reload(disk.as_deref(), &buffer))
        })
    }

    /// The user chose Keep mine.
    pub fn keep_mine(&self) -> Result<(), CoreError> {
        guarded(|| {
            self.lock().0.keep_mine();
            Ok(())
        })
    }

    /// The editor reported a change: true if it was a reload landing and
    /// the document now matches the file (mark it saved).
    pub fn editor_changed(&self, text: Arc<EditorText>) -> Result<bool, CoreError> {
        guarded(|| {
            let mut me = self.lock();
            if !me.0.is_pending() {
                return Ok(false);
            }
            let t = text.lock();
            Ok(me.0.editor_changed(t.bytes()))
        })
    }

    /// Before Save writes to `path`: whether it may. A file that cannot be
    /// read cannot be compared, and the write will report its own error.
    pub fn save_check(&self, path: String) -> Result<SaveCheck, CoreError> {
        guarded(|| {
            let disk = read(&path).ok().flatten();
            Ok(self.lock().0.save_check(disk.as_deref()))
        })
    }

    /// Whether a notice is showing.
    pub fn is_reporting(&self) -> Result<bool, CoreError> {
        guarded(|| Ok(self.lock().0.is_reporting()))
    }
}

/// `edits` as the editor's `agentEdit` takes them, as JSON
/// (`client::agent_edit_json`).
#[uniffi::export]
pub fn reload_edits_json(edits: Vec<ReloadEdit>) -> String {
    client::agent_edit_json(&edits)
}

/// `text` with `edits` applied, for a document whose editor is not up yet
/// (`client::apply_reload_edits`).
#[uniffi::export]
pub fn apply_reload_edits(text: String, edits: Vec<ReloadEdit>) -> String {
    client::apply_reload_edits(&text, &edits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_on_disk_reloads_a_clean_document_and_save_checks_the_disk() {
        let dir = std::env::temp_dir().join(format!("neoscad-ffi-disk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("doc.scad");
        let p = path.to_string_lossy().into_owned();
        std::fs::write(&path, "cube(1);\n").unwrap();
        let file = DocumentFile::new();
        file.loaded(std::fs::read(&path).unwrap()).unwrap();
        let text = EditorText::new("cube(1);\n".into());
        assert_eq!(
            file.check(p.clone(), text.clone(), false).unwrap(),
            DiskAction::None
        );
        std::fs::write(&path, "cube(2);\n").unwrap();
        let DiskAction::Reload { edits } = file.check(p.clone(), text.clone(), false).unwrap()
        else {
            panic!("no reload")
        };
        assert_eq!(
            reload_edits_json(edits),
            r#"[{"from":[0,5],"insert":"2","to":[0,6]}]"#
        );
        text.replace("cube(2);\n".into()).unwrap();
        assert!(file.editor_changed(text.clone()).unwrap());
        assert_eq!(file.save_check(p.clone()).unwrap(), SaveCheck::Write);
        // Changed again behind a dirty document: Save must ask.
        std::fs::write(&path, "cube(3);\n").unwrap();
        text.replace("sphere(1);\n".into()).unwrap();
        assert_eq!(
            file.check(p.clone(), text.clone(), true).unwrap(),
            DiskAction::Conflict { reloadable: true }
        );
        assert_eq!(file.save_check(p.clone()).unwrap(), SaveCheck::Changed);
        // Deleted: read again once, then reported; Save may write.
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            file.check(p.clone(), text.clone(), true).unwrap(),
            DiskAction::ReadAgain { .. }
        ));
        assert_eq!(
            file.check(p.clone(), text.clone(), true).unwrap(),
            DiskAction::Missing
        );
        assert_eq!(file.save_check(p).unwrap(), SaveCheck::Write);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
