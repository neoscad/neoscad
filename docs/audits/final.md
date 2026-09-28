# Audit: final (v0.1 readiness)

Audited at `ef73578` (clean tree) on an Apple M4 Pro (14 cores, 48 GB),
macOS 27.0, against the reference nightly 2026.09.23
(`--backend=manifold`). All numbers are from that machine. Every run was
bounded (a 2 GB RSS watchdog on the MCP runs; `-j 8` on the suites).

## Verdict per dimension of the brief

| Dimension | Rating | Done | Partial or missing |
|---|---|---|---|
| Language, features and tests | **Done within scope** | 1,719/1,729 in-scope conformance cases; BOSL2 3597/3597 echo, 976/976 tests; MCAD and fonts bundled | 1,529 skips (1,309 CGAL-only by scope, 117 experimental, 63 upstream `Bugs`, 37 disabled upstream). 10 image fails (5 non-planar-quad tessellation, 3 `--view edges`, 2 `.nef3` import). GUI: no Animate panel, customizer partial. PythonSCAD deferred. |
| Fastest | **Done, with caveats** | ~2.8× on heavy models; 3.7× geometric mean over 14 models including startup; served part edits 0.25–0.3 s; BOSL2 eval-only suite 29.7 s vs 153.6 s | Slower than the nightly on deep unions, Menger level 4 and many `text()` nodes. Unwind costs 5–9% on evaluation. |
| Best UX for people and agents | **Partial** | MCP, serve, JSON everywhere, check/measure/snapshot, fix hints, `Limits::AGENT`, LSP | Agent eval is n=1 per cell (anecdote). No human user testing. The memory limit is an estimate (real RSS 2–4×). No LSP setup page. |
| macOS client first | **Partial** | App builds and 80 XCTests pass; Quick Look, panels, export, App Intents; release script | Not Developer ID signed or notarized (`spctl` rejects all artifacts). `dist/` was 29 commits stale. No person has looked at the app; clean-machine checklist unchecked; Shortcuts/Siri unverified; arm64 only. |
| WASM portability | **Done (app deferred)** | All library crates, `render` included, build for wasm32 and pass 18 node checks | Web app deferred (phase 9). The 46 MB wasm is unoptimised for the web. |

## On firm ground

| Check | Result |
|---|---|
| `conformance run -j 8` | **1,719 pass**, 10 fail, 1,529 skip (3,258 total); all baseline ids pass |
| `cargo test --no-fail-fast` | 545 passed, 0 failed, 2 ignored |
| `cargo fmt --check`, `clippy -D warnings` | clean |
| `scripts/wasm-check.sh` | 18/18 ok; wasm 46,265,248 bytes |
| BOSL2 echo diff vs nightly | **3597/3597** (BOSL2 at `9948313`) |
| `xcodebuild build` / `test` | succeeded (20 + 60 tests) |
| Editor grammar `npm test` | 19/19 |

- **Bench geometric means recompute exactly** from
  `progress/bench/20260928T130500Z-dca0882.json` (clean tree, best of 3):
  3.695× vs the nightly with Manifold (14 models), 27.731× vs the nightly
  with CGAL (12; fractal_tree and csg_spheres time out), 61.116× vs 2021.01
  (12). Every model's mesh agrees with the nightly (volume, area, bbox).
  `eval_only` is 976/976 for both binaries.
- **Hero caption** (2.09 s vs 5.85 s): re-timed by hand at 2.05 s vs
  5.72 s best, a ratio of 2.79×. Volumes agree (364,841.53).
- **Determinism:** fractal_tree, csg_spheres and isosurface__006 give the
  same STL SHA-256 on 5 runs at default threads, and at 1 thread.
- **Limits:** all 7 H4 memory repros stop through MCP with a resource-limit
  error (6 within 0.02 s). Depth-40 shared-tree `==`, `<` and `chr()` stop
  or finish immediately.

## Findings, by importance

1. **The 3.7× headline is correct arithmetic on mostly small,
   startup-bound models (high: public claim).** 7 of 14 models finish in
   under 0.13 s; cold start is 2.4 ms vs 45.5 ms. Subtracting cold start
   gives 2.77×; the four models over 0.3 s give 2.77×; the hero gives
   2.79×. Reference timings come from the cache (measured at `b1d6bd0`,
   binary SHA-pinned), not interleaved with the NeoSCAD runs. Headline
   "about 2.8× on heavy models (3.7× geometric mean over 14 models, wall
   time including startup, M4 Pro)"; avoid a bare "3.7×".
