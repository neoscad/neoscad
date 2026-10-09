//! The rotational class: a circular edge's tool is its cross-section in
//! the meridian half-plane through the edge's start, revolved about the
//! edge's axis. The fillet arc sweeps a **torus**, a chamfer's line a
//! **cone** (or a plane or a cylinder where the line is square to the
//! axis or along it), and every other side of the region the cone, plane
//! or cylinder its own segment sweeps.
//!
//! The sections lie at the angles the caller gives
//! ([`super::Path::Arc`]'s `sections`): the vertices of the polygon the
//! faceted face beside the edge has in the mesh the tool is applied to.
//! A tool with its sections there meets the polygon along its vertical
//! edges, chord for chord, where a tool with sections of its own would
//! leave the polygon's facets standing over the blend between them, or
//! cut slivers out of them.

use super::{
    ArcFrame, BlendEdge, BlendError, BlendSpec, End, Frame, Mesh, Path, Prof, Profile,
    face_surface, side_face,
};
use crate::math::*;
use crate::model::Surface;

/// Regular sections to a turn when the caller gives none.
const REGULAR: f64 = 32.0;

/// Angles closer than this are one section.
const SAME: f64 = 1e-9;

/// A section: its angle from the arc's start, and how far it is moved out
/// along its radial.
pub(super) type Sect = (f64, f64);

/// What [`arc_ends`] gives: the sections, the end rings and their caps.
pub(super) type ArcEnds = (Vec<Sect>, [Vec<V>; 2], [Surface; 2]);

