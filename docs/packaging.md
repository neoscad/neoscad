# Packaging NeoSCAD

Scope: `neoscad` (the CLI, with `serve`, `mcp` and `lsp`) on every
platform; the GUI on macOS only, for now. Checked on 2026-09-28 against
https://openscad.org/downloads.html, its release/snapshot file lists and
the registry APIs. Status updated 2026-09-29, when the "Now" items were
built (GitHub org `neoscad`, repository `github.com/neoscad/neoscad`,
website neoscad.org). How a release runs is in `docs/release.md`.

## What OpenSCAD ships

| Platform / mechanism | Release (2021.01) | Nightly |
|---|---|---|
| macOS DMG | x86_64 | Universal, macOS 11+ |
| Homebrew cask | `openscad` **disabled 2026-09-01 (fails Gatekeeper)** | `openscad@snapshot` |
| MacPorts | 2021.01 | – |
| Windows NSIS installer + zip | x86 32/64-bit | x86_64 only (Win 11); no ARM |
| winget | `OpenSCAD.OpenSCAD` | `OpenSCAD.Nightly` |
| Chocolatey / Scoop | community, 2021.01 | – |
| AppImage | x86_64, aarch64 | x86_64 (aarch64 stale since 2023.09) |
| Snap | `openscad` (amd64/arm64/ppc64el/s390x) | `openscad-nightly` |
| Flatpak (Flathub) | 2021.01 | flathub-beta |
| Distros (Debian, Fedora, Arch, openSUSE) | 2021.01 | OBS `home:t-paul` |
| AUR | – | `openscad-git`, snapshot AppImage |
| BSD | NetBSD, FreeBSD, OpenBSD | – |
| Nix | `openscad` | `openscad-unstable` |
| Docker Hub `openscad/openscad` | amd64 + arm64 | snapshot |
| WASM | – | web and node zips |
| Source tarball + GPG signatures | yes | GitHub |

## NeoSCAD's plan and status

"Built" means the files exist and were checked locally as the Status
column says; nothing has run on GitHub yet (it cannot before the owner
pushes), so every CI path is unverified until the first tag.

