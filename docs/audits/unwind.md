# Audit: the cost of `panic = "unwind"` (at `970f630`)

This compares two scratch worktrees at `970f630` that differ only in
`[profile.release] panic`. Each had its own target directory. The runs
were interleaved A/B on an M4 Pro. Echo and STL output was byte-identical
between the builds.

## Findings

- **Unwind costs 5–7% again on evaluation-bound BOSL2 models, and 6–12%
  on call-heavy code.** A second session reproduced it within 0.5
  points. This contradicts `docs/audits/performance.md` §1.2/§4.3 ("0–1.5%
  at HEAD", measured at `df6731d`). The likely reason (not bisected):
  `bd4e4f0` removed the resource-limit hot-path cost that had masked this
  one.
- **Nothing measurable at cold start, and about 1% on geometry-bound
  runs.** The abort binary is 2.5 MB smaller (18.6 MB vs 16.1 MB).

| Measure | unwind | abort | Δ |
|---|---|---|---|
| fractal_tree (echo) | 3610 ms | 3387 ms | −6.2% |
| isosurface__006 (echo) | 820 ms | 768 ms | −6.4% |
| screws__001 (echo) | 144.6 ms | 136.6 ms | −5.5% |
| spring_handle (echo) | 151.6 ms | 144.0 ms | −5.0% |
| gears__003 (echo) | 41.9 ms | 40.8 ms | −2.4% |
| hero (echo) | 1657 ms | 1542 ms | −7.0% |
| hero (STL) | 2480 ms | 2358 ms | −4.9% |
| csg_spheres (STL) | 524 ms | 518 ms | −1.2% |
| fib(33) | 1255 ms | 1163 ms | −7.4% |
| 3,000 × row(1000) modules | 1647 ms | 1441 ms | −12.5% |
| serve hero evaluate / render | 1637 / 1949 ms | 1503 / 1833 ms | −8.1% / −6.0% |
| cold start | 3.0 ms | 3.1 ms | noise |

## Options

- **A: unwind everywhere (today).** It pays the cost above and protects
  the app (`ffi` `guarded`), serve, LSP and MCP from a panicking request.
- **B: abort everywhere, with auto-restart.** Not viable for the app as
  designed: the core runs in-process, so a panic would lose unsaved work.
  Servers would lose every in-flight request and warm cache, and no
  supervisor exists.
- **C: an abort CLI with unwind servers and app.** A custom profile
  (`[profile.release-abort] inherits = "release"`) can do it, but
  `serve`, `lsp` and `mcp` are subcommands of the same `neoscad` binary.
  So a clean C means shipping two builds. One-shot exports also hand off
  to a running server when there is one (`crates/cli/src/main.rs`).
- **D: keep unwind, and make the evaluator cheaper to unwind through.**
  In the unwind build, 17% of `eval_call`'s IR is cleanup. The invoke
  targets are 34 `panic_bounds_check`, 29 `Rc<Ctx>::drop_slow` and 28
  `drop_glue`.
  - **D1:** stop cloning and dropping `Rc<Ctx>` per call step (the owned
    `cur` clone, the double push in `simplify`/`eval_call`, the owned
    `defining` context). This is 30–50% of the gap on call-heavy code, so
    an estimated 2–3 points. It also helps abort builds.
  - **D2:** drop hot values through an `#[inline(never)] extern "C"`
    shim. Rust needs no landing pad to call one (verified on
    rustc 1.98.1). Estimated 1–3 points; it needs an A/B.
  - **D3:** hoist the per-step indexing into one borrowed `&Unit`. Under
    1 point on its own.
  - **D4:** an operand stack for intermediate `Value`s. Up to 3–6% on
    isosurface; the largest refactor.

## Recommendation

Keep A. Do D1, then D2, each measured with the same A/B method. C is a
product decision, and worth it only if one-shot and benchmark numbers
justify shipping two builds.

## Not verified

WASM under either setting; whether LSP and MCP clients restart a crashed
server; Cargo's custom-profile semantics (only `--profile` was checked
locally); `-Zpanic-in-drop=abort`; every D estimate (single-run profiles,
about ±10 ms per bucket).
