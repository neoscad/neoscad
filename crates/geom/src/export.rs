//! Mesh files in OpenSCAD's formats (`src/io/export_{stl,off,obj}.cc`), 2D
//! files (`export_{svg,dxf}.cc`) and the render summary
//! (`src/RenderStatistic.cc`).
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
use crate::polygon2d::Polygon2d;
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

/// `export_svg` (`export_svg.cc`) with the default options (no fill, a
/// black stroke 0.35 wide): a view box of whole millimetres around the
/// shape padded by half the stroke, then one path with every outline, y
/// flipped (so `0` prints as `-0`), six points to a line.
pub fn svg(p: &Polygon2d) -> Vec<u8> {
    let stroke_width = 0.35;
    let pad = stroke_width / 2.0;
    let (lo, hi) = p.bounds().unwrap_or(([f64::MAX; 2], [-f64::MAX; 2]));
    let minx = (lo[0] - pad).floor() as i32;
    let miny = (-hi[1] - pad).floor() as i32;
    let maxx = (hi[0] + pad).ceil() as i32;
    let maxy = (-lo[1] + pad).ceil() as i32;
    let (width, height) = (maxx - minx, maxy - miny);
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" standalone=\"no\"?>\n");
    out.push_str("<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">\n");
    out.push_str(&format!(
        "<svg width=\"{width}mm\" height=\"{height}mm\" viewBox=\"{minx} {miny} {width} {height}\" xmlns=\"http://www.w3.org/2000/svg\" version=\"1.1\">\n"
    ));
    out.push_str("<title>OpenSCAD Model</title>\n");
    out.push_str("<path d=\"\n");
    for o in &p.outlines {
        let Some(p0) = o.vertices.first() else { continue };
        out.push_str(&format!("M {},{}", fmt_g(p0[0]), fmt_g(-p0[1])));
        for (idx, v) in o.vertices.iter().enumerate().skip(1) {
            out.push_str(&format!(" L {},{}", fmt_g(v[0]), fmt_g(-v[1])));
            if idx % 6 == 5 {
                out.push('\n');
            }
        }
        out.push_str(" z\n");
    }
    out.push_str(&format!("\" stroke=\"black\" fill=\"none\" stroke-width=\"{}\"/>\n", fmt_g(stroke_width)));
    out.push_str("</svg>\n");
    out.into_bytes()
}

/// The fixed part of `export_dxf_header` (`export_dxf.cc:40-200`) after the
/// extents: line type, layer and style tables, and an empty BLOCKS section.
const DXF_TABLES: &str = "  0\nENDSEC\n  0\nSECTION\n  2\nTABLES\n  0\nTABLE\n  2\nLTYPE\n 70\n1\n  0\nLTYPE\n  2\nCONTINUOUS\n 70\n64\n  3\nSolid line\n 72\n65\n 73\n0\n 40\n0.000000\n  0\nENDTAB\n  0\nTABLE\n  2\nLAYER\n 70\n6\n  0\nLAYER\n  2\n0\n 70\n64\n 62\n7\n  6\nCONTINUOUS\n  0\nENDTAB\n  0\nTABLE\n  2\nSTYLE\n 70\n0\n  0\nENDTAB\n  0\nENDSEC\n  0\nSECTION\n  2\nBLOCKS\n  0\nENDSEC\n";

