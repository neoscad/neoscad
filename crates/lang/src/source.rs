//! Source files, byte spans and line lookup.
//!
//! Source text is kept as bytes, not `str`. OpenSCAD's lexer is byte-based:
//! a lone `0xA0` (Latin-1 no-break space) is whitespace and any other invalid
//! UTF-8 byte is a syntax error with a precise location. Forcing text through
//! `str` would either reject such files outright or shift every offset after
//! a replacement character.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Index of a file in a [`SourceMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct FileId(pub u32);

/// A half-open byte range in one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(file: FileId, start: u32, end: u32) -> Self {
        Self { file, start, end }
    }

    pub fn len(&self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// One loaded file.
#[derive(Debug)]
pub struct SourceFile {
    /// The path the file was loaded from, as OpenSCAD would report it
    /// (absolute; see `loader`).
    pub path: PathBuf,
    pub text: Box<[u8]>,
    line_starts: OnceLock<Vec<u32>>,
}

impl SourceFile {
    pub fn new(path: PathBuf, text: impl Into<Box<[u8]>>) -> Self {
        Self {
            path,
            text: text.into(),
            line_starts: OnceLock::new(),
        }
    }

    /// A copy for another program's [`SourceMap`] (an included file
    /// parsed once and used by many programs), with its line starts if
    /// they were already worked out.
    pub(crate) fn duplicate(&self) -> Self {
        let line_starts = OnceLock::new();
        if let Some(v) = self.line_starts.get() {
            let _ = line_starts.set(v.clone());
        }
        Self {
            path: self.path.clone(),
            text: self.text.clone(),
            line_starts,
        }
    }

    /// Byte offsets at which lines start. Only `\n` ends a line: OpenSCAD's
    /// lexer counts `\n` and ignores a bare `\r`, so a classic-Mac file is one
    /// long line there too.
    fn line_starts(&self) -> &[u32] {
        self.line_starts.get_or_init(|| {
            let mut v = Vec::with_capacity(self.text.len() / 32 + 1);
            v.push(0);
            v.extend(
                self.text
                    .iter()
                    .enumerate()
                    .filter(|&(_, &b)| b == b'\n')
                    .map(|(i, _)| i as u32 + 1),
            );
            v
        })
    }

    /// 1-based line of a byte offset.
    pub fn line_of(&self, offset: u32) -> u32 {
        let starts = self.line_starts();
        starts.partition_point(|&s| s <= offset) as u32
    }

    /// 1-based line and 1-based byte column of a byte offset.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let line = self.line_of(offset);
        let start = self.line_starts()[line as usize - 1];
        (line, offset - start + 1)
    }

    pub fn slice(&self, start: u32, end: u32) -> &[u8] {
        &self.text[start as usize..end as usize]
    }

    /// Number of lines (a text ending in `\n` has an empty last line).
    pub fn line_count(&self) -> u32 {
        self.line_starts().len() as u32
    }

    /// Byte offset where 1-based `line` starts; the end of the text past
    /// the last line.
    pub fn line_start(&self, line: u32) -> u32 {
        let starts = self.line_starts();
        match line.checked_sub(1) {
            Some(i) => starts
                .get(i as usize)
                .copied()
                .unwrap_or(self.text.len() as u32),
            None => 0,
        }
    }

    /// Byte offset where the 1-based `line` ends (before its `\n`).
    pub fn line_end(&self, line: u32) -> u32 {
        if line >= 1 && line < self.line_count() {
            // The next line starts just after this one's `\n`.
            self.line_start(line + 1) - 1
        } else {
            self.text.len() as u32
        }
    }

    /// A byte offset as an editor position: the 0-based line and the
    /// 0-based column in UTF-16 code units, which is how the Language
    /// Server Protocol and the JavaScript editor count (LSP's default
    /// `positionEncoding`). Bytes that are not UTF-8 count one unit each,
    /// as the U+FFFD an editor decodes them to does. An offset inside a
    /// character counts from that character's start. Lines end at `\n`
    /// only, as everywhere else here (see `line_starts`).
    pub fn utf16_position(&self, offset: u32) -> (u32, u32) {
        let offset = offset.min(self.text.len() as u32);
        let line = self.line_of(offset);
        let start = self.line_start(line);
        let col = utf16_len(&self.text[start as usize..offset as usize]);
        (line - 1, col)
    }

    /// The byte offset of an editor position (0-based line, UTF-16
    /// column): the inverse of [`SourceFile::utf16_position`]. A column
    /// past the line's end clamps to the end (LSP asks servers to), a line
    /// past the last to the end of the text, and a column inside a
    /// surrogate pair to the character's start.
    pub fn offset_at_utf16(&self, line: u32, col: u32) -> u32 {
        if line >= self.line_count() {
            return self.text.len() as u32;
        }
        let start = self.line_start(line + 1);
        let end = self.line_end(line + 1);
        let bytes = &self.text[start as usize..end as usize];
        let mut units = 0u32;
        let mut at = 0usize;
        for chunk in bytes.utf8_chunks() {
            for c in chunk.valid().chars() {
                let n = c.len_utf16() as u32;
                if units + n > col {
                    return start + at as u32;
                }
                units += n;
                at += c.len_utf8();
            }
            for _ in chunk.invalid() {
                if units + 1 > col {
                    return start + at as u32;
                }
                units += 1;
                at += 1;
            }
        }
        start + at as u32
    }
}

/// UTF-16 code units of `bytes`, invalid UTF-8 counting one per byte.
pub fn utf16_len(bytes: &[u8]) -> u32 {
    bytes
        .utf8_chunks()
        .map(|c| {
            c.valid()
                .chars()
                .map(|ch| ch.len_utf16() as u32)
                .sum::<u32>()
                + c.invalid().len() as u32
        })
        .sum()
}

