//! STL: `import_stl.cc` and `export_stl.cc`.
//!
//! The reader keeps OpenSCAD's format detection exactly, quirks included:
//! a file is binary when its size is `84 + 50 * n` for the facet count `n`
//! stored at byte 80 (even if it starts with `solid`); otherwise it is
//! ASCII only if it starts with `solid`. A file shorter than 84 bytes is
//! never recognised, not even a tiny ASCII one: the C++ tries to read the
//! facet count at byte 80, the failed read leaves the stream in a failed
//! state, and every later read fails too.

use crate::Message;
use crate::mesh::{Mesh, MeshBuilder, MeshRef};
use crate::text::{Lines, parse_f64, shortest, trim};

/// Read an STL file's bytes. `file` is the path as messages print it.
pub fn read(bytes: &[u8], file: &str, msgs: &mut Vec<Message>) -> Mesh {
    let size = bytes.len();
    let mut binary = false;
    if size >= 84 {
        // In 64 bits, as on the 64-bit platforms OpenSCAD's goldens come
        // from: the count of an ASCII file is four letters, and 50 times it
        // overflows a wasm32 `usize` (a panic in a debug build).
        let n = u64::from(u32::from_le_bytes([
            bytes[80], bytes[81], bytes[82], bytes[83],
        ]));
        binary = size as u64 == 84 + 50 * n;
    }
    if binary {
        let mut b = MeshBuilder::new();
        for facet in bytes[84..].as_chunks::<50>().0 {
            let f = |k: usize| {
                f64::from(f32::from_le_bytes([
                    facet[k],
                    facet[k + 1],
                    facet[k + 2],
                    facet[k + 3],
                ]))
            };
            let v = |i: usize| [f(12 + 12 * i), f(16 + 12 * i), f(20 + 12 * i)];
            b.append_polygon(&[v(0), v(1), v(2)]);
        }
        return b.build();
    }
    if size < 84 || !bytes.starts_with(b"solid") {
        msgs.push(Message::error(format!("STL format not recognized in '{file}'.")).at_call());
        return Mesh::default();
    }
    read_ascii(bytes, file, msgs)
}

fn read_ascii(bytes: &[u8], file: &str, msgs: &mut Vec<Message>) -> Mesh {
    let mut b = MeshBuilder::new();
    let mut lines = Lines::new(bytes);
    let mut i = 0usize;
    let mut lineno = 1;
    let mut vdata = [[0.0f64; 3]; 3];
    let err = |msgs: &mut Vec<Message>, lineno: i32, what: &str, line: &str| {
        msgs.push(
            Message::error(format!(
                "STL line {lineno}, {what} line '{line}' importing file '{file}'"
            ))
            .at_call(),
        );
    };
    lines.next_raw();
    let mut line = String::new();
    let mut reached_end = false;
    while !lines.eof {
        lineno += 1;
        line = trim(&lines.next_line()).to_string();
        let l = line.as_str();
        // `^\s*solid|^\s*facet|^\s*endfacet` on the trimmed line.
        if l.is_empty()
            || l.starts_with("solid")
            || l.starts_with("facet")
            || l.starts_with("endfacet")
        {
            continue;
        } else if l == "outer loop" {
            i = 0;
            continue;
        } else if l == "endloop" {
            if i < 3 {
                err(msgs, lineno, "missing vertex", l);
            }
            continue;
        } else if l.starts_with("endsolid") {
            reached_end = true;
            break;
        } else if i >= 3 {
            err(msgs, lineno, "extra vertex", l);
            return Mesh::default();
        } else if let Some(words) = vertex_words(l) {
            for (v, w) in words.iter().enumerate() {
                match parse_f64(w) {
                    Some(x) => vdata[i][v] = x,
                    None => {
                        err(msgs, lineno, "can't parse vertex", l);
                        return Mesh::default();
                    }
                }
            }
            i += 1;
            if i == 3 {
                b.append_polygon(&vdata);
            }
        }
    }
    if !reached_end {
        err(msgs, lineno, "file incomplete", &line);
    }
    b.build()
}

