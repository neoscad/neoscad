//! STEP export with exact surfaces (`--enable exact`; stage 1b of
//! `docs/audits/exact-geometry-rust.md`, path 1).
//!
//! The pipeline, for a tree the normal render has already built:
//!
//! 1. **Export render** ([`walk`]): the tree again, every triangle tagged
//!    with the exact plane, cylinder, cone or sphere it lies on, in a
//!    tessellation chosen for reconstruction rather than OpenSCAD's.
//!    Curves whose fragments come from `$fa`/`$fs` become exact; an
//!    explicit `$fn` keeps OpenSCAD's polygon; everything else is faceted.
//! 2. **Reconstruction** with `meshbrep`: faces, exact edges and vertices.
//!    When the mesh's topology differs from the exact model's
//!    (`TopologyMismatch`, slivers at a near-tangency), the export render is
//!    built again at twice the segments, once.
//! 3. **Checks** before anything is written: `meshbrep::validate`, then the
//!    exact volume against the tagged mesh corrected onto its surfaces
//!    ([`check`]), and the volume and bounding box against the normal
//!    render, within what the substituted curves can account for. Any
//!    failure is an error with no file, never a quietly wrong one (gate 4).
//! 4. **STEP AP214** with fixed header names and date, so the same model
//!    gives the same bytes.
//!
//! Library code takes no clock: a host that wants stage timings passes
//! [`ExactOptions::clock`].

pub mod check;
pub mod walk;

use eval::dump::Keys;
use eval::node::Node;
use lang::diag::Severity;
pub use meshbrep;
pub use walk::{Substitution, SubstitutionKind};

use crate::Geometry;
use crate::evaluate::{Msg, RenderOptions, Renderer, Unsupported};

/// Settings of one export.
pub struct ExactOptions<'a> {
    /// The STEP header and product names. Their date is fixed by default
    /// (never the clock), so a model always gives the same bytes.
    pub step: meshbrep::StepOptions,
    /// Milliseconds on a host clock, for [`ExactStats::timings`]; `None`
    /// leaves the timings at zero.
    pub clock: Option<&'a dyn Fn() -> f64>,
}

impl std::fmt::Debug for ExactOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExactOptions")
            .field("step", &self.step)
            .finish()
    }
}

/// Stage times in milliseconds (zero without a clock).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Timings {
    /// Every export render, the retry's included.
    pub export_render_ms: f64,
    /// Every reconstruction.
    pub reconstruct_ms: f64,
    /// Validation and the volume cross-checks.
    pub check_ms: f64,
    pub write_ms: f64,
}

/// What an export measured, for reports and the stop-rule sweep.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExactStats {
    /// Export renders run: 1, or 2 after a retry at twice the segments.
    pub attempts: u32,
    /// Why the first attempt was rejected, when it was.
    pub retried_because: Option<String>,
    pub triangles: usize,
    pub faces: usize,
    /// Faces on an exact surface (planes included), not faceted fallback.
    pub exact_faces: usize,
    pub edges: usize,
    /// Edges written as B-splines (no closed form).
    pub bspline_edges: usize,
    /// The B-rep's volume, integrated on its exact surfaces.
    pub volume: f64,
    /// The tagged mesh's volume corrected onto its surfaces ([`check`]).
    pub corrected_volume: f64,
    /// `|volume - corrected_volume| / |volume|`.
    pub volume_error: f64,
    /// The relative tolerance it was held to.
    pub volume_tolerance: f64,
    /// The normal render's volume.
    pub normal_volume: f64,
    /// Reconstruction's notes (merged arcs, tangencies resolved, ...).
    pub notes: Vec<String>,
    pub timings: Timings,
}

/// A finished export.
#[derive(Debug, Clone)]
pub struct ExactExport {
    pub step: String,
    pub stats: ExactStats,
    pub substitutions: Vec<Substitution>,
}

