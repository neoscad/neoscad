//! `surface()`: heightmaps from `.dat` text or PNG images, turned into a
//! closed mesh exactly as `SurfaceNode::createGeometry` builds it
//! (`src/core/SurfaceNode.cc`).
//!
//! Layout: each grid cell becomes four triangles meeting at the cell's
//! centre (height: the mean of its corners), the four sides are quads
//! down to one below the lowest height, and the bottom is one polygon.
//! Everything goes through the `PolySetBuilder` rules ([`MeshBuilder`]),
//! so equal positions share a vertex.
//!
//! PNG pixels are read as lodepng converts them to 16-bit RGBA (8-bit
//! channels are repeated into both bytes, low bit depths scaled up), then
//! weighted by the Rec. 709 luma coefficients into 0..100; rows are flipped
//! so the image's top row is at the largest y.

use crate::Message;
use crate::mesh::{Mesh, MeshBuilder};
use crate::text::{Lines, parse_f64, trim};

/// A heightmap: row-major, row 0 at y = 0.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Heightmap {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f64>,
    /// `img_data_t::min_val` as the readers leave it (not always the true
    /// minimum: see [`read_dat`]).
    pub min_val: f64,
}

/// `read_png_or_dat`: `bytes` is `None` when the file could not be opened.
pub fn read(bytes: Option<&[u8]>, file: &str, invert: bool, msgs: &mut Vec<Message>) -> Heightmap {
    let Some(bytes) = bytes else {
        msgs.push(Message::warning(format!(
            "The file '{file}' couldn't be opened."
        )));
        return Heightmap::default();
    };
    if !bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return read_dat(bytes, file, msgs);
    }
    match read_png(bytes, invert) {
        Some(h) => h,
        None => {
            msgs.push(Message::warning(format!("Can't read PNG image '{file}'")));
            Heightmap::default()
        }
    }
}

fn read_png(bytes: &[u8], invert: bool) -> Option<Heightmap> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (width, height) = (info.width as usize, info.height as usize);
    let channels = info.color_type.samples();
    let wide = info.bit_depth == png::BitDepth::Sixteen;
    let sample = |i: usize| -> u16 {
        if wide {
            u16::from_be_bytes([buf[2 * i], buf[2 * i + 1]])
        } else {
            u16::from(buf[i]) * 257
        }
    };
    let mut data = vec![0.0; width * height];
    let mut min_val = 200.0f64;
    for y in 0..height {
        for x in 0..width {
            let p = (y * width + x) * channels;
            let (r, g, b) = if channels >= 3 {
                (sample(p), sample(p + 1), sample(p + 2))
            } else {
                (sample(p), sample(p), sample(p))
            };
            let pixel = 0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b);
            let z = 100.0 / 65535.0 * if invert { 0.0 - pixel } else { pixel };
            data[x + width * (height - 1 - y)] = z;
            min_val = z.min(min_val);
        }
    }
    Some(Heightmap {
        width,
        height,
        data,
        min_val,
    })
}

/// `read_dat`: rows of numbers separated by spaces or tabs; blank lines
/// and lines starting with `#` are skipped, short rows are padded with 0.
/// `min_val` starts at 1, not at the first value ("this balances out with
/// the (min_val-1) inside createGeometry, to match old behavior").
///
/// A value that does not parse empties the whole map, with a warning
/// unless it is on the last line: the C++ returns its still-empty result.
/// That includes a comment on a last line with no newline after it, which
/// the skipping loop hands on as data.
pub fn read_dat(bytes: &[u8], file: &str, msgs: &mut Vec<Message>) -> Heightmap {
    let mut lines = Lines::new(bytes);
    let mut rows: Vec<Vec<f64>> = Vec::new();
    let mut columns = 0;
    let mut min_val = 1.0f64;
    while !lines.eof {
        let mut line = String::new();
        while !lines.eof && (line.is_empty() || line.starts_with('#')) {
            line = trim(&lines.next_line()).to_string();
        }
        if line.is_empty() && lines.eof {
            break;
        }
        let mut row = Vec::new();
        for token in line.split([' ', '\t']).filter(|t| !t.is_empty()) {
            match parse_f64(token) {
                Some(v) => {
                    row.push(v);
                    min_val = v.min(min_val);
                }
                None => {
                    if !lines.eof {
                        msgs.push(Message::warning(format!(
                            "Illegal value in '{file}': bad lexical cast: source type value could not be interpreted as target"
                        )));
                    }
                    return Heightmap::default();
                }
            }
        }
        columns = columns.max(row.len());
        rows.push(row);
    }
    let height = rows.len();
    let mut data = vec![0.0; height * columns];
    for (i, row) in rows.iter().enumerate() {
        data[i * columns..i * columns + row.len()].copy_from_slice(row);
    }
    Heightmap {
        width: columns,
        height,
        data,
        min_val,
    }
}

