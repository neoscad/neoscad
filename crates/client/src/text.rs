//! Editor edits in UTF-16 code units, applied to UTF-8 text.
//!
//! Every editor the hosts embed (CodeMirror in WebKit, WebKitGTK and
//! WebView2) counts UTF-16 units; the session counts UTF-8 bytes. The two
//! agree on ASCII only: "é" is one unit and two bytes, "漢" one and three,
//! "😀" two and four. The conversion itself is `lang::source`'s
//! ([`lang::source::byte_offset_of_utf16`]); this module applies a batch of
//! edits with it, so a host sends the editor's edits as they come and never
//! counts bytes itself.

use serde::{Deserialize, Serialize};

use crate::{Client, CoreError, DocInfo, TextEdit};
use lang::source::{Utf16OffsetError, byte_offset_of_utf16, utf16_len};

/// A replacement in an editor's UTF-16 offsets: `from..to` becomes
/// `insert`. Edits of one batch apply in order, each to the text the
/// previous one left (CodeMirror's `iterChanges` with `fromB`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Utf16Edit {
    pub from: u64,
    pub to: u64,
    pub insert: String,
}

fn offset_error(e: Utf16OffsetError) -> CoreError {
    CoreError::InvalidArgument {
        message: match e {
            Utf16OffsetError::OutOfRange(o) => {
                format!("editor offset {o} is past the end of the text")
            }
            Utf16OffsetError::SplitsCharacter(o) => {
                format!("editor offset {o} falls inside a character")
            }
        },
    }
}

/// Apply `edits` to `text` and return them in UTF-8 byte offsets (what
/// [`Client::edit`] and the session take).
///
/// An error means the editor's text and `text` no longer agree; `text` then
/// holds the edits before the failing one, and the host must replace it
/// with the editor's whole text (the bridge's `resync`).
pub fn apply_utf16_edits(
    text: &mut Vec<u8>,
    edits: &[Utf16Edit],
) -> Result<Vec<TextEdit>, CoreError> {
    let mut out = Vec::with_capacity(edits.len());
    for e in edits {
        if e.to < e.from {
            return Err(offset_error(Utf16OffsetError::OutOfRange(e.to)));
        }
        let start = byte_offset_of_utf16(text, e.from).map_err(offset_error)?;
        let end = byte_offset_of_utf16(text, e.to).map_err(offset_error)?;
        text.splice(start..end, e.insert.bytes());
        out.push(TextEdit {
            start: start as u64,
            end: end as u64,
            text: e.insert.clone(),
        });
    }
    Ok(out)
}

/// A host's copy of a document's text, edited in UTF-16 offsets: what
/// NSDocument (or a GTK or WinUI document) saves, kept in step with the
/// editor's. Its UTF-16 length is kept with each edit, because the bridge
/// checks it after every transaction and counting a long text anew for
/// each keystroke is a scan of it.
#[derive(Debug, Clone, Default)]
pub struct EditorText {
    bytes: Vec<u8>,
    utf16_length: u64,
}

impl EditorText {
    pub fn new(text: String) -> EditorText {
        let mut t = EditorText::default();
        t.replace(text);
        t
    }

    /// Replace the whole text (a file read, the editor's text after the
    /// copies disagreed).
    pub fn replace(&mut self, text: String) {
        self.bytes = text.into_bytes();
        self.utf16_length = u64::from(utf16_len(&self.bytes));
    }

    /// Apply the editor's edits; the same edits in byte offsets. On an
    /// error the text keeps the edits before the failing one (see
    /// [`apply_utf16_edits`]) and the length is recounted.
    pub fn apply(&mut self, edits: &[Utf16Edit]) -> Result<Vec<TextEdit>, CoreError> {
        match apply_utf16_edits(&mut self.bytes, edits) {
            Ok(out) => {
                for e in edits {
                    let inserted = e.insert.encode_utf16().count() as u64;
                    self.utf16_length = self.utf16_length + inserted - (e.to - e.from);
                }
                Ok(out)
            }
            Err(e) => {
                self.utf16_length = u64::from(utf16_len(&self.bytes));
                Err(e)
            }
        }
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn utf16_length(&self) -> u64 {
        self.utf16_length
    }

    pub fn byte_length(&self) -> u64 {
        self.bytes.len() as u64
    }
}

impl Client {
    /// Apply an editor's edits (UTF-16 offsets) to a document's buffer in
    /// the session, for a host that keeps no copy of its own. With
    /// `utf16_length` (the editor's length after the edits), a text that
    /// no longer agrees is refused rather than half-edited: the host then
    /// sends the whole text with [`Client::update`].
    pub fn edit_utf16(
        &self,
        path: &str,
        edits: &[Utf16Edit],
        utf16_length: Option<u64>,
    ) -> Result<DocInfo, CoreError> {
        let doc = self.doc_path(path)?;
        let mut text = self.text_now(&doc)?.to_vec();
        let byte_edits = apply_utf16_edits(&mut text, edits)?;
        if let Some(want) = utf16_length {
            let have = u64::from(utf16_len(&text));
            if have != want {
                return Err(CoreError::InvalidArgument {
                    message: format!(
                        "the editor's text ({want} UTF-16 units) and the document's ({have}) disagree"
                    ),
                });
            }
        }
        self.edit(path, byte_edits)
    }
}
