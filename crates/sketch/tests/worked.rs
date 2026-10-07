//! The worked examples of NeoSCAD's design (docs/language-extensions.md,
//! sections 6.1 and 6.2), the diagnosis on textbook cases, and stability
//! when a dimension changes.

use sketch_solver::{
    Along, Constraint, Coordinate, EntityId, Orientation, Pair, Sketch, SolveError, SolveOptions,
    Source, Status,
};

fn close(a: [f64; 2], b: [f64; 2], tol: f64) -> bool {
    (a[0] - b[0]).abs() <= tol && (a[1] - b[1]).abs() <= tol
}

struct Gusset {
    s: Sketch,
    pts: [EntityId; 7],
}

/// Section 6.1: an L-bracket profile with a gusset.
fn gusset(leg_a: f64, leg_b: f64, t: f64, gusset: f64, with_end_b_length: bool) -> Gusset {
    let mut s = Sketch::new();
    let p = |s: &mut Sketch, x: f64, y: f64| s.point(Some([x, y])).unwrap();
    let o = p(&mut s, 0.0, 0.0);
    let a = p(&mut s, leg_a, 0.0);
    let a2 = p(&mut s, leg_a, t);
    let g1 = p(&mut s, t + gusset, t);
    let g2 = p(&mut s, t, t + gusset);
    let b2 = p(&mut s, t, leg_b);
    let b = p(&mut s, 0.0, leg_b);
    let bottom = s.line(o, a).unwrap();
    let end_a = s.line(a, a2).unwrap();
    let top_a = s.line(a2, g1).unwrap();
    let _hyp = s.line(g1, g2).unwrap();
    let in_b = s.line(g2, b2).unwrap();
    let end_b = s.line(b2, b).unwrap();
    let back = s.line(b, o).unwrap();
    let mut add = |c| {
        s.add(c).unwrap();
    };
    add(Constraint::Fix {
        entity: o,
        at: None,
    });
    add(Constraint::Horizontal(Pair::Line(bottom)));
    add(Constraint::Length {
        line: bottom,
        value: leg_a,
    });
    add(Constraint::Vertical(Pair::Line(end_a)));
    add(Constraint::Length {
        line: end_a,
        value: t,
    });
    add(Constraint::Horizontal(Pair::Line(top_a)));
    add(Constraint::Vertical(Pair::Line(in_b)));
    add(Constraint::Horizontal(Pair::Line(end_b)));
    if with_end_b_length {
        add(Constraint::Length {
            line: end_b,
            value: t,
        });
    }
    add(Constraint::Vertical(Pair::Line(back)));
    add(Constraint::Length {
        line: back,
        value: leg_b,
    });
    add(Constraint::Distance {
        a: o,
        b: g1,
        value: t + gusset,
        along: Along::X,
    });
    add(Constraint::Distance {
        a: o,
        b: g2,
        value: t + gusset,
        along: Along::Y,
    });
    Gusset {
        s,
        pts: [o, a, a2, g1, g2, b2, b],
    }
}

#[test]
fn gusset_is_fully_constrained() {
    let g = gusset(40.0, 30.0, 4.0, 18.0, true);
    let sol = g.s.solve();
    assert_eq!(sol.status, Status::Solved);
    assert_eq!(
        (sol.unknowns, sol.equations, sol.rank, sol.dof),
        (14, 14, 14, 0)
    );
    assert!(sol.free.is_empty() && sol.redundant.is_empty() && sol.conflicts.is_empty());
    let want = [
        [0.0, 0.0],
        [40.0, 0.0],
        [40.0, 4.0],
        [22.0, 4.0],
        [4.0, 22.0],
        [4.0, 30.0],
        [0.0, 30.0],
    ];
    for (p, w) in g.pts.iter().zip(want) {
        // The drawing is the solution, so the solve must not move it at
        // all: exact equality.
        assert_eq!(sol.point(*p), Some(w));
    }
}

