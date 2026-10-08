//! Rebuild an exact boundary representation from a triangle mesh whose
//! triangles each name the exact surface they came from, and write it as
//! STEP.
//!
//! A mesh kernel such as Manifold does booleans robustly but only on
//! triangles. If every input triangle is tagged with the exact surface it
//! approximates (a plane, cylinder, cone, sphere, torus or B-spline
//! surface; [`spline`] builds the B-splines blends need), the tags survive the
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
//!   mesh-only regions mix with exact ones in one valid solid;
//! - features of the mesh below the touching tolerance (edges shorter
//!   than it, needle triangles narrower) are cleaned up first, and so are
//!   faces and closed components of no area or volume.
//!
//! Each face names the input triangles it came from
//! ([`Report::face_triangles`]), and [`reconstruct_located`] names the
//! triangles where a failure happened, so a caller can tag the faces it
//! cannot have exact as [`Surface::Faceted`] and try again.
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

pub mod blend;
mod bspline;
mod curve;
mod edges;
mod math;
mod measure;
mod model;
mod nurbs;
pub mod primitives;
mod reconstruct;
mod seams;
mod solve;
pub mod spline;
mod step;
mod surf;
mod tangency;
mod topo;
mod validate;

use std::fmt;

pub use measure::{Measure, measure};
pub use model::*;
pub use reconstruct::{Options, StopFn, Tolerances};
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
    /// removes), or a corner lands on another edge of its own face (bodies
    /// touching along an edge, joined on the wrong side by rounding). A
    /// finer attribution mesh usually cures the first; the caller owns the
    /// tessellation, so retrying is the caller's.
    TopologyMismatch(String),
    /// [`Options::should_stop`] asked reconstruction to stop.
    Stopped,
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
            Error::Stopped => write!(f, "stopped by the caller"),
        }
    }
}

impl std::error::Error for Error {}

/// Why [`reconstruct_located`] failed, and where.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    /// What went wrong.
    pub error: Error,
    /// Input triangles at the failure: the faces whose boundary folds, the
    /// face that has no frame, the triangles where a boundary walk went
    /// wrong. Empty when the failure has no place (a mesh that is not
    /// closed, malformed input). A caller can tag the faces these belong
    /// to [`Surface::Faceted`] and try again, so that one bad region does
    /// not cost the whole model its exact surfaces.
    pub triangles: Vec<u32>,
}

