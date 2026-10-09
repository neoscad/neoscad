//! STEP export with exact surfaces (`--enable exact`; stages 1b and 2 of
//! `docs/audits/exact-geometry-rust.md`, path 1).
//!
//! The pipeline, for a tree the normal render has already built:
//!
//! 1. **Export render** ([`walk`]): the tree again, every triangle tagged
//!    with the exact plane, cylinder, cone, sphere or torus it lies on, in
//!    a tessellation chosen for reconstruction rather than OpenSCAD's.
//!    Curves whose fragments come from `$fa`/`$fs` become exact; an
//!    explicit `$fn` keeps OpenSCAD's polygon; everything else is faceted.
//!    Extrusions sweep their profiles' exact curves ([`profile`]). A large
//!    subtree that recurs is built once and its copies placed; a model
//!    refused when built so is exported again with every subtree built in
//!    place ([`export_step`]).
//! 2. **Reconstruction** with `meshbrep`: faces, exact edges and vertices.
//!    When the mesh's topology differs from the exact model's
//!    (`TopologyMismatch`, slivers at a near-tangency, intersection curves
//!    far further off the exact edges than the tessellation explains), the
//!    export render is built again at twice the segments. A result whose
//!    volume check is too loose to trust is held until a finer mesh
//!    vouches for it (see `LOOSE_CHECK`). If the exact attempts fail
//!    somewhere in particular, the source regions there (connected
//!    triangles of one surface record, the faces they would have made)
//!    are written as facets and the rest stays exact ([`partial`]); if
//!    that fails too and the model has exact extrusions, they are built
//!    as facets instead, as stage 1 built them. Either is reported at the
//!    source location of what went to facets.
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
pub mod profile;
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
    /// How far the export render's intersection curves stand off the
    /// exact edges at most (`meshbrep::Report::max_chain_deviation`).
    pub chain_deviation: f64,
    /// How far its triangles stand off their curved surfaces at most:
    /// the scale `chain_deviation` is judged against.
    pub max_cap: f64,
    /// Why the extrusions were exported as facets after all, when the
    /// model did not reconstruct with them exact: the file is the one
    /// stage 1 would have written.
    pub fallback: Option<String>,
    /// What was written as facets after the exact attempts failed in
    /// places (the rest of the model exact), when it came to that.
    pub partial: Option<Partial>,
    /// The modules that fell back to facets in the exact attempts (before
    /// any [`ExactStats::fallback`]): what decides whether the model is
    /// one exact export should handle.
    pub exact_attempt_faceted: Vec<&'static str>,
    /// Reconstruction's notes (merged arcs, tangencies resolved, ...).
    pub notes: Vec<String>,
    pub timings: Timings,
}

/// A partial faceted fallback ([`partial`]): the regions around where
/// reconstruction or its checks failed, written as planar facets so that
/// the rest of the model keeps its exact surfaces.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Partial {
    /// Why the model did not export with every face exact.
    pub reason: String,
    /// Source regions written as facets: connected triangles of the
    /// export render on one exact surface, the faces they would have been.
    pub regions: usize,
    /// Their triangles.
    pub triangles: usize,
    /// The export render's triangles on exact surfaces (planes included),
    /// before any was retagged.
    pub exact_triangles: usize,
    /// Reconstructions it took, the one that passed included.
    pub rounds: u32,
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

/// The relative volume tolerance above which the first attempt's
/// cross-check is too weak to trust on its own ([`attempt`]).
///
/// A tessellation coarse against the model (a BOSL2 corner patch smaller
/// than its sections) made a B-rep 0.4% off the model that a 7% tolerance
/// let through. Such a result is held back; the export renders at twice
/// the segments, and either that attempt's own B-rep passes, or the held
/// one must agree with the finer mesh's corrected volume within the finer
/// tolerance (a coarse capsule, whose finer mesh does not reconstruct,
/// still exports), or the export fails rather than guess.
const LOOSE_CHECK: f64 = 1e-3;

/// What the regions [`partial`] retagged as facets change in the checks
/// of an [`attempt`] (nothing, by default).
#[derive(Debug, Clone, Copy, Default)]
struct Facets {
    /// The volume between those facets and their exact surfaces
    /// ([`check::Caps`]). The B-rep's volume plus their signed sum is the
    /// exact model's, which is what is compared with the normal render;
    /// their size must be small beside the volume, or the triangles were
    /// not on the surfaces they were tagged with and the comparison says
    /// nothing.
    caps: check::Caps,
    /// The largest cap height of the mesh as rendered. Retagged triangles
    /// no longer count in the corrected mesh's, but the chains along
    /// their edges are still the tessellation's.
    max_cap: f64,
}