#[test]
fn gusset_solves_from_a_rough_drawing_and_a_new_size() {
    // Drawn for the defaults, solved for other parameters: the customizer
    // case, where the drawing does not follow the parameters.
    for (leg_a, leg_b, t, gs) in [
        (80.0, 20.0, 2.0, 8.0),
        (20.0, 80.0, 8.0, 10.0),
        (60.0, 60.0, 6.0, 12.0),
    ] {
        let mut g = gusset(40.0, 30.0, 4.0, 18.0, true);
        // Rebuild with the new values but the old drawing.
        let mut s = Sketch::new();
        for e in g.s.entities() {
            match e {
                sketch_solver::Entity::Point { guess } => {
                    s.point(*guess).unwrap();
                }
                sketch_solver::Entity::Line { start, end } => {
                    s.line(*start, *end).unwrap();
                }
                _ => unreachable!(),
            }
        }
        for c in g.s.constraints() {
            let c = match c.clone() {
                Constraint::Length { line, value } => Constraint::Length {
                    line,
                    value: if value == 40.0 {
                        leg_a
                    } else if value == 30.0 {
                        leg_b
                    } else {
                        t
                    },
                },
                Constraint::Distance { a, b, along, .. } => Constraint::Distance {
                    a,
                    b,
                    value: t + gs,
                    along,
                },
                c => c,
            };
            s.add(c).unwrap();
        }
        g.s = s;
        let sol = g.s.solve();
        assert_eq!(sol.status, Status::Solved, "{leg_a} {leg_b} {t} {gs}");
        assert_eq!(sol.dof, 0);
        assert!(sol.flipped.is_empty(), "{:?}", sol.flipped);
        let want = [
            [0.0, 0.0],
            [leg_a, 0.0],
            [leg_a, t],
            [t + gs, t],
            [t, t + gs],
            [t, leg_b],
            [0.0, leg_b],
        ];
        for (p, w) in g.pts.iter().zip(want) {
            assert!(
                close(sol.point(*p).unwrap(), w, 1e-9),
                "{:?} vs {w:?}",
                sol.point(*p)
            );
        }
    }
}

#[test]
fn gusset_without_end_length_has_one_free_dof() {
    let g = gusset(40.0, 30.0, 4.0, 18.0, false);
    let sol = g.s.solve();
    assert_eq!(sol.status, Status::Solved);
    assert_eq!(sol.dof, 1);
    // b2 and g2 move together along x, and nothing else moves.
    let [_, _, _, _, g2, b2, _] = g.pts;
    let free: Vec<(EntityId, Coordinate)> =
        sol.free.iter().map(|f| (f.entity, f.coordinate)).collect();
    assert_eq!(free, vec![(g2, Coordinate::X), (b2, Coordinate::X)]);
    for f in &sol.free {
        assert!((f.mobility - 0.5f64.sqrt()).abs() < 1e-9);
    }
}

struct Slot {
    s: Sketch,
    c1: EntityId,
    top: [EntityId; 2],
    bot: [EntityId; 2],
    e1: EntityId,
    e2: EntityId,
}

/// Section 6.2: a slot with explicit tangent arcs, drawn for `drawn_w` and
/// dimensioned for `slot_w`.
fn slot(slot_len: f64, slot_w: f64, drawn_len: f64, drawn_w: f64) -> Slot {
    let mut s = Sketch::new();
    let c1 = s.point(Some([0.0, 0.0])).unwrap();
    let c2 = s.point(Some([drawn_len, 0.0])).unwrap();
    let axis = s.line(c1, c2).unwrap();
    s.set_construction(axis, true).unwrap();
    let ts = s.point(Some([0.0, drawn_w / 2.0])).unwrap();
    let te = s.point(Some([drawn_len, drawn_w / 2.0])).unwrap();
    let top = s.line(ts, te).unwrap();
    let bs = s.point(Some([drawn_len, -drawn_w / 2.0])).unwrap();
    let be = s.point(Some([0.0, -drawn_w / 2.0])).unwrap();
    let bot = s.line(bs, be).unwrap();
    let e1 = s.arc(c1, ts, be, false).unwrap();
    let e2 = s.arc(c2, bs, te, false).unwrap();
    for c in [
        Constraint::Fix {
            entity: c1,
            at: None,
        },
        Constraint::Horizontal(Pair::Line(axis)),
        Constraint::Length {
            line: axis,
            value: slot_len,
        },
        Constraint::Tangent(e1, top),
        Constraint::Tangent(e1, bot),
        Constraint::Tangent(e2, top),
        Constraint::Tangent(e2, bot),
        Constraint::Diameter {
            curve: e1,
            value: slot_w,
        },
        Constraint::Equal(e1, e2),
    ] {
        s.add(c).unwrap();
    }
    Slot {
        s,
        c1,
        top: [ts, te],
        bot: [bs, be],
        e1,
        e2,
    }
}

