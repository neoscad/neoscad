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
}
