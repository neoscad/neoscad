//! Tagged meshes of simple solids, tessellated for reconstruction.
//!
//! The tessellation that tags triangles with surfaces never reaches the
//! output (only the exact surfaces do), so it can be chosen to make
//! reconstruction reliable: segment counts that are multiples of 4 with a
//! vertex on each axis, and spheres with poles and an equator ring. Then a
//! cylinder tangent to an axis-aligned plane touches it along a mesh edge
//! instead of crossing it in slivers that have no exact counterpart.

use crate::math::*;
use crate::model::{Surface, TaggedMesh};

/// A rigid transform `p ↦ R p + t`, as rows `[R | t]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform(pub [[f64; 4]; 3]);

impl Default for Transform {
    fn default() -> Self {
        Transform::IDENTITY
    }
}

impl Transform {
    /// The identity.
    pub const IDENTITY: Transform = Transform([
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ]);

    /// A translation.
    pub fn translate(t: [f64; 3]) -> Transform {
        let mut m = Transform::IDENTITY;
        for (k, row) in m.0.iter_mut().enumerate() {
            row[3] = t[k];
        }
        m
    }

    /// OpenSCAD's `rotate([a, b, c])` in degrees: about x, then y, then z.
    /// Multiples of 90° are exact.
    pub fn rotate(deg: [f64; 3]) -> Transform {
        let (sa, ca) = (sin_deg(deg[0]), cos_deg(deg[0]));
        let (sb, cb) = (sin_deg(deg[1]), cos_deg(deg[1]));
        let (sc, cc) = (sin_deg(deg[2]), cos_deg(deg[2]));
        let rx = Transform([
            [1.0, 0.0, 0.0, 0.0],
            [0.0, ca, -sa, 0.0],
            [0.0, sa, ca, 0.0],
        ]);
        let ry = Transform([
            [cb, 0.0, sb, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [-sb, 0.0, cb, 0.0],
        ]);
        let rz = Transform([
            [cc, -sc, 0.0, 0.0],
            [sc, cc, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ]);
        rz.then_after(&ry).then_after(&rx)
    }

    /// `self ∘ o`: apply `o` first, then `self`.
    pub fn then_after(&self, o: &Transform) -> Transform {
        let (a, b) = (&self.0, &o.0);
        let mut r = [[0.0; 4]; 3];
        for i in 0..3 {
            for j in 0..4 {
                r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum::<f64>()
                    + if j == 3 { a[i][3] } else { 0.0 };
            }
        }
        Transform(r)
    }

    /// The image of a point.
    pub fn point(&self, p: [f64; 3]) -> [f64; 3] {
        let m = &self.0;
        [0, 1, 2].map(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2] + m[i][3])
    }

    /// The image of a direction (normalised).
    pub fn direction(&self, d: [f64; 3]) -> [f64; 3] {
        let m = &self.0;
        let q = V::from([0, 1, 2].map(|i| m[i][0] * d[0] + m[i][1] * d[1] + m[i][2] * d[2]));
        q.norm().arr()
    }

    /// The image of a surface.
    pub fn surface(&self, s: &Surface) -> Surface {
        match s {
            Surface::Plane { origin, normal } => Surface::Plane {
                origin: self.point(*origin),
                normal: self.direction(*normal),
            },
            Surface::Cylinder {
                origin,
                axis,
                radius,
            } => Surface::Cylinder {
                origin: self.point(*origin),
                axis: self.direction(*axis),
                radius: *radius,
            },
            Surface::Cone { apex, axis, slope } => Surface::Cone {
                apex: self.point(*apex),
                axis: self.direction(*axis),
                slope: *slope,
            },
            Surface::Sphere { center, radius } => Surface::Sphere {
                center: self.point(*center),
                radius: *radius,
            },
            Surface::Torus {
                center,
                axis,
                major_radius,
                minor_radius,
            } => Surface::Torus {
                center: self.point(*center),
                axis: self.direction(*axis),
                major_radius: *major_radius,
                minor_radius: *minor_radius,
            },
            other => other.clone(),
        }
    }
}

/// `n` rounded up to a multiple of 4 (at least 4): the segment count that
/// puts polygon vertices on both axes.
pub fn aligned_segments(n: u32) -> u32 {
    n.max(4).div_ceil(4) * 4
}

struct Builder {
    m: TaggedMesh,
    t: Transform,
}

impl Builder {
    fn new(t: &Transform) -> Builder {
        Builder {
            m: TaggedMesh::default(),
            t: *t,
        }
    }
    fn vert(&mut self, p: V) -> u32 {
        self.m.positions.push(self.t.point(p.arr()));
        (self.m.positions.len() - 1) as u32
    }
    fn surf(&mut self, s: Surface) -> u32 {
        self.m.surfaces.push(self.t.surface(&s));
        (self.m.surfaces.len() - 1) as u32
    }
    fn tri(&mut self, a: u32, b: u32, c: u32, s: u32) {
        self.m.triangles.push([a, b, c]);
        self.m.triangle_surface.push(s);
    }
    /// Fan-triangulates a convex polygon (counter-clockwise from outside).
    fn fan(&mut self, p: &[u32], s: u32) {
        for i in 1..p.len() - 1 {
            self.tri(p[0], p[i], p[i + 1], s);
        }
    }
}

fn ring(r: f64, z: f64, n: u32) -> Vec<V> {
    (0..n)
        .map(|i| {
            let a = 360.0 * i as f64 / n as f64;
            v(r * cos_deg(a), r * sin_deg(a), z)
        })
        .collect()
}

/// A box from the origin to `size`, then `t`. Six planes.
pub fn cuboid(size: [f64; 3], t: &Transform) -> TaggedMesh {
    let mut b = Builder::new(t);
    let s = V::from(size);
    let vs: Vec<u32> = (0..8)
        .map(|i| {
            b.vert(v(
                if i & 1 != 0 { s.x } else { 0.0 },
                if i & 2 != 0 { s.y } else { 0.0 },
                if i & 4 != 0 { s.z } else { 0.0 },
            ))
        })
        .collect();
    let faces: [([usize; 4], V, V); 6] = [
        ([4, 5, 7, 6], v(0.0, 0.0, s.z), v(0.0, 0.0, 1.0)),
        ([2, 3, 1, 0], v(0.0, 0.0, 0.0), v(0.0, 0.0, -1.0)),
        ([0, 1, 5, 4], v(0.0, 0.0, 0.0), v(0.0, -1.0, 0.0)),
        ([1, 3, 7, 5], v(s.x, 0.0, 0.0), v(1.0, 0.0, 0.0)),
        ([3, 2, 6, 7], v(0.0, s.y, 0.0), v(0.0, 1.0, 0.0)),
        ([2, 0, 4, 6], v(0.0, 0.0, 0.0), v(-1.0, 0.0, 0.0)),
    ];
    for (f, o, n) in faces {
        let id = b.surf(Surface::Plane {
            origin: o.arr(),
            normal: n.arr(),
        });
        b.fan(&f.map(|i| vs[i]), id);
    }
    b.m
}

/// A cylinder or cone frustum on the z axis from `z = 0` to `height`,
/// radius `r_bottom` below and `r_top` above (either may be 0: an apex),
/// with `segments` sides, then `t`.
pub fn frustum(height: f64, r_bottom: f64, r_top: f64, segments: u32, t: &Transform) -> TaggedMesh {
    let mut b = Builder::new(t);
    let n = segments.max(3);
    let side = if r_bottom == r_top {
        Surface::Cylinder {
            origin: [0.0; 3],
            axis: [0.0, 0.0, 1.0],
            radius: r_bottom,
        }
    } else {
        let k = (r_top - r_bottom).abs() / height;
        if r_top > r_bottom {
            Surface::Cone {
                apex: [0.0, 0.0, -r_bottom / k],
                axis: [0.0, 0.0, 1.0],
                slope: k,
            }
        } else {
            Surface::Cone {
                apex: [0.0, 0.0, height + r_top / k],
                axis: [0.0, 0.0, -1.0],
                slope: k,
            }
        }
    };
    let side = b.surf(side);
    let bottom: Vec<u32> = if r_bottom == 0.0 {
        vec![b.vert(v(0.0, 0.0, 0.0))]
    } else {
        ring(r_bottom, 0.0, n)
            .into_iter()
            .map(|p| b.vert(p))
            .collect()
    };
    let top: Vec<u32> = if r_top == 0.0 {
        vec![b.vert(v(0.0, 0.0, height))]
    } else {
        ring(r_top, height, n)
            .into_iter()
            .map(|p| b.vert(p))
            .collect()
    };
    let n = n as usize;
    for i in 0..n {
        let j = (i + 1) % n;
        match (bottom.len(), top.len()) {
            (1, _) => b.tri(bottom[0], top[j], top[i], side),
            (_, 1) => b.tri(bottom[i], bottom[j], top[0], side),
            _ => b.fan(&[bottom[i], bottom[j], top[j], top[i]], side),
        }
    }
    if bottom.len() > 1 {
        let s = b.surf(Surface::Plane {
            origin: [0.0; 3],
            normal: [0.0, 0.0, -1.0],
        });
        let p: Vec<u32> = bottom.iter().rev().copied().collect();
        b.fan(&p, s);
    }
    if top.len() > 1 {
        let s = b.surf(Surface::Plane {
            origin: [0.0, 0.0, height],
            normal: [0.0, 0.0, 1.0],
        });
        b.fan(&top, s);
    }
    b.m
}

/// A sphere of `radius` about the origin with `segments` (rounded up to a
/// multiple of 4) around the equator, `segments / 2` rings from pole to
/// pole, then `t`.
pub fn sphere(radius: f64, segments: u32, t: &Transform) -> TaggedMesh {
    let mut b = Builder::new(t);
    let n = aligned_segments(segments) as usize;
    let s = b.surf(Surface::Sphere {
        center: [0.0; 3],
        radius,
    });
    let rings = n / 2 - 1;
    let north = b.vert(v(0.0, 0.0, radius));
    let mut rows: Vec<Vec<u32>> = Vec::with_capacity(rings);
    for i in 1..=rings {
        let phi = 180.0 * i as f64 / (n / 2) as f64;
        let r = ring(radius * sin_deg(phi), radius * cos_deg(phi), n as u32);
        rows.push(r.into_iter().map(|p| b.vert(p)).collect());
    }
    let south = b.vert(v(0.0, 0.0, -radius));
    for j in 0..n {
        let k = (j + 1) % n;
        b.tri(north, rows[0][j], rows[0][k], s);
        b.tri(south, rows[rings - 1][k], rows[rings - 1][j], s);
    }
    for r in 0..rings - 1 {
        for j in 0..n {
            let k = (j + 1) % n;
            b.fan(&[rows[r][k], rows[r][j], rows[r + 1][j], rows[r + 1][k]], s);
        }
    }
    b.m
}

/// The convex polygon `points` (counter-clockwise, in the xy plane)
/// extruded from `z = 0` to `height`, then `t`: planes only (a polygonal
/// prism such as `cylinder($fn = 6)`).
pub fn prism(points: &[[f64; 2]], height: f64, t: &Transform) -> TaggedMesh {
    let mut b = Builder::new(t);
    let n = points.len();
    let lo: Vec<u32> = points.iter().map(|p| b.vert(v(p[0], p[1], 0.0))).collect();
    let hi: Vec<u32> = points
        .iter()
        .map(|p| b.vert(v(p[0], p[1], height)))
        .collect();
    for i in 0..n {
        let j = (i + 1) % n;
        let e = v(
            points[j][0] - points[i][0],
            points[j][1] - points[i][1],
            0.0,
        );
        let s = b.surf(Surface::Plane {
            origin: [points[i][0], points[i][1], 0.0],
            normal: v(e.y, -e.x, 0.0).norm().arr(),
        });
        b.fan(&[lo[i], lo[j], hi[j], hi[i]], s);
    }
    let s = b.surf(Surface::Plane {
        origin: [0.0; 3],
        normal: [0.0, 0.0, -1.0],
    });
    let p: Vec<u32> = lo.iter().rev().copied().collect();
    b.fan(&p, s);
    let s = b.surf(Surface::Plane {
        origin: [0.0, 0.0, height],
        normal: [0.0, 0.0, 1.0],
    });
    b.fan(&hi, s);
    b.m
}

/// The same mesh with every triangle on [`Surface::Faceted`]: a stand-in
/// for mesh-only geometry.
pub fn faceted(mut m: TaggedMesh) -> TaggedMesh {
    m.surfaces = vec![Surface::Faceted];
    m.triangle_surface = vec![0; m.triangles.len()];
    m
}

impl TaggedMesh {
    /// Appends `other`, renumbering its positions and surfaces.
    pub fn append(&mut self, other: &TaggedMesh) {
        let (pv, ps) = (self.positions.len() as u32, self.surfaces.len() as u32);
        self.positions.extend_from_slice(&other.positions);
        self.surfaces.extend(other.surfaces.iter().cloned());
        self.triangles
            .extend(other.triangles.iter().map(|t| t.map(|i| i + pv)));
        self.triangle_surface
            .extend(other.triangle_surface.iter().map(|s| s + ps));
    }
}

/// A torus about the z axis: the circle of radius `minor` centred
/// `major` from the axis in the xz plane, swept by `angle` degrees
/// (360 for a whole ring, otherwise from the xz plane counter-clockwise,
/// with planar ends), `segments` sections per turn about the axis and
/// `tube_segments` around the tube, then `t`.
pub fn torus(
    major: f64,
    minor: f64,
    segments: u32,
    tube_segments: u32,
    angle: f64,
    t: &Transform,
) -> TaggedMesh {
    let mut b = Builder::new(t);
    let whole = angle >= 360.0;
    let turn = if whole { 360.0 } else { angle };
    let n = aligned_segments(segments) as usize;
    let sections = if whole {
        n
    } else {
        ((n as f64 * turn / 360.0).ceil() as usize).max(1)
    };
    let m = aligned_segments(tube_segments) as usize;
    let s = b.surf(Surface::Torus {
        center: [0.0; 3],
        axis: [0.0, 0.0, 1.0],
        major_radius: major,
        minor_radius: minor,
    });
    let rings = if whole { sections } else { sections + 1 };
    let mut rows: Vec<Vec<u32>> = Vec::with_capacity(rings);
    for j in 0..rings {
        let phi = turn * j as f64 / sections as f64;
        let row = (0..m)
            .map(|i| {
                let th = 360.0 * i as f64 / m as f64;
                let rho = major + minor * cos_deg(th);
                b.vert(v(
                    rho * cos_deg(phi),
                    rho * sin_deg(phi),
                    minor * sin_deg(th),
                ))
            })
            .collect();
        rows.push(row);
    }
    for j in 0..sections {
        let k = (j + 1) % rings;
        for i in 0..m {
            let l = (i + 1) % m;
            // (axis angle, tube angle) counter-clockwise: the outward
            // normal is σ_φ × σ_θ.
            b.fan(&[rows[j][i], rows[k][i], rows[k][l], rows[j][l]], s);
        }
    }
    if !whole {
        let ends = [(0usize, 0.0f64, -1.0f64), (sections, turn, 1.0)];
        for (j, phi, sign) in ends {
            // The end planes face away from the swept solid.
            let nrm = v(-sin_deg(phi), cos_deg(phi), 0.0) * sign;
            let id = b.surf(Surface::Plane {
                origin: [0.0; 3],
                normal: nrm.arr(),
            });
            let mut p = rows[j].clone();
            let pts: Vec<V> = p
                .iter()
                .map(|&i| V::from(b.m.positions[i as usize]))
                .collect();
            let mut area = V::default();
            for w in 0..pts.len() {
                area = area + pts[w].cross(pts[(w + 1) % pts.len()]);
            }
            if area.dot(V::from(b.t.direction(nrm.arr()))) < 0.0 {
                p.reverse();
            }
            b.fan(&p, id);
        }
    }
    b.m
}
