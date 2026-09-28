//! `import()` and `surface()`: OpenSCAD's `ImportNode::createGeometry` and
//! `SurfaceNode::createGeometry` (`src/core/ImportNode.cc`,
//! `SurfaceNode.cc`) over the `io` crate's readers.
//!
//! Everything happens at geometry time, as in OpenSCAD: the evaluator only
//! resolves the path (relative to the file containing the call, so an
//! `import()` inside a `use`d library reads next to the library), and a
//! missing or broken file is a message here, after all `echo()` output,
//! with an empty result of the format's dimension. The result is cached
//! like any leaf, under a key holding the absolute path, the file's
//! modification time and size, and every parameter, so re-renders do not
//! read the file again until it changes.

use std::path::Path;

use eval::node::{Discretizer, Import};
use io::Message;

use manifold_rust::linalg::Vec2;
use manifold_rust::polygon::triangulate_idx;
use manifold_rust::types::PolyVert;

use crate::color::Scheme;
use crate::polygon2d::Polygon2d;
use crate::polyset::{PolySet, newell};
use crate::{Geometry, RenderOptions, clipper, fragments};

/// A node's `$fn`/`$fa`/`$fs` as the readers ask for them.
struct Curves<'a>(&'a Discretizer);

impl io::Curves for Curves<'_> {
    fn circular_segments(&self, r: f64, angle: f64) -> Option<i32> {
        fragments::circular_segments_for_angle(self.0, r, angle)
    }

    fn path_segments(&self) -> i32 {
        // `std::max(static_cast<int>(fn), 3)`.
        (self.0.fn_ as i32).max(3)
    }
}

fn read(opts: &RenderOptions, file: &str) -> Option<Vec<u8>> {
    if file.is_empty() {
        return None;
    }
    opts.fs.read(Path::new(file)).ok()
}

/// `Filename`'s `operator<<`: quoted, relative to the working directory.
fn quoted_relative(opts: &RenderOptions, file: &str) -> String {
    io::text::quoted(&relative(opts, file))
}

fn relative(opts: &RenderOptions, file: &str) -> String {
    if file.is_empty() || opts.work_dir.as_os_str().is_empty() {
        return file.to_string();
    }
    lang::diag::relative_path(Path::new(file), &opts.work_dir)
        .to_string_lossy()
        .replace('\\', "/")
}

/// `optionally_center` for a mesh: move the centre of the bounding box of
/// all its vertices (used or not) to the origin.
fn center_mesh(ps: &mut PolySet) {
    let Some(first) = ps.vertices.first().copied() else {
        return;
    };
    let (lo, hi) = ps.vertices.iter().fold((first, first), |(lo, hi), v| {
        (
            std::array::from_fn(|k| lo[k].min(v[k])),
            std::array::from_fn(|k| hi[k].max(v[k])),
        )
    });
    let c: [f64; 3] = std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0);
    for v in &mut ps.vertices {
        for k in 0..3 {
            v[k] -= c[k];
        }
    }
}

fn center_outlines(p: &mut Polygon2d) {
    let Some((lo, hi)) = p.bounds() else { return };
    let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    for o in &mut p.outlines {
        for v in &mut o.vertices {
            v[0] -= c[0];
            v[1] -= c[1];
        }
    }
}