fn check_slot(sl: &Slot, len: f64, w: f64) {
    let sol = sl.s.solve();
    let ctx = format!("slot {len} x {w}: {sol:?}");
    assert_eq!(sol.status, Status::Solved, "{ctx}");
    assert_eq!((sol.unknowns, sol.equations, sol.dof), (12, 12, 0), "{ctx}");
    assert!(sol.flipped.is_empty(), "{ctx}");
    let h = w / 2.0;
    assert!(
        close(sol.point(sl.top[0]).unwrap(), [0.0, h], 1e-9),
        "{ctx}"
    );
    assert!(
        close(sol.point(sl.top[1]).unwrap(), [len, h], 1e-9),
        "{ctx}"
    );
    assert!(
        close(sol.point(sl.bot[0]).unwrap(), [len, -h], 1e-9),
        "{ctx}"
    );
    assert!(
        close(sol.point(sl.bot[1]).unwrap(), [0.0, -h], 1e-9),
        "{ctx}"
    );
    assert!((sol.radius(sl.e1).unwrap() - h).abs() < 1e-9);
    assert!((sol.radius(sl.e2).unwrap() - h).abs() < 1e-9);
    assert_eq!(sol.point(sl.c1), Some([0.0, 0.0]));
}

#[test]
fn slot_is_fully_constrained() {
    check_slot(&slot(30.0, 8.0, 30.0, 8.0), 30.0, 8.0);
}

#[test]
fn slot_does_not_flip_as_its_width_changes() {
    // The drawing follows the parameters (guesses written as expressions,
    // as in the design's example)...
    for w in [0.5, 1.0, 4.0, 8.0, 16.0, 29.0, 31.0, 60.0, 200.0] {
        check_slot(&slot(30.0, w, 30.0, w), 30.0, w);
    }
    // ...and stays put (a literal drawing while a customizer slider
    // moves), down to slots much narrower and much wider than drawn.
    for w in [0.5, 1.0, 2.0, 4.0, 7.9, 8.1, 16.0, 29.0, 31.0, 60.0, 200.0] {
        check_slot(&slot(30.0, w, 30.0, 8.0), 30.0, w);
    }
    for len in [1.0, 5.0, 100.0, 1000.0] {
        check_slot(&slot(len, 8.0, 30.0, 8.0), len, 8.0);
    }
}

#[test]
fn conflicting_dimensions_are_reported_against_the_earlier_one() {
    let mut s = Sketch::new();
    let a = s.point(Some([0.0, 0.0])).unwrap();
    let b = s.point(Some([30.0, 0.0])).unwrap();
    let l = s.line(a, b).unwrap();
    s.add(Constraint::Fix {
        entity: a,
        at: None,
    })
    .unwrap();
    s.add(Constraint::Horizontal(Pair::Line(l))).unwrap();
    let len = s
        .add(Constraint::Length {
            line: l,
            value: 30.0,
        })
        .unwrap();
    let dist = s
        .add(Constraint::Distance {
            a,
            b,
            value: 25.0,
            along: Along::Direct,
        })
        .unwrap();
    let sol = s.solve();
    assert_eq!(sol.status, Status::NotConverged);
    assert_eq!(sol.conflicts.len(), 1, "{sol:?}");
    assert_eq!(sol.conflicts[0].source, Source::Constraint(dist));
    assert!(
        sol.conflicts[0].with.contains(&Source::Constraint(len)),
        "{sol:?}"
    );
    assert!(sol.redundant.is_empty());
}

