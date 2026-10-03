//! The open document's own file, as another program changes it.
//!
//! An AI agent working through `neoscad mcp`, or any other editor, can
//! write the file a window has open. Before this module the apps watched
//! only the document's includes, and Save wrote the window's copy over
//! whatever was on disk: the other program's edit never showed and was
//! lost on the next save (`docs/audits/agent-connection-desktop.md`,
//! finding 1). Every app now keeps a [`DiskTracker`] per document and asks
//! it what a change on disk means:
//!
//! - **The document is clean:** the change is applied to the editor as
//!   edits ([`reload_edits`]), which the editor's `agentEdit` makes one
//!   undoable, highlighted step. Reading the file anew would replace the
//!   editor's state and wipe its undo history, so the user could neither
//!   see what changed nor take it back.
//! - **The document has unsaved changes:** neither copy wins silently. The
//!   host shows a notice (Reload / Keep mine), and Save asks before
//!   overwriting ([`DiskTracker::save_check`]).
//! - **The file is gone** (deleted, or moved away): the host says so;
//!   saving writes it again.
//!
//! The tracker knows the file by a [`DiskStamp`] of its bytes at the last
//! load or save, so the host's own save, echoing back through the watcher,
//! is recognised and ignored. A program that writes in several steps (or
//! truncates, then writes) can be caught half way: a read that does not
//! parse is read again shortly after ([`DiskAction::ReadAgain`]), and is
//! taken once it parses or stops changing.
//!
//! Like the rest of the crate this never touches the disk or the clock:
//! the host reads the file, passes its bytes in, and keeps the timer.

use serde::{Deserialize, Serialize};

use lang::source::SourceFile;

/// How long a host waits before reading a file again that looked half
/// written. Long enough for a program's next write to land, short enough
/// that a file with a genuine syntax error still shows promptly.
pub const DISK_READ_AGAIN_MS: u64 = 150;

/// Reads of one change before the tracker takes what it sees, complete or
/// not: a file whose syntax error is real must still show.
const MAX_READS: u32 = 4;

/// What a file held: its length and a 64-bit FNV-1a hash of its bytes.
/// Comparing stamps rather than modification times also catches a write
/// within the file system's time resolution, and ignores a `touch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskStamp {
    pub length: u64,
    pub hash: u64,
}

impl DiskStamp {
    pub fn of(bytes: &[u8]) -> DiskStamp {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        DiskStamp {
            length: bytes.len() as u64,
            hash: h,
        }
    }
}

/// One replacement for the editor's `agentEdit`: the text between two
/// positions of the editor's current text (0-based lines and UTF-16
/// columns, as the editor and LSP count) becomes `insert`. The edits of one
/// reload are sorted, do not overlap, and all refer to the text before any
/// of them applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReloadEdit {
    pub start_line: u64,
    pub start_character: u64,
    pub end_line: u64,
    pub end_character: u64,
    pub insert: String,
}

/// What a host does about the file as it is now on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum DiskAction {
    /// Nothing: the file holds what was last loaded or saved (the host's
    /// own save, seen by its watcher), or a change already reported.
    None,
    /// The file may be half written: read it again after `ms` and ask
    /// again.
    ReadAgain { ms: u64 },
    /// The file now holds exactly the document's text (another program
    /// saved the same text, or the user typed what an agent wrote): mark
    /// the document saved, and hide any notice.
    Saved,
    /// The document was clean: apply `edits` with the editor's `agentEdit`
    /// (one undoable, highlighted step). Once the editor reports that
    /// change, [`DiskTracker::editor_changed`] says whether the document is
    /// clean again.
    Reload { edits: Vec<ReloadEdit> },
    /// The document has unsaved changes and the file changed: show the
    /// notice ("The file changed on disk": Reload / Keep mine). Reload is
    /// offered only when `reloadable` (the file is UTF-8 text).
    Conflict { reloadable: bool },
    /// The file is gone (deleted, or moved away): show a notice. The
    /// document keeps its text, and saving writes the file again.
    Missing,
    /// The file is back to what was last loaded or saved: hide the notice.
    Resolved,
}

/// Whether Save may write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SaveCheck {
    /// The file is as last loaded or saved, or absent: write it.
    Write,
    /// Another program changed the file since: ask before overwriting.
    Changed,
}

/// The notice a host is showing, so one change is reported once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reported {
    Conflict(DiskStamp),
    Missing,
}

