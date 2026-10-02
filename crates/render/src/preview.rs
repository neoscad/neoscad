//! OpenSCAD's two previews as [`Scene`]s: the OpenCSG preview
//! (`OpenCSGRenderer`) and the throwntogether view
//! (`ThrownTogetherRenderer`), both drawn from the CSG products
//! [`geom::csg::CsgTree`] builds.
//!
//! **OpenCSG.** OpenSCAD draws each product with OpenCSG's image-space
//! CSG: OpenCSG leaves the product's visible depth in the depth buffer,
//! and the product's leaves are then drawn in colour where their depth is
//! equal to it, positive leaves in their colour and subtracted leaves'
//! back faces (the cut surfaces) in the cut-out colour. Here the product's
//! visible surface comes from real booleans instead
//! ([`geom::csg::product_meshes`], faces coloured by the leaf they came
//! from), drawn the same way: a depth-only pass, then colour where the
//! depth is equal. A product of one leaf is drawn directly, as OpenSCAD
//! draws it without OpenCSG. Products share one depth buffer, which makes
//! their union. Transparent single leaves are drawn back faces first
//! (issue #1496), highlighted (`#`) and background (`%`) terms follow in
//! their own translucent colours.
//!
//! A boolean shows what OpenCSG shows only when every leaf bounds a solid
//! ([`PolySet::is_outward_solid`]). A product with a leaf that does not
//! (inside out, a face flipped, not closed) is drawn as OpenCSG draws it,
//! in image space ([`Scene::push_image_csg`], `crate::gpu`).
//!
//! What this cannot reproduce: image-space artefacts of products drawn
//! from booleans. Where a positive and a negative face are coplanar
//! OpenCSG shows z-fighting, which real booleans resolve cleanly; a leaf
//! whose `convexity` is set too low shows OpenCSG's holes.
//!
//! **Throwntogether.** Every leaf once, with no CSG at all: front faces in
//! its colour (the cut-out colour for a subtracted leaf), back faces in
//! magenta unless the leaf is coloured, which shows inside-out meshes.

use std::collections::HashMap;
use std::sync::Arc;

use geom::Matrix;
use geom::color::Color;
use geom::csg::{ChainObject, CsgTree, FLAG_HIGHLIGHT, Negative, ProductJob, Products, Stop};

use geom::polyset::PolySet;

use crate::scene::{CsgOp, CsgPrimitive, Cull, Depth, DrawState, Scene, Surface};
use crate::scheme::ColorScheme;

/// OpenSCAD's `Renderer::ColorMode`, for the modes previews use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// No scheme colour: only the object's own.
    None,
    Material,
    Cutout,
    Highlight,
    Background,
}

/// Which preview to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Previewer {
    OpenCsg,
    ThrownTogether,
}

/// `Renderer::getShaderColor` starting from an unset colour: the object's
/// own RGB and alpha where set (except in highlight mode, which always
/// uses the highlight colour), the mode's colour for the rest.
pub fn shader_color(mode: ColorMode, object: Color, scheme: &ColorScheme) -> Color {
    shader_color_from(mode, object, scheme, Color([-1.0; 4]))
}

fn has_rgb(c: &Color) -> bool {
    c.0[..3].iter().all(|&x| x >= 0.0)
}

fn has_alpha(c: &Color) -> bool {
    c.0[3] >= 0.0
}

/// `Renderer::getShaderColor` into an `out` colour that may already hold
/// components (the throwntogether back faces start from magenta).
fn shader_color_from(mode: ColorMode, object: Color, scheme: &ColorScheme, out: Color) -> Color {
    let mut out = out;
    if mode != ColorMode::Highlight {
        if has_rgb(&object) {
            out.0[..3].copy_from_slice(&object.0[..3]);
        }
        if has_alpha(&object) {
            out.0[3] = object.0[3];
        }
        if out.is_valid() {
            return out;
        }
    }
    // `Renderer::Renderer()` and `setColorScheme`: the highlight and
    // background colours are fixed, the others the scheme's preview
    // colours.
    let base = match mode {
        ColorMode::None => return out,
        ColorMode::Material => scheme.opencsg_face_front,
        ColorMode::Cutout => scheme.opencsg_face_back,
        ColorMode::Highlight => Color::from_ints(255, 81, 81, 128),
        ColorMode::Background => Color::from_ints(180, 180, 180, 128),
    };
    if !has_rgb(&out) {
        out.0[..3].copy_from_slice(&base.0[..3]);
    }
    if !has_alpha(&out) {
        out.0[3] = base.0[3];
    }
    out
}

