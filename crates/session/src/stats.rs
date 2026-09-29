//! Geometry statistics for JSON output (`docs/cli-json.md`: the
//! `geometry` object): bounding box, volume, area, triangle and vertex
//! counts, whether the solid is manifold and how many pieces it has.
//! Terse by design: never a mesh dump.

use geom::Geometry;
use geom::manifold_geom::{GlobalIds, ManifoldGeometry};
use geom::polyset::PolySet;
use serde_json::{Value, json};

/// A 3D solid for statistics and booleans: a Manifold result as it is, a
/// mesh converted as `--render=force` would, a 2D shape as the preview's
/// one-unit slab (so a 2D diff still has volumes to compare).
pub fn solid(g: &Geometry) -> ManifoldGeometry {
    match g {
        Geometry::Manifold(m) => (**m).clone(),
        Geometry::PolySet(ps) => {
            ManifoldGeometry::from_polyset(ps, &GlobalIds, &mut Vec::new(), &mut Vec::new())
        }
        Geometry::Polygon2d(p) => ManifoldGeometry::from_polyset(
            &geom::csg::slab(p),
            &GlobalIds,
            &mut Vec::new(),
            &mut Vec::new(),
        ),
    }
}

/// Connected pieces of a mesh: faces sharing a vertex are one piece.
pub fn components(ps: &PolySet) -> usize {
    let mut parent: Vec<usize> = (0..ps.vertices.len()).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut used = vec![false; ps.vertices.len()];
    for f in &ps.faces {
        for w in f.windows(2) {
            let (a, b) = (
                find(&mut parent, w[0] as usize),
                find(&mut parent, w[1] as usize),
            );
            parent[a] = b;
        }
        for &v in f {
            used[v as usize] = true;
        }
    }
    (0..ps.vertices.len())
        .filter(|&v| used[v] && find(&mut parent, v) == v)
        .count()
}

fn bbox(lo: &[f64], hi: &[f64]) -> Value {
    let size: Vec<f64> = lo.iter().zip(hi).map(|(l, h)| h - l).collect();
    json!({"min": lo, "max": hi, "size": size})
}

/// The `geometry` object.
pub fn geometry(g: &Geometry, scheme: &geom::color::Scheme) -> Value {
    if let Geometry::Polygon2d(p) = g {
        // Outlines are sanitised: outer ones counter-clockwise, holes
        // clockwise, so the signed areas add up to the shape's.
        let area: f64 = p
            .outlines
            .iter()
            .map(|o| {
                let v = &o.vertices;
                (0..v.len())
                    .map(|i| {
                        let (a, b) = (v[i], v[(i + 1) % v.len()]);
                        a[0] * b[1] - b[0] * a[1]
                    })
                    .sum::<f64>()
                    / 2.0
            })
            .sum();
        let (lo, hi) = p.bounds().unwrap_or(([0.0; 2], [0.0; 2]));
        return json!({
            "dimensions": 2,
            "bbox": bbox(&lo, &hi),
            "area": area.abs(),
            "contours": p.outlines.len(),
        });
    }
    let m = solid(g);
    let ps = m.to_polyset(scheme);
    let (lo, hi) = m.bounds().unwrap_or(([0.0; 3], [0.0; 3]));
    let weld = if m.is_valid() {
        weld(&ps)
    } else {
        crate::mesh::Weld::default()
    };
    let pinched = weld.exact;
    let mut v = json!({
        "dimensions": 3,
        "bbox": bbox(&lo, &hi),
        "volume": m.manifold.volume(),
        "area": m.manifold.surface_area(),
        "triangles": m.manifold.num_tri(),
        "vertices": m.manifold.num_vert(),
        "manifold": m.is_valid() && pinched.is_none(),
        "components": components(&ps),
    });
    if let Some(p) = pinched {
        v["pinched"] = pinched_json(&p);
    }
    if let Some(p) = weld.f32.filter(|p| p.nonmanifold_edges > 0) {
        v["stl_precision"] = stl_precision_json(&p, &crate::mesh::Aabb { lo, hi });
    }
    v
}