/// `^\s*vertex\s+([^\s]+)\s+([^\s]+)\s+([^\s]+)\s*$`.
fn vertex_words(l: &str) -> Option<[&str; 3]> {
    let rest = l.strip_prefix("vertex")?;
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

/// `export_stl`, ASCII or binary, of a triangulated mesh.
pub fn write(mesh: MeshRef<'_>, binary: bool) -> Vec<u8> {
    let normal = |t: &[u32]| -> [f64; 3] {
        let p = |k: usize| mesh.vertices[t[k] as usize];
        let (p0, p1, p2) = (p(0), p(1), p(2));
        let a = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
        let b = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
        let n = [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ];
        // Eigen's `normalize()`: divide by the norm unless it is zero.
        let sq = n[0] * n[0] + n[1] * n[1] + n[2] * n[2];
        if sq > 0.0 {
            let len = sq.sqrt();
            n.map(|c| c / len)
        } else {
            n
        }
    };
    if binary {
        let mut out = Vec::with_capacity(84 + mesh.faces.len() * 50);
        let mut header = [0u8; 80];
        header[..15].copy_from_slice(b"OpenSCAD Model\n");
        out.extend_from_slice(&header);
        out.extend_from_slice(&(mesh.faces.len() as u32).to_le_bytes());
        for t in mesh.faces {
            let n = normal(t);
            let mut put = |v: [f64; 3]| {
                v.iter()
                    .for_each(|&c| out.extend_from_slice(&(c as f32).to_le_bytes()))
            };
            put(n);
            for &i in t {
                put(mesh.vertices[i as usize]);
            }
            out.extend_from_slice(&[0, 0]);
        }
        return out;
    }
    let vec3 = |v: [f64; 3]| format!("{} {} {}", shortest(v[0]), shortest(v[1]), shortest(v[2]));
    let strings: Vec<String> = mesh.vertices.iter().map(|v| vec3(*v)).collect();
    let mut out = String::from("solid OpenSCAD_Model\n");
    for t in mesh.faces {
        out.push_str("  facet normal ");
        out.push_str(&vec3(normal(t)));
        out.push_str("\n    outer loop\n");
        for &i in t {
            out.push_str("      vertex ");
            out.push_str(&strings[i as usize]);
            out.push('\n');
        }
        out.push_str("    endloop\n  endfacet\n");
    }
    out.push_str("endsolid OpenSCAD_Model\n");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ascii(body: &str) -> Vec<u8> {
        format!("solid x\n{body}endsolid x\n").into_bytes()
    }

    const TRI: &str = "facet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\n";

    #[test]
    fn ascii_and_binary_round_trip() {
        let mut msgs = Vec::new();
        let m = read(&ascii(&TRI.repeat(2)), "f", &mut msgs);
        assert!(msgs.is_empty());
        assert_eq!(m.vertices.len(), 3);
        assert_eq!(m.faces.len(), 2);
        let bin = write(m.as_ref(), true);
        let back = read(&bin, "f", &mut msgs);
        assert_eq!(back.faces, m.faces);
        assert_eq!(back.vertices, m.vertices);
    }

    #[test]
    fn short_files_are_not_recognised() {
        // Nightly 2026.09.23: a 19-byte ASCII STL is "not recognized".
        let mut msgs = Vec::new();
        let m = read(b"solid x\nendsolid x\n", "s1.stl", &mut msgs);
        assert!(m.is_empty());
        assert_eq!(
            msgs,
            vec![Message::error("STL format not recognized in 's1.stl'.").at_call()]
        );
    }

    #[test]
    fn ascii_errors_match_the_nightly() {
        // The nightly's messages for these files, line numbers included.
        let mut msgs = Vec::new();
        let body = "facet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nendloop\nendfacet\nfacet\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nvertex 0 1 1\n";
        read(format!("solid x\n{body}").as_bytes(), "s2", &mut msgs);
        let texts: Vec<&str> = msgs.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "STL line 6, missing vertex line 'endloop' importing file 's2'",
                "STL line 13, extra vertex line 'vertex 0 1 1' importing file 's2'"
            ]
        );
        msgs.clear();
        read(format!("solid x\n{TRI}").as_bytes(), "s3", &mut msgs);
        assert_eq!(
            msgs[0].text,
            "STL line 9, file incomplete line '' importing file 's3'"
        );
    }
}
