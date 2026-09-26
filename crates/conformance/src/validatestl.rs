//! `tests/validatestl.py`: the STL check behind `export-stl-sanitytest`.
//!
//! An STL passes when it has at least one triangle, no infinite or NaN
//! vertex or normal component, and every directed edge (between vertices
//! identified by exact coordinates) is matched by the reverse edge as often
//! as it occurs (`validatestl.py:103-127`).

use std::collections::HashMap;

/// One facet: normal and three points.
type Facet = ([f64; 3], [[f64; 3]; 3]);

/// `read_stl`: a file starting with `solid` is ASCII, anything else binary.
fn read(data: &[u8]) -> Result<Vec<Facet>, String> {
    if data.starts_with(b"solid") {
        let text = String::from_utf8_lossy(data);
        let mut out = Vec::new();
        let mut normal = [0.0; 3];
        let mut points = Vec::new();
        // Python's `float()` accepts "nan" and "inf" too.
        let num = |s: &str| -> Result<f64, String> { s.parse::<f64>().map_err(|_| format!("bad number '{s}'")) };
        for line in text.lines() {
            let line = line.trim();
            let parts: Vec<&str> = line.split(' ').collect();
            if line.starts_with("facet") {
                for i in 2..5 {
                    normal[i - 2] = num(parts.get(i).copied().unwrap_or(""))?;
                }
            } else if line.starts_with("vertex") {
                let mut p = [0.0; 3];
                for i in 1..4 {
                    p[i - 1] = num(parts.get(i).copied().unwrap_or(""))?;
                }
                points.push(p);
            } else if line.starts_with("endfacet") {
                if points.len() != 3 {
                    return Err("facet without three vertices".into());
                }
                out.push((normal, [points[0], points[1], points[2]]));
                points.clear();
                normal = [0.0; 3];
            }
        }
        return Ok(out);
    }
    if data.len() < 84 {
        return Err("Invalid binary stl format".into());
    }
    let count = u32::from_le_bytes(data[80..84].try_into().expect("4 bytes")) as usize;
    let f = |o: usize| f64::from(f32::from_le_bytes(data[o..o + 4].try_into().expect("4 bytes")));
    let mut out = Vec::with_capacity(count);
    for k in 0..count {
        let o = 84 + k * 50;
        if o + 48 > data.len() {
            return Err("Invalid binary stl format".into());
        }
        let v = |i: usize| [f(o + 12 * i), f(o + 12 * i + 4), f(o + 12 * i + 8)];
        out.push((v(0), [v(1), v(2), v(3)]));
    }
    Ok(out)
}

/// `validateSTL`: `Ok` when the mesh is valid, otherwise the message the
/// script prints.
pub fn validate(data: &[u8]) -> Result<(), String> {
    let facets = read(data)?;
    if facets.is_empty() {
        return Err("No triangles found".into());
    }
    let bad = |v: &[f64; 3]| v.iter().any(|c| c.is_nan() || c.is_infinite());
    if facets.iter().any(|(_, p)| p.iter().any(bad)) {
        return Err("NaN of Inf vertices found".into());
    }
    if facets.iter().any(|(n, _)| bad(n)) {
        return Err("NaN of Inf normals found".into());
    }
    // Points are identified by value (`points.index(point)`); -0.0 == 0.0.
    let mut ids: HashMap<[u64; 3], usize> = HashMap::new();
    let mut id = |p: &[f64; 3]| {
        let k = p.map(|c| (c + 0.0).to_bits());
        let n = ids.len();
        *ids.entry(k).or_insert(n)
    };
    let tris: Vec<[usize; 3]> = facets.iter().map(|(_, p)| [id(&p[0]), id(&p[1]), id(&p[2])]).collect();
    let mut edges: HashMap<(usize, usize), i64> = HashMap::new();
    for t in &tris {
        for i in 0..3 {
            *edges.entry((t[i], t[(i + 1) % 3])).or_default() += 1;
            *edges.entry((t[(i + 1) % 3], t[i])).or_default() -= 1;
        }
    }
    let open = edges.values().filter(|&&c| c > 0).count();
    if open > 0 {
        return Err(format!("Non-manifold STL: {open} unmatched edges"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TETRA: &str = "solid t
  facet normal 0 0 -1
    outer loop
      vertex 0 0 0
      vertex 0 1 0
      vertex 1 0 0
    endloop
  endfacet
  facet normal 0 -1 0
    outer loop
      vertex 0 0 0
      vertex 1 0 0
      vertex 0 0 1
    endloop
  endfacet
  facet normal -1 0 0
    outer loop
      vertex 0 0 0
      vertex 0 0 1
      vertex 0 1 0
    endloop
  endfacet
  facet normal 1 1 1
    outer loop
      vertex 1 0 0
      vertex 0 1 0
      vertex 0 0 1
    endloop
  endfacet
endsolid t
";

    #[test]
    fn closed_tetrahedron_passes_and_open_one_fails() {
        assert_eq!(validate(TETRA.as_bytes()), Ok(()));
        let open = TETRA.replacen("vertex 0 0 1\n      vertex 0 1 0", "vertex 0 1 0\n      vertex 0 0 1", 1);
        assert!(validate(open.as_bytes()).is_err());
        let nan = TETRA.replacen("normal 0 0 -1", "normal nan 0 -1", 1);
        assert_eq!(validate(nan.as_bytes()), Err("NaN of Inf normals found".into()));
    }
}