/// One attempt's outcome: the written file, or why it was rejected and
/// whether a finer export render could cure it.
enum Attempt {
    /// The STEP text.
    Done(String),
    /// A STEP text that passed every check, but a volume check too loose
    /// to trust on its own (see [`LOOSE_CHECK`]): it needs a second
    /// opinion from a finer mesh.
    Loose(String),
    Rejected {
        message: String,
        retry: bool,
        /// The mesh's triangles where it failed, when that is known
        /// ([`partial`] writes the faces around them as facets).
        at: Vec<u32>,
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
    let mut placed = 0;
    let first = export_once(renderer, top, keys, opts, normal, x, false, &mut placed);
    let failed = match &first {
        Err(f) if placed > 0 && f.interrupted.is_none() => f.stats.clone(),
        _ => return first,
    };
    // The export render placed copies of recurring subtrees (`walk::memo`),
    // which differ from in-place builds by rounding; in a model of flush
    // or touching faces that can decide whether the mesh reconstructs. A
    // model refused so is exported as it was before the memo, every
    // subtree built in place, so that no model the memo touches is refused
    // where it exported without it. Both exports' work is counted.
    let mut unused = 0;
    let mut second = export_once(renderer, top, keys, opts, normal, x, true, &mut unused);
    let stats = match &mut second {
        Ok(e) => &mut e.stats,
        Err(f) => &mut f.stats,
    };
    stats.attempts += failed.attempts;
    let (t, u) = (&mut stats.timings, failed.timings);
    t.export_render_ms += u.export_render_ms;
    t.reconstruct_ms += u.reconstruct_ms;
    t.check_ms += u.check_ms;
    t.write_ms += u.write_ms;
    second
}

/// [`export_step`]'s attempts, with the export render placing copies of
/// recurring subtrees unless `in_place`; `placed` counts the copies.
#[allow(clippy::too_many_arguments)]
fn export_once(
    renderer: &Renderer,
    top: &Node,
    keys: &Keys,
    opts: &RenderOptions,
    normal: &Geometry,
    x: &ExactOptions<'_>,
    in_place: bool,
    placed: &mut u32,
) -> Result<ExactExport, Box<ExactFailure>> {
    let now = || x.clock.map_or(0.0, |c| c());
    let mut stats = ExactStats::default();
    let (normal_volume, normal_box) = normal_measures(normal);
    stats.normal_volume = normal_volume;
    let mut substitutions: Vec<Substitution> = Vec::new();
    let mut first_reason: Option<String> = None;
    let mut exact_failure: Option<String> = None;
    let mut exact_extrusions = 0u32;
    // A first attempt that passed with a loose check: (file, its stats,
    // its substitutions).
    let mut held: Option<(String, ExactStats, Vec<Substitution>)> = None;
    // The latest exact attempt that failed somewhere in particular: its
    // mesh, where, and why. A partial fallback starts from it.
    let mut located: Option<(walk::ExportMesh, Vec<u32>)> = None;
    // Whether the export has fitted blends between curved faces, and the
    // last attempt failed where a finer mesh may succeed.
    let mut fitted = false;
    let mut last_retry = false;
    // An attempt with fitted blends that passed every check with some
    // faces written as facets: (file, its stats, its substitutions). It
    // is what the export writes unless a finer attempt makes every face
    // exact (see the `Done` arm below).
    let mut partly: Option<(String, ExactStats, Vec<Substitution>)> = None;
    let plan = [
        (1u32, walk::Extrusions::Exact),
        (2, walk::Extrusions::Exact),
        (4, walk::Extrusions::Exact),
        (1, walk::Extrusions::Faceted),
    ];
    for (k, (mult, mode)) in plan.into_iter().enumerate() {
        if k == 1 && exact_failure.is_some() {
            // Rejected for a reason a finer mesh cannot cure.
            continue;
        }
        if k == 2 && held.is_none() && partly.is_none() && !(fitted && last_retry) {
            // Four times the segments only to settle a loose first check,
            // or for a blend between curved faces that twice failed to
            // reconstruct: one smaller than about twice the depth of the
            // polygons beside it cannot follow their facets
            // (`meshbrep::blend`, `sweep`), and at four times their
            // segments that depth is a sixteenth.
            continue;
        }
        if mode == walk::Extrusions::Faceted
            && let Some((step, mut s, subs)) = partly.take()
        {
            // No finer attempt did better than the one with some faces
            // as facets, which passed every check.
            s.attempts = stats.attempts;
            s.timings = stats.timings;
            s.retried_because = None;
            return Ok(ExactExport {
                step,
                stats: s,
                substitutions: subs,
            });
        }
        if mode == walk::Extrusions::Faceted {
            // Before giving up on exact extrusions altogether: the exact
            // attempt with the regions where it failed written as facets.
            if let (Some(why), Some((b, at))) = (&exact_failure, located.take()) {
                let before = faceted_modules(&substitutions);
                if let Some((step, subs)) =
                    partial(&b, &at, why, normal_volume, normal_box, x, &mut stats, &now)
                {
                    stats.exact_attempt_faceted = before;
                    stats.retried_because = None;
                    return Ok(ExactExport {
                        step,
                        stats,
                        substitutions: subs,
                    });
                }
            }
            // Only a model whose extrusions were built exact can do
            // better with them as facets.
            if exact_extrusions == 0 {
                break;
            }
            let Some(why) = exact_failure.take() else {
                break;
            };
            stats.fallback = Some(why);
            stats.exact_attempt_faceted = faceted_modules(&substitutions);
            first_reason = None;
        }
        stats.attempts += 1;
        let t0 = now();
        let walked = if in_place {
            walk::export_render_in_place(renderer, top, keys, opts, mult, mode)
        } else {
            walk::export_render_with(renderer, top, keys, opts, mult, mode)
        };
        let built = match walked {
            Ok(b) => {
                *placed += b.placed_copies;
                b
            }
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
        // A large mesh at four times its segments costs more than a
        // retry is worth (a fine `$fs` already makes the facets shallow).
        fitted |= built.surface_fit > 0.0 && built.mesh.triangles.len() <= RETRY_FITTED_TRIANGLES;
        if mode == walk::Extrusions::Exact {
            exact_extrusions = exact_extrusions.max(built.exact_extrusions);
        }
        let outcome = attempt(
            &built,
            &built.mesh,
            Facets::default(),
            normal_volume,
            normal_box,
            k == 0,
            x,
            &mut stats,
            &now,
        );
        if mode == walk::Extrusions::Exact
            && k > 0
            && matches!(outcome, Attempt::Rejected { .. })
            && let Some((step, held_stats, held_subs)) = held.take()
        {
            // The finer mesh did not reconstruct; its corrected volume
            // can still vouch for the held B-rep, if it is close enough
            // to be a check.
            let (finer, tolerance) = volume_check(&built.mesh, held_stats.volume);
            let accept = (held_stats.volume - finer).abs() <= tolerance
                && tolerance <= LOOSE_CHECK * held_stats.volume.abs();
            if accept {
                let mut s = held_stats;
                s.attempts = stats.attempts;
                s.timings = stats.timings;
                s.retried_because = None;
                return Ok(ExactExport {
                    step,
                    stats: s,
                    substitutions: held_subs,
                });
            }
            if k == 1 {
                // Held for the next, finer opinion.
                held = Some((step, held_stats, held_subs));
                continue;
            }
        }
        match outcome {
            Attempt::Loose(step) => {
                held = Some((step, stats.clone(), substitutions.clone()));
                first_reason = Some(format!(
                    "the mesh is too coarse to check the B-rep's volume closely (tolerance {:.1e})",
                    stats.volume_tolerance
                ));
                continue;
            }
            Attempt::Done(step)
                if fitted
                    && mode == walk::Extrusions::Exact
                    && k < 2
                    && stats.exact_faces < stats.faces =>
            {
                // A blend between curved faces whose tool left slivers
                // standing beside it (lens-shaped faces of two edges
                // where its contact runs steeply across the polygon's
                // facets of the face it meets, so they reconstruct as
                // facets). The faces' polygons at twice or four times
                // the segments are shallow enough that the tool follows
                // them; this file is kept in case they do not.
                if partly.is_none() {
                    partly = Some((step, stats.clone(), substitutions.clone()));
                }
                first_reason = Some(format!(
                    "{} of {} faces were written as facets",
                    stats.faces - stats.exact_faces,
                    stats.faces
                ));
                continue;
            }
            Attempt::Done(step) => {
                stats.retried_because = first_reason;
                return Ok(ExactExport {
                    step,
                    stats,
                    substitutions,
                });
            }
            Attempt::Rejected { message, retry, at } => {
                last_retry = retry;
                if mode == walk::Extrusions::Exact && !at.is_empty() {
                    located = Some((built, at));
                } else if mode == walk::Extrusions::Faceted && !at.is_empty() {
                    // The last resort's own failure, located: its regions
                    // as facets too.
                    let before = stats.exact_attempt_faceted.clone();
                    if let Some((step, subs)) = partial(
                        &built,
                        &at,
                        &message,
                        normal_volume,
                        normal_box,
                        x,
                        &mut stats,
                        &now,
                    ) {
                        stats.exact_attempt_faceted = before;
                        stats.retried_because = None;
                        return Ok(ExactExport {
                            step,
                            stats,
                            substitutions: subs,
                        });
                    }
                }
                if retry && k == 0 {
                    first_reason = Some(message);
                    continue;
                }
                let message = match first_reason.take() {
                    Some(first) if first != message => {
                        format!("{message} (and at the default resolution: {first})")
                    }
                    _ => message,
                };
                if mode == walk::Extrusions::Exact {
                    exact_failure = Some(message);
                    continue;
                }
                let message = match &stats.fallback {
                    Some(exact) => format!("{message} (and with its extrusions exact: {exact})"),
                    None => message,
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
    stats.retried_because = None;
    Err(Box::new(ExactFailure {
        message: exact_failure.unwrap_or_else(|| "the export failed".into()),
        interrupted: None,
        stats,
        substitutions,
    }))
}

/// The largest export mesh, in triangles, whose fitted blends get a
/// retry at four times the segments after two failures.
const RETRY_FITTED_TRIANGLES: usize = 20_000;

/// How many rounds [`partial`] grows the faceted region before giving up.
/// Each is one more reconstruction; the corpora's models that pass take
/// one or two.
const PARTIAL_ROUNDS: u32 = 8;

/// Union-find root with path halving, over `u32` parents.
fn root(parent: &mut [u32], mut i: u32) -> u32 {
    while parent[i as usize] != i {
        parent[i as usize] = parent[parent[i as usize] as usize];
        i = parent[i as usize];
    }
    i
}

/// The partial faceted fallback: `built`'s exact attempt failed at the
/// triangles `at`, so the source regions there (connected triangles of
/// one exact surface record, which would have been faces) are retagged
/// as facets and the mesh is reconstructed again, growing the region
/// while each new failure is located, up to [`PARTIAL_ROUNDS`] times.
///
/// The result is held to every check an exact attempt is, with the
/// facets' caps added to what the volume may differ from the normal
/// render by, and to a volume check tight enough to trust
/// ([`LOOSE_CHECK`]): a file written this way is never the wrong solid,
/// only a less exact one. On success, `stats` is the passing attempt's,
/// with [`ExactStats::partial`] set, and the substitutions are `built`'s
/// plus one for each source location that lost faces. On failure `stats`
/// is as it was, with the attempts and their time added.
#[allow(clippy::too_many_arguments)]
fn partial(
    built: &walk::ExportMesh,
    at: &[u32],
    reason: &str,
    normal_volume: f64,
    normal_box: Bounds,
    x: &ExactOptions<'_>,
    stats: &mut ExactStats,
    now: &dyn Fn() -> f64,
) -> Option<(String, Vec<Substitution>)> {
    let before = stats.clone();
    let out = partial_rounds(built, at, reason, normal_volume, normal_box, x, stats, now);
    if out.is_none() {
        let (attempts, timings) = (stats.attempts, stats.timings);
        *stats = before;
        stats.attempts = attempts;
        stats.timings = timings;
    }
    out
}

/// [`partial`]'s rounds.
#[allow(clippy::too_many_arguments)]
fn partial_rounds(
    built: &walk::ExportMesh,
    at: &[u32],
    reason: &str,
    normal_volume: f64,
    normal_box: Bounds,
    x: &ExactOptions<'_>,
    stats: &mut ExactStats,
    now: &dyn Fn() -> f64,
) -> Option<(String, Vec<Substitution>)> {
    let mesh = &built.mesh;
    let nt = mesh.triangles.len();
    let tagged_faceted = |t: usize| {
        matches!(
            mesh.surfaces[mesh.triangle_surface[t] as usize],
            meshbrep::Surface::Faceted
        )
    };
    // The triangles across each edge, by sorting the edges.
    let mut keyed: Vec<(u32, u32, u32)> = Vec::with_capacity(3 * nt);
    for (t, tri) in mesh.triangles.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (tri[k], tri[(k + 1) % 3]);
            keyed.push((a.min(b), a.max(b), t as u32));
        }
    }
    keyed.sort_unstable();
    let mut neighbours: Vec<Vec<u32>> = vec![Vec::new(); nt];
    for w in keyed.windows(2) {
        if (w[0].0, w[0].1) == (w[1].0, w[1].1) && w[0].2 != w[1].2 {
            neighbours[w[0].2 as usize].push(w[1].2);
            neighbours[w[1].2 as usize].push(w[0].2);
        }
    }
    // Source regions: neighbours on one surface record.
    let mut parent: Vec<u32> = (0..nt as u32).collect();
    for (t, around) in neighbours.iter().enumerate() {
        for &u in around {
            if mesh.triangle_surface[t] == mesh.triangle_surface[u as usize] {
                let (a, b) = (root(&mut parent, t as u32), root(&mut parent, u));
                if a != b {
                    parent[a.max(b) as usize] = a.min(b);
                }
            }
        }
    }
    let region: Vec<u32> = (0..nt as u32).map(|t| root(&mut parent, t)).collect();
    let mut members: std::collections::BTreeMap<u32, Vec<u32>> = Default::default();
    for (t, &r) in region.iter().enumerate() {
        members.entry(r).or_default().push(t as u32);
    }
    let exact_triangles = (0..nt).filter(|&t| !tagged_faceted(t)).count();
    let max_cap = check::corrected_volume(mesh).max_cap;
    let mut faceted = vec![false; nt];
    let mut chosen: std::collections::BTreeSet<u32> = Default::default();
    let mut at = at.to_vec();
    let open = |t: u32, faceted: &[bool]| !faceted[t as usize] && !tagged_faceted(t as usize);
    for round in 1..=PARTIAL_ROUNDS {
        let mut seeds: Vec<u32> = at.iter().copied().filter(|&t| open(t, &faceted)).collect();
        if seeds.is_empty() {
            // The failure is inside what is faceted already: the regions
            // next to it.
            seeds = at
                .iter()
                .flat_map(|&t| neighbours[t as usize].iter().copied())
                .filter(|&t| open(t, &faceted))
                .collect();
        }
        if seeds.is_empty() {
            return None;
        }
        for t in seeds {
            let r = region[t as usize];
            if chosen.insert(r) {
                for &m in &members[&r] {
                    faceted[m as usize] = true;
                }
            }
        }
        let mut retagged = mesh.clone();
        let tag = retagged.surfaces.len() as u32;
        retagged.surfaces.push(meshbrep::Surface::Faceted);
        let gone: Vec<u32> = (0..nt as u32).filter(|&t| faceted[t as usize]).collect();
        for &t in &gone {
            retagged.triangle_surface[t as usize] = tag;
        }
        let facets = Facets {
            caps: check::cap_volume(mesh, &gone),
            max_cap,
        };
        stats.attempts += 1;
        let outcome = attempt(
            built,
            &retagged,
            facets,
            normal_volume,
            normal_box,
            false,
            x,
            stats,
            now,
        );
        let step = match outcome {
            Attempt::Done(step) | Attempt::Loose(step) => step,
            Attempt::Rejected { at: next, .. } => {
                if next.is_empty() {
                    return None;
                }
                at = next;
                continue;
            }
        };
        if stats.volume_tolerance > LOOSE_CHECK {
            // Too coarse a check to vouch for a solid that is not the
            // exact model's anyway.
            return None;
        }
        // One substitution per source location that lost faces.
        let mut per_origin: std::collections::BTreeMap<u32, usize> = Default::default();
        for r in &chosen {
            let s = mesh.triangle_surface[*r as usize] as usize;
            if let Some(&o) = built.surface_origin.get(s) {
                *per_origin.entry(o).or_default() += 1;
            }
        }
        let why = reason.split("; ").next().unwrap_or(reason);
        let mut subs = built.substitutions.clone();
        for (o, n) in per_origin {
            let (module, loc) = built.origins[o as usize].clone();
            subs.push(Substitution {
                kind: SubstitutionKind::Faceted,
                module,
                detail: format!(
                    "is exported partly as planar facets ({n} face{} of it), where the model did not reconstruct exact: {why}",
                    if n == 1 { "" } else { "s" }
                ),
                loc,
                count: 1,
            });
        }
        stats.partial = Some(Partial {
            reason: reason.to_string(),
            regions: chosen.len(),
            triangles: gone.len(),
            exact_triangles,
            rounds: round,
        });
        return Some((step, subs));
    }
    None
}

/// The modules among `subs` that fell back to facets, each once, sorted.
fn faceted_modules(subs: &[Substitution]) -> Vec<&'static str> {
    let mut m: Vec<&'static str> = subs
        .iter()
        .filter(|s| s.kind == SubstitutionKind::Faceted)
        .map(|s| s.module)
        .collect();
    m.sort_unstable();
    m.dedup();
    m
}

/// One reconstruction of `mesh` (the export render's, or it with some
/// regions retagged as facets by [`partial`]) and every check on it.
/// `facets` says what retagged regions change in the checks.
#[allow(clippy::too_many_arguments)]
fn attempt(
    built: &walk::ExportMesh,
    mesh: &meshbrep::TaggedMesh,
    facets: Facets,
    normal_volume: f64,
    normal_box: Bounds,
    first: bool,
    x: &ExactOptions<'_>,
    stats: &mut ExactStats,
    now: &dyn Fn() -> f64,
) -> Attempt {
    stats.triangles = mesh.triangles.len();
    if mesh.triangles.is_empty() {
        return Attempt::Rejected {
            message: "the export render is empty".into(),
            retry: false,
            at: Vec::new(),
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
            at: Vec::new(),
        };
    }
    let t0 = now();
    let options = meshbrep::Options {
        tolerances: walk::tolerances(built),
        ..meshbrep::Options::default()
    };
    let brep = meshbrep::reconstruct_located(mesh, &options);
    stats.timings.reconstruct_ms += now() - t0;
    let brep = match brep {
        Ok(b) => b,
        Err(f) => {
            let retry = matches!(
                f.error,
                meshbrep::Error::TopologyMismatch(_) | meshbrep::Error::Reconstruction(_)
            );
            return Attempt::Rejected {
                message: format!("reconstruction failed: {}", f.error),
                retry,
                at: f.triangles,
            };
        }
    };
    // The input triangles of some of the B-rep's faces.
    let triangles_of = |faces: &mut dyn Iterator<Item = usize>| -> Vec<u32> {
        let mut t: Vec<u32> = faces
            .filter_map(|f| brep.report.face_triangles.get(f))
            .flatten()
            .copied()
            .collect();
        t.sort_unstable();
        t.dedup();
        t
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
    stats.chain_deviation = brep.report.max_chain_deviation;
    stats.notes.extend(validation.notes.iter().cloned());
    if !validation.is_valid() {
        stats.timings.check_ms += now() - t0;
        let mut errs = validation.errors.clone();
        errs.truncate(3);
        return Attempt::Rejected {
            message: format!("the B-rep is invalid: {}", errs.join("; ")),
            retry: true,
            at: triangles_of(&mut validation.error_faces.iter().map(|&f| f as usize)),
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
            at: Vec::new(),
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
                at: Vec::new(),
            };
        }
    };
    stats.volume = volume;
    let corrected = check::corrected_volume(mesh);
    stats.corrected_volume = corrected.volume;
    stats.max_cap = corrected.max_cap;
    // The mesh's intersection curves stand off the exact edges by about
    // its sagitta (at most 1.6 times the largest cap on every valid model
    // of the stop-rule corpora). Far more means the mesh went round a
    // feature smaller than its tessellation: two masks of one profile,
    // one extruded and one revolved, meeting in a corner patch made a
    // B-rep that passed every check and was 0.4% off the model.
    let chain_limit = 4.0 * corrected.max_cap.max(facets.max_cap) + 1e-6 * scale;
    if stats.chain_deviation > chain_limit {
        stats.timings.check_ms += now() - t0;
        // The faces either side of every edge whose chain stands too far
        // off: where the mesh went round a feature it could not resolve.
        let mut faces = brep.faces.iter().enumerate().filter_map(|(fi, f)| {
            f.loops
                .iter()
                .flat_map(|l| &l.coedges)
                .any(|c| {
                    brep.report
                        .edge_chain_deviation
                        .get(c.edge as usize)
                        .is_some_and(|&d| d > chain_limit)
                })
                .then_some(fi)
        });
        let at = triangles_of(&mut faces);
        return Attempt::Rejected {
            at,
            message: format!(
                "the mesh's topology differs from the exact model's: its intersection curves stand {:.3e} off the exact edges, more than its tessellation accounts for ({chain_limit:.3e})",
                stats.chain_deviation
            ),
            retry: true,
        };
    }
    let denom = volume.abs().max(1e-300);
    stats.volume_error = (volume - corrected.volume).abs() / denom;
    // The residual bound with a margin of 4 (it is an estimate of a
    // second-order term, not a proof), plus what the STEP file's own
    // precision (1e-7 in model units) and rounding allow.
    let tolerance =
        4.0 * corrected.residual_bound + 1e-7 * volume.abs() + 1e-9 * scale * scale * scale;
    stats.volume_tolerance = tolerance / denom;
    stats.timings.check_ms += now() - t0;
    let loose = first && stats.volume_tolerance > LOOSE_CHECK;
    if (volume - corrected.volume).abs() > tolerance {
        return Attempt::Rejected {
            message: format!(
                "the B-rep's volume {volume:.6} does not match its mesh ({:.6} corrected onto its surfaces, tolerance {:.2e})",
                corrected.volume, tolerance
            ),
            retry: true,
            at: Vec::new(),
        };
    }
    // Regions written as facets (a partial fallback) stand off their
    // exact surfaces by their caps. The export render's tessellation is at
    // least as fine as the normal render's, so its caps are within what
    // the normal render's polygons stand off the curves (or a small part
    // of the model, where they are not curves the normal render counts);
    // more means the triangles were not on the surfaces they named, and
    // nothing below would then vouch for the file.
    let cap_limit = 1.05 * built.normal_volume_bound + LOOSE_CHECK * volume.abs();
    if facets.caps.size > cap_limit {
        return Attempt::Rejected {
            message: format!(
                "the regions written as facets stand {:.3e} off their exact surfaces in volume, too much to check against the rendered mesh",
                facets.caps.size
            ),
            retry: false,
            at: Vec::new(),
        };
    }
    // Against the normal render: the exact curves stand off its polygons
    // by at most their sagitta, so the volumes differ by at most the
    // curved area times that, and each side of the box by the sagitta.
    // Beyond that, the export render built a different model. With some
    // regions as facets, their caps put back give the exact model's
    // volume, which is what is held to that.
    let allowance = 1.05 * built.normal_volume_bound
        + 2.0 * corrected.residual_bound
        + facets.caps.error
        + 1e-7 * normal_volume.abs()
        + 1e-9 * scale * scale * scale;
    let exact_volume = volume + facets.caps.signed;
    if (exact_volume - normal_volume).abs() > allowance {
        return Attempt::Rejected {
            message: format!(
                "the exact model's volume {exact_volume:.6} is further from the rendered mesh's ({normal_volume:.6}) than its curves can account for ({allowance:.3e})"
            ),
            retry: false,
            at: Vec::new(),
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
                at: Vec::new(),
            };
        }
    }
    let t0 = now();
    let step = meshbrep::write_step(&brep, &x.step);
    stats.timings.write_ms += now() - t0;
    if loose {
        Attempt::Loose(step)
    } else {
        Attempt::Done(step)
    }
}

/// The tagged mesh's corrected volume and the tolerance a B-rep's volume
/// is held to against it (as [`attempt`] computes them).
fn volume_check(mesh: &meshbrep::TaggedMesh, volume: f64) -> (f64, f64) {
    let corrected = check::corrected_volume(mesh);
    let scale = mesh_bounds(mesh).map_or(0.0, |(lo, hi)| {
        (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max)
    });
    let tolerance =
        4.0 * corrected.residual_bound + 1e-7 * volume.abs() + 1e-9 * scale * scale * scale;
    (corrected.volume, tolerance)
}