/// One document's file on disk: what it held at the last load or save,
/// the notice shown, and a read in progress (see the module docs).
#[derive(Debug, Clone, Default)]
pub struct DiskTracker {
    known: Option<DiskStamp>,
    reported: Option<Reported>,
    /// A reload sent to the editor, not yet reported back by it.
    pending: bool,
    /// The previous read of an unsettled change, and how many reads it
    /// has taken (`None` inside: the file was missing).
    last_read: Option<Option<DiskStamp>>,
    reads: u32,
}

impl DiskTracker {
    pub fn new() -> DiskTracker {
        DiskTracker::default()
    }

    /// The file was read (open) or written (save) with `bytes`: from now
    /// on that is the file as this document knows it.
    pub fn loaded(&mut self, bytes: &[u8]) {
        *self = DiskTracker {
            known: Some(DiskStamp::of(bytes)),
            ..DiskTracker::default()
        };
    }

    /// See [`DiskTracker::loaded`].
    pub fn saved(&mut self, bytes: &[u8]) {
        self.loaded(bytes);
    }

    /// The document no longer has a file (untitled): nothing to compare.
    pub fn forget(&mut self) {
        *self = DiskTracker::default();
    }

    /// The file as last loaded or saved.
    pub fn known(&self) -> Option<DiskStamp> {
        self.known
    }

    /// Whether a reload was handed to the editor and has not come back as
    /// a change yet (a host can skip copying its text until then).
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Whether a notice (a conflict, a missing file) is showing.
    pub fn is_reporting(&self) -> bool {
        self.reported.is_some()
    }

    /// The watcher saw the file change, and the host read it: `disk` is its
    /// bytes, or `None` if there is no file at the path. `buffer` is the
    /// document's text and `dirty` whether it has unsaved changes.
    pub fn check(&mut self, disk: Option<&[u8]>, buffer: &str, dirty: bool) -> DiskAction {
        let now = disk.map(DiskStamp::of);
        if self.known.is_none() && self.reported.is_none() && now.is_none() {
            // An untitled document, or a file never read: nothing to say.
            return DiskAction::None;
        }
        if now.is_some() && now == self.known {
            self.settled();
            return match self.reported.take() {
                Some(_) => DiskAction::Resolved,
                None => DiskAction::None,
            };
        }
        // A change already reported (a burst of events about one write).
        let already = match now {
            None => Reported::Missing,
            Some(stamp) => Reported::Conflict(stamp),
        };
        if self.reported == Some(already) {
            self.settled();
            return DiskAction::None;
        }
        // A reload is still on its way to the editor: edits worked out now
        // would be against the text before it, and the editor would apply
        // them to the text after it. Wait for it (a bounded number of
        // times: an editor that never answers must not stall this).
        if self.pending && self.reads < MAX_READS {
            self.reads += 1;
            return DiskAction::ReadAgain {
                ms: DISK_READ_AGAIN_MS,
            };
        }
        self.pending = false;
        // A change: make sure it is the whole of one before acting on it.
        if self.reads < MAX_READS && self.last_read != Some(now) && !complete(disk) {
            self.last_read = Some(now);
            self.reads += 1;
            return DiskAction::ReadAgain {
                ms: DISK_READ_AGAIN_MS,
            };
        }
        self.settled();
        let (Some(bytes), Some(stamp)) = (disk, now) else {
            self.reported = Some(Reported::Missing);
            return DiskAction::Missing;
        };
        if bytes == buffer.as_bytes() {
            self.known = Some(stamp);
            self.reported = None;
            return DiskAction::Saved;
        }
        match std::str::from_utf8(bytes) {
            Ok(text) if !dirty => {
                self.known = Some(stamp);
                self.reported = None;
                self.pending = true;
                DiskAction::Reload {
                    edits: reload_edits(buffer, text),
                }
            }
            text => {
                self.reported = Some(Reported::Conflict(stamp));
                DiskAction::Conflict {
                    reloadable: text.is_ok(),
                }
            }
        }
    }

    /// The user chose Reload on the notice: the edits that make `buffer`
    /// the file's text (`disk`), applied as [`DiskAction::Reload`]'s are.
    /// `None` if the file is gone or not UTF-8 (the notice stays).
    pub fn reload(&mut self, disk: Option<&[u8]>, buffer: &str) -> Option<Vec<ReloadEdit>> {
        let bytes = disk?;
        let text = std::str::from_utf8(bytes).ok()?;
        self.known = Some(DiskStamp::of(bytes));
        self.reported = None;
        self.settled();
        self.pending = true;
        Some(reload_edits(buffer, text))
    }

    /// The user chose Keep mine: the notice goes, and the document stays
    /// as it is. The file is still not what this document last read, so
    /// Save will still ask ([`DiskTracker::save_check`]); a further change
    /// on disk is reported again.
    pub fn keep_mine(&mut self) {
        // `reported` stays, so the change just dismissed is not reported
        // again by the next event about the same write.
    }

