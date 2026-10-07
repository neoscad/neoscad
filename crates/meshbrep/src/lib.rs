//! Rebuild an exact boundary representation from a triangle mesh whose
//! triangles each name the exact surface they came from, and write it as
//! STEP.
//!
//! A mesh kernel such as Manifold does booleans robustly but only on
//! triangles. If every input triangle is tagged with the exact surface it
//! approximates (a plane, cylinder, cone or sphere), the tags survive the
//! booleans, and the output mesh says which surface each region lies on.
//! This crate turns that into a B-rep:
//!
//! - **faces** are connected regions on one surface (equal surfaces from
//!   different solids are merged, so two coplanar tops give one face);
//! - **edges** are the boundaries between two faces, with exact curves
//!   where a closed form exists (lines, circles, ellipses) and cubic
//!   B-splines within a tolerance otherwise;
//! - **vertices** are where three or more faces meet, solved on the exact
//!   surfaces; tangent surfaces are detected analytically and their
//!   contact lines used instead of an ill-conditioned solve;
//! - **seams** and **parameter-space curves** are added on curved faces,
//!   so that a STEP reader trims them exactly instead of re-fitting;
//! - triangles tagged [`Surface::Faceted`] become planar faces, so
//!   mesh-only regions mix with exact ones in one valid solid.
//!
//! The mesh's tessellation never reaches the output, only its topology:
//! build it with the [`primitives`] (or the same rules: segment counts a
//! multiple of 4 with vertices on the axes, spheres with poles and an
//! equator) so that the mesh's topology matches the exact model's at
//! tangencies.
//!
//! Output is deterministic: the same input gives the same B-rep and the
//! same STEP bytes on every platform (transcendental functions come from
//! `libm`, not the platform).
//!
//! ```
//! use meshbrep::{primitives, reconstruct, validate, measure, write_step, Options, StepOptions};
//! use meshbrep::primitives::Transform;
//!
//! let mesh = primitives::frustum(10.0, 5.0, 5.0, 32, &Transform::IDENTITY);
//! let brep = reconstruct(&mesh, &Options::default()).unwrap();
//! assert!(validate(&brep, 1e-6).is_valid());
//! let volume = measure(&brep).unwrap().volume;
//! assert!((volume - std::f64::consts::PI * 250.0).abs() < 1e-9);
//! let step = write_step(&brep, &StepOptions::default());
//! assert!(step.contains("CYLINDRICAL_SURFACE"));
//! ```

mod bspline;
mod curve;
mod edges;
mod math;
mod measure;
mod model;
pub mod primitives;
mod reconstruct;
mod seams;
mod solve;
mod step;
mod surf;
mod tangency;
mod topo;
mod validate;

use std::fmt;

pub use measure::{Measure, measure};
pub use model::*;
pub use reconstruct::{Options, Tolerances};
pub use step::{StepOptions, write_step};
pub use tangency::find_tangencies;
pub use validate::{Validation, validate};

use crate::math::UnionFind;