| Mechanism | Decision | Status |
|---|---|---|
| macOS DMG (app) + CLI tarball | Exists (`scripts/apple/release.sh`) | Now ships `LICENSE`, `NOTICE` and `licenses/` (fix 3). In CI as `.github/workflows/publish-macos-app.yml`, signed from `NEOSCAD_*` secrets. **Universal** (arm64 + x86_64, as OpenSCAD's nightly DMG), owner decision 2026-09-29: the app, its core and the CLI (`docs/release.md`, "Universal"); Debug builds stay arm64 |
| CLI archives, 6 targets | **Now** | Built: cargo-dist 0.33.0 (`[workspace.metadata.dist]`, `.github/workflows/release.yml`). macOS arm64/x86_64 as two archives: cargo-dist 0.33.0 cannot make a universal one (`universal2-apple-darwin` is a FIXME in its `cargo-dist/src/config/v1/mod.rs:416` and `src/tasks.rs:438`, "Lipo(LipoStep)"), so only the DMG's CLI is universal, Linux x86_64/aarch64 (glibc 2.28, manylinux_2_28 containers), Windows x86_64/aarch64; `.tar.xz`/`.zip` with `.sha256`. `dist plan` lists all of them; `dist build` of the Linux aarch64 archive ran in Docker |
| Shell / PowerShell installers | **Now** | Built (cargo-dist); install to `CARGO_HOME` |
| Homebrew tap: formula | **Now** | Built: cargo-dist generates `neoscad.rb` and pushes it to `neoscad/homebrew-tap` (needs `HOMEBREW_TAP_TOKEN`) |
| Homebrew tap: cask (app) | **Now**, after Developer ID | Template: `packaging/homebrew/neoscad-app.rb`, filled by `scripts/release/fill-manifests.sh`; add to the tap only once the DMG is notarized |
| Linux musl static tarball | **No** (owner decision 2026-09-29) | Not built. It would be portable, but PNG export needs Vulkan or GL loaded with `dlopen`, which a static musl binary cannot do; the glibc 2.28 archives already run on every current distribution |
| `.deb` / `.rpm` release assets | **Now** | Built: nfpm (`packaging/nfpm.yaml`, `scripts/release/linux-packages.sh`); aarch64 packages built in Docker, checked with `dpkg-deb --info`/`--contents` and `rpm -qip`/`-qlp`, installed and run in `debian:bookworm-slim` and `fedora:42`. In CI: `publish-packages.yml`, from the release's glibc 2.28 binaries |
| Signed apt/rpm repo | Later | Needs a GPG key and hosting (OpenSCAD uses OBS) |
| Official Debian/Fedora | Not now | Distro Rust is too old (1.96 vs 1.98) and every crate would need packaging |
| AppImage | Never | Adds nothing over a single binary |
| Flatpak | Never for a CLI | Flathub doesn't accept console software |
| Snap | Later/never | Strict confinement conflicts with MCP roots; reserve the name only |
| AUR `neoscad-bin` | Template now | `packaging/aur/PKGBUILD`, filled per release into `neoscad-package-manifests.tar.gz`; a maintainer pushes it to the AUR |
| Nix flake | Template now | `flake.nix` + `flake.lock` (`buildRustPackage` with rust-overlay's 1.98.1 from `rust-toolchain.toml`). Evaluated in `nixos/nix` (derivation `neoscad-0.1.0`); not built |
| Windows x86_64/aarch64 zip | **Now** | Built (cargo-dist), CI only (see "Needs the owner", `cargo xwin`) |
| MSI | **Now**, unsigned | Built (cargo-dist + `crates/cli/wix/main.wxs`, with the licence files hand-added; `allow-dirty = ["msi"]`). Adds `bin` to PATH. Unsigned: SmartScreen will warn |
| winget / Scoop | Templates now | `packaging/winget/` (portable zip, schema 1.10.0) and `packaging/scoop/neoscad.json`, filled per release; submitting needs the owner's fork (winget) or a bucket |
| Chocolatey | Never, unless asked | OpenSCAD isn't there officially |
| `cargo install` (crates.io) | Later, **blocked** | Fix 2. The name reservation is prepared at `packaging/crates-io-placeholder/` (`cargo package --list` checked; not published) |
| `cargo binstall` | After fix 2 | binstall starts from the crate's crates.io metadata, so the placeholder alone does not make it work (unverified) |
| npm (WASM) | With the web demo | Reserve `neoscad` and `@neoscad` |
| Docker (`ghcr.io`) | **Now** | `packaging/docker/Dockerfile` (debian-slim + Mesa lavapipe/llvmpipe, from the release archive). Built locally from a local binary (573 MB, most of it Mesa and LLVM) and rendered a PNG. In CI: `publish-packages.yml`, amd64 + arm64 |
| Source + `cargo vendor` tarballs | **Now** | `scripts/release/source-tarballs.sh`; an offline build from the two tarballs passed in a `--network none` container. cargo-dist also attaches a plain `source.tar.gz` |

## Toolchain and CI

`rust-toolchain.toml` pins 1.98.1 (with rustfmt, clippy and the wasm32
target: pinning installs a separate toolchain without `stable`'s targets,
and `scripts/wasm-check.sh` silently skips when wasm32 is missing), and
x86_64-apple-darwin, which the universal macOS app and CLI build on an
arm64 Mac.

| Workflow | Runs | What |
|---|---|---|
| `ci.yml` | PRs, pushes to main, and as the release's plan job | fmt, licence copies, `dist generate --check` and `dist plan`; clippy, tests and conformance on macOS arm64 (`macos-15`), Linux x86_64 (`ubuntu-22.04`) and aarch64 (`ubuntu-22.04-arm`); the Windows build and CLI/lang tests (`windows-2025`, including the user library path and `neoscad serve` on a named pipe); `wasm-check.sh` |
| `release.yml` | version tags | cargo-dist, generated: see `docs/release.md` |
| `publish-macos-app.yml` | called by `release.yml` | `release.sh` signed and notarized; attaches the DMG |
| `publish-packages.yml` | called by `release.yml` | `.deb`/`.rpm`, vendored source, filled manifests, the ghcr.io image |

cargo-dist was kept: its macOS signing is not used, and the app job fits
as a custom publish job, so nothing had to be hand-written around it.
All four files pass actionlint 1.7.12 (the only findings are shellcheck
style notes inside cargo-dist's generated steps).

**Conformance in CI.** macOS runs the whole suite, with the
`openscad@snapshot` cask drawing tier 3 (not the pinned 2026.09.23
nightly, so an upstream renderer change could move a few images). Linux
runs every tier but 3: tier 3 needs an OpenSCAD nightly to draw
neoscad's meshes, and there is none for Linux aarch64. The Docker runs
below show tier 3 would pass there.

**Local Linux proofs** (`scripts/release/linux-docker.sh`, Docker on this
Mac, 8 GB container cap): see the next section. There is no zig, cross,
xwin or nfpm on the host; nfpm, actionlint, dist and nix ran in
containers.

## Linux results (2026-09-29)

Built with Rust 1.98.1 in `rust:1.98.1-bookworm` (plus Mesa 22.3.6
lavapipe/llvmpipe and poppler), from this tree:

| | aarch64 (native) | x86_64 (Rosetta emulation) |
|---|---|---|
| `cargo build --release -p neoscad-cli -p neoscad-conformance` | pass (48 s) | pass (1 min 52 s) |
| `cargo test -p neoscad-cli` | 74 passed | not run |
| `cargo test --workspace --no-fail-fast` | 614 passed, 1 failed: `neoscad-ffi`'s `viewport::tests::an_unknown_scheme_is_refused` (since fixed: `Viewport::new` checks the scheme before it looks for a GPU) | not run |
| conformance, tiers 0-2, 4, 5 | 1,001 of 1,001 baseline passes | 1,001 of 1,001 |
| tier 3, mesh identical to macOS's | 734 of 772 | 724 of 772 |
| tier 3, differing mesh drawn by the macOS nightly | 38 of 38 pass | 48 of 48 pass |
| **total** | **1,773 of 1,773** (macOS: 1,773) | **1,773 of 1,773** |

How tier 3 was run: there is no OpenSCAD for Linux aarch64 to draw the
meshes, so `conformance --cached-renderer` stands in a stub that answers
`--version` as the macOS nightly does, and the harness's image cache
(keyed by the mesh bytes) was seeded from the macOS run. A mesh
byte-identical to macOS's therefore reuses macOS's image; any other fails
the stub. Each failing case's Linux mesh was then handed to the macOS
harness (a stand-in `--binary` that returns the Linux output) and drawn
by the real nightly: all passed.

Why the meshes differ:
- **3MF (32 cases on each architecture):** the archive carries a
  creation date, so its bytes differ on every run; macOS's own runs
  always redraw them.
- **Floating point (6 cases on aarch64, 16 on x86_64):** last digits
  differ. On aarch64 (`example020`, `example024`, `candleStand`,
  `minkowski3-difference-test` and both `fn_bug` exports), which shares
  macOS's architecture, the likely cause is the platform libm (glibc
  against Apple's), which Rust uses for trigonometry: in `example020` 8 of
  12,741 OFF lines differ, all near-zero coordinates such as
  `-1.54408e-10` against macOS's `-1.54394e-10`. x86_64 differs in
  `example020` too but in a mostly different set
  (`rotate_extrude-touch-vertex` in five formats, `sphere-tests`, `logo`,
  `CSG-modules` and others), and matches macOS on the other five aarch64
  cases, so code generation matters as well; the causes were not traced
  further. No case failed on its image.

Tier 4 (627 images drawn by neoscad itself) passed on lavapipe, so
Mesa's software Vulkan draws within the suite's tolerances. The x86_64
results are Rosetta's, which implements x86_64 floating point exactly but
is not a real x86_64 CPU; the CI x86_64 job is the check that counts.

**PNG export without a GPU** (`linux-docker.sh png-smoke`, and CI):
lavapipe (Vulkan) and, with no Vulkan driver, llvmpipe through EGL both
render; the image was byte-identical to the Metal one for the smoke
model. With neither, the error names both backends and Mesa's packages.
Mesa's Vulkan device-select layer prints "error: XDG_RUNTIME_DIR is
invalid or not set" on every device open when that variable is missing
(containers, services); the Docker images and CI set it.

## Portability fixes, in priority order

1. **Headless Linux PNG.** Done (`crates/cli/src/png.rs`,
   `open_offscreen`): off Apple, `Backends::PRIMARY`, then `Backends::GL`;
   the "no GPU adapter" error names Mesa's Vulkan and GL packages for
   Debian, Fedora and Arch. `crates/render/src/offscreen.rs` is
   unchanged: `Offscreen::new` already takes the backends. macOS still
   uses Metal alone.
2. **Make crates.io publishable.** Not done (unchanged plan):
   - `[patch.crates-io]` is ignored for published crates, so the patched
     manifold/clipper need renamed forks or upstream fixes;
   - `neoscad-assets` is `publish = false`;
   - path dependencies need versions;
   - rename the package to `neoscad`;
   - `build.rs` must tolerate a missing lockfile.
3. **Licences in every artifact.** Done: `LICENSE`, `NOTICE` (libtess2)
   and `licenses/` (Liberation OFL 1.1, MCAD LGPL 2.1, manifold-rust
   Apache 2.0, clipper2-rust Boost 1.0, and a README naming what each
   covers) in the cargo-dist archives (`include`), the MSI, the `.deb` and
   `.rpm` (`/usr/share/doc/neoscad`), the AUR package, the Docker image,
   the Nix package, `release.sh`'s tarball and its DMG. The copies live
   in `packaging/licenses/`; `scripts/release/licenses.sh --check` (run
   in CI) fails when they drift from `assets/` and `vendor/`.
4. **Windows user library path.** Done
   (`lang::loader::LibraryPath::user_dir`, which `--info` now uses too):
   `<Documents>\OpenSCAD\libraries`, as OpenSCAD's `userLibraryPath()` is
   `documentsPath()/OpenSCAD/libraries` (`src/platform/PlatformUtils.cc`,
   `userPath`) and Windows' `documentsPath()` is `CSIDL_PERSONAL` from
   `SHGetFolderPathW` (`src/platform/PlatformUtils-win.cc`). NeoSCAD asks
   for the same folder through the `dirs` crate's `FOLDERID_Documents`
   (CSIDL_PERSONAL's modern name) rather than `%USERPROFILE%\Documents`,
   which is wrong whenever Documents is redirected (OneDrive does this by
   default on Windows 11). Windows-only dependency; macOS and Linux paths
   are unchanged. The `~/.fonts` directory in `host.rs` stays on `HOME`
   everywhere, as OpenSCAD's `FontCache` reads `HOME` on every platform
   (`src/FontCache.cc`). Checked only by the CI Windows job.
5. **Serve sockets on Windows.** Done (owner decision 2026-09-29):
   `neoscad serve --socket` listens on a named pipe,
   `\\.\pipe\neoscad-<SID>` by default, and the command line hands
   exports to it as it does to a Unix socket
   (`crates/cli/src/transport.rs`; `docs/serve-protocol.md`,
   "Platforms"). The pipe is `interprocess` 2.4.4's synchronous listener
   (no async runtime); its security descriptor makes the user the owner
   and grants no one else access, and a client talks only to a pipe its
   own user owns (pipe names are global, so another user could create it
   first). That check needs Win32 calls no maintained crate wraps safely,
   so `neoscad-cli`'s `unsafe_code` lint is `deny` instead of the
   workspace's `forbid`, lifted only in the Windows-only
   `crates/cli/src/transport/win.rs`. Checked here with `cargo clippy
   --target x86_64-pc-windows-msvc -p neoscad-cli --all-targets
   --no-default-features --features bundled-assets -- -D warnings` in
   `rust:1.98.1` (clean); mimalloc's C build needs MSVC, hence the new
   default `mimalloc` feature that check turns off. Run only by CI's
   Windows job (unit tests, the served-output tests, a release-binary
   serve step).
6. **Output depends on the architecture and libm.** CI runs conformance
   on Linux x86_64 and aarch64; the local results are above.

Already fine: `-delay_framework` and the objc2 dev-dep are gated to Apple,
mimalloc to non-wasm, `nix` to Unix (with fallbacks), `OPENSCADPATH` uses
`;` on Windows, and Windows enumerates WARP (unverified).

## Needs the owner

Nothing here was pushed, published, signed up for or accepted.

- **Push** the repository to `github.com/neoscad/neoscad` and enable
  Actions; the first PR runs `ci.yml`, the first tag `release.yml`.
- **Create `neoscad/homebrew-tap`** and a token with write access to it,
  stored as the `HOMEBREW_TAP_TOKEN` secret.
- **Apple:** a Developer ID Application certificate and an App Store
  Connect API key, stored as the `NEOSCAD_*` secrets
  (`docs/release.md`); until then the app job uploads nothing.
- **Windows Authenticode** (SSL.com eSigner or Azure Artifact Signing;
  cargo-dist supports both) before promoting the MSI or submitting to
  winget.
- **Name reservations:** `cargo publish` in
  `packaging/crates-io-placeholder/` (prepared, `neoscad` 0.0.1); npm
  `neoscad`/`@neoscad`; Snap; Docker Hub. The ghcr.io package appears on
  the first release; make it public in the org's package settings.
- **The maintainer address** `packages@neoscad.org` in `packaging/nfpm.yaml`
  and the PKGBUILD is a placeholder: create it or change it. The MSI and
  formula credit "The NeoSCAD contributors" (`authors`).
- **Submissions** (each release): AUR `neoscad-bin` (an AUR account and
  SSH key), a winget-pkgs fork for PRs, a Scoop bucket, nixpkgs.
- **Consent to the Microsoft CRT/SDK licence** only if Windows builds
  should also happen locally (`cargo xwin`); CI does not need it.
- A GPG release key, if signed tarballs or apt/rpm repositories are
  wanted; OBS or self-hosted repositories.
- Decisions: publishing the kernel forks (fix 2). (Settled 2026-09-29:
  no musl build; a universal macOS app; named pipes on Windows.)

## Follow-ups

- Tier 3 on Linux in CI needs an OpenSCAD renderer there (an x86_64
  AppImage nightly, or the `openscad/openscad` image), or the image cache
  approach above made reproducible.
- The app bundle does not carry the third-party licences inside it
  (`apple/project.yml`); the DMG's `Licenses` folder does.
- The archives name the crates.io crates the binary contains only
  through `Cargo.lock`; a generated third-party notice file
  (cargo-about) would list each crate's licence text.
- The release binaries on Linux keep function names but not line tables
  (`[profile.dist] strip = "debuginfo"`, 87 MB to 21 MB); a separate
  debug-info artifact would restore file and line for crash reports.
- The Docker image is 573 MB, most of it Mesa's LLVM; a Vulkan-only or
  GL-only variant would be smaller.
- The universal CLI's x86_64 slice writes last-digit float differences
  from the arm64 slice (a small cube-minus-sphere-and-text STL: 2,019 of its lines differ),
  as Linux x86_64 does against aarch64 ("Linux results" above); both
  pass conformance.

## Not yet verified

Anything on GitHub's runners (all four workflows); Windows at all
(compile, tests, WARP, paths, MSI, named pipes); macOS x86_64 on an
Intel CPU (the universal CLI's x86_64 slice passed conformance, 1,773 of
1,773, under Rosetta only); the manylinux_2_28
builds and their glibc floor; the Nix build; Scoop
and Snap review rules.
