//! JSON-RPC messages and the protocol's positions.
//!
//! Positions are `{line, character}` with the character in UTF-16 code
//! units, the protocol's default and the only encoding this server
//! offers (it answers `positionEncoding: "utf-16"`). The conversion is
//! `lang::source::SourceFile`'s, the one place byte offsets meet UTF-16.

use serde_json::{Value, json};

use lang::source::SourceFile;

/// JSON-RPC's and the protocol's error codes.
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    pub const SERVER_NOT_INITIALIZED: i64 = -32002;
    /// The request cannot be answered as asked (a rename that is not
    /// safe, formatting a file with a syntax error).
    pub const REQUEST_FAILED: i64 = -32803;
}

pub fn response(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

pub fn error(id: &Value, code: i64, message: impl Into<String>) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
        .to_string()
}

pub fn notification(method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

/// A byte offset as a position.
pub fn position(f: &SourceFile, offset: u32) -> Value {
    let (line, character) = f.utf16_position(offset);
    json!({"line": line, "character": character})
}

/// A byte range as a range.
pub fn range(f: &SourceFile, (a, b): (u32, u32)) -> Value {
    json!({"start": position(f, a), "end": position(f, b.max(a))})
}

/// A position's byte offset, if the value is a position.
pub fn offset(f: &SourceFile, pos: &Value) -> Option<u32> {
    let line = u32::try_from(pos.get("line")?.as_u64()?).ok()?;
    let character = u32::try_from(pos.get("character")?.as_u64()?).unwrap_or(u32::MAX);
    Some(f.offset_at_utf16(line, character))
}

/// A range's byte offsets.
pub fn offsets(f: &SourceFile, r: &Value) -> Option<(u32, u32)> {
    let a = offset(f, r.get("start")?)?;
    let b = offset(f, r.get("end")?)?;
    Some((a.min(b), a.max(b)))
}

/// A 1-based line and 1-based byte column (the session's diagnostics) as
/// a byte offset, clamped to the line.
pub fn line_col_offset(f: &SourceFile, line: u32, col: u32) -> u32 {
    let line = line.clamp(1, f.line_count());
    let start = f.line_start(line);
    let end = f.line_end(line);
    (start + col.saturating_sub(1)).min(end.max(start))
}