    /// The editor reported a change. After a reload, whether this is it
    /// and the document now matches the file (the host then marks the
    /// document saved). A keystroke that raced the reload leaves the text
    /// different, and the document then stays edited, which costs at most
    /// a save. Free when no reload is pending.
    pub fn editor_changed(&mut self, buffer: &[u8]) -> bool {
        if !std::mem::take(&mut self.pending) {
            return false;
        }
        Some(DiskStamp::of(buffer)) == self.known
    }

    /// Before Save writes: `disk` is the file now (`None` if absent).
    pub fn save_check(&self, disk: Option<&[u8]>) -> SaveCheck {
        match (self.known, disk) {
            (Some(known), Some(bytes)) if DiskStamp::of(bytes) != known => SaveCheck::Changed,
            _ => SaveCheck::Write,
        }
    }

    fn settled(&mut self) {
        self.last_read = None;
        self.reads = 0;
    }
}

/// Whether a read looks like a whole file rather than one caught mid-write:
/// it exists and parses. A missing file may be one deleted and about to be
/// written again; a syntax error, a write cut short. Either is read again,
/// and taken if it has not changed by then.
fn complete(disk: Option<&[u8]>) -> bool {
    let Some(bytes) = disk else { return false };
    if std::str::from_utf8(bytes).is_err() {
        return false;
    }
    let text = bytes.to_vec();
    // The parser recurses on nesting, so it runs with the evaluator's stack
    // (as the customizer's parse does, `document.rs`): a host may ask from
    // a thread with a small one.
    eval::with_stack(eval::DEFAULT_THREAD_STACK, move || {
        !lang::parse_file("document.scad".into(), text).has_syntax_errors()
    })
}

/// The edits that turn `old` into `new`, for the editor's `agentEdit`: one
/// per run of changed lines, each trimmed to the characters that differ,
/// so the highlight shows what changed rather than the whole file.
///
/// Lines are matched with Myers' diff. Its cost grows with the number of
/// differences, so past a bound (a file rewritten wholesale) the changed
/// middle becomes one edit instead: the result is still right, only less
/// finely highlighted.
pub fn reload_edits(old: &str, new: &str) -> Vec<ReloadEdit> {
    if old == new {
        return Vec::new();
    }
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (am, bm) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let hunks = line_hunks(am, bm).unwrap_or_else(|| vec![(0, am.len(), 0, bm.len())]);
    // Byte offsets of each line's start, in both texts.
    let starts = |lines: &[&str]| -> Vec<usize> {
        let mut v = Vec::with_capacity(lines.len() + 1);
        let mut at = 0;
        v.push(0);
        for l in lines {
            at += l.len();
            v.push(at);
        }
        v
    };
    let (sa, sb) = (starts(&a), starts(&b));
    let source = SourceFile::new("document.scad".into(), old.as_bytes().to_vec());
    let position = |offset: usize| {
        let (line, character) = source.utf16_position(offset as u32);
        (u64::from(line), u64::from(character))
    };
    hunks
        .into_iter()
        .map(|(a0, a1, b0, b1)| {
            let (s, e) = (sa[prefix + a0], sa[prefix + a1]);
            let (ns, ne) = (sb[prefix + b0], sb[prefix + b1]);
            let (from, to) = (&old[s..e], &new[ns..ne]);
            let head = common_prefix(from, to);
            let tail = common_suffix(&from[head..], &to[head..]);
            let (start, end) = (s + head, e - tail);
            let (start_line, start_character) = position(start);
            let (end_line, end_character) = position(end);
            ReloadEdit {
                start_line,
                start_character,
                end_line,
                end_character,
                insert: to[head..to.len() - tail].to_string(),
            }
        })
        .collect()
}

/// `text` with `edits` applied, as the editor's `agentEdit` applies them:
/// for a host whose editor is not up yet (its page still loading), which
/// then shows the result when it is.
pub fn apply_reload_edits(text: &str, edits: &[ReloadEdit]) -> String {
    let source = SourceFile::new("document.scad".into(), text.as_bytes().to_vec());
    let at = |l: u64, c: u64| {
        let clamp = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
        source.offset_at_utf16(clamp(l), clamp(c)) as usize
    };
    let mut out = text.to_string();
    // Back to front, so the positions before each edit stay valid.
    for e in edits.iter().rev() {
        let start = at(e.start_line, e.start_character);
        let end = at(e.end_line, e.end_character).max(start);
        out.replace_range(start..end, &e.insert);
    }
    out
}

