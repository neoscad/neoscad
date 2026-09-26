//! OFF: `import_off.cc` and `export_off.cc`.
//!
//! The reader accepts the header flags OpenSCAD's regex does
//! (`[ST][C][N][4][n]OFF[ BINARY]`), rejects binary and non-3D files, and
//! reads per-face colours: integers are bytes, a number with a dot is a
//! fraction of 255. Quirks kept: the header may be missing entirely (the
//! counts are then read from the first line), `#` comments may end any
//! line, a face index out of range is dropped with an error rather than
//! failing the file, and every coloured face gets its own palette entry
//! (the C++ keeps its colour map inside the face loop).

use crate::mesh::{Mesh, MeshRef};
use crate::text::{Lines, fmt_g, parse_f64, parse_i32, parse_u64, trim};
use crate::{Color, Message};

/// Read an OFF file's bytes (`None` when it could not be opened). `file`
/// is the path as messages print it.
pub fn read(bytes: Option<&[u8]>, file: &str, msgs: &mut Vec<Message>) -> Mesh {
    let mut r = Reader { lines: Lines::new(bytes.unwrap_or_default()), lineno: 0, line: String::new(), file, msgs };
    if bytes.is_none() {
        r.error("File error");
        return Mesh::default();
    }
    r.read().unwrap_or_default()
}

struct Reader<'a, 'm> {
    lines: Lines<'a>,
    lineno: i32,
    line: String,
    file: &'a str,
    msgs: &'m mut Vec<Message>,
}

/// Split on single spaces and tabs with runs compressed
/// (`boost::split(..., token_compress_on)`): a leading separator still
/// yields one empty word, as Boost's does.
fn words(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        match rest.find([' ', '\t']) {
            Some(i) => {
                out.push(&rest[..i]);
                rest = rest[i..].trim_start_matches([' ', '\t']);
            }
            None => {
                out.push(rest);
                return out;
            }
        }
    }
}

impl Reader<'_, '_> {
    fn error(&mut self, what: &str) {
        let text = format!("OFF File line {}, {what} line '{}' importing file '{}'", self.lineno, self.line, self.file);
        self.msgs.push(Message::error(text).at_call());
    }

    /// `getline_clean`: the next line that is not empty after removing a
    /// trailing `\r`, a `#` comment and surrounding space.
    fn getline_clean(&mut self, what: &str) -> Option<()> {
        loop {
            self.lineno += 1;
            let raw = self.lines.next_line();
            if raw.is_empty() && self.lines.eof {
                self.line = raw;
                self.error(what);
                return None;
            }
            let mut l = raw.strip_suffix('\r').unwrap_or(&raw).to_string();
            // `\s*#.*$`: the comment and the space before it.
            if let Some(i) = l.find('#') {
                l.truncate(i);
                l.truncate(l.trim_end_matches(crate::text::is_space).len());
            }
            self.line = trim(&l).to_string();
            if !self.line.is_empty() {
                return Some(());
            }
        }
    }

