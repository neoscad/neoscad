//! The sketch solver's cross-platform determinism check: a fixed set of
//! generated sketches, solved, with every solved coordinate's bits (and
//! each diagnosis) folded into one digest. `cases.json` holds the digest,
//! so the native test on each CI platform (macOS arm64, Linux x86_64 and
//! aarch64) and the wasm32 build in node must all produce exactly it
//! (docs/language-extensions.md, section 9, "Determinism").
//!
//! A change to the solver that moves any bit changes the digest; the new
//! one goes into `cases.json` (`WASM_CHECK_PRINT=1 cargo test -p
//! neoscad-wasm-check -- --nocapture` prints it), after checking that the
//! change was meant.

use sketch_solver::{Along, Constraint, EntityId, Pair, Sketch, Status};

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [lo, hi).
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * ((self.next_u64() >> 11) as f64 / (1u64 << 53) as f64)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// FNV-1a, 64 bits.
struct Digest(u64);

impl Digest {
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }

    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
}

/// A drawn point: the truth plus noise.
fn drawn(rng: &mut Rng, p: [f64; 2], noise: f64) -> Option<[f64; 2]> {
    Some([
        p[0] + rng.range(-noise, noise),
        p[1] + rng.range(-noise, noise),
    ])
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((b[0] - a[0]) * (b[0] - a[0]) + (b[1] - a[1]) * (b[1] - a[1])).sqrt()
}

