//! `Sketch::completion`: which suggested constraints would remove the free
//! degrees of freedom, and `Solution::unmet` for a solve that fails.

use sketch_solver::{Along, Constraint, Pair, Sketch, SolveOptions, Source, Status};

/// A rectangle with horizontal and vertical sides and nothing else: free
/// to move (2) and to change its width and height (2).
fn loose_rectangle() -> (
    Sketch,
    [sketch_solver::EntityId; 4],
    [sketch_solver::EntityId; 4],
) {
    let mut s = Sketch::new();
    let p = [
        s.point(Some([0.0, 0.0])).unwrap(),
        s.point(Some([20.0, 0.0])).unwrap(),
        s.point(Some([20.0, 10.0])).unwrap(),
        s.point(Some([0.0, 10.0])).unwrap(),
    ];
    let l = [
        s.line(p[0], p[1]).unwrap(),
        s.line(p[1], p[2]).unwrap(),
        s.line(p[2], p[3]).unwrap(),
        s.line(p[3], p[0]).unwrap(),
    ];
    s.add(Constraint::Horizontal(Pair::Line(l[0]))).unwrap();
    s.add(Constraint::Horizontal(Pair::Line(l[2]))).unwrap();
    s.add(Constraint::Vertical(Pair::Line(l[1]))).unwrap();
    s.add(Constraint::Vertical(Pair::Line(l[3]))).unwrap();
    (s, p, l)
}

#[test]
fn completion_takes_the_candidates_that_each_remove_freedom() {
    let (s, p, l) = loose_rectangle();
    let sol = s.solve();
    assert_eq!(sol.status, Status::Solved);
    assert_eq!(sol.dof, 4);
    let candidates = vec![
        // Already implied by the sides: refused.
        Constraint::Horizontal(Pair::Line(l[0])),
        Constraint::Length {
            line: l[0],
            value: 20.0,
        },
        // The opposite side's length follows from the first's.
        Constraint::Length {
            line: l[2],
            value: 20.0,
        },
        Constraint::Length {
            line: l[1],
            value: 10.0,
        },
        // A wrong kind of entity: the model refuses it.
        Constraint::Length {
            line: p[0],
            value: 1.0,
        },
        Constraint::Fix {
            entity: p[0],
            at: None,
        },
        // Nothing is left to remove.
        Constraint::Distance {
            a: p[0],
            b: p[2],
            value: 10.0,
            along: Along::Y,
        },
    ];
    let taken = s
        .completion(&sol, &candidates, &SolveOptions::default())
        .unwrap();
    assert_eq!(taken, vec![1, 3, 5]);
    // With them added, the rectangle is fully constrained and nothing is
    // redundant.
    let mut full = s.clone();
    for k in &taken {
        full.add(candidates[*k].clone()).unwrap();
    }
    let sol = full.solve();
    assert!(sol.is_fully_constrained(), "{sol:?}");
    assert!(sol.redundant.is_empty() && sol.conflicts.is_empty());
}

#[test]
fn a_two_equation_candidate_is_taken_only_whole() {
    // A fixed point and a second one free on a horizontal line through it:
    // one degree of freedom, so fixing the second point (two equations)
    // would make one of them redundant and is refused; its x alone is
    // taken.
    let mut s = Sketch::new();
    let o = s.point(Some([0.0, 0.0])).unwrap();
    let a = s.point(Some([5.0, 0.0])).unwrap();
    s.add(Constraint::Fix {
        entity: o,
        at: None,
    })
    .unwrap();
    s.add(Constraint::Horizontal(Pair::Points(o, a))).unwrap();
    let sol = s.solve();
    assert_eq!(sol.dof, 1);
    let candidates = vec![
        Constraint::Fix {
            entity: a,
            at: None,
        },
        Constraint::Distance {
            a: o,
            b: a,
            value: 5.0,
            along: Along::X,
        },
    ];
    let taken = s
        .completion(&sol, &candidates, &SolveOptions::default())
        .unwrap();
    assert_eq!(taken, vec![1]);
}

#[test]
fn completion_can_be_interrupted() {
    let (s, _, l) = loose_rectangle();
    let sol = s.solve();
    let stop = || true;
    let opt = SolveOptions {
        interrupt: Some(&stop),
        ..SolveOptions::default()
    };
    let c = [Constraint::Length {
        line: l[0],
        value: 20.0,
    }];
    assert_eq!(
        s.completion(&sol, &c, &opt),
        Err(sketch_solver::SolveError::Interrupted)
    );
}

#[test]
fn unmet_names_the_equations_a_failed_solve_leaves() {
    // Two points 10 apart along x and 3 apart in all: no solution.
    let mut s = Sketch::new();
    let o = s.point(Some([0.0, 0.0])).unwrap();
    let a = s.point(Some([10.0, 0.0])).unwrap();
    s.add(Constraint::Fix {
        entity: o,
        at: None,
    })
    .unwrap();
    s.add(Constraint::Distance {
        a: o,
        b: a,
        value: 10.0,
        along: Along::X,
    })
    .unwrap();
    let d = s
        .add(Constraint::Distance {
            a: o,
            b: a,
            value: 3.0,
            along: Along::Direct,
        })
        .unwrap();
    let sol = s.solve();
    assert_eq!(sol.status, Status::NotConverged);
    assert!(!sol.unmet.is_empty());
    assert!(
        sol.unmet
            .iter()
            .any(|(src, r)| *src == Source::Constraint(d) && *r > 1.0),
        "{:?}",
        sol.unmet
    );
    // Worst first.
    assert!(sol.unmet.windows(2).all(|w| w[0].1 >= w[1].1));
    // A solved sketch leaves none.
    let (r, _, _) = loose_rectangle();
    assert!(r.solve().unmet.is_empty());
}
