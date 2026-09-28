//! Geometry to files: the mesh a result exports as, the format writers
//! (which live in the `io` crate) fed with it, and the render summary
//! (`src/RenderStatistic.cc`).
//!
//! STL, OBJ and 3MF need triangles, so meshes are tessellated here first
//! (`PolySetUtils::tessellate_faces`), with a port of the libtess2 that
//! OpenSCAD uses (see `PolySet::tessellate`): quads and larger faces get
//! OpenSCAD's diagonals, triangle order and first vertices. STL facet
//! normals can still differ in the last bits (`docs/followups.md`).

use crate::Geometry;
use crate::color::{Color, Scheme};
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

/// `createSortedPolySet` (see [`io::mesh::sorted`]): the mesh every 3D
/// writer but DXF/SVG/PDF writes under `--enable=predictible-output`.
pub fn sorted(ps: &PolySet) -> PolySet {
    let m = io::mesh::sorted(ps.mesh());
    PolySet {
        vertices: m.vertices,
        faces: m.faces,
        colors: m.colors,
        color_indices: m.color_indices,
        convex: ps.convex,
        triangular: ps.triangular,
    }
}

/// `ps`, or its sorted copy when `sort` (`predictible-output`) is on.
/// Borrowing in the default case keeps plain exports free of the copy.
pub fn ordered(ps: &PolySet, sort: bool) -> std::borrow::Cow<'_, PolySet> {
    if sort {
        std::borrow::Cow::Owned(sorted(ps))
    } else {
        std::borrow::Cow::Borrowed(ps)
    }
}

/// `export_off` (see `io::off::write`); `sort` is `predictible-output`.
pub fn off(ps: &PolySet, sort: bool, warnings: &mut Warnings) -> Vec<u8> {
    io::off::write(ordered(ps, sort).mesh(), warnings)
}

/// `export_obj`: always triangulated. Upstream sorts after triangulating,
/// so the sort sees (and orders) the triangles, not the polygons.
pub fn obj(ps: &PolySet, sort: bool, warnings: &mut Warnings) -> Vec<u8> {
    let tri = ps.tessellate(warnings);
    io::obj::write(ordered(&tri, sort).mesh())
}

/// `export_stl`, ASCII or binary: always triangulated, then sorted when
/// `sort` is on, as `append_stl` does.
pub fn stl(ps: &PolySet, binary: bool, sort: bool, warnings: &mut Warnings) -> Vec<u8> {
    let tri = ps.tessellate(warnings);
    io::stl::write(ordered(&tri, sort).mesh(), binary)
}

/// `export_svg` with the given paint (`-O export-svg/...`).
pub fn svg(p: &Polygon2d, style: &io::svg::SvgStyle) -> Vec<u8> {
    io::svg::write_styled(&p.outlines, style)
}

/// `export_pdf`: the file and its `EXPORT-WARNING` texts.
pub fn pdf(
    p: &Polygon2d,
    options: &io::pdf::PdfOptions,
    info: &io::pdf::PdfInfo<'_>,
) -> (Vec<u8>, Vec<String>) {
    io::pdf::write(&p.outlines, options, info)
}

/// `export_wrl` (see `io::wrl::write`); `sort` is `predictible-output`.
pub fn wrl(ps: &PolySet, sort: bool, warnings: &mut Warnings) -> Vec<u8> {
    io::wrl::write(ordered(ps, sort).mesh(), warnings)
}

/// `export_dxf`.
pub fn dxf(p: &Polygon2d) -> Vec<u8> {
    io::dxf::write(&p.outlines)
}

/// `export_3mf` with the default options: triangulated, faces coloured by
/// base material, the scheme's front colour as the default material. The
/// messages are OpenSCAD's (`Some` severity) or its plain `EXPORT-ERROR`
/// lines; an empty file means the export failed.
pub fn threemf(
    ps: &PolySet,
    title: &str,
    creation_date: &str,
    default_color: Color,
    warnings: &mut Warnings,
) -> (Vec<u8>, Vec<io::Message>) {
    let tri = ps.tessellate(warnings);
    io::threemf::write(
        tri.mesh(),
        &io::threemf::WriteOptions {
            title,
            creation_date,
            default_color,
        },
    )
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
                format!(
                    "   Convex:       {}",
                    if ps.is_convex() { "yes" } else { "no" }
                ),
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
            format!(
                "   Status:     {}",
                crate::manifold_geom::status_name(m.manifold.status())
            ),
            format!("   Genus:      {}", m.manifold.genus()),
            format!("   Vertices:   {:6}", m.manifold.num_vert()),
            format!("   Facets:     {:6}", m.manifold.num_tri()),
        ],
        Geometry::Polygon2d(p) => vec![
            "Top level object is a 2D object:".to_string(),
            format!("   Contours:   {:6}", p.outlines.len()),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    #[test]
    fn off_of_a_cube_matches_the_nightly() {
        let c = primitives::cube([1.0; 3], false);
        let text = String::from_utf8(off(&c, false, &mut Vec::new())).unwrap();
        // `openscad -o c.off` on `cube(1);`, 2026.09.23 nightly.
        let expected = "OFF\n8 6 0\n0 0 0 \n1 0 0 \n0 1 0 \n1 1 0 \n0 0 1 \n1 0 1 \n0 1 1 \n1 1 1 \n4 4 5 7 6\n4 2 3 1 0\n4 0 1 5 4\n4 1 3 7 5\n4 3 2 6 7\n4 2 0 4 6\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn sorted_off_of_a_cube_matches_the_nightly() {
        let c = primitives::cube([1.0; 3], false);
        let text = String::from_utf8(off(&c, true, &mut Vec::new())).unwrap();
        // `openscad --enable=predictible-output -o c.off` on `cube(1);`,
        // 2026.09.23 nightly.
        let expected = "OFF\n8 6 0\n0 0 0 \n0 0 1 \n0 1 0 \n0 1 1 \n1 0 0 \n1 0 1 \n1 1 0 \n1 1 1 \n4 0 1 3 2\n4 0 2 6 4\n4 0 4 5 1\n4 1 5 7 3\n4 2 3 7 6\n4 4 6 7 5\n";
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
        assert_eq!(
            String::from_utf8(svg(&p, &io::svg::SvgStyle::default())).unwrap(),
            expected
        );
    }

    #[test]
    fn binary_stl_layout() {
        let c = primitives::cube([1.0; 3], false);
        let b = stl(&c, true, false, &mut Vec::new());
        assert_eq!(b.len(), 84 + 12 * 50);
        assert_eq!(&b[80..84], &12u32.to_le_bytes());
        assert!(b.starts_with(b"OpenSCAD Model\n\0"));
    }
}