    fn read(&mut self) -> Option<Mesh> {
        self.getline_clean("bad header: end of file")?;
        let (header_len, has_ndim, binary, mut dimension) = header(&self.line);
        self.line.drain(..header_len);
        if binary {
            self.error("binary OFF format not supported");
            return None;
        }
        if has_ndim {
            if self.line.is_empty() {
                self.getline_clean("bad header: end of file")?;
            }
            let w: Vec<String> = words(&self.line).iter().map(|s| s.to_string()).collect();
            if self.lines.eof {
                self.error("bad header: missing Ndim");
                return None;
            }
            let skip = w[0].len() + usize::from(w.len() > 1);
            self.line.drain(..skip.min(self.line.len()));
            match parse_u64(&w[0]) {
                Some(n) => dimension = (n as u32).wrapping_add(dimension).wrapping_sub(3),
                None => {
                    self.error("bad header: bad data for Ndim");
                    return None;
                }
            }
        }
        if dimension != 3 {
            self.error(&format!("unhandled vertex dimensions ({dimension})"));
            return None;
        }
        if self.line.is_empty() {
            self.getline_clean("bad header: end of file")?;
        }
        let w: Vec<String> = words(&self.line).iter().map(|s| s.to_string()).collect();
        if self.lines.eof || w.len() < 3 {
            self.error("bad header: missing data");
            return None;
        }
        let (Some(nv), Some(nf), Some(_)) = (parse_u64(&w[0]), parse_u64(&w[1]), parse_u64(&w[2])) else {
            self.error("bad header: bad data");
            return None;
        };
        if self.lines.eof || nv < 1 || nf < 1 {
            self.error("bad header: not enough data");
            return None;
        }
        let mut mesh = Mesh::default();
        let mut vertex = 0u64;
        while !self.lines.eof && vertex < nv {
            vertex += 1;
            self.getline_clean("reading vertices: end of file")?;
            let w = words(&self.line);
            if w.len() < 3 {
                self.error("can't parse vertex: not enough data");
                return None;
            }
            let mut v = [0.0; 3];
            for (i, c) in v.iter_mut().enumerate() {
                match parse_f64(w[i]) {
                    Some(x) => *c = x,
                    None => {
                        self.error("can't parse vertex: bad data");
                        return None;
                    }
                }
            }
            mesh.vertices.push(v);
        }
        let mut face = 0u64;
        while !self.lines.eof && face < nf {
            face += 1;
            self.getline_clean("reading faces: end of file")?;
            let w: Vec<String> = words(&self.line).iter().map(|s| s.to_string()).collect();
            let Some(n) = parse_u64(&w[0]) else {
                self.error("can't parse face: bad data");
                return None;
            };
            if ((w.len() - 1) as u64) < n {
                self.error("can't parse face: missing indices");
                return None;
            }
            let n = n as usize;
            let face_idx = mesh.faces.len();
            let mut f = Vec::with_capacity(n);
            for word in &w[1..=n] {
                let Some(ind) = parse_i32(word) else {
                    mesh.faces.push(f);
                    self.error("can't parse face: bad data");
                    return None;
                };
                // `size_t ind = lexical_cast<int>(...)`: a negative index
                // wraps and prints as a huge unsigned number.
                let ind = ind as i64 as u64;
                if ind < mesh.vertices.len() as u64 {
                    f.push(ind as u32);
                } else {
                    self.error(&format!("ignored bad face vertex index: {ind}"));
                }
            }
            mesh.faces.push(f);
            if w.len() >= n + 4 {
                let mut i = n + 1;
                let channel = |r: &mut Self, i: &mut usize| -> Option<i32> {
                    let c = r.color(&w[*i]);
                    *i += 1;
                    c
                };
                let red = channel(self, &mut i)?;
                let green = channel(self, &mut i)?;
                let blue = channel(self, &mut i)?;
                let alpha = if i < w.len() { channel(self, &mut i)? } else { 255 };
                mesh.colors.push(Color::from_ints(red, green, blue, alpha));
                mesh.color_indices.resize(face_idx, -1);
                mesh.color_indices.push(mesh.colors.len() as i32 - 1);
            }
        }
        if !mesh.color_indices.is_empty() {
            mesh.color_indices.resize(mesh.faces.len(), -1);
        }
        Some(mesh)
    }

    /// `getcolor`: `None` is a `bad_lexical_cast` (the face fails); a
    /// fraction that does not parse reports "Parse error" and reads as 0.
    fn color(&mut self, word: &str) -> Option<i32> {
        if word.contains('.') {
            match from_chars_f32(word) {
                Some(f) => Some((f * 255.0) as i32),
                None => {
                    self.error("Parse error");
                    Some(0)
                }
            }
        } else {
            match parse_i32(word) {
                Some(c) => Some(c),
                None => {
                    self.error("can't parse face: bad data");
                    None
                }
            }
        }
    }
}

/// `std::from_chars` for `float`: the longest numeric prefix (no leading
/// `+`, no leading space); `None` when there is none.
fn from_chars_f32(s: &str) -> Option<f32> {
    let b = s.as_bytes();
    let mut end = 0;
    if b.first() == Some(&b'-') {
        end = 1;
    }
    let digits_start = end;
    while end < b.len() && b[end].is_ascii_digit() {
        end += 1;
    }
    if end < b.len() && b[end] == b'.' {
        end += 1;
        while end < b.len() && b[end].is_ascii_digit() {
            end += 1;
        }
    }
    if end == digits_start || (end == digits_start + 1 && b[digits_start] == b'.') {
        return None;
    }
    if end < b.len() && (b[end] == b'e' || b[end] == b'E') {
        let mut e = end + 1;
        if e < b.len() && (b[e] == b'+' || b[e] == b'-') {
            e += 1;
        }
        let ds = e;
        while e < b.len() && b[e].is_ascii_digit() {
            e += 1;
        }
        if e > ds {
            end = e;
        }
    }
    s[..end].parse::<f32>().ok()
}

