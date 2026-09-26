//! Geometry to files: the mesh a result exports as, the format writers
//! (which live in the `io` crate) fed with it, and the render summary
//! (`src/RenderStatistic.cc`).
//!
//! STL, OBJ and 3MF need triangles, so meshes are tessellated here first
//! (`PolySetUtils::tessellate_faces`). Triangulations of non-triangular
//! faces can differ from OpenSCAD's (libtess2 there, ear clipping here;
//! see `PolySet::tessellate`), so files of meshes with quads or larger
//! faces describe the same surface with possibly different diagonals.

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

/// `export_off` (see `io::off::write`).
pub fn off(ps: &PolySet, warnings: &mut Warnings) -> Vec<u8> {
    io::off::write(ps.mesh(), warnings)
}

/// `export_obj`: always triangulated.
pub fn obj(ps: &PolySet, warnings: &mut Warnings) -> Vec<u8> {
    io::obj::write(ps.tessellate(warnings).mesh())
}

/// `export_stl`, ASCII or binary: always triangulated.
pub fn stl(ps: &PolySet, binary: bool, warnings: &mut Warnings) -> Vec<u8> {
    io::stl::write(ps.tessellate(warnings).mesh(), binary)
}

/// `export_svg` with the default options.
pub fn svg(p: &Polygon2d) -> Vec<u8> {
    io::svg::write(&p.outlines)
}

/// `export_dxf`.
pub fn dxf(p: &Polygon2d) -> Vec<u8> {
    io::dxf::write(&p.outlines)
}

/// `export_3mf` with the default options: triangulated, faces coloured by
/// base material, the scheme's front colour as the default material. The
/// messages are OpenSCAD's (`Some` severity) or its plain `EXPORT-ERROR`
/// lines; an empty file means the export failed.
pub fn threemf(ps: &PolySet, title: &str, creation_date: &str, default_color: Color, warnings: &mut Warnings) -> (Vec<u8>, Vec<io::Message>) {
    let tri = ps.tessellate(warnings);
    io::threemf::write(tri.mesh(), &io::threemf::WriteOptions { title, creation_date, default_color })
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
    fn binary_stl_layout() {
        let c = primitives::cube([1.0; 3], false);
        let b = stl(&c, true, &mut Vec::new());
        assert_eq!(b.len(), 84 + 12 * 50);
        assert_eq!(&b[80..84], &12u32.to_le_bytes());
        assert!(b.starts_with(b"OpenSCAD Model\n\0"));
    }
}
