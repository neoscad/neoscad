//! Mesh files in OpenSCAD's formats (`src/io/export_{stl,off,obj}.cc`) and
//! the render summary (`src/RenderStatistic.cc`).
//!
//! The byte layout follows OpenSCAD: OFF and OBJ print coordinates with
//! C++'s default `ostream << double` (`%g`, 6 significant digits), ASCII STL
//! with double-conversion's shortest round-trip form, binary STL with `f32`.
//! Triangulations of non-triangular faces can differ from OpenSCAD's
//! (libtess2 there, ear clipping here; see `PolySet::tessellate`), so files
//! of meshes with quads or larger faces describe the same surface with
//! possibly different diagonals.

use lang::number::fmt_g;

use crate::Geometry;
use crate::color::Scheme;
use crate::polyset::{PolySet, Warnings};

/// `PolySetUtils::getGeometryAsPolySet`: the mesh a 3D result exports as.
/// `None` for 2D geometry.
pub fn as_polyset(g: &Geometry, scheme: &Scheme) -> Option<PolySet> {
    match g {
        Geometry::PolySet(p) => Some((**p).clone()),
        Geometry::Manifold(m) => Some(m.to_polyset(scheme)),
        Geometry::Polygon2d(_) => None,
    }
}

/// `export_off` (`export_off.cc:47-83`). Faces keep their colours as
/// `r g b` bytes, plus alpha when it is not 255.
///
/// An invalid colour (from `color()` with nothing usable) is where this
/// differs on purpose: OpenSCAD warns "Invalid color in OFF export" and
/// then prints four uninitialised ints (0 0 0 0 on the nightly, which an
/// importer draws as transparent black). Here the warning is the same but
/// the face is written without a colour, so readers fall back to their
/// default colour, which is what OpenSCAD's own render shows.
pub fn off(ps: &PolySet, warnings: &mut Warnings) -> Vec<u8> {
    let mut out = String::with_capacity(ps.vertices.len() * 32 + ps.faces.len() * 24);
    out.push_str(&format!("OFF\n{} {} 0\n", ps.vertices.len(), ps.faces.len()));
    for v in &ps.vertices {
        out.push_str(&format!("{} {} {} \n", fmt_g(v[0]), fmt_g(v[1]), fmt_g(v[2])));
    }
    let has_color = !ps.color_indices.is_empty();
    for (i, f) in ps.faces.iter().enumerate() {
        out.push_str(&f.len().to_string());
        for idx in f {
            out.push(' ');
            out.push_str(&idx.to_string());
        }
        if has_color {
            let ci = ps.color_indices[i];
            if ci >= 0 {
                match ps.colors[ci as usize].rgba_int() {
                    Some([r, g, b, a]) => {
                        out.push_str(&format!(" {r} {g} {b}"));
                        if a != 255 {
                            out.push_str(&format!(" {a}"));
                        }
                    }
                    None => warnings.push("Invalid color in OFF export".into()),
                }
            }
        }
        out.push('\n');
    }
    out.into_bytes()
}

/// `export_obj` (`export_obj.cc`): always triangulated, 1-based indices,
/// and OpenSCAD's `"f "` followed by `" " + index` (two spaces after `f`).
pub fn obj(ps: &PolySet, warnings: &mut Warnings) -> Vec<u8> {
    let ps = ps.tessellate(warnings);
    let mut out = String::from("# OpenSCAD obj exporter\n");
    for v in &ps.vertices {
        out.push_str(&format!("v {} {} {}\n", fmt_g(v[0]), fmt_g(v[1]), fmt_g(v[2])));
    }
    for f in &ps.faces {
        out.push_str("f ");
        for idx in f {
            out.push_str(&format!(" {}", idx + 1));
        }
        out.push('\n');
    }
    out.into_bytes()
}