/// `^(ST)?(C)?(N)?(4)?(n)?OFF( BINARY)? *`: the match length and the
/// flags (Ndim, binary, dimension). No match is length 0. The colour,
/// normal and texture flags are parsed but, as in OpenSCAD, unused: face
/// colours are read whenever a face line has them.
fn header(line: &str) -> (usize, bool, bool, u32) {
    let mut s = line;
    let take = |s: &mut &str, p: &str| -> bool {
        match s.strip_prefix(p) {
            Some(r) => {
                *s = r;
                true
            }
            None => false,
        }
    };
    take(&mut s, "ST");
    take(&mut s, "C");
    take(&mut s, "N");
    let four = take(&mut s, "4");
    let ndim = take(&mut s, "n");
    if !take(&mut s, "OFF") {
        return (0, false, false, 3);
    }
    let binary = take(&mut s, " BINARY");
    let s = s.trim_start_matches(' ');
    (line.len() - s.len(), ndim, binary, if four { 4 } else { 3 })
}

/// `export_off`. Faces keep their colours as `r g b` bytes, plus alpha
/// when it is not 255.
///
/// An invalid colour (from `color()` with nothing usable) is where this
/// differs on purpose: OpenSCAD warns "Invalid color in OFF export" and
/// then prints four uninitialised ints (0 0 0 0 on the nightly, which an
/// importer draws as transparent black). Here the warning is the same but
/// the face is written without a colour, so readers fall back to their
/// default colour, which is what OpenSCAD's own render shows.
pub fn write(mesh: MeshRef<'_>, warnings: &mut Vec<String>) -> Vec<u8> {
    let mut out = String::with_capacity(mesh.vertices.len() * 32 + mesh.faces.len() * 24);
    out.push_str(&format!("OFF\n{} {} 0\n", mesh.vertices.len(), mesh.faces.len()));
    for v in mesh.vertices {
        out.push_str(&format!("{} {} {} \n", fmt_g(v[0]), fmt_g(v[1]), fmt_g(v[2])));
    }
    let has_color = !mesh.color_indices.is_empty();
    for (i, f) in mesh.faces.iter().enumerate() {
        out.push_str(&f.len().to_string());
        for idx in f {
            out.push(' ');
            out.push_str(&idx.to_string());
        }
        if has_color && let Some(c) = mesh.face_color(i) {
            match c.rgba_int() {
                Some([r, g, b, a]) => {
                    out.push_str(&format!(" {r} {g} {b}"));
                    if a != 255 {
                        out.push_str(&format!(" {a}"));
                    }
                }
                None => warnings.push("Invalid color in OFF export".into()),
            }
        }
        out.push('\n');
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_colours_and_headers() {
        let mut msgs = Vec::new();
        let text = b"COFF\n# c\n4 2 0\n0 0 0\n1 0 0\n0 1 0\n0 0 1 # comment\n3 0 1 2 255 0 0\n3 0 2 3 0.5 0.5 0.5 0.5\n";
        let m = read(Some(text), "f", &mut msgs);
        assert!(msgs.is_empty(), "{msgs:?}");
        assert_eq!(m.faces, vec![vec![0, 1, 2], vec![0, 2, 3]]);
        assert_eq!(m.color_indices, vec![0, 1]);
        assert_eq!(m.colors[0], Color::from_ints(255, 0, 0, 255));
        assert_eq!(m.colors[1], Color::from_ints(127, 127, 127, 127));
    }

    #[test]
    fn counts_without_magic_and_errors() {
        let mut msgs = Vec::new();
        let m = read(Some(b"3 1 0\n0 0 0\n1 0 0\n0 1 0\n3 0 1 5\n"), "f", &mut msgs);
        assert_eq!(m.faces, vec![vec![0, 1]]);
        assert_eq!(msgs[0].text, "OFF File line 5, ignored bad face vertex index: 5 line '3 0 1 5' importing file 'f'");
        msgs.clear();
        read(Some(b""), "e.off", &mut msgs);
        // The nightly on an empty file.
        assert_eq!(msgs[0].text, "OFF File line 1, bad header: end of file line '' importing file 'e.off'");
        msgs.clear();
        read(None, "m.off", &mut msgs);
        assert_eq!(msgs[0].text, "OFF File line 0, File error line '' importing file 'm.off'");
    }
}