#[test]
fn redundant_constraints_are_reported_and_still_solve() {
    // A rectangle with a horizontal on both long sides, vertical on both
    // short ones, and one more vertical than it needs... via a parallel.
    let mut s = Sketch::new();
    let p = [[0.0, 0.0], [10.0, 0.5], [10.2, 5.0], [0.3, 5.1]].map(|g| s.point(Some(g)).unwrap());
    let l: Vec<EntityId> = (0..4)
        .map(|i| s.line(p[i], p[(i + 1) % 4]).unwrap())
        .collect();
    s.add(Constraint::Fix {
        entity: p[0],
        at: None,
    })
    .unwrap();
    s.add(Constraint::Horizontal(Pair::Line(l[0]))).unwrap();
    s.add(Constraint::Horizontal(Pair::Line(l[2]))).unwrap();
    s.add(Constraint::Vertical(Pair::Line(l[1]))).unwrap();
    s.add(Constraint::Vertical(Pair::Line(l[3]))).unwrap();
    let extra = s.add(Constraint::Parallel(l[1], l[3])).unwrap();
    s.add(Constraint::Length {
        line: l[0],
        value: 12.0,
    })
    .unwrap();
    s.add(Constraint::Length {
        line: l[1],
        value: 7.0,
    })
    .unwrap();
    let sol = s.solve();
    assert_eq!(sol.status, Status::Solved);
    assert_eq!(sol.dof, 0);
    assert_eq!(sol.redundant.len(), 1, "{sol:?}");
    assert_eq!(sol.redundant[0].source, Source::Constraint(extra));
    assert_eq!(sol.point(p[2]), Some([12.0, 7.0]));
}

#[test]
fn an_unfixed_sketch_keeps_three_rigid_dof_and_stays_where_drawn() {
    let mut s = Sketch::new();
    let p = [[1.0, 1.0], [5.0, 1.2], [3.0, 4.0]].map(|g| s.point(Some(g)).unwrap());
    let l: Vec<EntityId> = (0..3)
        .map(|i| s.line(p[i], p[(i + 1) % 3]).unwrap())
        .collect();
    for li in &l {
        s.add(Constraint::Length {
            line: *li,
            value: 4.0,
        })
        .unwrap();
    }
    let sol = s.solve();
    assert_eq!(sol.status, Status::Solved);
    assert_eq!(sol.dof, 3);
    // Equilateral, and its centroid has not wandered far.
    let q: Vec<[f64; 2]> = p.iter().map(|e| sol.point(*e).unwrap()).collect();
    let cx = (q[0][0] + q[1][0] + q[2][0]) / 3.0;
    let cy = (q[0][1] + q[1][1] + q[2][1]) / 3.0;
    assert!(
        (cx - 3.0).abs() < 0.2 && (cy - 2.0667).abs() < 0.2,
        "{cx} {cy}"
    );
}

#[test]
fn exactify_gives_equal_coordinates_the_same_bits() {
    let mut s = Sketch::new();
    let a = s.point(Some([0.1, 0.2])).unwrap();
    let b = s.point(Some([9.7, 0.3])).unwrap();
    let c = s.point(Some([9.9, 5.0])).unwrap();
    let d = s.point(Some([9.8, 5.1])).unwrap();
    let ab = s.line(a, b).unwrap();
    let bc = s.line(b, c).unwrap();
    s.add(Constraint::Fix {
        entity: a,
        at: Some([0.1, 0.2]),
    })
    .unwrap();
    s.add(Constraint::Horizontal(Pair::Line(ab))).unwrap();
    s.add(Constraint::Vertical(Pair::Line(bc))).unwrap();
    let ac = s.line(a, c).unwrap();
    s.add(Constraint::Angle {
        from: ab,
        to: ac,
        degrees: 33.0,
    })
    .unwrap();
    s.add(Constraint::Length {
        line: bc,
        value: 6.3,
    })
    .unwrap();
    s.add(Constraint::Coincident(c, d)).unwrap();
    let sol = s.solve();
    assert_eq!(sol.status, Status::Solved);
    let (pa, pb, pc, pd) = (
        sol.point(a).unwrap(),
        sol.point(b).unwrap(),
        sol.point(c).unwrap(),
        sol.point(d).unwrap(),
    );
    assert_eq!(pa, [0.1, 0.2]);
    assert_eq!(pb[1].to_bits(), pa[1].to_bits());
    assert_eq!(pc[0].to_bits(), pb[0].to_bits());
    assert_eq!(pc, pd);
}