2. **"0.29 s re-render" needs its condition (high: public claim).** In a
   warm `serve`, an edit to the gearbox carrier re-renders in 0.25–0.27 s;
   an edit to the plinth takes 1.55 s, because the isosurface statement
   re-evaluates. Say "an edit to one part re-renders in 0.25–0.3 s".
3. **The 3597-file BOSL2 corpus can't be rebuilt from the repo (medium:
   reproducibility).** `examples_x` (2,554) and `tests_x` (976) are
   untracked in the BOSL2 clone, and no code generates them. Add a
   generator and cite it.
4. **`str()`/`echo()` of shared lists pass the string limit before they
   stop (medium: agent robustness).** `echo(str(t(40)))` with a shared
   tree passes 2 GB within 3.3 s under default limits through MCP. With
   `--limit memory=512M` it stops at about 1 GB. `print.rs` checks only the
   memory limit while writing; the 64 MiB string limit is applied after
   the text is built. `chr()` already stops in the walk (`f8c8971`).
5. **Docs contradicted HEAD (low).** The register/pure-frame port was
   still "in progress" in `architecture.md` and followups (it landed in
   `1ca570e` at 1.08–1.16×). There were several stale followups, and
   `launch-channels.md` quoted a superseded bench.
6. **Library crates touch the host file system outside `FileSystem` (low;
   WASM unaffected).** `lang/src/diag.rs` `weakly_canonical` calls
   `current_dir()` and `canonicalize()` for message paths, and
   `eval::Options::default()` uses `StdFs`. **Owner decision:** reword the
   CLAUDE.md rule, or route through `FileSystem`.

## Release readiness: blockers ranked

1. Developer ID signing and notarization have never run; Gatekeeper
   rejects every artifact.
2. No person has checked the app: the clean-machine checklist
   (`docs/release.md`) and the editor checks (IME, dead keys, VoiceOver,
   versions browser) are open.
3. The repo is private, with no README or homepage. Claims need a scope
   page carrying findings 1–3's conditions.
4. Maintainer outreach and naming (**owner decision**), a week before any
   post.
5. Rebuild `dist/` from a clean HEAD.
6. Make the BOSL2 corpus reproducible, and keep docs current.

## Top followups by user impact

1. Developer ID release path, then the clean-machine checklist.
2. A scripted 30-minute manual editor session before v0.1.
3. The memory limit is an estimate: add a host-side RSS probe, and make
   `str()`/`echo()` enforce the string limit while printing.
4. Kernel operations can't be cancelled: accept for v0.1 and document it.
5. Document-loop gaps (Animate panel, includers of unsaved buffers).
6. Slower than the nightly on deep unions, Menger 4 and many `text()`
   nodes: publish them on the benchmark page.
7. System fonts replaced by Liberation: have the host supply system font
   directories.
8. An LSP setup page (VS Code, Zed, Neovim).
9. Platform scope: macOS arm64 only (**owner decision**; a Linux CLI is
   the cheapest extension).
10. Unwind's 5–9% (**owner decision**; keep for v0.1).

## Code health

- **No VM code on `main`**; the prototype is on `mr/vm-spike`.
- **Unsafe:** the workspace forbids it. The exceptions are one block in
  `crates/ffi/src/layer.rs` and 5 `#[unsafe(no_mangle)]` wasm exports,
  both documented.
- **Tests and TODOs:** 2 opt-in `#[ignore]` tests, and 1 TODO (quoting
  upstream).
- **Dead code:** `render::overlay::small_axes_clip` is unused on wasm32.
- **Size:** about 102k lines of Rust and 7.6k of Swift. The largest files
  are `eval.rs` 2,395, `geom/evaluate.rs` 2,180 and `session/lib.rs` 1,946
  lines.

## Could not verify

Anything needing a person (app appearance, IME, VoiceOver), Gatekeeper on
another Mac, whether maintainer outreach happened, 120 Hz pacing, the MCP
2 s exit during a kernel operation, and outside claims.