/// Why an export failed: no file is written.
#[derive(Debug, Clone)]
pub struct ExactFailure {
    /// The reason, in words, for the user.
    pub message: String,
    /// The render was interrupted or passed a limit (the caller reports
    /// that as it reports any interrupted render).
    pub interrupted: Option<Unsupported>,
    /// What was measured before the failure.
    pub stats: ExactStats,
    pub substitutions: Vec<Substitution>,
}

/// The substitutions as messages in OpenSCAD's channel: an exact curve and
/// a kept polygon are `INFO`, a faceted region a `WARNING` (it is the one
/// that falls short of what exact export promises).
pub fn substitution_messages(subs: &[Substitution]) -> Vec<Msg> {
    subs.iter()
        .map(|s| {
            let times = if s.count > 1 {
                format!(" ({} instances)", s.count)
            } else {
                String::new()
            };
            Msg {
                severity: Some(match s.kind {
                    SubstitutionKind::Faceted => Severity::Warning,
                    SubstitutionKind::Exact | SubstitutionKind::Polygon => Severity::Info,
                }),
                text: format!("STEP export: {}() {}{times}", s.module, s.detail),
                loc: s.loc.clone(),
            }
        })
        .collect()
}

/// An axis-aligned box, low and high corners.
type Bounds = Option<([f64; 3], [f64; 3])>;

/// The volume and bounding box of the normal render's result.
fn normal_measures(g: &Geometry) -> (f64, Bounds) {
    match g {
        Geometry::Manifold(m) => (m.manifold.volume(), m.bounds()),
        Geometry::PolySet(ps) => {
            let mut v = 0.0;
            for f in &ps.faces {
                for i in 1..f.len().saturating_sub(1) {
                    let [a, b, c] = [f[0], f[i], f[i + 1]].map(|k| ps.vertices[k as usize]);
                    v += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                        + a[2] * (b[0] * c[1] - b[1] * c[0]))
                        / 6.0;
                }
            }
            (v, ps.bounds())
        }
        Geometry::Polygon2d(_) => (0.0, None),
    }
}