/// Which products list is being drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    Root,
    Background,
    Highlight,
}

/// Scale 2D subtracted slabs by 1.1 in z "to avoid z fighting"
/// (`OpenCSGRenderer.cc:320-323`, `ThrownTogetherRenderer.cc:196-199`).
fn scaled_z(m: &Matrix) -> Matrix {
    let mut out = *m;
    for row in &mut out {
        row[2] *= 1.1;
    }
    out
}

/// The preview of `tree` in `scheme`'s colours.
pub fn scene(tree: &CsgTree, scheme: &ColorScheme, previewer: Previewer) -> Scene {
    match scene_until(tree, scheme, previewer, &Stop::default()) {
        Ok(scene) => scene,
        // Nothing sets a default `Stop`.
        Err(_) => Scene::empty(scheme, None),
    }
}

/// [`scene`] for a host with limits or a cancel button: the products'
/// booleans give up once `stop` says so, between kernel operations, with
/// [`geom::Unsupported::interrupted`]. Which limit it was, if any, is on
/// `stop` ([`Stop::exceeded`]); otherwise the request was cancelled.
pub fn scene_until(
    tree: &CsgTree,
    scheme: &ColorScheme,
    previewer: Previewer,
    stop: &Stop,
) -> Result<Scene, geom::Unsupported> {
    // Past `geom::csg::BOOLEAN_LIMIT` the products' booleans would take
    // minutes and gigabytes; the tree carries the warning that says so.
    let previewer = if tree.booleans {
        previewer
    } else {
        Previewer::ThrownTogether
    };
    let throwntogether = previewer == Previewer::ThrownTogether;
    let mut scene = Scene::empty(scheme, tree.bounding_box(throwntogether));
    let lists = [
        (Pass::Root, &tree.root),
        (Pass::Background, &tree.background),
        (Pass::Highlight, &tree.highlights),
    ];
    match previewer {
        Previewer::OpenCsg => opencsg(&mut scene, &lists, scheme, stop)?,
        Previewer::ThrownTogether => {
            for (pass, list) in lists {
                if let Some(p) = list {
                    thrown_together(&mut scene, p, pass, scheme);
                }
            }
        }
    }
    Ok(scene)
}

/// A leaf mesh coloured for a product boolean: moved into model
/// coordinates, every face in the colour it is drawn in.
fn job_mesh(obj: &ChainObject, matrix: &Matrix, color: Color, force: bool) -> Option<PolySet> {
    let mut ps = coloured(obj, color, force)?;
    ps.transform(matrix);
    Some(ps)
}

/// A leaf mesh, in its own coordinates, with every face in the colour it
/// is drawn in.
fn coloured(obj: &ChainObject, color: Color, force: bool) -> Option<PolySet> {
    let mesh = obj.leaf.mesh.as_ref()?;
    let mut ps = PolySet::clone(mesh);
    if force || ps.color_indices.is_empty() {
        ps.set_color(color);
    } else {
        // Faces keep a valid colour of their own; the rest take `color`.
        let default = ps.colors.len() as i32;
        ps.colors.push(color);
        let colors = ps.colors.clone();
        for ci in &mut ps.color_indices {
            let valid = usize::try_from(*ci)
                .ok()
                .and_then(|i| colors.get(i))
                .is_some_and(Color::is_valid);
            if !valid {
                *ci = default;
            }
        }
        ps.color_indices.resize(ps.faces.len(), default);
    }
    Some(ps)
}

/// The colour a positive leaf is drawn in, and whether it replaces the
/// mesh's own face colours.
fn positive_color(obj: &ChainObject, pass: Pass, scheme: &ColorScheme) -> (Color, bool) {
    let c = obj.leaf.color;
    match pass {
        Pass::Highlight => (shader_color(ColorMode::Highlight, c, scheme), true),
        Pass::Background => (shader_color(ColorMode::Background, c, scheme), true),
        Pass::Root => (shader_color(ColorMode::Material, c, scheme), c.is_valid()),
    }
}

fn negative_color(obj: &ChainObject, pass: Pass, scheme: &ColorScheme) -> Color {
    let mode = match pass {
        Pass::Highlight => ColorMode::Highlight,
        Pass::Background => ColorMode::Background,
        Pass::Root => ColorMode::Cutout,
    };
    shader_color(mode, obj.leaf.color, scheme)
}

