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

use crate::polygon2d::Polygon2d;
use crate::polyset::PolySet;
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
    if file.is_empty() || opts.doc_dir.as_os_str().is_empty() {
        return file.to_string();
    }
    lang::diag::relative_path(Path::new(file), &opts.doc_dir)
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