/// Why a flat UTF-16 offset has no byte offset in a text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Utf16OffsetError {
    /// The offset is past the end of the text.
    OutOfRange(u64),
    /// The offset falls between the two halves of a surrogate pair. An
    /// editor never sends one for text it shares with us, so this means
    /// the two copies disagree; editing there would leave half a
    /// character behind.
    SplitsCharacter(u64),
}

/// The byte offset of a flat UTF-16 offset (how CodeMirror, and every
/// JavaScript, WebView2 or GTK text model that counts code units, names a
/// position) into `bytes`. Bytes that are not UTF-8 count one unit each, as
/// in [`utf16_len`], so the two stay inverse on any text.
///
/// Positions convert only here and in [`SourceFile`] (`CLAUDE.md`): the
/// macOS app once carried its own copy of this in Swift, and each port
/// would have added another that disagreed on some edge.
pub fn byte_offset_of_utf16(bytes: &[u8], offset: u64) -> Result<usize, Utf16OffsetError> {
    let mut units = 0u64;
    let mut at = 0usize;
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            if units == offset {
                return Ok(at);
            }
            let n = c.len_utf16() as u64;
            if units + n > offset {
                return Err(Utf16OffsetError::SplitsCharacter(offset));
            }
            units += n;
            at += c.len_utf8();
        }
        for _ in chunk.invalid() {
            if units == offset {
                return Ok(at);
            }
            units += 1;
            at += 1;
        }
    }
    if units == offset {
        Ok(at)
    } else {
        Err(Utf16OffsetError::OutOfRange(offset))
    }
}

/// Every file taking part in one parse: the main file and everything it
/// includes.
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, path: PathBuf, text: impl Into<Box<[u8]>>) -> FileId {
        self.files.push(SourceFile::new(path, text));
        FileId(self.files.len() as u32 - 1)
    }

    pub(crate) fn push(&mut self, file: SourceFile) -> FileId {
        self.files.push(file);
        FileId(self.files.len() as u32 - 1)
    }

    pub(crate) fn into_files(self) -> Vec<SourceFile> {
        self.files
    }

    pub fn get(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn path(&self, id: FileId) -> &Path {
        &self.get(id).path
    }

    pub fn text(&self, span: Span) -> &[u8] {
        self.get(span.file).slice(span.start, span.end)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (FileId, &SourceFile)> {
        self.files
            .iter()
            .enumerate()
            .map(|(i, f)| (FileId(i as u32), f))
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn flat_utf16_offsets_map_to_bytes() {
        let t = "a\u{1F600}b\u{6F22}".as_bytes();
        assert_eq!(byte_offset_of_utf16(t, 0), Ok(0));
        assert_eq!(byte_offset_of_utf16(t, 1), Ok(1));
        assert_eq!(
            byte_offset_of_utf16(t, 2),
            Err(Utf16OffsetError::SplitsCharacter(2))
        );
        assert_eq!(byte_offset_of_utf16(t, 3), Ok(5));
        assert_eq!(byte_offset_of_utf16(t, 5), Ok(9));
        assert_eq!(
            byte_offset_of_utf16(t, 6),
            Err(Utf16OffsetError::OutOfRange(6))
        );
        // Invalid bytes count one unit each, as utf16_len counts them.
        let bad = b"x\xffy";
        assert_eq!(utf16_len(bad), 3);
        assert_eq!(byte_offset_of_utf16(bad, 2), Ok(2));
    }
    use super::*;

    #[test]
    fn lines_count_only_newlines() {
        let f = SourceFile::new("x".into(), b"a\nbc\r\n\rd".to_vec());
        assert_eq!(f.line_of(0), 1);
        assert_eq!(f.line_of(1), 1);
        assert_eq!(f.line_of(2), 2);
        assert_eq!(f.line_col(3), (2, 2));
        assert_eq!(f.line_of(6), 3);
        assert_eq!(f.line_of(8), 3);
        assert_eq!(f.line_of(9), 3);
    }

    #[test]
    fn utf16_positions_round_trip() {
        // "é" is two bytes and one unit, "😀" four bytes and two units, and
        // 0xFF is not UTF-8 (one unit, as its U+FFFD).
        let f = SourceFile::new("x".into(), b"a\xc3\xa9b\n\xf0\x9f\x98\x80c\xffd\n".to_vec());
        assert_eq!(f.utf16_position(0), (0, 0));
        assert_eq!(f.utf16_position(3), (0, 2));
        assert_eq!(f.utf16_position(5), (1, 0));
        assert_eq!(f.utf16_position(9), (1, 2));
        assert_eq!(f.utf16_position(11), (1, 4));
        assert_eq!(f.utf16_position(13), (2, 0));
        assert_eq!(f.offset_at_utf16(0, 2), 3);
        assert_eq!(f.offset_at_utf16(1, 2), 9);
        // Inside the surrogate pair: the character's start.
        assert_eq!(f.offset_at_utf16(1, 1), 5);
        assert_eq!(f.offset_at_utf16(1, 4), 11);
        // Past the line's end: its end, before the newline.
        assert_eq!(f.offset_at_utf16(0, 99), 4);
        assert_eq!(f.offset_at_utf16(2, 0), 13);
        assert_eq!(f.offset_at_utf16(7, 0), 13);
        for off in [0, 1, 3, 4, 5, 9, 10, 11, 12, 13] {
            let (l, c) = f.utf16_position(off);
            assert_eq!(f.offset_at_utf16(l, c), off, "offset {off}");
        }
        let g = SourceFile::new("y".into(), b"ab".to_vec());
        assert_eq!(g.line_end(1), 2);
        assert_eq!(g.offset_at_utf16(0, 5), 2);
    }
}