/// What goes into the scene, in order: a surface, a product waiting for
/// its boolean, or a product drawn in image space.
enum Slot {
    Surface(Surface),
    Boolean,
    Image(Vec<CsgPrimitive>),
}

/// A product waiting for its boolean: where its draws go in the scene.
struct Pending {
    at: usize,
    depth: Depth,
    bias: bool,
}

/// `OpenCSGRenderer::createCSGVBOProducts` and `draw`.
fn opencsg(
    scene: &mut Scene,
    lists: &[(Pass, &Option<Products>); 3],
    scheme: &ColorScheme,
    stop: &Stop,
) -> Result<(), geom::Unsupported> {
    // `paintGL` starts with `GL_LESS`; each product leaves `GL_LEQUAL`.
    let mut depth = Depth::Less;
    let mut jobs: Vec<ProductJob> = Vec::new();
    let mut pending: Vec<Pending> = Vec::new();
    // Surfaces are collected in order; a product needing a boolean leaves
    // a placeholder to be filled once all booleans are done in parallel.
    let mut slots: Vec<Slot> = Vec::new();
    // Whether each leaf mesh bounds a solid (`PolySet::is_outward_solid`),
    // by mesh: a leaf may be in many products. And by shape, since equal
    // meshes are often not one: the threaded-ring example evaluates its
    // 39,000-vertex channel once per wedge, and checking each of the 36
    // copies took most of the time before the booleans began (180 ms
    // natively, three times that in the web core).
    let mut solid: HashMap<*const PolySet, bool> = HashMap::new();
    let mut by_shape: HashMap<u64, Vec<(Arc<PolySet>, bool)>> = HashMap::new();
    let mut is_solid = |obj: &ChainObject| {
        let mesh = obj.leaf.mesh.as_ref().expect("filtered");
        *solid.entry(Arc::as_ptr(mesh)).or_insert_with(|| {
            let same = by_shape.entry(mesh.shape_hash()).or_default();
            if let Some((_, s)) = same.iter().find(|(m, _)| m.same_shape(mesh)) {
                return *s;
            }
            let s = mesh.is_outward_solid();
            same.push((mesh.clone(), s));
            s
        })
    };
    for (pass, list) in lists {
        let Some(products) = list else { continue };
        for product in &products.products {
            let pos: Vec<&ChainObject> = product
                .intersections
                .iter()
                .filter(|o| o.leaf.mesh.is_some())
                .collect();
            let neg: Vec<&ChainObject> = product
                .subtractions
                .iter()
                .filter(|o| o.leaf.mesh.is_some())
                .collect();
            match (pos.len(), neg.len()) {
                (0, 0) => {}
                (1, 0) => {
                    let obj = pos[0];
                    let (color, force) = positive_color(obj, *pass, scheme);
                    let mesh = obj.leaf.mesh.clone().expect("filtered");
                    let surface = |cull| Surface {
                        mesh: mesh.clone(),
                        matrix: Some(obj.leaf.matrix),
                        color,
                        force_color: force,
                        lit: true,
                        state: DrawState {
                            cull,
                            depth,
                            color_write: true,
                            bias: *pass == Pass::Highlight,
                        },
                    };
                    if color.0[3] == 1.0 {
                        slots.push(Slot::Surface(surface(Cull::None)));
                    } else {
                        // Transparent: rear faces first (issue #1496).
                        slots.push(Slot::Surface(surface(Cull::Front)));
                        slots.push(Slot::Surface(surface(Cull::Back)));
                    }
                }
                _ if !pos.iter().chain(&neg).all(|o| is_solid(o)) => {
                    image_product(&mut slots, &pos, &neg, *pass, scheme);
                }
                _ => {
                    let mut job = ProductJob::default();
                    for obj in &pos {
                        let (color, force) = positive_color(obj, *pass, scheme);
                        job.positives
                            .extend(job_mesh(obj, &obj.leaf.matrix, color, force));
                    }
                    for obj in &neg {
                        let color = negative_color(obj, *pass, scheme);
                        // Placed, not moved: copies of a repeated subtree
                        // share one union (`geom::csg::Negative`).
                        job.negatives
                            .extend(coloured(obj, color, true).map(|ps| Negative {
                                mesh: Arc::new(ps),
                                matrix: Some(negative_matrix(obj)),
                                tint: color,
                                slab: obj.leaf.dim == 2,
                                chain: obj.leaf.chain.clone(),
                            }));
                    }
                    jobs.push(job);
                    pending.push(Pending {
                        at: slots.len(),
                        depth,
                        bias: *pass == Pass::Highlight,
                    });
                    slots.push(Slot::Boolean);
                }
            }
            depth = Depth::LessEqual;
        }
    }
    let scheme_colors = geom::color::Scheme {
        face_front: scheme.opencsg_face_front,
        face_back: scheme.opencsg_face_back,
    };
    let meshes = geom::csg::product_meshes_until(jobs, &scheme_colors, stop)?;
    let mut solved: Vec<Option<(Arc<PolySet>, Depth, bool)>> = vec![None; slots.len()];
    for (p, m) in pending.iter().zip(meshes) {
        solved[p.at] = m.map(|m| (Arc::new(m), p.depth, p.bias));
    }
    for (s, solved) in slots.into_iter().zip(solved) {
        match (s, solved) {
            (Slot::Surface(s), _) => scene.push(s),
            (Slot::Image(primitives), _) => scene.push_image_csg(primitives),
            (Slot::Boolean, Some((mesh, depth, bias))) => {
                // OpenCSG's depth pass, then colour where the depth is
                // the product's.
                let base = Surface {
                    mesh,
                    matrix: None,
                    color: scheme.opencsg_face_front,
                    force_color: false,
                    lit: true,
                    state: DrawState {
                        cull: Cull::None,
                        depth,
                        color_write: false,
                        bias,
                    },
                };
                let color = Surface {
                    state: DrawState {
                        cull: Cull::None,
                        depth: Depth::Equal,
                        color_write: true,
                        bias,
                    },
                    ..base.clone()
                };
                scene.push(base);
                scene.push(color);
            }
            (Slot::Boolean, None) => {}
        }
    }
    Ok(())
}

