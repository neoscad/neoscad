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
    json!({
        "dimensions": 3,
        "bbox": bbox(&lo, &hi),
        "volume": m.manifold.volume(),
        "area": m.manifold.surface_area(),
        "triangles": m.manifold.num_tri(),
        "vertices": m.manifold.num_vert(),
        "manifold": m.is_valid(),
        "components": components(&ps),
    })
}

/// A box as JSON (`{"min", "max", "size"}`).
pub fn bbox_json(lo: &[f64], hi: &[f64]) -> Value {
    bbox(lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

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
