//! Random, mostly nonsensical sketches: the solver must never panic, and
//! must either solve or say it did not. Bounded: at most 12 entities and
//! 16 constraints per sketch, so each solve is tiny.

use sketch_solver::{Along, Constraint, EntityId, Pair, Sketch, Status};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn f(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * ((self.next() >> 11) as f64 / (1u64 << 53) as f64)
    }

    fn pick(&mut self, v: &[EntityId]) -> EntityId {
        v[(self.next() % v.len() as u64) as usize]
    }
}

#[test]
fn random_sketches_never_panic() {
    let mut rng = Rng(42);
    let (mut solved, mut failed) = (0, 0);
    for _ in 0..400 {
        let mut s = Sketch::new();
        let scale = rng.f(0.001, 1000.0);
        let mut points = Vec::new();
        for _ in 0..(3 + rng.next() % 6) {
            // Some points coincide in the drawing, some have no guess.
            let g = match rng.next() % 6 {
                0 => None,
                1 => Some([0.0, 0.0]),
                _ => Some([rng.f(-scale, scale), rng.f(-scale, scale)]),
            };
            points.push(s.point(g).unwrap());
        }
        let mut all = points.clone();
        for _ in 0..(rng.next() % 5) {
            let (a, b, c) = (rng.pick(&points), rng.pick(&points), rng.pick(&points));
            let made = match rng.next() % 3 {
                0 => s.line(a, b),
                1 => s.arc(a, b, c, rng.next().is_multiple_of(2)),
                _ => s.circle(a, Some(rng.f(0.0, scale))),
            };
            // Degenerate requests (a line from a point to itself) are
            // refused, which is fine.
            if let Ok(id) = made {
                all.push(id);
            }
        }
        for _ in 0..(rng.next() % 17) {
            let (x, y, z) = (rng.pick(&all), rng.pick(&all), rng.pick(&all));
            let v = rng.f(-scale, scale);
            let c = match rng.next() % 17 {
                0 => Constraint::Coincident(x, y),
                1 => Constraint::On { point: x, curve: y },
                2 => Constraint::Horizontal(Pair::Line(x)),
                3 => Constraint::Vertical(Pair::Points(x, y)),
                4 => Constraint::Parallel(x, y),
                5 => Constraint::Perpendicular(x, y),
                6 => Constraint::Tangent(x, y),
                7 => Constraint::Distance {
                    a: x,
                    b: y,
                    value: v.abs(),
                    along: [Along::Direct, Along::X, Along::Y][(rng.next() % 3) as usize],
                },
                8 => Constraint::Length {
                    line: x,
                    value: v.abs(),
                },
                9 => Constraint::Radius {
                    curve: x,
                    value: v.abs(),
                },
                10 => Constraint::Diameter {
                    curve: x,
                    value: v.abs(),
                },
                11 => Constraint::Angle {
                    from: x,
                    to: y,
                    degrees: rng.f(-400.0, 400.0),
                },
                12 => Constraint::Sweep {
                    arc: x,
                    degrees: rng.f(-400.0, 400.0),
                },
                13 => Constraint::Equal(x, y),
                14 => Constraint::Midpoint { point: x, line: y },
                15 => Constraint::Symmetric {
                    a: x,
                    b: y,
                    about: z,
                },
                _ => Constraint::Fix {
                    entity: x,
                    at: (rng.next().is_multiple_of(2)).then_some([v, -v]),
                },
            };
            // Ill-typed constraints are refused, which is fine.
            let _ = s.add(c);
        }
        let sol = s.solve();
        match sol.status {
            Status::Solved => {
                solved += 1;
                assert!(sol.residual <= sol.tolerance);
                assert!(sol.values().iter().all(|v| v.is_finite()));
            }
            Status::NotConverged => failed += 1,
        }
        assert_eq!(sol.unknowns - sol.rank, sol.dof);
    }
    eprintln!("fuzz: {solved} solved, {failed} not");
    assert!(solved > 0 && failed > 0);
}