fn angle(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> f64 {
    let (u, v) = ([b[0] - a[0], b[1] - a[1]], [d[0] - c[0], d[1] - c[1]]);
    libm::atan2(u[0] * v[1] - u[1] * v[0], u[0] * v[0] + u[1] * v[1])
        * (180.0 / std::f64::consts::PI)
}

/// Sketch `i` of the set: a polygon closed by lengths and angles (up to 40
/// sides), a slot of tangent arcs, a rectangle with circles, or a chain of
/// tangent circles; with noise in the drawing, some points left unguessed,
/// and sometimes a constraint dropped, duplicated or contradicted.
fn sketch(i: u64, rng: &mut Rng) -> Sketch {
    let mut s = Sketch::new();
    let size = libm::exp(rng.range(-2.0, 7.0));
    let noise = size * [0.0, 0.01, 0.05, 0.15][rng.below(4) as usize];
    let add = |s: &mut Sketch, c| {
        s.add(c).expect("generated constraints are well formed");
    };
    let mut dims = Vec::new();
    match i % 4 {
        0 => {
            let n = 3 + rng.below(if i.is_multiple_of(20) { 38 } else { 6 }) as usize;
            // A star-shaped polygon with its first edge horizontal.
            let mut truth = Vec::new();
            for k in 0..n {
                let t = (k as f64 + rng.range(0.1, 0.9)) / n as f64 * std::f64::consts::TAU;
                let r = size * rng.range(0.5, 1.0);
                truth.push([r * libm::cos(t), r * libm::sin(t)]);
            }
            truth[1][1] = truth[0][1];
            let p: Vec<EntityId> = truth
                .iter()
                .enumerate()
                .map(|(k, t)| {
                    // Every seventh point is left for the solver to place.
                    let guess = if k % 7 == 6 {
                        None
                    } else {
                        drawn(rng, *t, noise)
                    };
                    s.point(if k == 0 { Some(*t) } else { guess }).unwrap()
                })
                .collect();
            let l: Vec<EntityId> = (0..n)
                .map(|k| s.line(p[k], p[(k + 1) % n]).unwrap())
                .collect();
            add(
                &mut s,
                Constraint::Fix {
                    entity: p[0],
                    at: None,
                },
            );
            add(&mut s, Constraint::Horizontal(Pair::Line(l[0])));
            for k in 0..n - 1 {
                dims.push(s.constraints().len());
                add(
                    &mut s,
                    Constraint::Length {
                        line: l[k],
                        value: dist(truth[k], truth[k + 1]),
                    },
                );
            }
            for k in 1..n - 1 {
                dims.push(s.constraints().len());
                add(
                    &mut s,
                    Constraint::Angle {
                        from: l[k - 1],
                        to: l[k],
                        degrees: angle(truth[k - 1], truth[k], truth[k], truth[k + 1]),
                    },
                );
            }
        }
        1 => {
            let (len, w) = (size * rng.range(0.5, 2.0), size * rng.range(0.05, 1.5));
            let c1 = s.point(Some([0.0, 0.0])).unwrap();
            let c2 = s.point(drawn(rng, [len, 0.0], noise)).unwrap();
            let axis = s.line(c1, c2).unwrap();
            s.set_construction(axis, true).unwrap();
            let ts = s.point(drawn(rng, [0.0, w / 2.0], noise)).unwrap();
            let te = s.point(drawn(rng, [len, w / 2.0], noise)).unwrap();
            let bs = s.point(drawn(rng, [len, -w / 2.0], noise)).unwrap();
            let be = s.point(drawn(rng, [0.0, -w / 2.0], noise)).unwrap();
            let top = s.line(ts, te).unwrap();
            let bot = s.line(bs, be).unwrap();
            let e1 = s.arc(c1, ts, be, false).unwrap();
            let e2 = s.arc(c2, bs, te, false).unwrap();
            add(
                &mut s,
                Constraint::Fix {
                    entity: c1,
                    at: None,
                },
            );
            add(&mut s, Constraint::Horizontal(Pair::Line(axis)));
            dims.push(s.constraints().len());
            add(
                &mut s,
                Constraint::Length {
                    line: axis,
                    value: len,
                },
            );
            for (a, l) in [(e1, top), (e1, bot), (e2, top), (e2, bot)] {
                add(&mut s, Constraint::Tangent(a, l));
            }
            dims.push(s.constraints().len());
            add(
                &mut s,
                Constraint::Diameter {
                    curve: e1,
                    value: w,
                },
            );
            add(&mut s, Constraint::Equal(e1, e2));
        }
        2 => {
            let (w, h) = (size * rng.range(0.2, 1.0), size * rng.range(0.2, 1.0));
            let t = [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]];
            let c: Vec<EntityId> = t
                .iter()
                .enumerate()
                .map(|(k, p)| {
                    s.point(if k == 0 {
                        Some(*p)
                    } else {
                        drawn(rng, *p, noise)
                    })
                    .unwrap()
                })
                .collect();
            let l: Vec<EntityId> = (0..4)
                .map(|k| s.line(c[k], c[(k + 1) % 4]).unwrap())
                .collect();
            add(
                &mut s,
                Constraint::Fix {
                    entity: c[0],
                    at: None,
                },
            );
            add(&mut s, Constraint::Horizontal(Pair::Line(l[0])));
            add(&mut s, Constraint::Vertical(Pair::Line(l[1])));
            add(&mut s, Constraint::Horizontal(Pair::Line(l[2])));
            add(&mut s, Constraint::Vertical(Pair::Line(l[3])));
            dims.push(s.constraints().len());
            add(
                &mut s,
                Constraint::Length {
                    line: l[0],
                    value: w,
                },
            );
            dims.push(s.constraints().len());
            add(
                &mut s,
                Constraint::Length {
                    line: l[1],
                    value: h,
                },
            );
            let diag = s.line(c[0], c[2]).unwrap();
            let m = s.point(None).unwrap();
            add(
                &mut s,
                Constraint::Midpoint {
                    point: m,
                    line: diag,
                },
            );
            let r = w.min(h) * rng.range(0.1, 0.4);
            let circ = s
                .circle(m, drawn(rng, [r, 0.0], noise * 0.1).map(|g| g[0].abs()))
                .unwrap();
            dims.push(s.constraints().len());
            add(
                &mut s,
                Constraint::Radius {
                    curve: circ,
                    value: r,
                },
            );
            // A second circle tangent to the first and to the bottom edge.
            let r2 = r * rng.range(0.2, 0.9);
            let k = s.point(drawn(rng, [w / 2.0 + r + r2, r2], noise)).unwrap();
            let c2 = s.circle(k, Some(r2)).unwrap();
            add(&mut s, Constraint::Tangent(circ, c2));
            add(&mut s, Constraint::Tangent(l[0], c2));
            dims.push(s.constraints().len());
            add(
                &mut s,
                Constraint::Radius {
                    curve: c2,
                    value: r2,
                },
            );
            let sym = s.point(drawn(rng, [w * 0.25, h * 0.75], noise)).unwrap();
            let twin = s.point(drawn(rng, [w * 0.75, h * 0.25], noise)).unwrap();
            add(
                &mut s,
                Constraint::Symmetric {
                    a: sym,
                    b: twin,
                    about: m,
                },
            );
            add(
                &mut s,
                Constraint::On {
                    point: sym,
                    curve: diag,
                },
            );
        }
        _ => {
            // A chain of circles, each tangent to the last, centres on a
            // horizontal line, and an arc swept through a set angle.
            let n = 2 + rng.below(6) as usize;
            let base = s.point(Some([0.0, 0.0])).unwrap();
            add(
                &mut s,
                Constraint::Fix {
                    entity: base,
                    at: None,
                },
            );
            let mut x = 0.0;
            let mut last: Option<(EntityId, EntityId)> = None;
            for k in 0..n {
                let r = size * rng.range(0.1, 0.5);
                let c = if k == 0 {
                    base
                } else {
                    s.point(drawn(rng, [x, 0.0], noise)).unwrap()
                };
                let circle = s.circle(c, Some(r * rng.range(0.8, 1.2))).unwrap();
                dims.push(s.constraints().len());
                add(
                    &mut s,
                    Constraint::Radius {
                        curve: circle,
                        value: r,
                    },
                );
                if let Some((prev, prev_c)) = last {
                    add(&mut s, Constraint::Tangent(prev, circle));
                    add(&mut s, Constraint::Horizontal(Pair::Points(prev_c, c)));
                }
                last = Some((circle, c));
                x += 2.0 * r;
            }
            let a0 = s.point(drawn(rng, [size, size], noise)).unwrap();
            let a1 = s.point(drawn(rng, [size * 2.0, size], noise)).unwrap();
            let a2 = s.point(drawn(rng, [size, size * 2.0], noise)).unwrap();
            let arc = s.arc(a0, a1, a2, rng.below(2) == 0).unwrap();
            add(
                &mut s,
                Constraint::Fix {
                    entity: a0,
                    at: Some([size, size]),
                },
            );
            add(
                &mut s,
                Constraint::Distance {
                    a: a0,
                    b: a1,
                    value: size,
                    along: Along::X,
                },
            );
            add(
                &mut s,
                Constraint::Distance {
                    a: a0,
                    b: a1,
                    value: 0.0,
                    along: Along::Y,
                },
            );
            dims.push(s.constraints().len());
            add(
                &mut s,
                Constraint::Sweep {
                    arc,
                    degrees: rng.range(10.0, 350.0),
                },
            );
        }
    }
    // A fifth of the sketches lose a dimension, a fifth gain a duplicate
    // and a fifth a contradiction.
    if !dims.is_empty() {
        let pick = dims[rng.below(dims.len() as u64) as usize];
        let c = s.constraints()[pick].clone();
        match rng.below(5) {
            0 => {
                let mut t = Sketch::new();
                for e in s.entities() {
                    match e {
                        sketch_solver::Entity::Point { guess } => t.point(*guess),
                        sketch_solver::Entity::Line { start, end } => t.line(*start, *end),
                        sketch_solver::Entity::Arc {
                            center,
                            start,
                            end,
                            clockwise,
                        } => t.arc(*center, *start, *end, *clockwise),
                        sketch_solver::Entity::Circle {
                            center,
                            radius_guess,
                        } => t.circle(*center, *radius_guess),
                    }
                    .unwrap();
                }
                for (k, c) in s.constraints().iter().enumerate() {
                    if k != pick {
                        t.add(c.clone()).unwrap();
                    }
                }
                s = t;
            }
            1 => add(&mut s, c),
            2 => add(&mut s, bump(c)),
            _ => {}
        }
    }
    s
}

