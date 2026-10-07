//! The validation corpus: test cases translated from FreeCAD's and
//! SolveSpace's own test suites (tests/corpus/README.md), each solved here
//! and checked against what the upstream test asserts.
//!
//! For SolveSpace's regression files, which store a solved sketch, two
//! more checks: solving from that solution must leave it where it is (our
//! equations hold at SolveSpace's solution, so the two agree on what each
//! constraint means), and solving from a perturbed drawing must converge,
//! back to SolveSpace's solution when the sketch is fully constrained.

#[path = "support/corpus.rs"]
mod corpus;

use std::path::Path;

use corpus::{Start, build, check, distance_to_solved, load};
use sketch_solver::Status;

#[test]
fn corpus() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let files = load(&root);
    assert!(
        !files.is_empty(),
        "no corpus files under {}",
        root.display()
    );
    let mut failures = Vec::new();
    let mut total = 0;
    for f in &files {
        assert!(
            !f.commit.is_empty() && !f.licence.is_empty(),
            "{}: provenance",
            f.path.display()
        );
        for case in &f.cases {
            total += 1;
            let name = case["name"].as_str().unwrap_or("?");
            let mut fail = |what: String| failures.push(format!("{name}: {what}"));
            let b = match build(case, Start::Drawing, &[]) {
                Ok(b) => b,
                Err(e) => {
                    fail(format!("does not build: {e}"));
                    continue;
                }
            };
            let sol = b.sketch.solve();
            for e in check(&b, &sol, &case["expect"]) {
                fail(e);
            }
            if let Some(d) = distance_to_solved(&b, &sol, case) {
                // Started at upstream's solution: it must stay there.
                if d > 1e-9 * sol.size {
                    fail(format!(
                        "moved {d:e} from the upstream solution it started at"
                    ));
                }
                let p = build(case, Start::Perturbed, &[]).unwrap();
                let ps = p.sketch.solve();
                if ps.status != Status::Solved {
                    fail(format!(
                        "perturbed: {:?}, residual {:e}",
                        ps.status, ps.residual
                    ));
                } else if sol.dof == 0 {
                    let d = distance_to_solved(&p, &ps, case).unwrap();
                    if d > 1e-9 * ps.size {
                        fail(format!(
                            "perturbed: solved {d:e} away from the upstream solution"
                        ));
                    }
                }
            }
            let mut set = Vec::new();
            for (k, stage) in case["stages"].as_array().into_iter().flatten().enumerate() {
                for s in stage["set"].as_array().unwrap() {
                    set.push((s[0].as_u64().unwrap() as usize, s[1].as_f64().unwrap()));
                }
                let b = build(case, Start::Drawing, &set).unwrap();
                let sol = b.sketch.solve();
                for e in check(&b, &sol, &stage["expect"]) {
                    fail(format!("stage {}: {e}", k + 1));
                }
            }
        }
    }
    eprintln!("corpus: {total} cases, {} failures", failures.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