#[test]
fn a_large_change_follows_the_drawn_branch() {
    // A triangle from three distances, drawn small and dimensioned large:
    // the mirror image is an equally good solution of the equations, and
    // the corner signature keeps the drawn one.
    for scale in [0.01, 10.0, 1000.0] {
        let mut s = Sketch::new();
        let p = [[0.0, 0.0], [4.0, 0.0], [1.0, 3.0]].map(|g| s.point(Some(g)).unwrap());
        let l: Vec<EntityId> = (0..3)
            .map(|i| s.line(p[i], p[(i + 1) % 3]).unwrap())
            .collect();
        s.add(Constraint::Fix {
            entity: p[0],
            at: None,
        })
        .unwrap();
        s.add(Constraint::Horizontal(Pair::Line(l[0]))).unwrap();
        s.add(Constraint::Length {
            line: l[0],
            value: 4.0 * scale,
        })
        .unwrap();
        s.add(Constraint::Length {
            line: l[1],
            value: 4.2 * scale,
        })
        .unwrap();
        s.add(Constraint::Length {
            line: l[2],
            value: 3.1 * scale,
        })
        .unwrap();
        let sol = s.solve();
        assert_eq!(sol.status, Status::Solved, "{scale}");
        assert!(sol.flipped.is_empty(), "{scale}: {:?}", sol.flipped);
        let top = sol.point(p[2]).unwrap();
        assert!(top[1] > 0.0, "{scale}: {top:?}");
        assert!(sol.point(p[1]).unwrap()[0] > 0.0);
    }
}

#[test]
fn unguessed_points_are_placed_and_solved() {
    let mut s = Sketch::new();
    let o = s.point(Some([0.0, 0.0])).unwrap();
    let a = s.point(None).unwrap();
    let b = s.point(None).unwrap();
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
    s.add(Constraint::Distance {
        a: o,
        b: a,
        value: 0.0,
        along: Along::Y,
    })
    .unwrap();
    let oa = s.line(o, a).unwrap();
    s.add(Constraint::Midpoint { point: b, line: oa }).unwrap();
    let sol = s.solve();
    assert_eq!(sol.placed, vec![a, b]);
    assert_eq!(sol.status, Status::Solved);
    assert_eq!(sol.dof, 0);
    assert!(close(sol.point(a).unwrap(), [10.0, 0.0], 1e-12));
    assert!(close(sol.point(b).unwrap(), [5.0, 0.0], 1e-12));
}

#[test]
fn tangent_circles_keep_inside_and_outside() {
    let mut s = Sketch::new();
    let c1 = s.point(Some([0.0, 0.0])).unwrap();
    let c2 = s.point(Some([3.0, 0.5])).unwrap();
    let c3 = s.point(Some([12.0, 0.0])).unwrap();
    let big = s.circle(c1, Some(10.0)).unwrap();
    let inner = s.circle(c2, Some(6.0)).unwrap();
    let outer = s.circle(c3, Some(3.0)).unwrap();
    s.add(Constraint::Fix {
        entity: c1,
        at: None,
    })
    .unwrap();
    s.add(Constraint::Radius {
        curve: big,
        value: 10.0,
    })
    .unwrap();
    s.add(Constraint::Radius {
        curve: inner,
        value: 4.0,
    })
    .unwrap();
    s.add(Constraint::Radius {
        curve: outer,
        value: 2.0,
    })
    .unwrap();
    s.add(Constraint::Tangent(big, inner)).unwrap();
    s.add(Constraint::Tangent(big, outer)).unwrap();
    let sol = s.solve();
    assert_eq!(sol.status, Status::Solved);
    let d = |p: EntityId| {
        let q = sol.point(p).unwrap();
        (q[0] * q[0] + q[1] * q[1]).sqrt()
    };
    assert!((d(c2) - 6.0).abs() < 1e-9, "{}", d(c2));
    assert!((d(c3) - 12.0).abs() < 1e-9, "{}", d(c3));
    // Each small circle can still roll around the big one.
    assert_eq!(sol.dof, 2);
}

