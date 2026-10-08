//! Every test case at the six attribution resolutions: reconstruction
//! must succeed, validate, match the closed-form volume to 1e-6 relative
//! and write byte-identical STEP when run again.
//!
//! `cargo test -p meshbrep --release --test cases -- --nocapture` prints a
//! table of the results.

mod common;

use common::*;
use meshbrep::{Brep, Options, StepOptions, measure, reconstruct, validate, write_step};
use std::time::Instant;

struct Outcome {
    line: String,
    failure: Option<String>,
}

fn kinds(b: &Brep) -> String {
    let mut m = std::collections::BTreeMap::new();
    for e in &b.edges {
        *m.entry(if e.seam { "seam" } else { e.curve.kind() })
            .or_insert(0) += 1;
    }
    m.iter()
        .map(|(k, v)| format!("{v} {k}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn run(name: &str, res: Res) -> Outcome {
    let (mesh, mesh_volume, reference, result, rejected) = build(name, res);
    let brep = match result {
        Ok(b) => b,
        Err(e) => {
            return Outcome {
                line: format!("{name} {}: error {e}", res.name()),
                failure: Some(format!("{name} {}: {e}", res.name())),
            };
        }
    };
    let notes = if brep.report.notes.is_empty() {
        String::new()
    } else {
        format!("  notes: {}", brep.report.notes.join("; "))
    };
    let retried = if rejected.is_empty() {
        String::new()
    } else {
        format!(
            "  [topology mismatch at {}; retried finer]",
            rejected
                .iter()
                .map(|r| r.name())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let t0 = Instant::now();
    let _ = reconstruct(&mesh, &Options::default());
    let recon = t0.elapsed().as_secs_f64() * 1e3;
    let t1 = Instant::now();
    let step = write_step(&brep, &StepOptions::default());
    let write = t1.elapsed().as_secs_f64() * 1e3;
    let v = validate(&brep, 1e-6);
    let m = measure(&brep);
    let mut failure = None;
    if !v.is_valid() {
        failure = Some(format!("{name} {}: invalid: {:?}", res.name(), v.errors));
    }
    // Without a closed form (a faceted polyhedron cut by an exact hole),
    // the reference is the B-rep from the finest attribution mesh: the
    // exact model does not depend on the attribution resolution, so
    // neither may the result.
    let reference = reference.or_else(|| {
        let (fine, _, _) = tagged(name, Res::Fn(64));
        Some(
            measure(&reconstruct(&fine, &Options::default()).ok()?)
                .ok()?
                .volume,
        )
    });
    let (vol, rel) = match (&m, reference) {
        (Ok(m), Some(r)) => (m.volume, (m.volume - r).abs() / r),
        (Ok(m), None) => (m.volume, f64::NAN),
        (Err(e), _) => {
            failure.get_or_insert(format!("{name} {}: measure: {e}", res.name()));
            (f64::NAN, f64::NAN)
        }
    };
    // NaN (no reference, or measurement failed) fails too.
    if rel.is_nan() || rel >= 1e-6 {
        failure.get_or_insert(format!(
            "{name} {}: volume {vol} vs {:?} (mesh {mesh_volume}), rel {rel:.2e}",
            res.name(),
            reference
        ));
    }
    // Determinism: the whole pipeline again, Manifold included.
    let (_, _, _, again, _) = build(name, res);
    let step2 = write_step(&again.expect("second run"), &StepOptions::default());
    if step != step2 {
        failure.get_or_insert(format!("{name} {}: STEP differs between runs", res.name()));
    }
    Outcome {
        line: format!(
            "{name} {:7} tris {:6} faces {:3} edges {:3} ({}) vol {vol:.6} rel {rel:.1e} genus {:?} recon {recon:.2} ms write {write:.2} ms {} bytes, pcurve dev {:.1e}, edge dev {:.1e}, chain dev {:.1e}{retried}{notes}{}",
            res.name(),
            mesh.triangles.len(),
            brep.faces.len(),
            brep.edges.len(),
            kinds(&brep),
            v.genus,
            step.len(),
            brep.report.max_pcurve_deviation,
            brep.report.max_edge_deviation,
            brep.report.max_chain_deviation,
            if failure.is_some() { "  FAIL" } else { "" }
        ),
        failure,
    }
}

fn run_all(names: &[&str]) {
    let mut failures = Vec::new();
    for name in names {
        for res in RESOLUTIONS {
            let o = run(name, res);
            eprintln!("{}", o.line);
            failures.extend(o.failure);
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn the_fifteen_boolean_cases() {
    run_all(&FIFTEEN);
}

#[test]
fn common_idioms() {
    run_all(&IDIOMS);
}

#[test]
fn faceted_voids_poles_and_fillets() {
    run_all(&MORE);
}

#[test]
fn tori() {
    run_all(&TORI);
}

/// Reconstruction time of one case, repeated (`MESHBREP_BENCH=c02,200`),
/// for profiling. Does nothing without the variable.
#[test]
fn bench() {
    let Some(spec) = std::env::var("MESHBREP_BENCH").ok() else {
        return;
    };
    let (name, reps) = spec.split_once(',').unwrap_or((&spec, "50"));
    let reps: usize = reps.parse().unwrap();
    let t = Instant::now();
    let (mesh, _, reference) = tagged(name, RESOLUTIONS[0]);
    let mesh_ms = t.elapsed().as_secs_f64() * 1e3;
    let mut times = Vec::new();
    let mut brep = None;
    for _ in 0..reps {
        let t0 = Instant::now();
        brep = Some(std::hint::black_box(
            reconstruct(&mesh, &Options::default()).unwrap(),
        ));
        times.push(t0.elapsed().as_secs_f64() * 1e3);
    }
    let brep = brep.unwrap();
    let t1 = Instant::now();
    let step = write_step(&brep, &StepOptions::default());
    let write_ms = t1.elapsed().as_secs_f64() * 1e3;
    let t2 = Instant::now();
    let v = validate(&brep, 1e-6);
    let validate_ms = t2.elapsed().as_secs_f64() * 1e3;
    times.sort_by(f64::total_cmp);
    let vol = measure(&brep).unwrap().volume;
    eprintln!(
        "{name}: {} triangles (Manifold {mesh_ms:.0} ms), {} faces; reconstruction median {:.2} ms (min {:.2}) of {reps}; write {write_ms:.2} ms, {} bytes; validate {validate_ms:.1} ms, valid {}; volume rel {:.1e}",
        mesh.triangles.len(),
        brep.faces.len(),
        times[times.len() / 2],
        times[0],
        step.len(),
        v.is_valid(),
        reference.map_or(f64::NAN, |r| (vol - r).abs() / r)
    );
}

/// Writes every case's STEP file at the default resolution into
/// `$MESHBREP_DUMP`, for reading back with an external tool (the OCCT
/// oracle in `oracle/`). Does nothing without the variable.
#[test]
fn dump() {
    let Some(dir) = std::env::var_os("MESHBREP_DUMP") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in FIFTEEN.iter().chain(&IDIOMS).chain(&MORE).chain(&TORI) {
        let (_, _, reference, b, _) = build(name, RESOLUTIONS[0]);
        let b = b.unwrap();
        let mut sizes = Vec::new();
        for e in &b.edges {
            if let meshbrep::Curve::BSpline(bs) = &e.curve {
                sizes.push(format!("edge {}", bs.control.len()));
            }
        }
        for f in &b.faces {
            for c in f.loops.iter().flat_map(|l| &l.coedges) {
                if let Some(pc) = c.pcurve.as_ref().filter(|p| p.control.len() > 2) {
                    sizes.push(format!("pcurve {}", pc.control.len()));
                }
            }
        }
        eprintln!(
            "{name}: reference {reference:?}; B-spline control points: {}",
            sizes.join(", ")
        );
        let step = write_step(&b, &StepOptions::default());
        std::fs::write(dir.join(format!("{name}.step")), step).unwrap();
    }
}

/// A tagged mesh of `node` at `res`, as [`tagged`] makes one for a case.
fn mesh_of(node: &Node, res: Res) -> meshbrep::TaggedMesh {
    let mut table = Vec::new();
    let gl = eval(node, res, &mut table).get_mesh_gl64(-1);
    let np = gl.num_prop as usize;
    meshbrep::TaggedMesh {
        positions: gl
            .vert_properties
            .chunks(np)
            .map(|c| [c[0], c[1], c[2]])
            .collect(),
        triangles: gl
            .tri_verts
            .chunks(3)
            .map(|c| [c[0] as u32, c[1] as u32, c[2] as u32])
            .collect(),
        triangle_surface: gl.face_id.iter().map(|&f| f as u32).collect(),
        surfaces: table,
    }
}

/// Every face names the input triangles it was built from, each triangle
/// at most once (`Report::face_triangles`), so that a caller can write
/// the faces it cannot use as facets; and a failure names the triangles
/// where it happened (`reconstruct_located`): x07, a faceted sphere minus
/// an exact skew hole, folds at 12 segments.
#[test]
fn faces_and_failures_name_their_triangles() {
    let (mesh, _, _) = tagged("c14", Res::Fn(16));
    let b = reconstruct(&mesh, &Options::default()).unwrap();
    assert_eq!(b.report.face_triangles.len(), b.faces.len());
    let mut uses = vec![0u32; mesh.triangles.len()];
    for t in b.report.face_triangles.iter().flatten() {
        uses[*t as usize] += 1;
    }
    assert!(uses.iter().all(|&n| n == 1), "every triangle in one face");
    assert_eq!(
        b.report.edge_chain_deviation.len(),
        b.edges.len(),
        "a chain deviation per edge"
    );

    let (mesh, _, _) = tagged("x07", Res::Fn(12));
    let f = meshbrep::reconstruct_located(&mesh, &Options::default()).unwrap_err();
    assert!(
        matches!(f.error, meshbrep::Error::TopologyMismatch(_)),
        "{f}"
    );
    assert!(!f.triangles.is_empty());
    assert!(
        f.triangles
            .iter()
            .all(|&t| (t as usize) < mesh.triangles.len())
    );
}

/// A cut stopping 2.1e-9 short of the far face leaves a wall thinner than
/// the tolerance. Manifold's mesh has needle triangles along its top, and
/// the face beside them used to have a boundary touching itself
/// (`TopologyMismatch`). They are flipped into their neighbours before
/// reconstruction, and the solid is the cut block, less a wall no STEP
/// file could hold.
#[test]
fn needles_thinner_than_the_tolerance_are_flipped() {
    let node = Node::Op(
        'D',
        vec![
            Node::Cube([20.0, 2.1, 7.0], meshbrep::primitives::Transform::IDENTITY),
            Node::Cube(
                [5.0, 3.0999999979, 5.0],
                meshbrep::primitives::Transform::translate([4.0, -1.0, 3.5]),
            ),
        ],
    );
    let mesh = mesh_of(&node, Res::Fn(16));
    let b = reconstruct(&mesh, &Options::default()).unwrap();
    assert!(
        b.report.notes.iter().any(|n| n.contains("needle")),
        "{:?}",
        b.report.notes
    );
    let v = validate(&b, 1e-6);
    assert!(v.is_valid(), "{:?}", v.errors);
    let vol = measure(&b).unwrap().volume;
    assert!((vol - 257.25).abs() < 1e-7, "{vol}");
}