/// Both welds of a valid solid's mesh ([`crate::mesh::weld`]): by exact
/// position ([`pinched`]) and by `f32` position, as a slicer reads an STL.
pub fn weld(ps: &PolySet) -> crate::mesh::Weld {
    crate::mesh::weld(
        &ps.vertices,
        ps.faces
            .iter()
            .filter(|f| f.len() == 3)
            .map(|f| [f[0], f[1], f[2]]),
    )
}

/// The `stl_precision` object: what rounding to `f32` breaks, where, and
/// the `f32` spacing at the model's largest coordinate (`bbox`).
pub fn stl_precision_json(p: &crate::mesh::StlPrecision, bbox: &crate::mesh::Aabb) -> Value {
    json!({
        "collapsed_faces": p.collapsed_faces,
        "nonmanifold_edges": p.nonmanifold_edges,
        "point": p.at.map(round6),
        "spacing": round6(crate::mesh::f32_spacing(bbox)),
    })
}

/// Edges of a valid solid's mesh that a file of it would show shared by
/// more than two faces ([`crate::mesh::bad_edges`]): where two pieces
/// touch along an edge, Manifold keeps a vertex for each and calls the
/// result manifold, and an STL of it is not.
pub fn pinched(ps: &PolySet) -> Option<crate::mesh::BadEdges> {
    crate::mesh::bad_edges(
        &ps.vertices,
        ps.faces
            .iter()
            .filter(|f| f.len() == 3)
            .map(|f| [f[0], f[1], f[2]]),
    )
}

/// The `pinched` object: how many edges, and the first one's midpoint.
pub fn pinched_json(p: &crate::mesh::BadEdges) -> Value {
    json!({"edges": p.edges, "point": p.at.map(round6)})
}

/// Six significant digits, as the tools print numbers.
///
/// Below 1e-9 (a picometre, far under any kernel's precision) a value is
/// rounding noise and reads 0: a size of `4e-15` is a flat face.
pub fn round6(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    if x.abs() < 1e-9 {
        return 0.0;
    }
    let digits = 5 - x.abs().log10().floor() as i32;
    // Dividing by an exact power of ten gives the nearest double to the
    // decimal, which prints short; multiplying by 0.01 would not.
    let y = if digits >= 0 {
        let f = 10f64.powi(digits);
        (x * f).round() / f
    } else {
        let f = 10f64.powi(-digits);
        (x / f).round() * f
    };
    if y == 0.0 { 0.0 } else { y }
}

/// A box as JSON (`{"min", "max", "size"}`).
pub fn bbox_json(lo: &[f64], hi: &[f64]) -> Value {
    bbox(lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_significant_digits() {
        assert_eq!(round6(23.282123456), 23.2821);
        assert_eq!(round6(0.1 + 0.2), 0.3);
        assert_eq!(round6(13105.098123), 13105.1);
        assert_eq!(round6(12345678.0), 12345700.0);
        assert_eq!(round6(-0.000_012_345_67), -0.0000123457);
        assert_eq!(round6(0.0), 0.0);
        assert_eq!(round6(-1e-300 * 0.0), 0.0);
    }

    #[test]
    fn two_cubes_are_two_components() {
        let mut a = geom::primitives::cube([1.0; 3], false);
        let b = geom::primitives::cube([1.0; 3], false);
        let n = a.vertices.len() as u32;
        a.vertices
            .extend(b.vertices.iter().map(|v| [v[0] + 3.0, v[1], v[2]]));
        a.faces.extend(
            b.faces
                .iter()
                .map(|f| f.iter().map(|&i| i + n).collect::<Vec<_>>()),
        );
        assert_eq!(components(&a), 2);
    }
}