/// A subtracted leaf's placement: 2D slabs are stretched in z.
fn negative_matrix(obj: &ChainObject) -> Matrix {
    if obj.leaf.dim == 2 {
        scaled_z(&obj.leaf.matrix)
    } else {
        obj.leaf.matrix
    }
}

/// A product with a leaf that does not bound a solid, drawn as OpenSCAD
/// draws every product: OpenCSG finds its depth in image space from the
/// primitives' faces, then each leaf is drawn in colour where its depth is
/// equal (`OpenCSGRenderer::draw`), positive leaves whole and subtracted
/// ones by their back faces. A boolean would first repair the leaf into
/// some solid, which is not what OpenCSG shows: it reads a face pointing
/// away from the camera as the far side of a solid wherever it lies, so an
/// inside-out leaf cut by anything disappears, and one with a single face
/// flipped keeps only part of itself (`polyhedron-tests.scad`).
///
/// OpenCSG picks SCS when no primitive's convexity is 2 or more
/// (`opencsgRender.cpp`, `chooseAlgorithm`). Meshes here do not carry the
/// convexity, so SCS is used always: it is right for the default of 1.
fn image_product(
    slots: &mut Vec<Slot>,
    pos: &[&ChainObject],
    neg: &[&ChainObject],
    pass: Pass,
    scheme: &ColorScheme,
) {
    let primitive = |obj: &ChainObject, matrix: Matrix, op| CsgPrimitive {
        mesh: obj.leaf.mesh.clone().expect("filtered"),
        matrix: Some(matrix),
        op,
    };
    let primitives = pos
        .iter()
        .map(|o| primitive(o, o.leaf.matrix, CsgOp::Intersection))
        .chain(
            neg.iter()
                .map(|o| primitive(o, negative_matrix(o), CsgOp::Subtraction)),
        )
        .collect();
    slots.push(Slot::Image(primitives));
    // The colour pass (`createCSGVBOProducts`): positive leaves unculled,
    // or rear faces first when transparent; subtracted leaves' rear faces
    // only, in the cut-out colour.
    let state = |cull| DrawState {
        cull,
        depth: Depth::Equal,
        color_write: true,
        bias: false,
    };
    for obj in pos {
        let (color, force) = positive_color(obj, pass, scheme);
        let surface = |cull| {
            Slot::Surface(Surface {
                mesh: obj.leaf.mesh.clone().expect("filtered"),
                matrix: Some(obj.leaf.matrix),
                color,
                force_color: force,
                lit: true,
                state: state(cull),
            })
        };
        if color.0[3] == 1.0 {
            slots.push(surface(Cull::None));
        } else {
            slots.push(surface(Cull::Front));
            slots.push(surface(Cull::Back));
        }
    }
    for obj in neg {
        slots.push(Slot::Surface(Surface {
            mesh: obj.leaf.mesh.clone().expect("filtered"),
            matrix: Some(negative_matrix(obj)),
            color: negative_color(obj, pass, scheme),
            force_color: true,
            lit: true,
            state: state(Cull::Front),
        }));
    }
}