/// The constraint with its value moved enough to contradict the original.
fn bump(c: Constraint) -> Constraint {
    match c {
        Constraint::Length { line, value } => Constraint::Length {
            line,
            value: value * 1.25 + 0.1,
        },
        Constraint::Radius { curve, value } => Constraint::Radius {
            curve,
            value: value * 1.25 + 0.1,
        },
        Constraint::Diameter { curve, value } => Constraint::Diameter {
            curve,
            value: value * 1.25 + 0.1,
        },
        Constraint::Angle { from, to, degrees } => Constraint::Angle {
            from,
            to,
            degrees: degrees + 20.0,
        },
        Constraint::Sweep { arc, degrees } => Constraint::Sweep {
            arc,
            degrees: degrees * 0.5,
        },
        c => c,
    }
}

/// Solve `count` generated sketches; one summary line with the digest.
pub fn run(count: u64) -> String {
    let mut rng = Rng(0x0005_EED0_F5E7_C4E5);
    let mut digest = Digest(0xCBF2_9CE4_8422_2325);
    let (mut solved, mut unknowns) = (0, 0);
    for i in 0..count {
        let s = sketch(i, &mut rng);
        let sol = s.solve();
        unknowns += sol.unknowns;
        if sol.status == Status::Solved {
            solved += 1;
        }
        for v in sol.values() {
            digest.u64(v.to_bits());
        }
        digest.u64(u64::from(sol.status == Status::Solved));
        digest.u64(sol.dof as u64);
        digest.u64(u64::from(sol.iterations));
        digest.u64(sol.residual.to_bits());
        digest.u64(sol.redundant.len() as u64);
        digest.u64(sol.conflicts.len() as u64);
        digest.u64(sol.flipped.len() as u64);
        digest.u64(u64::from(sol.continuation));
        for f in &sol.free {
            digest.u64(f.entity.index() as u64);
            digest.u64(f.mobility.to_bits());
        }
    }
    format!(
        "sketch: {count} sketches, {unknowns} unknowns, {solved} solved, digest {:016x}\n",
        digest.0
    )
}