/// `edits` as the editor's `agentEdit` takes them, in JSON: `[{from: [line,
/// character], to: [line, character], insert}]`. Every host passes this
/// string to the page as is (`NeoSCADEditor.agentEdit(JSON.parse(edits))`),
/// so the shape is written once.
pub fn agent_edit_json(edits: &[ReloadEdit]) -> String {
    let v: Vec<serde_json::Value> = edits
        .iter()
        .map(|e| {
            serde_json::json!({
                "from": [e.start_line, e.start_character],
                "to": [e.end_line, e.end_character],
                "insert": e.insert,
            })
        })
        .collect();
    serde_json::Value::Array(v).to_string()
}

/// Bytes `a` and `b` share at their start, ending on a character boundary.
fn common_prefix(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or(a.len().min(b.len()), |((i, _), _)| i)
        .min(a.len())
        .min(b.len())
}

/// Bytes `a` and `b` share at their end, starting on a character boundary.
fn common_suffix(a: &str, b: &str) -> usize {
    let mut n = 0;
    for (x, y) in a.chars().rev().zip(b.chars().rev()) {
        if x != y {
            break;
        }
        n += x.len_utf8();
    }
    n
}

/// The most differences Myers' search explores: its trace keeps
/// `(d + 1)^2` positions, about 4 MB at this bound.
const MAX_DIFFERENCES: usize = 1000;
/// And its work, about lines times differences, stays under this.
const MAX_DIFF_WORK: usize = 20_000_000;

/// The changed runs between line lists `a` and `b`, as `(a0, a1, b0, b1)`:
/// `a[a0..a1]` becomes `b[b0..b1]`. `None` when they differ by more than
/// the search's bounds.
#[allow(clippy::type_complexity)]
fn line_hunks(a: &[&str], b: &[&str]) -> Option<Vec<(usize, usize, usize, usize)>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = MAX_DIFFERENCES.min(MAX_DIFF_WORK / (a.len() + b.len() + 1)) as isize;
    // trace[d][k + d]: the furthest x on diagonal k after d differences.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let snake = |mut x: isize, mut y: isize| {
        while x < n && y < m && a[x as usize] == b[y as usize] {
            x += 1;
            y += 1;
        }
        x
    };
    let mut found = None;
    for d in 0..=max.min(n + m) {
        let mut v = vec![0isize; (2 * d + 1) as usize];
        for k in (-d..=d).step_by(2) {
            let x = if d == 0 {
                0
            } else {
                let prev = &trace[(d - 1) as usize];
                let at = |k: isize| prev[(k + d - 1) as usize];
                if k == -d || (k != d && at(k - 1) < at(k + 1)) {
                    at(k + 1)
                } else {
                    at(k - 1) + 1
                }
            };
            let x = snake(x, x - k);
            v[(k + d) as usize] = x;
            if x >= n && x - k >= m {
                found = Some(d);
            }
        }
        trace.push(v);
        if found.is_some() {
            break;
        }
    }
    let d_end = found?;
    // Walk back, collecting the matched line pairs (in reverse).
    let mut matched = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..=d_end).rev() {
        let k = x - y;
        let (mx, my) = if d == 0 {
            (0, 0)
        } else {
            let prev = &trace[(d - 1) as usize];
            let at = |k: isize| prev[(k + d - 1) as usize];
            let down = k == -d || (k != d && at(k - 1) < at(k + 1));
            let pk = if down { k + 1 } else { k - 1 };
            let px = at(pk);
            let py = px - pk;
            if down { (px, py + 1) } else { (px + 1, py) }
        };
        while x > mx && y > my {
            x -= 1;
            y -= 1;
            matched.push((x as usize, y as usize));
        }
        if d > 0 {
            let prev = &trace[(d - 1) as usize];
            let at = |k: isize| prev[(k + d - 1) as usize];
            let down = k == -d || (k != d && at(k - 1) < at(k + 1));
            let pk = if down { k + 1 } else { k - 1 };
            x = at(pk);
            y = x - pk;
        }
    }
    matched.reverse();
    let mut hunks = Vec::new();
    let (mut ca, mut cb) = (0, 0);
    for (i, j) in matched {
        if i > ca || j > cb {
            hunks.push((ca, i, cb, j));
        }
        ca = i + 1;
        cb = j + 1;
    }
    if ca < a.len() || cb < b.len() {
        hunks.push((ca, a.len(), cb, b.len()));
    }
    Some(hunks)
}

#[cfg(test)]
#[path = "disk_tests.rs"]
mod tests;