/// Why reconstruction failed.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The input is malformed (lengths, indices, non-finite numbers).
    InvalidInput(String),
    /// The mesh is not a closed, consistently oriented 2-manifold.
    NotManifold(String),
    /// A triangle names a surface kind not supported yet.
    Unsupported(&'static str),
    /// The mesh's topology could not be matched to the exact surfaces.
    Reconstruction(String),
    /// The mesh's topology differs from the exact model's: with its
    /// corners at their exact positions, a face folds over itself (a
    /// sliver the polygonal approximation left and the exact geometry
    /// removes). A finer attribution mesh usually cures it; the caller owns
    /// the tessellation, so retrying is the caller's.
    TopologyMismatch(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidInput(s) => write!(f, "invalid input: {s}"),
            Error::NotManifold(s) => write!(f, "not a manifold: {s}"),
            Error::Unsupported(k) => write!(f, "{k} surfaces are not supported yet"),
            Error::Reconstruction(s) => write!(f, "reconstruction failed: {s}"),
            Error::TopologyMismatch(s) => {
                write!(f, "the mesh's topology differs from the exact model's: {s}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Rebuilds the exact B-rep of a tagged mesh.
///
/// The result is checked as it is built, but not validated in full: run
/// [`validate`] and compare [`measure`] with the mesh's own volume before
/// trusting it.
pub fn reconstruct(mesh: &TaggedMesh, options: &Options) -> Result<Brep, Error> {
    reconstruct_with(mesh, options, true)
}

fn reconstruct_with(mesh: &TaggedMesh, options: &Options, contacts: bool) -> Result<Brep, Error> {
    let built = reconstruct::build(mesh, options, contacts)?;
    let mut topo = built.topo;
    let max_pcurve = seams::parametrise(&mut topo, built.scale, options.tolerances.fit)?;
    let max_edge = topo.edges.iter().map(|e| e.dev).fold(0.0, f64::max);
    let mut b = assemble(topo);
    let folded: Vec<String> = (0..b.faces.len())
        .filter_map(|f| validate::face_crossing(&b, f))
        .collect();
    if !folded.is_empty() {
        return Err(Error::TopologyMismatch(folded.join("; ")));
    }
    // Shells, and which of them are voids.
    let mut uf = UnionFind::new(b.faces.len());
    let mut first_face = vec![usize::MAX; b.edges.len()];
    for (fi, f) in b.faces.iter().enumerate() {
        for lp in &f.loops {
            for c in &lp.coedges {
                let e = c.edge as usize;
                if first_face[e] == usize::MAX {
                    first_face[e] = fi;
                } else {
                    uf.join(first_face[e], fi);
                }
            }
        }
    }
    let mut shells: std::collections::BTreeMap<usize, Vec<u32>> = Default::default();
    for f in 0..b.faces.len() {
        shells.entry(uf.find(f)).or_default().push(f as u32);
    }
    // A shell enclosing negative volume (by the mesh, which is cheap and
    // independent of the exact geometry) is a cavity.
    b.shells = shells
        .into_values()
        .map(|faces| {
            let v: f64 = faces
                .iter()
                .map(|&f| built.face_mesh_volume[f as usize])
                .sum();
            Shell {
                faces,
                void: v < 0.0,
            }
        })
        .collect();
    b.report.scale = built.scale;
    b.report.mesh_genus = built.mesh_genus;
    b.report.mesh_components = built.mesh_components;
    b.report.max_vertex_residual = built.max_vertex_residual;
    b.report.max_edge_deviation = max_edge;
    b.report.max_pcurve_deviation = max_pcurve;
    b.report.max_chain_deviation = built.max_chain_deviation;
    b.report.tangencies = built
        .tangencies
        .iter()
        .map(|&(a, c, k)| Tangency {
            surfaces: [a.min(c), a.max(c)],
            contact: k.to_public(),
        })
        .collect();
    Ok(b)
}

/// The published form of the working topology: unused vertices dropped,
/// vertices numbered in order of first use by an edge.
fn assemble(topo: topo::Topo) -> Brep {
    let mut vnum = vec![u32::MAX; topo.verts.len()];
    let mut vertices = Vec::new();
    let mut edges = Vec::with_capacity(topo.edges.len());
    for e in &topo.edges {
        for vi in [e.v0, e.v1] {
            if vnum[vi] == u32::MAX {
                vnum[vi] = vertices.len() as u32;
                vertices.push(topo.verts[vi].arr());
            }
        }
        edges.push(Edge {
            start: vnum[e.v0],
            end: vnum[e.v1],
            curve: e.curve.clone(),
            range: e.range,
            seam: e.seam,
        });
    }
    let faces = topo
        .faces
        .into_iter()
        .map(|f| {
            let p = f.param.expect("param");
            let loops = f
                .loops
                .iter()
                .enumerate()
                .map(|(li, lp)| Loop {
                    coedges: lp
                        .iter()
                        .enumerate()
                        .map(|(ci, &(e, fwd))| Coedge {
                            edge: e as u32,
                            forward: fwd,
                            pcurve: f.pcurves.get(li).and_then(|l| l[ci].clone()),
                        })
                        .collect(),
                    outer: f.outer.get(li).copied().unwrap_or(false),
                })
                .collect();
            Face {
                surface: p.s.to_public(),
                frame: p.frame(),
                ref_radius: p.r0,
                same_sense: f.same_sense,
                loops,
                faceted: f.faceted,
            }
        })
        .collect();
    Brep {
        vertices,
        edges,
        faces,
        shells: Vec::new(),
        report: Report {
            notes: topo.notes,
            ..Report::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_rust::manifold::Manifold;
    use manifold_rust::types::{MeshGL64, OpType};
    use primitives::Transform;

    /// The capsule (a cylinder and two spheres, tangent along two
    /// circles), unioned with Manifold.
    fn capsule() -> TaggedMesh {
        let parts = [
            primitives::frustum(20.0, 5.0, 5.0, 32, &Transform::IDENTITY),
            primitives::sphere(5.0, 32, &Transform::IDENTITY),
            primitives::sphere(5.0, 32, &Transform::translate([0.0, 0.0, 20.0])),
        ];
        let mut surfaces = Vec::new();
        let ms: Vec<Manifold> = parts
            .iter()
            .map(|m| {
                let off = surfaces.len() as u64;
                surfaces.extend(m.surfaces.iter().cloned());
                Manifold::from_mesh_gl64(&MeshGL64 {
                    num_prop: 3,
                    vert_properties: m.positions.iter().flatten().copied().collect(),
                    tri_verts: m
                        .triangles
                        .iter()
                        .flatten()
                        .map(|&i| u64::from(i))
                        .collect(),
                    face_id: m
                        .triangle_surface
                        .iter()
                        .map(|&s| u64::from(s) + off)
                        .collect(),
                    run_index: vec![0, 3 * m.triangles.len() as u64],
                    run_original_id: vec![Manifold::reserve_ids(1)],
                    ..Default::default()
                })
            })
            .collect();
        let gl = Manifold::batch_boolean(&ms, OpType::Add).get_mesh_gl64(-1);
        TaggedMesh {
            positions: gl
                .vert_properties
                .chunks(3)
                .map(|c| [c[0], c[1], c[2]])
                .collect(),
            triangles: gl
                .tri_verts
                .chunks(3)
                .map(|c| [c[0] as u32, c[1] as u32, c[2] as u32])
                .collect(),
            triangle_surface: gl.face_id.iter().map(|&f| f as u32).collect(),
            surfaces,
        }
    }

    /// Without analytic contacts, every vertex on the capsule's tangent
    /// circles looks like a tangent crossing and splits the circle (the
    /// audit's x02 came out with 16 arcs each); arc merging must join them
    /// back into one closed circle per side.
    #[test]
    fn arcs_split_at_tangent_vertices_are_merged() {
        let mesh = capsule();
        let with = reconstruct_with(&mesh, &Options::default(), true).unwrap();
        let without = reconstruct_with(&mesh, &Options::default(), false).unwrap();
        assert!(with.report.notes.iter().all(|n| !n.contains("merged")));
        assert!(
            without.report.notes.iter().any(|n| n.contains("merged")),
            "{:?}",
            without.report.notes
        );
        let circles = |b: &Brep| {
            b.edges
                .iter()
                .filter(|e| !e.seam && matches!(e.curve, Curve::Circle { .. }))
                .count()
        };
        assert_eq!(circles(&with), 2);
        assert_eq!(circles(&without), 2);
        assert!(validate(&without, 1e-6).is_valid());
        let v = measure(&without).unwrap().volume;
        let exact = std::f64::consts::PI * (500.0 + 500.0 / 3.0);
        assert!((v - exact).abs() < 1e-9 * exact, "{v} vs {exact}");
    }

    #[test]
    fn tangencies_are_found_from_the_records() {
        let surfaces = [
            Surface::Plane {
                origin: [0.0, 0.0, 0.0],
                normal: [0.0, 1.0, 0.0],
            },
            Surface::Cylinder {
                origin: [0.0, 3.0, 0.0],
                axis: [0.0, 0.0, 1.0],
                radius: 3.0,
            },
            Surface::Sphere {
                center: [0.0, 3.0, 7.0],
                radius: 3.0,
            },
            Surface::Faceted,
        ];
        let t = find_tangencies(&surfaces, 1e-9);
        assert_eq!(t.len(), 3, "{t:?}");
        assert!(matches!(t[0].contact, Contact::Line { .. }));
        assert!(matches!(t[1].contact, Contact::Point { .. }));
        assert!(matches!(t[2].contact, Contact::Circle { .. }));
    }
}
