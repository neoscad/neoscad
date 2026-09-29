# Packaging NeoSCAD

Scope: `neoscad` (the CLI, with `serve`, `mcp` and `lsp`) on every
platform; the GUI on macOS only, for now. Checked on 2026-09-28 against
https://openscad.org/downloads.html, its release/snapshot file lists and
the registry APIs.

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

## NeoSCAD's plan

| Mechanism | Decision | Why |
|---|---|---|
| macOS DMG (app) + CLI tarball | Exists (`scripts/apple/release.sh`) | arm64 only; add x86_64 or a universal CLI in CI |
| Homebrew tap (formula + cask) | **Now** | Own tap. homebrew-core needs a source build and popularity thresholds; a cask must pass Gatekeeper, so Developer ID comes first |
| Linux x86_64/aarch64 glibc tarball | **Now** | Single binary (fonts and MCAD embedded); target glibc 2.28; PNG needs a Vulkan driver (fix 1) |
| Linux musl static tarball | Later, optional | Portable, but PNG won't work (no dlopen); label it |
| `.deb` / `.rpm` release assets | **Now** | nfpm or cargo-deb/cargo-generate-rpm; recommend Mesa Vulkan drivers |
| Signed apt/rpm repo | Later | Needs a GPG key and hosting (OpenSCAD uses OBS) |
| Official Debian/Fedora | Not now | Distro Rust is too old (1.96 vs 1.98) and every crate would need packaging |
| AppImage | Never | Adds nothing over a single binary |
| Flatpak | Never for a CLI | Flathub doesn't accept console software |
| Snap | Later/never | Strict confinement conflicts with MCP roots; reserve the name only |
| AUR `neoscad-bin` | Later | A trivial PKGBUILD after the first public tag |
| Nix flake | Later | `buildRustPackage`, then nixpkgs |
| Windows x86_64/aarch64 zip | **Now** | Native runners exist; we'd beat OpenSCAD on ARM |
| MSI | Later | Useful for PATH, but needs Authenticode |
| winget / Scoop | Later | Portable-zip manifests; winget wants signing |
| Chocolatey | Never, unless asked | OpenSCAD isn't there officially |
| `cargo install` (crates.io) | Later, **blocked** | Fix 2 |
| `cargo binstall` | **Now, for free** | Works from cargo-dist's release naming |
| npm (WASM) | With the web demo | Reserve `neoscad` and `@neoscad` |
| Docker (`ghcr.io`) | Later, small | debian-slim + Mesa Vulkan, so PNG works headless |
| Source + `cargo vendor` tarballs | **Now** | For offline builds |

## Toolchain and CI

**On this Mac:**
- Rust 1.98.1, with the `aarch64-apple-darwin` and `wasm32` targets;
- Docker (linux/aarch64), gh, brew, node 18;
- no zig, cargo-zigbuild, cross, cargo-dist, nfpm or xwin;
- no `.github/` and no `rust-toolchain.toml` in the repo.

**CI (the owner enables it on push):** cargo-dist for the CLI across six
targets:

| Target | Runner |
|---|---|
| macOS arm64 and x86_64 | `macos-15` |
| Linux x86_64 | `ubuntu-22.04` |
| Linux aarch64 | `ubuntu-22.04-arm` |
| Windows x86_64 | `windows-2025` |
| Windows aarch64 | `windows-11-arm` |

- It produces archives, sha256, shell/PowerShell installers, a Homebrew
  formula, an MSI and attestations.
- A separate macOS job runs `release.sh` for the app and notarisation,
  since cargo-dist's macOS signing isn't there yet.
- A release is gated on per-target tests; on **conformance on Linux x86_64
  and aarch64** (FMA fusion is arm64-only, so x86_64 is a separate, so far
  untested output path); and on a lavapipe PNG smoke test.
- Pin `rust-toolchain.toml` to 1.98.1.

**Local compile proofs:** add the rustup targets, then `cargo zigbuild
--release -p neoscad-cli --target aarch64-unknown-linux-gnu.2.28` (and
x86_64), and `cargo xwin build --release -p neoscad-cli --target
x86_64-pc-windows-msvc`. Build only `-p neoscad-cli`: `ffi` and
`conformance` don't ship off macOS. Docker can run the aarch64 Linux
binary natively.

## Portability fixes, in priority order

1. **Headless Linux PNG** (`crates/cli/src/png.rs`,
   `crates/render/src/offscreen.rs`): off Apple, try `PRIMARY`, then `GL`,
   so Mesa llvmpipe works, and name Mesa Vulkan drivers in the "no GPU
   adapter" error.
2. **Make crates.io publishable:**
   - `[patch.crates-io]` is ignored for published crates, so the patched
     manifold/clipper need renamed forks or upstream fixes;
   - `neoscad-assets` is `publish = false`;
   - path dependencies need versions;
   - rename the package to `neoscad`;
   - `build.rs` must tolerate a missing lockfile.
3. **Licences in every artifact:** LICENSE, NOTICE (libtess2), the
   Liberation fonts' OFL, and MCAD's LGPL (`release.sh` ships only
   LICENSE).
4. **Windows user library path** should be Documents\OpenSCAD\libraries,
   as OpenSCAD does (`crates/lang/src/loader.rs`, `crates/cli/src/info.rs`,
   `crates/cli/src/host.rs` key off `HOME`).
5. **Serve sockets are Unix-only.** Stdio works everywhere and non-Unix
   gets a clean error. Named pipes are a later decision; document it.
6. **Output depends on the architecture:** add an x86_64 conformance job
   and document that x86_64 output matches x86_64 OpenSCAD.

Already fine: `-delay_framework` and the objc2 dev-dep are gated to Apple,
mimalloc to non-wasm, `nix` to Unix (with fallbacks), `OPENSCADPATH` uses
`;` on Windows, and Windows enumerates WARP.

## Needs the owner

- Apple Developer ID and a notarytool profile (a prerequisite for the
  cask).
- Windows Authenticode: SSL.com eSigner or Azure Artifact Signing (costs
  money and identity checks).
- A GPG release key; OBS or self-hosted repos.
- Name reservations (all free on 2026-09-28): crates.io `neoscad`, npm
  `neoscad`/`@neoscad`, Homebrew, Snap, the Docker Hub namespace, the
  GitHub org `neoscad`.
- A `homebrew-tap` repo and token; a GitHub fork for winget-pkgs PRs.
- Consent to the Microsoft CRT/SDK licence that `cargo xwin` accepts.
- Decisions:
  - cargo-dist or a hand-written matrix;
  - musl without PNG;
  - universal or x86_64 macOS app;
  - Windows named pipes;
  - publishing the kernel forks.

## Not yet verified

Anything compiling for Linux or Windows; musl dlopen behaviour; WARP and
lavapipe actually rendering; Windows path handling; Scoop and Snap review
rules.