/// `export_dxf` (`export_dxf.cc`): an R12-style header with the extents,
/// then one entity per outline (a POINT, a LINE, or a closed LWPOLYLINE).
///
/// The extents start from `DBL_MAX` and `DBL_MIN` as in OpenSCAD, and
/// `DBL_MIN` is the smallest positive double, not the most negative: a
/// shape entirely left of or below the origin keeps `2.22507e-308` as its
/// maximum. That is copied so the files compare equal.
pub fn dxf(p: &Polygon2d) -> Vec<u8> {
    let (mut x_min, mut y_min) = (f64::MAX, f64::MAX);
    let (mut x_max, mut y_max) = (f64::MIN_POSITIVE, f64::MIN_POSITIVE);
    for v in p.outlines.iter().flat_map(|o| o.vertices.iter()) {
        if x_min > v[0] {
            x_min = v[0];
        }
        if x_max < v[0] {
            x_max = v[0];
        }
        if y_min > v[1] {
            y_min = v[1];
        }
        if y_max < v[1] {
            y_max = v[1];
        }
    }
    let (x0, y0, x1, y1) = (fmt_g(x_min), fmt_g(y_min), fmt_g(x_max), fmt_g(y_max));
    let mut out = String::from("999\nDXF from OpenSCAD\n");
    out.push_str("  0\nSECTION\n  2\nHEADER\n  9\n$ACADVER\n  1\nAC1006\n  9\n$INSBASE\n 10\n0.0\n 20\n0.0\n 30\n0.0\n");
    out.push_str(&format!("  9\n$EXTMIN\n 10\n{x0}\n 20\n{y0}\n  9\n$EXTMAX\n 10\n{x1}\n 20\n{y1}\n"));
    out.push_str(&format!("  9\n$LINMIN\n 10\n{x0}\n 20\n{y0}\n  9\n$LINMAX\n 10\n{x1}\n 20\n{y1}\n"));
    out.push_str(DXF_TABLES);
    out.push_str("  0\nSECTION\n  2\nENTITIES\n");
    for o in &p.outlines {
        match o.vertices.as_slice() {
            [a] => out.push_str(&format!("  0\nPOINT\n100\nAcDbEntity\n  8\n0\n100\nAcDbPoint\n 10\n{}\n 20\n{}\n", fmt_g(a[0]), fmt_g(a[1]))),
            [a, b] => out.push_str(&format!(
                "  0\nLINE\n100\nAcDbEntity\n  8\n0\n100\nAcDbLine\n 10\n{}\n 20\n{}\n 11\n{}\n 21\n{}\n",
                fmt_g(a[0]),
                fmt_g(a[1]),
                fmt_g(b[0]),
                fmt_g(b[1])
            )),
            vs => {
                out.push_str(&format!("  0\nLWPOLYLINE\n100\nAcDbEntity\n  8\n0\n100\nAcDbPolyline\n 90\n{}\n 70\n1\n", vs.len()));
                for v in vs {
                    out.push_str(&format!(" 10\n{}\n 20\n{}\n", fmt_g(v[0]), fmt_g(v[1])));
                }
            }
        }
    }
    out.push_str("  0\nENDSEC\n  0\nEOF\n");
    out.into_bytes()
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
    fn svg_of_an_offset_matches_the_nightly() {
        // `offset(r=1, $fn=8) square(10);` exported with `-o x.svg` by the
        // 2026.09.23 nightly: Clipper's vertex order and start point, the
        // flipped y with its `-0`, and the six-points-a-line wrapping.
        let n = 8.0f64;
        let tol = 1.0 - eval::trig::cos_degrees(180.0 / n);
        let sq = primitives::square([10.0, 10.0], false);
        let p = crate::clipper::offset(&sq, 1.0, crate::clipper::Join::Round, 2.0, tol);
        let expected = "<?xml version=\"1.0\" standalone=\"no\"?>
<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">
<svg width=\"14mm\" height=\"14mm\" viewBox=\"-2 -12 14 14\" xmlns=\"http://www.w3.org/2000/svg\" version=\"1.1\">
<title>OpenSCAD Model</title>
<path d=\"
M 10.7071,0.707107 L 11,-0 L 11,-10 L 10.7071,-10.7071 L 10,-11 L 0,-11
 L -0.707107,-10.7071 L -1,-10 L -1,-0 L -0.707107,0.707107 L 0,1 L 10,1
 z
\" stroke=\"black\" fill=\"none\" stroke-width=\"0.35\"/>
</svg>
";
        assert_eq!(String::from_utf8(svg(&p)).unwrap(), expected);
    }

    #[test]
    fn dxf_keeps_openscads_extent_quirk() {
        // `translate([-5,-3]) polygon([[0,0],[2,0],[1,1]]);` on the nightly:
        // the maximum extents stay at DBL_MIN for a shape left of and below
        // the origin.
        let p = Polygon2d::from_outline(vec![[-4.0, -2.0], [-5.0, -3.0], [-3.0, -3.0]]);
        let text = String::from_utf8(dxf(&p)).unwrap();
        assert!(text.starts_with("999\nDXF from OpenSCAD\n  0\nSECTION\n  2\nHEADER\n"));
        assert!(text.contains("  9\n$EXTMIN\n 10\n-5\n 20\n-3\n  9\n$EXTMAX\n 10\n2.22507e-308\n 20\n2.22507e-308\n"));
        assert!(text.ends_with(
            "  0\nLWPOLYLINE\n100\nAcDbEntity\n  8\n0\n100\nAcDbPolyline\n 90\n3\n 70\n1\n 10\n-4\n 20\n-2\n 10\n-5\n 20\n-3\n 10\n-3\n 20\n-3\n  0\nENDSEC\n  0\nEOF\n"
        ));
        assert_eq!(text.len(), 708);
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