/// `ImportNode::createGeometry`. `line` is the call's line; `union` joins
/// the meshes of a multi-object 3MF with the Manifold backend and returns
/// the result as a mesh. Never fails: problems are messages and an empty
/// result.
pub(crate) fn import(
    opts: &RenderOptions,
    node: &Import,
    line: u32,
    union: &dyn Fn(Vec<PolySet>) -> PolySet,
) -> (Geometry, Vec<Message>) {
    let mut msgs = Vec::new();
    let file = node.file.as_str();
    let cant_open = |msgs: &mut Vec<Message>| {
        msgs.push(Message::warning(format!(
            "Can't open import file '{file}', import() at line {line}"
        )))
    };
    let mesh = |ps: PolySet| {
        let mut ps = ps;
        if node.center {
            center_mesh(&mut ps);
        }
        Geometry::PolySet(std::sync::Arc::new(ps))
    };
    let geom = match node.kind.as_str() {
        "stl" => match read(opts, file) {
            None => {
                cant_open(&mut msgs);
                mesh(PolySet::default())
            }
            Some(b) => mesh(PolySet::from_mesh(io::stl::read(&b, file, &mut msgs))),
        },
        "obj" => match read(opts, file) {
            None => {
                cant_open(&mut msgs);
                mesh(PolySet::default())
            }
            Some(b) => mesh(PolySet::from_mesh(io::obj::read(&b, file, &mut msgs))),
        },
        "off" => mesh(PolySet::from_mesh(io::off::read(
            read(opts, file).as_deref(),
            file,
            &mut msgs,
        ))),
        "3mf" => {
            let meshes: Vec<PolySet> =
                io::threemf::read(read(opts, file).as_deref(), file, line, &mut msgs)
                    .into_iter()
                    .map(PolySet::from_mesh)
                    .collect();
            let ps = match meshes.len() {
                0 => PolySet::default(),
                1 => meshes.into_iter().next().unwrap_or_default(),
                _ => union(meshes),
            };
            mesh(ps)
        }
        // No `center`: OpenSCAD's NEF3 case never calls `optionally_center`.
        "nef3" => match read(opts, file) {
            None => {
                cant_open(&mut msgs);
                Geometry::PolySet(std::sync::Arc::new(PolySet::default()))
            }
            Some(b) => {
                let faces = io::nef3::read(&b, file, line, &mut msgs);
                Geometry::PolySet(std::sync::Arc::new(nef3_polyset(
                    faces,
                    &opts.scheme,
                    &mut msgs,
                )))
            }
        },
        "svg" => {
            let svg_opts = io::svg::Options {
                id: node.id.as_deref(),
                layer: node.layer.as_deref(),
                dpi: node.dpi,
                center: node.center,
            };
            let shapes = io::svg::read(
                read(opts, file).as_deref(),
                file,
                line,
                &svg_opts,
                &Curves(&node.disc),
                &mut msgs,
            );
            let polys: Vec<Polygon2d> = shapes
                .into_iter()
                .map(|outlines| Polygon2d {
                    outlines,
                    sanitized: false,
                })
                .collect();
            let refs: Vec<Option<&Polygon2d>> = polys.iter().map(Some).collect();
            Geometry::Polygon2d(std::sync::Arc::new(clipper::apply(
                &refs,
                clipper::Op2::Union,
            )))
        }
        "dxf" => {
            let display = relative(opts, file);
            let req = io::dxf::Request {
                file,
                display: &display,
                layer: node.layer.as_deref().unwrap_or(""),
                origin: node.origin,
                scale: node.scale,
            };
            let bytes = read(opts, file);
            let mut warnings = Vec::new();
            let data = io::dxf::read(bytes.as_deref(), &req, &Curves(&node.disc), &mut |w| {
                warnings.push(w)
            });
            msgs.extend(warnings.into_iter().map(Message::warning));
            let mut p = Polygon2d {
                outlines: data.to_outlines(),
                sanitized: false,
            };
            if node.center {
                center_outlines(&mut p);
            }
            Geometry::Polygon2d(std::sync::Arc::new(if p.is_empty() {
                p
            } else {
                clipper::sanitize(&p)
            }))
        }
        _ => {
            msgs.push(Message::error(format!(
                "Unsupported file format while trying to import file '{}', import() at line {line}",
                quoted_relative(opts, file)
            )));
            Geometry::PolySet(std::sync::Arc::new(PolySet::default()))
        }
    };
    (geom, msgs)
}

/// Steps 3 to 5 of `createPolySetFromNefPolyhedron3` (`cgalutils.cc:288`),
/// after the reader's steps 1 and 2: each facet tessellated with its
/// holes, the triangles checked for unmatched edges, and every vertex the
/// reader met kept (used or not). Triangles are painted with the scheme's
/// front colour when their halffacet is marked and its back colour when
/// not, which is what the nightly's OFF export of an imported `.nef3`
/// carries.
fn nef3_polyset(f: io::nef3::Faces, scheme: &Scheme, msgs: &mut Vec<Message>) -> PolySet {
    let mut faces: Vec<Vec<u32>> = Vec::new();
    let mut color_indices = Vec::new();
    for facet in &f.facets {
        for t in tessellate_with_holes(&f.vertices, &facet.cycles) {
            faces.push(t.to_vec());
            color_indices.push(if facet.mark { 0 } else { 1 });
        }
    }
    if faces.is_empty() {
        return PolySet::default();
    }
    let unconnected = io::nef3::unconnected_edges(faces.iter().map(Vec::as_slice));
    if unconnected > 0 {
        msgs.push(Message::error(format!(
            "Non-manifold mesh created: {unconnected} unconnected edges"
        )));
    }
    PolySet {
        vertices: f.vertices,
        faces,
        colors: vec![scheme.face_front, scheme.face_back],
        color_indices,
        convex: None,
        triangular: true,
    }
}

