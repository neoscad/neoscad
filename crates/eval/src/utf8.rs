//! UTF-8 handling with GLib's semantics.
//!
//! OpenSCAD strings are byte strings that GLib walks as UTF-8
//! (`str_utf8_wrapper`). GLib does not validate while walking: a lead byte
//! decides how many bytes a "character" takes (`g_utf8_skip`), so an
//! invalid Latin-1 string still splits into characters, just not the ones
//! a validating decoder would produce. `ord-tests` checks exactly that
//! (`"\xA4\xC4\xD6\xDC\xDF"` is three characters), so this module
//! reproduces GLib rather than using `str`.

/// `g_utf8_skip`: the byte length GLib assigns to a character starting
/// with `b`.
pub fn skip(b: u8) -> usize {
    match b {
        0x00..=0xBF => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        0xF8..=0xFB => 5,
        0xFC..=0xFD => 6,
        _ => 1,
    }
}

/// `g_utf8_strlen(s, len)`: the number of characters, stopping at a NUL
/// byte and not counting a trailing partial character.
pub fn char_count(s: &[u8]) -> usize {
    if s.is_empty() || s[0] == 0 {
        return 0;
    }
    if s.is_ascii() && !s.contains(&0) {
        return s.len();
    }
    let max = s.len();
    let mut len = 0;
    let mut p = skip(s[0]);
    while p < max && s[p] != 0 {
        len += 1;
        p += skip(s[p]);
    }
    if p <= max {
        len += 1;
    }
    len
}

/// Byte offset of character `n` (`g_utf8_offset_to_pointer`), clamped to
/// the end.
pub fn offset_of(s: &[u8], n: usize) -> usize {
    let mut p = 0;
    for _ in 0..n {
        if p >= s.len() {
            break;
        }
        p += skip(s[p]);
    }
    p.min(s.len())
}

/// The characters of `s` as byte ranges, the way `str_utf8_wrapper`'s
/// iterator yields them (a final partial character is cut at the end).
pub fn chars(s: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut p = 0;
    std::iter::from_fn(move || {
        if p >= s.len() {
            return None;
        }
        let end = (p + skip(s[p])).min(s.len());
        let c = &s[p..end];
        p = end;
        Some(c)
    })
}

/// Character `i` of `s` (`str_utf8_wrapper::operator[]`), or `None` when out
/// of range.
pub fn char_at(s: &[u8], i: usize) -> Option<&[u8]> {
    if i >= s.len() {
        return None;
    }
    if s.is_ascii() {
        return Some(&s[i..=i]);
    }
    if i >= char_count(s) {
        return None;
    }
    let start = offset_of(s, i);
    let end = (start + skip(s[start])).min(s.len());
    // g_utf8_strncpy copies into a NUL-terminated buffer: stop at a NUL.
    let c = &s[start..end];
    let n = c.iter().position(|&b| b == 0).unwrap_or(c.len());
    Some(&c[..n])
}

/// `g_utf8_get_char`: decode the first character, or `u32::MAX` (GLib's
/// `(gunichar)-1`) when it is malformed.
pub fn first_char(s: &[u8]) -> u32 {
    let Some(&c) = s.first() else { return 0 };
    let (mask, len) = match c {
        0x00..=0x7F => (0x7F, 1),
        _ if c & 0xE0 == 0xC0 => (0x1F, 2),
        _ if c & 0xF0 == 0xE0 => (0x0F, 3),
        _ if c & 0xF8 == 0xF0 => (0x07, 4),
        _ if c & 0xFC == 0xF8 => (0x03, 5),
        _ if c & 0xFE == 0xFC => (0x01, 6),
        _ => return u32::MAX,
    };
    let mut r = u32::from(c) & mask;
    for i in 1..len {
        let b = s.get(i).copied().unwrap_or(0);
        if b & 0xC0 != 0x80 {
            return u32::MAX;
        }
        r = (r << 6) | u32::from(b & 0x3F);
    }
    r
}

/// `g_utf8_validate(s, -1)`: valid UTF-8 up to the first NUL.
pub fn validate(s: &[u8]) -> bool {
    let n = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    std::str::from_utf8(&s[..n]).is_ok()
}

/// `g_unichar_validate(c) && c != 0` followed by `g_unichar_to_utf8`.
pub fn encode(c: u32, out: &mut Vec<u8>) {
    if c == 0 {
        return;
    }
    if let Some(ch) = char::from_u32(c) {
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glib_walks_invalid_utf8_by_lead_byte() {
        let s = b"\xA4\xC4\xD6\xDC\xDF";
        assert_eq!(char_count(s), 3);
        let cs: Vec<&[u8]> = chars(s).collect();
        assert_eq!(cs, vec![&b"\xA4"[..], &b"\xC4\xD6"[..], &b"\xDC\xDF"[..]]);
        assert!(!validate(s));
    }

    #[test]
    fn indexing_and_decoding() {
        let s = "a\u{2190}b".as_bytes();
        assert_eq!(char_count(s), 3);
        assert_eq!(char_at(s, 1), Some("\u{2190}".as_bytes()));
        assert_eq!(char_at(s, 3), None);
        assert_eq!(first_char("\u{1F640}".as_bytes()), 0x1F640);
        let mut out = Vec::new();
        encode(0xD800, &mut out);
        encode(0, &mut out);
        encode(0x41, &mut out);
        assert_eq!(out, b"A");
    }
}
