//! Wavefront OBJ: `import_obj.cc` and `export_obj.cc`.
//!
//! Only `v` (exactly three coordinates) and `f` lines are read; texture,
//! normal, material, object, smoothing and group lines are skipped, and
//! anything else warns. Quirks kept: line numbers in messages are one more
//! than the real line (the C++ counter starts at 1 and is incremented
//! before the first line), and the out-of-range warning prints the file
//! name where the index should be (its format arguments are in the wrong
//! order). One deliberate difference: a face with two separators in a row
//! (`f 1  2 3`) crashes OpenSCAD with an uncaught `bad_lexical_cast`; here
//! the empty word is skipped.

use crate::Message;
use crate::mesh::{Mesh, MeshBuilder, MeshRef};
use crate::text::{Lines, parse_f64, parse_i32, trim, write_g, write_int};

pub fn read(bytes: &[u8], file: &str, msgs: &mut Vec<Message>) -> Mesh {
    let mut b = MeshBuilder::new();
    let mut lines = Lines::new(bytes);
    let mut lineno = 1;
    let mut vertex_map: Vec<u32> = Vec::new();
    while !lines.eof {
        lineno += 1;
        let raw = lines.next_line();
        let line = trim(&raw);
        let starts = |p: &str| line.starts_with(p);
        if line.is_empty() || starts("#") {
            continue;
        }
        if let Some(coords) = vertex_coords(line) {
            let mut v = [0.0; 3];
            for (c, w) in v.iter_mut().zip(coords) {
                match parse_f64(w) {
                    Some(x) => *c = x,
                    None => {
                        let text = format!(
                            "OBJ File line {lineno}, can't parse vertex line '{line}' importing file '{file}'"
                        );
                        msgs.push(Message::error(text).at_call());
                        return Mesh::default();
                    }
                }
            }
            vertex_map.push(b.vertex_index(v));
        } else if let Some(rest) = face_rest(line) {
            b.begin_polygon();
            for word in rest.split([' ', '\t']).filter(|w| !w.is_empty()) {
                // `boost::split(word, "/")[0]` then `lexical_cast<int>`,
                // stored in a `size_t` (negative wraps, so it is out of
                // range).
                let first = word.split('/').next().unwrap_or("");
                let Some(ind) = parse_i32(first) else {
                    msgs.push(Message::warning(format!(
                        "Index {file} out of range in Line {lineno}"
                    )));
                    continue;
                };
                if ind >= 1 && (ind as usize) <= vertex_map.len() {
                    b.add_index(vertex_map[ind as usize - 1]);
                } else {
                    msgs.push(Message::warning(format!(
                        "Index {file} out of range in Line {lineno}"
                    )));
                }
            }
        } else if starts("vt")
            || starts("vn")
            || starts("mtllib")
            || starts("usemtl")
            || starts("o")
            || starts("s")
            || starts("g")
        {
        } else {
            msgs.push(Message::warning(format!(
                "Unrecognized Line  {line} in line Line {lineno}"
            )));
        }
    }
    b.build()
}

/// `^\s*v\s+([^\s]+)\s+([^\s]+)\s+([^\s]+)\s*$`.
fn vertex_coords(l: &str) -> Option<[&str; 3]> {
    let rest = l.strip_prefix('v')?;
    if !rest.starts_with(crate::text::is_space) {
        return None;
    }
    let mut it = rest.split(crate::text::is_space).filter(|w| !w.is_empty());
    let words = [it.next()?, it.next()?, it.next()?];
    if it.next().is_some() {
        None
    } else {
        Some(words)
    }
}

/// `^\s*f\s+(.*)$`: the text after `f` and its spaces.
fn face_rest(l: &str) -> Option<&str> {
    let rest = l.strip_prefix('f')?;
    if !rest.starts_with(crate::text::is_space) {
        return None;
    }
    Some(rest.trim_start_matches(crate::text::is_space))
}

/// `export_obj` of a triangulated mesh: 1-based indices, and OpenSCAD's
/// `"f "` followed by `" " + index` (two spaces after `f`).
pub fn write(mesh: MeshRef<'_>) -> Vec<u8> {
    // Numbers go straight into the output, as in the OFF writer.
    let mut out = Vec::with_capacity(32 + mesh.vertices.len() * 32 + mesh.faces.len() * 24);
    out.extend_from_slice(b"# OpenSCAD obj exporter\n");
    for v in mesh.vertices {
        out.push(b'v');
        for &c in v {
            out.push(b' ');
            write_g(&mut out, c);
        }
        out.push(b'\n');
    }
    for f in mesh.faces {
        out.extend_from_slice(b"f ");
        for &idx in f {
            out.push(b' ');
            write_int(&mut out, u64::from(idx) + 1);
        }
        out.push(b'\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_match_the_nightly() {
        // The nightly's output for this file, including the line numbers
        // one past the real ones and the file name in the index warning.
        let mut msgs = Vec::new();
        let m = read(b"# hi\nv 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 1 1\nf 1/1/1 2//3 3 9\nfoo bar\nvt 1 2\nf 1 2\n", "e2.obj", &mut msgs);
        let texts: Vec<&str> = msgs.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Unrecognized Line  v 1 1 1 1 in line Line 6",
                "Index e2.obj out of range in Line 7",
                "Unrecognized Line  foo bar in line Line 8"
            ]
        );
        assert_eq!(m.faces, vec![vec![0, 1, 2]]);
    }
}