/// `ThrownTogetherRenderer::createCSGProducts` for one products list: each
/// leaf once (the first time it appears), positive leaves in their colour
/// and subtracted ones in the cut-out colour.
fn thrown_together(scene: &mut Scene, products: &Products, pass: Pass, scheme: &ColorScheme) {
    let mut seen = std::collections::HashSet::new();
    for product in &products.products {
        let objs = product
            .intersections
            .iter()
            .map(|o| (o, false))
            .chain(product.subtractions.iter().map(|o| (o, true)));
        for (obj, subtracted) in objs {
            let Some(mesh) = obj.leaf.mesh.clone() else {
                continue;
            };
            if !seen.insert(Arc::as_ptr(&obj.leaf)) {
                continue;
            }
            let leaf_color = obj.leaf.color;
            let highlighted = obj.flags & FLAG_HIGHLIGHT != 0;
            // `getColorMode`.
            let mode = match pass {
                Pass::Highlight => ColorMode::Highlight,
                Pass::Background if highlighted => ColorMode::Highlight,
                Pass::Background => ColorMode::Background,
                Pass::Root if highlighted => ColorMode::Highlight,
                Pass::Root if subtracted => ColorMode::Cutout,
                Pass::Root => ColorMode::Material,
            };
            let color = shader_color(mode, leaf_color, scheme);
            let depth = Depth::LessEqual;
            if pass != Pass::Root {
                scene.push(Surface {
                    mesh,
                    matrix: Some(obj.leaf.matrix),
                    color,
                    force_color: true,
                    lit: true,
                    state: DrawState {
                        cull: Cull::None,
                        depth,
                        color_write: true,
                        bias: false,
                    },
                });
                continue;
            }
            let matrix = if obj.leaf.dim == 2 && subtracted {
                scaled_z(&obj.leaf.matrix)
            } else {
                obj.leaf.matrix
            };
            scene.push(Surface {
                mesh: mesh.clone(),
                matrix: Some(matrix),
                color,
                force_color: subtracted || leaf_color.is_valid(),
                lit: true,
                state: DrawState {
                    cull: Cull::Back,
                    depth,
                    color_write: true,
                    bias: false,
                },
            });
            // Back faces: magenta, "override leaf color on front/back
            // error", unless the leaf has a colour of its own.
            let magenta = Color([1.0, 0.0, 1.0, color.0[3]]);
            let back = shader_color_from(ColorMode::None, leaf_color, scheme, magenta);
            scene.push(Surface {
                mesh,
                matrix: Some(obj.leaf.matrix),
                color: back,
                force_color: true,
                lit: true,
                state: DrawState {
                    cull: Cull::Front,
                    depth,
                    color_write: true,
                    bias: false,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_colours_follow_openscads_rules() {
        let s = ColorScheme::cornfield();
        let unset = Color([-1.0; 4]);
        assert_eq!(
            shader_color(ColorMode::Material, unset, &s),
            s.opencsg_face_front
        );
        assert_eq!(
            shader_color(ColorMode::Cutout, unset, &s),
            s.opencsg_face_back
        );
        // Alpha alone keeps the scheme's RGB.
        let half = Color([-1.0, -1.0, -1.0, 0.5]);
        let c = shader_color(ColorMode::Material, half, &s);
        assert_eq!(c.0[..3], s.opencsg_face_front.0[..3]);
        assert_eq!(c.0[3], 0.5);
        // A coloured subtracted object shows its own colour.
        let red = Color([1.0, 0.0, 0.0, 1.0]);
        assert_eq!(shader_color(ColorMode::Cutout, red, &s), red);
        // Highlight ignores the object's colour.
        assert_eq!(
            shader_color(ColorMode::Highlight, red, &s),
            Color::from_ints(255, 81, 81, 128)
        );
        // Background keeps the object's RGB and takes its alpha if set.
        let c = shader_color(ColorMode::Background, red, &s);
        assert_eq!(c, red);
    }
}