impl From<Error> for Failure {
    fn from(error: Error) -> Failure {
        Failure {
            error,
            triangles: Vec::new(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for Failure {}

/// Rebuilds the exact B-rep of a tagged mesh.
///
/// The result is checked as it is built, but not validated in full: run
/// [`validate`] and compare [`measure`] with the mesh's own volume before
/// trusting it.
pub fn reconstruct(mesh: &TaggedMesh, options: &Options) -> Result<Brep, Error> {
    reconstruct_with(mesh, options, true).map_err(|f| f.error)
}

/// [`reconstruct`], with the input triangles at a failure (see
/// [`Failure::triangles`]).
pub fn reconstruct_located(mesh: &TaggedMesh, options: &Options) -> Result<Brep, Failure> {
    reconstruct_with(mesh, options, true)
}

fn reconstruct_with(mesh: &TaggedMesh, options: &Options, contacts: bool) -> Result<Brep, Failure> {
    let built = reconstruct::build(mesh, options, contacts)?;
    let mut topo = built.topo;
    options.poll()?;
    let max_pcurve = seams::parametrise(&mut topo, built.scale, options.tolerances.fit)?;
    options.poll()?;
    let max_edge = topo.edges.iter().map(|e| e.dev).fold(0.0, f64::max);
    let mut b = assemble(topo);
    let touch_tol = options.tolerances.fit.max(1e-9 * built.scale);
    let mut stopped = false;
    let folded: Vec<(usize, String)> = (0..b.faces.len())
        .filter_map(|f| {
            // Once per face: each check walks the face's loops, which is
            // most of the time left on a large model.
            if stopped || options.stopped() {
                stopped = true;
                return None;
            }
            validate::face_crossing(&b, f)
                .or_else(|| validate::face_touch(&b, f, touch_tol))
                .map(|m| (f, m))
        })
        .collect();
    if stopped {
        return Err(Error::Stopped.into());
    }
    if !folded.is_empty() {
        // The first few: a rotated Menger sponge has dozens, and the
        // message ends up in a user's report.
        let more = folded.len().saturating_sub(3);
        let mut msg = folded[..folded.len().min(3)]
            .iter()
            .map(|(_, m)| m.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        if more > 0 {
            msg.push_str(&format!("; and {more} more"));
        }
        let mut triangles: Vec<u32> = folded
            .iter()
            .flat_map(|&(f, _)| b.report.face_triangles[f].iter().copied())
            .collect();
        triangles.sort_unstable();
        return Err(Failure {
            error: Error::TopologyMismatch(msg),
            triangles,
        });
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
        .flat_map(|(a, c, k)| {
            let (a, c) = (*a, *c);
            k.to_public(a, c).into_iter().map(move |contact| Tangency {
                surfaces: [a.min(c), a.max(c)],
                contact,
            })
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
    let edge_chain_deviation = topo.edges.iter().map(|e| e.chain_dev).collect();
    let face_triangles = topo.faces.iter().map(|f| f.source.clone()).collect();
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
            edge_chain_deviation,
            face_triangles,
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

    /// `first` minus `rest`, through Manifold, tagged.
    fn minus(first: &TaggedMesh, rest: &[TaggedMesh]) -> TaggedMesh {
        let mut surfaces = Vec::new();
        let mut to = |m: &TaggedMesh| {
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
        };
        let mut out = to(first);
        for r in rest {
            out = out.boolean(&to(r), OpType::Subtract);
        }
        let gl = out.get_mesh_gl64(-1);
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

    fn says(v: &Validation, what: &str) -> bool {
        v.errors.iter().any(|e| e.contains(what))
    }

    /// A planar face narrower than the tolerance has no trustworthy area
    /// or orientation (OCCT reads it as a wire crossing itself), and the
    /// validator says which faces: the four sides of a slab 5e-7 thick.
    #[test]
    fn a_face_narrower_than_the_tolerance_is_an_error() {
        let slab = primitives::cuboid([10.0, 5e-7, 10.0], &Transform::IDENTITY);
        let b = reconstruct(&slab, &Options::default()).unwrap();
        let v = validate(&b, 1e-6);
        assert!(says(&v, "narrower than the tolerance"), "{:?}", v.errors);
        assert_eq!(v.error_faces.len(), 4, "{:?}", v.error_faces);
        assert!(validate(&b, 1e-7).is_valid());
    }

    /// A closed edge belongs in a loop of its own. Inside a longer loop it
    /// is a hole touching the boundary, which OCCT rejects: here the
    /// cylinder's top circle, made to go round twice.
    #[test]
    fn a_closed_edge_inside_a_longer_loop_is_an_error() {
        let mesh = primitives::frustum(10.0, 5.0, 5.0, 32, &Transform::IDENTITY);
        let mut b = reconstruct(&mesh, &Options::default()).unwrap();
        assert!(validate(&b, 1e-6).is_valid());
        let f = b
            .faces
            .iter()
            .position(|f| {
                matches!(f.surface, Surface::Plane { .. })
                    && f.loops.len() == 1
                    && f.loops[0].coedges.len() == 1
            })
            .expect("a cap bounded by its circle");
        let c = b.faces[f].loops[0].coedges[0].clone();
        b.faces[f].loops[0].coedges.push(c);
        let v = validate(&b, 1e-6);
        assert!(says(&v, "inside a longer loop"), "{:?}", v.errors);
        assert!(v.error_faces.contains(&(f as u32)));
    }

    /// Holes nest inside their face's outer loop, outside each other: a
    /// plate's hole marked as the outer loop leaves the square outside it.
    #[test]
    fn a_hole_outside_its_outer_loop_is_an_error() {
        let plate = primitives::cuboid([20.0, 20.0, 5.0], &Transform::IDENTITY);
        let hole =
            primitives::frustum(7.0, 3.0, 3.0, 32, &Transform::translate([10.0, 10.0, -1.0]));
        let mesh = minus(&plate, &[hole]);
        let mut b = reconstruct(&mesh, &Options::default()).unwrap();
        assert!(validate(&b, 1e-6).is_valid());
        let f = b
            .faces
            .iter()
            .position(|f| f.loops.len() == 2 && matches!(f.surface, Surface::Plane { .. }))
            .expect("a face with a hole");
        for l in &mut b.faces[f].loops {
            l.outer = !l.outer;
        }
        let v = validate(&b, 1e-6);
        assert!(says(&v, "a hole not inside"), "{:?}", v.errors);
    }

    /// Without analytic contacts, every vertex on the capsule's tangent
    /// circles looks like a tangent crossing and splits the circle (the test
    /// case x02 came out with 16 arcs each); arc merging must join them
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

    /// A closed component of zero volume lying in one plane (four
    /// triangles over four coplanar points: Manifold leaves these where
    /// coplanar cuts meet after a rotation) has no exact counterpart. It
    /// used to fail as "face has no boundary"; it is dropped with a note,
    /// and the rest reconstructs as if it were not there.
    #[test]
    fn a_flat_closed_component_is_dropped() {
        let cube = primitives::cuboid([2.0, 2.0, 2.0], &Transform::IDENTITY);
        let mut mesh = cube.clone();
        let base = mesh.positions.len() as u32;
        // A doubly covered quadrilateral in the plane z = 5: a
        // tetrahedron flattened, closed and consistently oriented.
        mesh.positions.extend([
            [0.0, 0.0, 5.0],
            [1.0, 0.0, 5.0],
            [1.0, 1.0, 5.0],
            [0.0, 1.0, 5.0],
        ]);
        let plane = mesh.surfaces.len() as u32;
        mesh.surfaces.push(Surface::Plane {
            origin: [0.0, 0.0, 5.0],
            normal: [0.0, 0.0, 1.0],
        });
        for t in [[0, 1, 2], [0, 2, 3], [0, 3, 1], [1, 3, 2]] {
            mesh.triangles.push(t.map(|i| base + i));
            mesh.triangle_surface.push(plane);
        }
        let b = reconstruct(&mesh, &Options::default()).unwrap();
        assert!(
            b.report
                .notes
                .iter()
                .any(|n| n.contains("flat closed component")),
            "{:?}",
            b.report.notes
        );
        assert_eq!(b.faces.len(), 6);
        assert_eq!(b.shells.len(), 1);
        assert!(validate(&b, 1e-6).is_valid());
        assert!((measure(&b).unwrap().volume - 8.0).abs() < 1e-12);
        // With volume it is a real body, not something to drop silently.
        let mut solid = mesh.clone();
        solid.positions[base as usize + 2][2] = 6.0;
        assert!(reconstruct(&solid, &Options::default()).is_err());
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

    /// A torus touches coaxial planes, cylinders and spheres along
    /// circles, found from the profiles in its half-plane.
    #[test]
    fn torus_tangencies_are_circles() {
        let z = [0.0, 0.0, 1.0];
        let surfaces = [
            Surface::Torus {
                center: [0.0; 3],
                axis: z,
                major_radius: 7.0,
                minor_radius: 3.0,
            },
            // Its top, its hole and its outside.
            Surface::Plane {
                origin: [0.0, 0.0, 3.0],
                normal: z,
            },
            Surface::Cylinder {
                origin: [0.0; 3],
                axis: z,
                radius: 4.0,
            },
            Surface::Cylinder {
                origin: [0.0, 0.0, -5.0],
                axis: [0.0, 0.0, -1.0],
                radius: 10.0,
            },
            // Crossing, not tangent.
            Surface::Cylinder {
                origin: [0.0; 3],
                axis: z,
                radius: 8.0,
            },
        ];
        let t = find_tangencies(&surfaces, 1e-9);
        let radii: Vec<f64> = t
            .iter()
            .filter(|x| x.surfaces[0] == 0)
            .map(|x| match x.contact {
                Contact::Circle { radius, .. } => radius,
                _ => f64::NAN,
            })
            .collect();
        assert_eq!(radii, [7.0, 4.0, 10.0], "{t:?}");
    }
}