/// An arc's sections (its ends, and the extensions past open ends,
/// included), and, for a partial arc, its two end rings and their caps.
pub(super) fn arc_ends(
    e: &BlendEdge,
    i: usize,
    fr: &Frame,
    a: &ArcFrame,
    prof: &Prof,
) -> Result<ArcEnds, BlendError> {
    let given: Vec<Sect> = match &e.path {
        Path::Arc { sections, .. } => sections
            .iter()
            .filter(|x| x[0].is_finite() && x[1].is_finite())
            .map(|x| (x[0], x[1]))
            .collect(),
        Path::Line | Path::Curve { .. } => Vec::new(),
    };
    let order = |t: &mut Vec<Sect>| {
        t.sort_by(|x, y| x.0.total_cmp(&y.0));
        t.dedup_by(|x, y| (x.0 - y.0).abs() <= SAME);
    };
    let sweep = a.sweep;
    let full = sweep >= TAU * (1.0 - 1e-12);
    if full {
        let mut t: Vec<Sect> = given.iter().map(|x| (x.0.rem_euclid(TAU), x.1)).collect();
        order(&mut t);
        if t.len() > 1 && t[t.len() - 1].0 - t[0].0 >= TAU - SAME {
            t.pop();
        }
        if t.len() < 3 {
            let n = REGULAR as usize;
            t = (0..n).map(|j| (TAU * j as f64 / n as f64, 0.0)).collect();
        }
        let none = || Surface::Faceted;
        return Ok((t, [Vec::new(), Vec::new()], [none(), none()]));
    }
    // The ends' own shifts, when the polygon has its vertices there.
    let shift_at = |t: f64| {
        given
            .iter()
            .find(|x| (x.0 - t).abs() <= SAME)
            .map_or(0.0, |x| x.1)
    };
    let mut inner: Vec<Sect> = if given.is_empty() {
        let n = ((REGULAR * sweep / TAU).ceil() as usize).max(1);
        (1..n).map(|j| (sweep * j as f64 / n as f64, 0.0)).collect()
    } else {
        given
            .iter()
            .copied()
            .filter(|x| x.0 > SAME && x.0 < sweep - SAME)
            .collect()
    };
    order(&mut inner);
    // Past an open end the tool runs on by an angle, into the air beyond
    // the end face (a plane through the axis), but never round into the
    // other end's extension.
    let delta = (PI / 8.0).min((TAU - sweep) / 4.0);
    let mut t = Vec::with_capacity(inner.len() + 4);
    let open = [0, 1].map(|k| matches!(e.ends[k], End::Open { .. }));
    let (s0, s1) = (shift_at(0.0), shift_at(sweep));
    if open[0] {
        t.push((-delta, s0));
    }
    t.push((0.0, s0));
    t.extend(inner);
    t.push((sweep, s1));
    if open[1] {
        t.push((sweep + delta, s1));
    }
    let ring = prof.ring();
    let mer: Vec<(f64, f64)> = ring.iter().map(|q| a.meridian(prof.base + *q)).collect();
    let mut rings: [Vec<V>; 2] = [Vec::new(), Vec::new()];
    let mut caps = [Surface::Faceted, Surface::Faceted];
    for end in 0..2 {
        let (at, dr) = if end == 0 { t[0] } else { t[t.len() - 1] };
        rings[end] = mer.iter().map(|&(r, z)| a.at(r + dr, z, at)).collect();
        let tangent = a.a.cross(a.radial(at));
        let out = if end == 0 { tangent * -1.0 } else { tangent };
        caps[end] = match &e.ends[end] {
            End::Plane { origin, normal } => {
                // The face the arc ends on: a plane through the axis.
                let (o, n) = (V::from(*origin), V::from(*normal));
                if n.dot(a.a).abs() > 1e-6 || (a.c - o).dot(n).abs() > 1e-6 * (a.rho + fr.len) {
                    return Err(BlendError::Invalid(format!(
                        "edge {i}: an arc's tool ends only on a plane through its axis"
                    )));
                }
                // On that plane exactly, along the tangent: the axis may
                // miss it in the last digits (a 2D offset's circle and the
                // line it runs on into, rounded apart), and a cap off the
                // plane its neighbour's face ends on leaves a sliver.
                let dn = tangent.dot(n);
                if dn.abs() > 1e-6 {
                    for x in &mut rings[end] {
                        *x = *x + tangent * ((o - *x).dot(n) / dn);
                    }
                }
                Surface::Plane {
                    origin: *origin,
                    normal: *normal,
                }
            }
            End::Chain { .. } | End::Open { .. } => Surface::Plane {
                origin: a.c.arr(),
                normal: out.arr(),
            },
            End::Corner(_) | End::Mitre { .. } => {
                return Err(BlendError::Invalid(format!(
                    "edge {i}: an arc's tool has no corner or mitre ends"
                )));
            }
        };
    }
    Ok((t, rings, caps))
}

/// The surface a segment of the meridian from `p` to `q` (distance from
/// the axis, height along it) sweeps about the axis: a plane square to
/// it, a cylinder about it, or a cone.
fn revolved(a: &ArcFrame, p: (f64, f64), q: (f64, f64)) -> Surface {
    let ((r1, z1), (r2, z2)) = (p, q);
    let scale = r1.abs().max(r2.abs()).max(z1.abs()).max(z2.abs());
    let eps = 1e-12 * scale.max(1e-300);
    if (z1 - z2).abs() <= eps {
        return Surface::Plane {
            origin: (a.c + a.a * (0.5 * (z1 + z2))).arr(),
            normal: a.a.arr(),
        };
    }
    if (r1 - r2).abs() <= eps {
        return Surface::Cylinder {
            origin: a.c.arr(),
            axis: a.a.arr(),
            radius: 0.5 * (r1 + r2),
        };
    }
    // Where the line meets the axis, and which way the nappe opens.
    let z_apex = z1 + (z2 - z1) * (r1 / (r1 - r2));
    let up = 0.5 * (z1 + z2) > z_apex;
    Surface::Cone {
        apex: (a.c + a.a * z_apex).arr(),
        axis: (if up { a.a } else { a.a * -1.0 }).arr(),
        slope: ((r2 - r1) / (z2 - z1)).abs(),
    }
}