/// `GeometryUtils::tessellatePolygonWithHoles` without a normal: clean the
/// cycles as OpenSCAD does (repeated indices, "null ears" such as 23 24 23,
/// and non-finite vertices go; nothing comes out when the first cycle has
/// fewer than three vertices left, and holes that collapse are dropped),
/// pass a lone triangle through, and triangulate the rest. OpenSCAD hands
/// the cycles to libtess2 with the odd winding rule; this projects them
/// onto the plane of their summed Newell normal and ear-clips them with
/// Manifold's triangulator, which fills the same region (a hole winds
/// opposite to its outline in a Nef facet) but may pick other diagonals.
/// Triangles keep the cycles' orientation.
fn tessellate_with_holes(verts: &[[f64; 3]], cycles: &[Vec<u32>]) -> Vec<[u32; 3]> {
    let mut clean: Vec<Vec<u32>> = cycles.to_vec();
    for face in &mut clean {
        let mut i = 0usize;
        while face.len() >= 3 && i < face.len() {
            let n = face.len();
            if face[i] == face[(i + 1) % n] {
                face.remove(i);
            } else if face[(i + n - 1) % n] == face[(i + 1) % n] {
                if i == 0 {
                    face.drain(0..2);
                    // The C++ decrements an unsigned 0 here, which ends its
                    // loop: the rest of this cycle is left as it is.
                    break;
                }
                face.drain(i - 1..i + 1);
                i -= 1;
            } else if verts[face[i] as usize].iter().any(|c| !c.is_finite()) {
                face.remove(i);
            } else {
                i += 1;
            }
        }
    }
    if clean.first().is_none_or(|c| c.len() < 3) {
        return Vec::new();
    }
    let first = clean.remove(0);
    clean.retain(|c| c.len() >= 3);
    clean.insert(0, first);
    if clean.len() == 1 && clean[0].len() == 3 {
        return vec![[clean[0][0], clean[0][1], clean[0][2]]];
    }
    let mut n = [0.0; 3];
    for c in &clean {
        let pts: Vec<[f64; 3]> = c.iter().map(|&i| verts[i as usize]).collect();
        let m = newell(&pts);
        for k in 0..3 {
            n[k] += m[k];
        }
    }
    // Drop the axis the face is most perpendicular to, flipping one kept
    // axis when the normal points down it so outlines stay
    // counter-clockwise, as `PolySet::tessellate` projects.
    let axis = (0..3)
        .max_by(|&a, &b| n[a].abs().total_cmp(&n[b].abs()))
        .unwrap_or(2);
    let (u, v) = match axis {
        0 => (1, 2),
        1 => (2, 0),
        _ => (0, 1),
    };
    let flip = n[axis] < 0.0;
    let flat: Vec<u32> = clean.iter().flatten().copied().collect();
    let mut k = 0;
    let polys: Vec<Vec<PolyVert>> = clean
        .iter()
        .map(|c| {
            c.iter()
                .map(|&i| {
                    let p = verts[i as usize];
                    let x = if flip { -p[u] } else { p[u] };
                    k += 1;
                    PolyVert {
                        pos: Vec2::new(x, p[v]),
                        idx: k - 1,
                    }
                })
                .collect()
        })
        .collect();
    let tris: Vec<[u32; 3]> = triangulate_idx(&polys, -1.0, true)
        .iter()
        .map(|t| [t.x, t.y, t.z].map(|j| flat[j as usize]))
        .collect();
    if tris.is_empty() {
        // No area (every point collinear): fan the outline so its
        // neighbours still find its edges.
        let c = &clean[0];
        return (1..c.len() - 1).map(|j| [c[0], c[j], c[j + 1]]).collect();
    }
    tris
}

/// `SurfaceNode::createGeometry`.
pub(crate) fn surface(
    opts: &RenderOptions,
    file: &str,
    center: bool,
    invert: bool,
) -> (Geometry, Vec<Message>) {
    let mut msgs = Vec::new();
    let h = io::surface::read(read(opts, file).as_deref(), file, invert, &mut msgs);
    let ps = PolySet::from_mesh(io::surface::mesh(&h, center));
    (Geometry::PolySet(std::sync::Arc::new(ps)), msgs)
}