#[test]
fn limits_and_interrupts() {
    let g = gusset(40.0, 30.0, 4.0, 18.0, true);
    let err =
        g.s.solve_with(&SolveOptions {
            max_unknowns: 10,
            ..SolveOptions::default()
        })
        .unwrap_err();
    assert_eq!(
        err,
        SolveError::TooManyUnknowns {
            unknowns: 14,
            limit: 10
        }
    );
    // The drawing is already solved, so an interrupt is never polled;
    // perturb it.
    let s = slot(30.0, 3.0, 30.0, 8.0).s;
    let stop = || true;
    let err = s
        .solve_with(&SolveOptions {
            interrupt: Some(&stop),
            ..SolveOptions::default()
        })
        .unwrap_err();
    assert_eq!(err, SolveError::Interrupted);
}

#[test]
fn a_sweep_constraint_sets_an_arc() {
    let mut s = Sketch::new();
    let c = s.point(Some([0.0, 0.0])).unwrap();
    let a = s.point(Some([5.0, 0.0])).unwrap();
    let b = s.point(Some([0.0, 5.0])).unwrap();
    let arc = s.arc(c, a, b, false).unwrap();
    s.add(Constraint::Fix {
        entity: c,
        at: None,
    })
    .unwrap();
    s.add(Constraint::Fix {
        entity: a,
        at: None,
    })
    .unwrap();
    s.add(Constraint::Sweep {
        arc,
        degrees: 270.0,
    })
    .unwrap();
    let sol = s.solve();
    assert_eq!(sol.status, Status::Solved);
    assert_eq!(sol.dof, 0);
    assert!(
        close(sol.point(b).unwrap(), [0.0, -5.0], 1e-9),
        "{:?}",
        sol.point(b)
    );
    assert!(sol.flipped.is_empty());
    let _ = Orientation::ArcSweep(arc);
}

/// What the README says about its example without the length, and with a
/// second, different one.
#[test]
fn readme_claims() {
    let build = |lengths: &[f64]| {
        let mut s = Sketch::new();
        let o = s.point(Some([0.0, 0.0])).unwrap();
        let a = s.point(Some([38.0, 1.0])).unwrap();
        let b = s.point(Some([39.0, 28.0])).unwrap();
        let base = s.line(o, a).unwrap();
        let side = s.line(a, b).unwrap();
        s.line(b, o).unwrap();
        s.add(Constraint::Fix {
            entity: o,
            at: None,
        })
        .unwrap();
        s.add(Constraint::Horizontal(Pair::Line(base))).unwrap();
        s.add(Constraint::Vertical(Pair::Line(side))).unwrap();
        let mut ids = Vec::new();
        for v in lengths {
            ids.push(
                s.add(Constraint::Length {
                    line: base,
                    value: *v,
                })
                .unwrap(),
            );
        }
        s.add(Constraint::Distance {
            a,
            b,
            value: 30.0,
            along: Along::Y,
        })
        .unwrap();
        (s, a, b, ids)
    };
    let (s, a, b, _) = build(&[]);
    let sol = s.solve();
    assert_eq!(sol.status, Status::Solved);
    assert_eq!(sol.dof, 1);
    let free: Vec<(EntityId, Coordinate)> =
        sol.free.iter().map(|f| (f.entity, f.coordinate)).collect();
    assert_eq!(free, vec![(a, Coordinate::X), (b, Coordinate::X)]);
    let (s, _, _, ids) = build(&[40.0, 41.0]);
    let sol = s.solve();
    assert_eq!(sol.status, Status::NotConverged);
    assert_eq!(sol.conflicts.len(), 1);
    assert_eq!(sol.conflicts[0].source, Source::Constraint(ids[1]));
    assert!(sol.conflicts[0].with.contains(&Source::Constraint(ids[0])));
}

/// Values that overflow must not come out as solved.
#[test]
fn overflow_is_not_a_solution() {
    let mut s = Sketch::new();
    let a = s.point(Some([0.0, 0.0])).unwrap();
    let b = s.point(Some([1e300, 1e300])).unwrap();
    let l = s.line(a, b).unwrap();
    s.add(Constraint::Fix {
        entity: a,
        at: None,
    })
    .unwrap();
    s.add(Constraint::Length {
        line: l,
        value: 1.7e308,
    })
    .unwrap();
    let sol = s.solve();
    assert!(
        sol.status == Status::NotConverged || sol.residual.is_finite(),
        "{sol:?}"
    );
}