/// `SurfaceNode::createGeometry` for a heightmap.
pub fn mesh(h: &Heightmap, center: bool) -> Mesh {
    let lines = h.height as i64;
    let columns = h.width as i64;
    let min_val = h.min_val - 1.0;
    let ox = if center {
        -((columns - 1) as f64) / 2.0
    } else {
        0.0
    };
    let oy = if center {
        -((lines - 1) as f64) / 2.0
    } else {
        0.0
    };
    let d = |x: i64, y: i64| h.data[(x + y * columns) as usize];
    let p = |x: f64, y: f64, z: f64| [ox + x, oy + y, z];
    let mut b = MeshBuilder::new();
    for i in 1..lines {
        for j in 1..columns {
            let (v1, v2, v3, v4) = (d(j - 1, i - 1), d(j, i - 1), d(j - 1, i), d(j, i));
            let vx = (v1 + v2 + v3 + v4) / 4.0;
            let (fi, fj) = (i as f64, j as f64);
            let mid = p(fj - 0.5, fi - 0.5, vx);
            b.append_polygon(&[p(fj - 1.0, fi - 1.0, v1), p(fj, fi - 1.0, v2), mid]);
            b.append_polygon(&[p(fj, fi - 1.0, v2), p(fj, fi, v4), mid]);
            b.append_polygon(&[p(fj, fi, v4), p(fj - 1.0, fi, v3), mid]);
            b.append_polygon(&[p(fj - 1.0, fi, v3), p(fj - 1.0, fi - 1.0, v1), mid]);
        }
    }
    let last_c = (columns - 1) as f64;
    let last_l = (lines - 1) as f64;
    // Edges along Y.
    for i in 1..lines {
        let (v1, v2, v3, v4) = (
            d(0, i - 1),
            d(0, i),
            d(columns - 1, i - 1),
            d(columns - 1, i),
        );
        let fi = i as f64;
        b.append_polygon(&[
            p(0.0, fi - 1.0, min_val),
            p(0.0, fi - 1.0, v1),
            p(0.0, fi, v2),
            p(0.0, fi, min_val),
        ]);
        b.append_polygon(&[
            p(last_c, fi, min_val),
            p(last_c, fi, v4),
            p(last_c, fi - 1.0, v3),
            p(last_c, fi - 1.0, min_val),
        ]);
    }
    // Edges along X.
    for i in 1..columns {
        let (v1, v2, v3, v4) = (d(i - 1, 0), d(i, 0), d(i - 1, lines - 1), d(i, lines - 1));
        let fi = i as f64;
        b.append_polygon(&[
            p(fi, 0.0, min_val),
            p(fi, 0.0, v2),
            p(fi - 1.0, 0.0, v1),
            p(fi - 1.0, 0.0, min_val),
        ]);
        b.append_polygon(&[
            p(fi - 1.0, last_l, min_val),
            p(fi - 1.0, last_l, v3),
            p(fi, last_l, v4),
            p(fi, last_l, min_val),
        ]);
    }
    // The bottom, one below the lowest height.
    if columns > 1 && lines > 1 {
        b.begin_polygon();
        for i in 0..lines - 1 {
            b.add_vertex(p(0.0, i as f64, min_val));
        }
        for i in 0..columns - 1 {
            b.add_vertex(p(i as f64, last_l, min_val));
        }
        for i in (1..lines).rev() {
            b.add_vertex(p(last_c, i as f64, min_val));
        }
        for i in (1..columns).rev() {
            b.add_vertex(p(i as f64, 0.0, min_val));
        }
    }
    b.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dat_grid_to_mesh() {
        let mut msgs = Vec::new();
        // A short row is padded with 0, which does not count towards the
        // minimum (that starts at 1 and only sees parsed values).
        let h = read_dat(b"# c\n1 2\n3\n", "f", &mut msgs);
        assert!(msgs.is_empty());
        assert_eq!((h.width, h.height), (2, 2));
        assert_eq!(h.data, vec![1.0, 2.0, 3.0, 0.0]);
        assert_eq!(h.min_val, 1.0);
        let h = read_dat(b"1 2\n3 4", "f", &mut msgs);
        let m = mesh(&h, false);
        // 4 triangles, 4 side quads, the bottom quad.
        assert_eq!(m.faces.len(), 9);
        // 4 corners on top, the centre, 4 corners at the bottom.
        assert_eq!(m.vertices.len(), 9);
    }

    #[test]
    fn bad_values_empty_the_map() {
        let mut msgs = Vec::new();
        let h = read_dat(b"1 2\nx 3\n4 5\n", "f", &mut msgs);
        assert_eq!(h, Heightmap::default());
        assert_eq!(
            msgs[0].text,
            "Illegal value in 'f': bad lexical cast: source type value could not be interpreted as target"
        );
        // A trailing comment without a newline is read as data, silently.
        msgs.clear();
        let h = read_dat(b"1 2\n# end", "f", &mut msgs);
        assert_eq!(h, Heightmap::default());
        assert!(msgs.is_empty());
    }
}