fn mesh_bounds(m: &meshbrep::TaggedMesh) -> Bounds {
    let mut it = m.positions.iter();
    let first = *it.next()?;
    let (mut lo, mut hi) = (first, first);
    for p in it {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    Some((lo, hi))
}

/// A void shell (enclosing negative volume) that no outer shell contains:
/// an inside-out body rather than a cavity. Returns its shell index.
fn stray_void(brep: &meshbrep::Brep, tol: f64) -> Option<usize> {
    let bounds = |sh: &meshbrep::Shell| {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for &f in &sh.faces {
            for l in &brep.faces[f as usize].loops {
                for c in &l.coedges {
                    let e = &brep.edges[c.edge as usize];
                    for v in [e.start, e.end] {
                        let p = brep.vertices[v as usize];
                        for k in 0..3 {
                            lo[k] = lo[k].min(p[k]);
                            hi[k] = hi[k].max(p[k]);
                        }
                    }
                }
            }
        }
        (lo, hi)
    };
    let boxes: Vec<_> = brep.shells.iter().map(bounds).collect();
    brep.shells.iter().enumerate().position(|(i, sh)| {
        sh.void
            && !brep.shells.iter().enumerate().any(|(j, o)| {
                !o.void
                    && (0..3).all(|k| {
                        boxes[j].0[k] <= boxes[i].0[k] + tol && boxes[i].1[k] <= boxes[j].1[k] + tol
                    })
            })
    })
}

/// One attempt's outcome: the written file, or why it was rejected and
/// whether a finer export render could cure it.
enum Attempt {
    /// The STEP text.
    Done(String),
    Rejected {
        message: String,
        retry: bool,
    },
}

/// Exports `top` (already rendered as `normal` by `renderer` with `keys`
/// and `opts`) as STEP with exact surfaces.
pub fn export_step(
    renderer: &Renderer,
    top: &Node,
    keys: &Keys,
    opts: &RenderOptions,
    normal: &Geometry,
    x: &ExactOptions<'_>,
) -> Result<ExactExport, Box<ExactFailure>> {
    let now = || x.clock.map_or(0.0, |c| c());
    let mut stats = ExactStats::default();
    let (normal_volume, normal_box) = normal_measures(normal);
    stats.normal_volume = normal_volume;
    let mut substitutions;
    let mut first_reason: Option<String> = None;
    for mult in [1u32, 2] {
        stats.attempts += 1;
        let t0 = now();
        let built = match walk::export_render(renderer, top, keys, opts, mult) {
            Ok(b) => b,
            Err((u, subs)) => {
                stats.timings.export_render_ms += now() - t0;
                substitutions = subs;
                let message = if u.is_interrupted() {
                    "the export render was interrupted".to_string()
                } else {
                    format!(
                        "{}() gave a mesh that is not a closed manifold solid, so it cannot be exported as STEP",
                        u.what
                    )
                };
                return Err(Box::new(ExactFailure {
                    message,
                    interrupted: u.is_interrupted().then_some(u),
                    stats,
                    substitutions,
                }));
            }
        };
        stats.timings.export_render_ms += now() - t0;
        substitutions = built.substitutions.clone();
        match attempt(&built, normal_volume, normal_box, x, &mut stats, &now) {
            Attempt::Done(step) => {
                stats.retried_because = first_reason;
                return Ok(ExactExport {
                    step,
                    stats,
                    substitutions,
                });
            }
            Attempt::Rejected { message, retry } => {
                if retry && mult == 1 {
                    first_reason = Some(message);
                    continue;
                }
                let message = match first_reason {
                    Some(first) if first != message => {
                        format!("{message} (and at the default resolution: {first})")
                    }
                    _ => message,
                };
                stats.retried_because = None;
                return Err(Box::new(ExactFailure {
                    message,
                    interrupted: None,
                    stats,
                    substitutions,
                }));
            }
        }
    }
    unreachable!("the second attempt always returns")
}

fn attempt(
    built: &walk::ExportMesh,
    normal_volume: f64,
    normal_box: Bounds,
    x: &ExactOptions<'_>,
    stats: &mut ExactStats,
    now: &dyn Fn() -> f64,
) -> Attempt {
    let mesh = &built.mesh;
    stats.triangles = mesh.triangles.len();
    if mesh.triangles.is_empty() {
        return Attempt::Rejected {
            message: "the export render is empty".into(),
            retry: false,
        };
    }
    if let Some(bad) = mesh
        .triangle_surface
        .iter()
        .find(|&&s| s as usize >= mesh.surfaces.len())
    {
        return Attempt::Rejected {
            message: format!("a triangle lost its surface (index {bad})"),
            retry: false,
        };
    }
    let t0 = now();
    let brep = meshbrep::reconstruct(mesh, &meshbrep::Options::default());
    stats.timings.reconstruct_ms += now() - t0;
    let brep = match brep {
        Ok(b) => b,
        Err(e) => {
            let retry = matches!(
                e,
                meshbrep::Error::TopologyMismatch(_) | meshbrep::Error::Reconstruction(_)
            );
            return Attempt::Rejected {
                message: format!("reconstruction failed: {e}"),
                retry,
            };
        }
    };
    let t0 = now();
    let scale = brep.report.scale.max(1e-300);
    let validation = meshbrep::validate(&brep, 1e-6f64.max(1e-8 * scale));
    stats.faces = brep.faces.len();
    stats.exact_faces = brep.faces.iter().filter(|f| !f.faceted).count();
    stats.edges = brep.edges.iter().filter(|e| !e.seam).count();
    stats.bspline_edges = brep
        .edges
        .iter()
        .filter(|e| !e.seam && matches!(e.curve, meshbrep::Curve::BSpline(_)))
        .count();
    stats.notes = brep.report.notes.clone();
    stats.notes.extend(validation.notes.iter().cloned());
    if !validation.is_valid() {
        stats.timings.check_ms += now() - t0;
        let mut errs = validation.errors.clone();
        errs.truncate(3);
        return Attempt::Rejected {
            message: format!("the B-rep is invalid: {}", errs.join("; ")),
            retry: true,
        };
    }
    if let Some(i) = stray_void(&brep, 1e-9 * scale) {
        stats.timings.check_ms += now() - t0;
        return Attempt::Rejected {
            message: format!(
                "body {} is inside out (its faces point inwards, as a polyhedron with reversed faces gives), which a STEP solid cannot hold",
                i + 1
            ),
            retry: false,
        };
    }
    // The validator integrated every shell's volume on the exact
    // geometry already (each face is in one shell); integrating again
    // took a sixth of a faceted model's export.
    let measured = if validation.shell_volumes.len() == brep.shells.len() {
        Ok(validation.shell_volumes.iter().sum())
    } else {
        meshbrep::measure(&brep).map(|m| m.volume)
    };
    let volume = match measured {
        Ok(v) => v,
        Err(e) => {
            stats.timings.check_ms += now() - t0;
            return Attempt::Rejected {
                message: format!("its volume could not be measured: {e}"),
                retry: true,
            };
        }
    };
    stats.volume = volume;
    let corrected = check::corrected_volume(mesh);
    stats.corrected_volume = corrected.volume;
    let denom = volume.abs().max(1e-300);
    stats.volume_error = (volume - corrected.volume).abs() / denom;
    // The residual bound with a margin of 4 (it is an estimate of a
    // second-order term, not a proof), plus what the STEP file's own
    // precision (1e-7 in model units) and rounding allow.
    let tolerance =
        4.0 * corrected.residual_bound + 1e-7 * volume.abs() + 1e-9 * scale * scale * scale;
    stats.volume_tolerance = tolerance / denom;
    stats.timings.check_ms += now() - t0;
    if (volume - corrected.volume).abs() > tolerance {
        return Attempt::Rejected {
            message: format!(
                "the B-rep's volume {volume:.6} does not match its mesh ({:.6} corrected onto its surfaces, tolerance {:.2e})",
                corrected.volume, tolerance
            ),
            retry: true,
        };
    }
    // Against the normal render: the exact curves stand off its polygons
    // by at most their sagitta, so the volumes differ by at most the
    // curved area times that, and each side of the box by the sagitta.
    // Beyond that, the export render built a different model.
    let allowance = 1.05 * built.normal_volume_bound
        + 2.0 * corrected.residual_bound
        + 1e-7 * normal_volume.abs()
        + 1e-9 * scale * scale * scale;
    if (volume - normal_volume).abs() > allowance {
        return Attempt::Rejected {
            message: format!(
                "the exact model's volume {volume:.6} is further from the rendered mesh's ({normal_volume:.6}) than its curves can account for ({allowance:.3e})"
            ),
            retry: false,
        };
    }
    if let (Some((nlo, nhi)), Some((elo, ehi))) = (normal_box, mesh_bounds(mesh)) {
        let slack = built.normal_sagitta + corrected.max_cap + 1e-6 * scale;
        let off = (0..3)
            .map(|k| (nlo[k] - elo[k]).abs().max((nhi[k] - ehi[k]).abs()))
            .fold(0.0, f64::max);
        if off > slack {
            return Attempt::Rejected {
                message: format!(
                    "the exact model's bounding box is {off:.3e} off the rendered mesh's (allowed {slack:.3e})"
                ),
                retry: false,
            };
        }
    }
    let t0 = now();
    let step = meshbrep::write_step(&brep, &x.step);
    stats.timings.write_ms += now() - t0;
    Attempt::Done(step)
}