/// `export_stl` (`export_stl.cc`), ASCII or binary.
pub fn stl(ps: &PolySet, binary: bool, warnings: &mut Warnings) -> Vec<u8> {
    let ps = ps.tessellate(warnings);
    let normal = |t: &[u32]| -> [f64; 3] {
        let p = |k: usize| ps.vertices[t[k] as usize];
        let (p0, p1, p2) = (p(0), p(1), p(2));
        let a = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
        let b = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
        let n = [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
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
        let mut out = Vec::with_capacity(84 + ps.faces.len() * 50);
        let mut header = [0u8; 80];
        header[..15].copy_from_slice(b"OpenSCAD Model\n");
        out.extend_from_slice(&header);
        out.extend_from_slice(&(ps.faces.len() as u32).to_le_bytes());
        for t in &ps.faces {
            let n = normal(t);
            let mut put = |v: [f64; 3]| v.iter().for_each(|&c| out.extend_from_slice(&(c as f32).to_le_bytes()));
            put(n);
            for &i in t {
                put(ps.vertices[i as usize]);
            }
            out.extend_from_slice(&[0, 0]);
        }
        return out;
    }
    let strings: Vec<String> = ps.vertices.iter().map(|v| vec3_shortest(*v)).collect();
    let mut out = String::from("solid OpenSCAD_Model\n");
    for t in &ps.faces {
        out.push_str("  facet normal ");
        out.push_str(&vec3_shortest(normal(t)));
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

fn vec3_shortest(v: [f64; 3]) -> String {
    format!("{} {} {}", shortest(v[0]), shortest(v[1]), shortest(v[2]))
}

/// double-conversion `ToShortest` as `export_stl.cc:52-61` configures it:
/// shortest round-trip digits, decimal notation for exponents -6..=20,
/// otherwise `1.5e-7` / `1e21` (no `+`), `-0` printed as `0`, and nothing at
/// all for infinities and NaN (the converter has no symbols for them).
pub fn shortest(v: f64) -> String {
    if !v.is_finite() {
        return String::new();
    }
    if v == 0.0 {
        return "0".into();
    }
    // Rust's `{:e}` prints the shortest round-trip digits.
    let e = format!("{:e}", v.abs());
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let n = digits.len() as i32;
    let mut out = String::new();
    if v < 0.0 {
        out.push('-');
    }
    if (-6..21).contains(&exp) {
        if exp < 0 {
            out.push_str("0.");
            (0..(-exp - 1)).for_each(|_| out.push('0'));
            out.push_str(&digits);
        } else if exp >= n - 1 {
            out.push_str(&digits);
            (0..(exp - (n - 1))).for_each(|_| out.push('0'));
        } else {
            let p = (exp + 1) as usize;
            out.push_str(&digits[..p]);
            out.push('.');
            out.push_str(&digits[p..]);
        }
    } else {
        out.push_str(&digits[..1]);
        if n > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push_str(&format!("e{exp}"));
    }
    out
}

/// The top-level object lines of OpenSCAD's render summary
/// (`LogVisitor::visit`, `RenderStatistic.cc:224-303`). Empty geometry
/// prints nothing.
pub fn summary(g: &Geometry) -> Vec<String> {
    if g.is_empty() {
        return Vec::new();
    }
    match g {
        Geometry::PolySet(ps) => {
            let mut l = vec![
                "Top level object is a 3D object (PolySet):".to_string(),
                format!("   Convex:       {}", if ps.is_convex() { "yes" } else { "no" }),
            ];
            if ps.triangular {
                l.push(format!("   Triangles: {:6}", ps.faces.len()));
            } else {
                l.push(format!("   Facets:    {:6}", ps.faces.len()));
            }
            l
        }
        Geometry::Manifold(m) => vec![
            "   Top level object is a 3D object (manifold):".to_string(),
            format!("   Status:     {}", crate::manifold_geom::status_name(m.manifold.status())),
            format!("   Genus:      {}", m.manifold.genus()),
            format!("   Vertices:   {:6}", m.manifold.num_vert()),
            format!("   Facets:     {:6}", m.manifold.num_tri()),
        ],
        Geometry::Polygon2d(p) => vec!["Top level object is a 2D object:".to_string(), format!("   Contours:   {:6}", p.outlines.len())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    #[test]
    fn shortest_matches_double_conversion() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-2.5, "-2.5"),
            (0.1, "0.1"),
            (1e-6, "0.000001"),
            (1.5e-7, "1.5e-7"),
            (123456.0, "123456"),
            (1e20, "100000000000000000000"),
            (1e21, "1e21"),
            (0.8660254037844386, "0.8660254037844386"),
            (f64::NAN, ""),
        ];
        for &(v, s) in cases {
            assert_eq!(shortest(v), s, "{v:e}");
        }
    }

    #[test]
    fn off_of_a_cube_matches_the_nightly() {
        let c = primitives::cube([1.0; 3], false);
        let text = String::from_utf8(off(&c, &mut Vec::new())).unwrap();
        // `openscad -o c.off` on `cube(1);`, 2026.09.23 nightly.
        let expected = "OFF\n8 6 0\n0 0 0 \n1 0 0 \n0 1 0 \n1 1 0 \n0 0 1 \n1 0 1 \n0 1 1 \n1 1 1 \n4 4 5 7 6\n4 2 3 1 0\n4 0 1 5 4\n4 1 3 7 5\n4 3 2 6 7\n4 2 0 4 6\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn binary_stl_layout() {
        let c = primitives::cube([1.0; 3], false);
        let b = stl(&c, true, &mut Vec::new());
        assert_eq!(b.len(), 84 + 12 * 50);
        assert_eq!(&b[80..84], &12u32.to_le_bytes());
        assert!(b.starts_with(b"OpenSCAD Model\n\0"));
    }
}