/// One arc's tool into `m`: the cross-section revolved through
/// `angles`, its end rings `rings` (a partial arc's; a whole circle has
/// none) and their caps, but for those `skip` names (joined to a chained
/// neighbour). Returns its blend's surface entry.
#[allow(clippy::too_many_arguments)]
pub(super) fn arc_into(
    m: &mut Mesh,
    spec: &BlendSpec,
    e: &BlendEdge,
    fr: &Frame,
    a: &ArcFrame,
    prof: &Prof,
    angles: &[Sect],
    rings: &[Vec<V>; 2],
    caps: &[Surface; 2],
    skip: [bool; 2],
) -> Result<u32, BlendError> {
    let ring = prof.ring();
    let k = ring.len();
    let n_arc = prof.arc.len();
    let mer: Vec<(f64, f64)> = ring.iter().map(|q| a.meridian(prof.base + *q)).collect();
    let blend = m.surf(match spec.profile {
        Profile::Fillet => {
            let (rc, zc) = a.meridian(prof.base);
            Surface::Torus {
                center: (a.c + a.a * zc).arr(),
                axis: a.a.arr(),
                major_radius: rc,
                minor_radius: spec.size,
            }
        }
        Profile::Chamfer => revolved(a, mer[0], mer[1]),
    });
    let full = rings[0].is_empty();
    let last = angles.len() - 1;
    let secs: Vec<Vec<V>> = angles
        .iter()
        .enumerate()
        .map(|(s, &(t, dr))| {
            if !full && s == 0 {
                rings[0].clone()
            } else if !full && s == last {
                rings[1].clone()
            } else {
                mer.iter().map(|&(r, z)| a.at(r + dr, z, t)).collect()
            }
        })
        .collect();
    // The ring's winding about the direction the sections advance in
    // (the arc's tangent at the start) decides which way the sides face,
    // as for a straight tool.
    let wind: f64 = (0..k)
        .map(|j| ring[j].cross(ring[(j + 1) % k]).dot(fr.d))
        .sum();
    let ccw = wind > 0.0;
    let surf: Vec<u32> = (0..k)
        .map(|j| {
            let j1 = (j + 1) % k;
            if j + 1 < n_arc {
                blend
            } else if let Some(f) = side_face(spec, e, fr, prof, ring[j], ring[j1]) {
                m.surf(face_surface(&e.faces[f]))
            } else {
                m.surf(revolved(a, mer[j], mer[j1]))
            }
        })
        .collect();
    let spans = if full { secs.len() } else { secs.len() - 1 };
    for s in 0..spans {
        let (r0, r1) = (&secs[s], &secs[(s + 1) % secs.len()]);
        for j in 0..k {
            let j1 = (j + 1) % k;
            let (a0, b0) = (m.vert(r0[j]), m.vert(r0[j1]));
            let (a1, b1) = (m.vert(r1[j]), m.vert(r1[j1]));
            if ccw {
                m.tri([a0, b0, b1], surf[j]);
                m.tri([a0, b1, a1], surf[j]);
            } else {
                m.tri([a0, b1, b0], surf[j]);
                m.tri([a0, a1, b1], surf[j]);
            }
        }
    }
    if !full {
        for end in 0..2 {
            if skip[end] {
                continue;
            }
            let at = if end == 0 {
                angles[0].0
            } else {
                angles[last].0
            };
            let tangent = a.a.cross(a.radial(at));
            let out = if end == 0 { tangent * -1.0 } else { tangent };
            let pts = &rings[end];
            let mut nrm = V::default();
            for j in 0..k {
                nrm = nrm + pts[j].cross(pts[(j + 1) % k]);
            }
            let nrm = if nrm.dot(out) < 0.0 { nrm * -1.0 } else { nrm };
            let s = m.surf(caps[end].clone());
            m.polygon(pts, nrm.norm(), s)?;
        }
    }
    Ok(blend)
}
