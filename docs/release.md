# Releases

How to cut a release of the macOS app and the `neoscad` command-line tool,
what the release script checks, and what still needs a person and another
Mac. Phase 8j (`docs/audits/macos-prep.md`). The command-line tool for
every platform, the Linux packages and the package-manager manifests come
from GitHub Actions on a version tag: see "The cross-platform release"
below and `docs/packaging.md`.

    scripts/apple/release.sh              # build, sign, package, verify, smoke test
    scripts/apple/release.sh --no-smoke   # the same without launching the app
    scripts/apple/smoke-release.sh DMG CLI [VERSION]   # the smoke test alone

The app and the CLI are universal, arm64 + x86_64, as OpenSCAD's macOS
DMG is ("Universal, macOS 11+" in `docs/packaging.md`): the Release
configuration builds `ARCHS = arm64 x86_64` (`apple/project.yml`),
`build-core.sh --universal` builds the Rust core for
`aarch64-apple-darwin` and `x86_64-apple-darwin` and joins them with
`lipo`, and the CLI is two cargo builds joined the same way. Debug builds
stay arm64 only, so everyday builds and `xcodebuild test` compile the core
once. The x86_64 target is pinned in `rust-toolchain.toml`.

## Artifacts

Everything goes to `dist/` (gitignored), which each run empties first:

| File | What |
|---|---|
| `NeoSCAD-<version>-<build>.dmg` | the app, an `Applications` link and a `Licenses` folder; HFS+, zlib |
| `neoscad-<version>-<build>-macos-universal.tar.gz` | the CLI (arm64 + x86_64), `LICENSE`, `NOTICE` and `licenses/` |
| `neoscad` | the same CLI, bare |
| `NeoSCAD-<version>-<build>-dSYMs.zip` | `NeoSCAD.app.dSYM`, `NeoSCADCore.framework.dSYM`, `neoscad.dSYM` |
| `BUILDINFO.txt` | version, commit, dirty flag, toolchains, signing mode, Gatekeeper verdicts |
| `SHA256SUMS` | `shasum -a 256` of all of the above (`shasum -a 256 -c SHA256SUMS`) |

**Versions.** `<version>` is `[workspace.package] version` in the root
`Cargo.toml`; it becomes `CFBundleShortVersionString` (passed to
`xcodebuild` as `MARKETING_VERSION`) and is what `neoscad --version`
prints (`env!("CARGO_PKG_VERSION")`, `crates/cli/src/main.rs:464`).
`<build>` is `git rev-list --count HEAD` and becomes `CFBundleVersion`
(`CURRENT_PROJECT_VERSION`), so it only grows along main. The script
checks both in the built `Info.plist` and the CLI's output. A tree with
uncommitted changes builds, with a warning, and `BUILDINFO.txt` says so.

## Cutting a release

1. Bump `version` in `Cargo.toml`'s `[workspace.package]` if it changes,
   and commit. Release from a clean tree on main.
2. Check at least 5 GB free (the script refuses less): the core's static
   library is ~300 MB and is copied into the XCFramework, and the archive
   and dSYMs add more.
3. Run, with the signing variables below exported:

       CARGO_TARGET_DIR=... scripts/apple/release.sh

4. Read the end of the output: the Gatekeeper verdicts, the smoke test and
   the artifact list. With Developer ID and a notary profile the script
   fails unless Gatekeeper accepts the notarized app.
5. Publish the DMG, the tarball and `SHA256SUMS`. Keep the dSYM zip (and
   `BUILDINFO.txt`) for every published build: it is the only way to
   symbolicate that build's crash reports, and it cannot be regenerated
   later (a rebuild gets new UUIDs).
6. Run the clean-machine checklist below on the published DMG.

## Environment

Nothing is prompted for or stored. The keychain holds the certificate and
the notary credentials; the script only names them.

| Variable | Effect |
|---|---|
| `NEOSCAD_SIGN_IDENTITY` | A **Developer ID Application** identity (name or SHA-1) in the keychain. Unset: ad-hoc signing. The script refuses any other kind (Apple Development, Apple Distribution), which would sign but fail notarization or Gatekeeper later. |
| `NEOSCAD_TEAM_ID` | Its team. Read from the identity's `(TEAMID)` if unset. |
| `NEOSCAD_NOTARY_PROFILE` | A `notarytool` keychain profile. With an identity: notarize and staple. Without one: an error. Checked with `notarytool history` before the build starts. |
| `CARGO_TARGET_DIR` | Honoured, by `build-core.sh` too. |

One-time owner setup, not done yet (no Developer ID identity exists on the
build Mac as of 8j; `security find-identity -v -p codesigning` lists two
Apple Development and one Apple Distribution identity, which the script
will not use):

1. Create a "Developer ID Application" certificate in the Apple Developer
   account and install it with its private key in the login keychain.
2. `xcrun notarytool store-credentials neoscad-notary --apple-id ...
   --team-id ...` (interactive, once; it asks for an app-specific password
   and keeps it in the keychain).
3. `export NEOSCAD_SIGN_IDENTITY="Developer ID Application: ..."
   NEOSCAD_NOTARY_PROFILE=neoscad-notary`.

## What the script does

1. **Core and editor.** `scripts/apple/build-core.sh --universal` (cargo
   release profile: thin LTO, `debug = "line-tables-only"`; both
   architectures, one universal static library in the XCFramework) and
   `scripts/apple/build-editor.sh`, then `xcodegen generate`.
2. **Archive.** `xcodebuild archive`, Release, in a fresh DerivedData
   under `apple/build/release` (the Swift side is always a clean build;
   cargo's fingerprints decide what Rust rebuilds).
3. **Sign.**
   - *Developer ID:* the archive is signed with the identity and a secure
     timestamp, then `xcodebuild -exportArchive` with a generated
     `ExportOptions.plist` (`method` `developer-id`, manual signing).
   - *Ad hoc:* the app is copied out of the archive and re-signed inside
     out (nested frameworks, dylibs, app extensions and XPC services
     first, keeping their entitlements; the app last with
     `apple/App/NeoSCAD.entitlements`), all with `--options runtime`.
     Ad-hoc signing adds one entitlement,
     `com.apple.security.cs.disable-library-validation`: under the
     hardened runtime, library validation only loads libraries of the
     process's own team, an ad-hoc signature has none, and the first
     ad-hoc release build died in dyld loading `NeoSCADCore.framework`
     ("mapping process and mapped file (non-platform) have different Team
     IDs"). Xcode's Debug builds never met this: they sign ad hoc without
     the runtime. A Developer ID build must not carry the exception; the
     script fails if it does.
4. **Verify** (both modes): `codesign --verify --deep --strict`; the
   hardened-runtime flag and both architectures (`lipo -verify_arch`,
   one architecture per call) on every Mach-O in the bundle; no
   `get-task-allow`; no dSYM inside the app; `Info.plist` versions; each
   dSYM's UUID equals its binary's. Then `spctl -a -vv -t exec`.
5. **Notarize** (with a profile): the app is zipped, submitted with
   `notarytool submit --wait` and stapled, so a copy dragged out of the
   DMG opens offline; then the DMG is signed, submitted and stapled;
   the CLI is submitted in a zip. A rejection prints Apple's log and
   fails the run.
6. **DMG**: `hdiutil create -srcfolder` of the app and an
   `/Applications` symlink, HFS+ and UDZO, then `hdiutil verify`. No
   background or icon layout: that needs Finder scripting, which asks for
   automation access. The steps are reproducible; the image's bytes are
   not (hdiutil writes times and a UUID), so compare builds by their
   contents, not their checksums.
7. **CLI**: `cargo build --release -p neoscad-cli` for
   `aarch64-apple-darwin` and `x86_64-apple-darwin` in the same clean
   environment as `build-core.sh` (shared dependencies,
   `MACOSX_DEPLOYMENT_TARGET=15.0`, and the developer's `target/release`
   untouched), joined with `lipo -create`; one dSYM whose DWARF file is
   the two architectures' joined likewise; `strip -x`; `--version` from
   each slice, the x86_64 one under Rosetta when it is installed (a skip,
   recorded in `BUILDINFO.txt`, when not); signed with the
   hardened runtime and the identifier `org.neoscad.neoscad`; tarball
   with root ownership and no AppleDouble files. The tarball and the DMG
   carry `LICENSE`, `NOTICE` and `licenses/` from
   `scripts/release/licenses.sh`: the embedded Liberation fonts (OFL),
   MCAD (LGPL) and the vendored kernels (Apache 2.0, Boost) require their
   notices to travel with the binary, and until September 2026 only
   `LICENSE` did. (The app bundle itself does not carry them yet; the
   DMG's `Licenses` folder sits beside it.)
8. **dSYM zip, `BUILDINFO.txt`, `SHA256SUMS`**, then the smoke test.

### Size

The app's size is almost all `NeoSCADCore.framework`, which links the
Rust core's static library. The core's static library is ~291 MB, but it
does not ship: its DWARF stays in the library's objects and reaches only
the dSYM, so the line tables cost nothing in the app and give crash
reports file and line.

What did matter was exports. The static library's ~14,000 Rust symbols
are global, a dylib exports every global symbol, and dead-code stripping
keeps whatever an export reaches. NeoSCADCore's Release configuration
now exports only Swift's `_$s*` symbols and its version symbols (the app
never calls the UniFFI C functions directly), and strips locals
(`STRIP_STYLE = non-global`) (`apple/project.yml`, NeoSCADCore
`configs: Release`). Measured on Release archives of the same commit:

| | before | after |
|---|---|---|
| NeoSCADCore binary | 22.6 MB (`__TEXT` 17.0 MB, `__LINKEDIT` 5.0 MB) | 13.8 MB (`__TEXT` 13.3 MB, `__LINKEDIT` 0.1 MB) |
| exported symbols | 15,510 | 1,501 |
| NeoSCAD.app | 23 MB | 14 MB |
| NeoSCADCore dSYM | 90 MB | 66 MB |

The Debug app is 28 MB. The CLI goes from 18.3 MB to 15.7 MB by
`strip -x`.

**Universal (2026-09-29).** An x86_64 slice beside every arm64 one
roughly doubles each binary. Measured on one ad-hoc `release.sh
--no-smoke` build (build 99), against the same build's arm64 slices
thinned out with `lipo -thin` (and a DMG made from them the same way):

| | arm64 | universal |
|---|---|---|
| NeoSCADCore binary | 14.3 MB | 29.7 MB (x86_64 15.4 MB) |
| NeoSCAD.app, unpacked | 18.6 MiB | 35.0 MiB |
| DMG | 11.6 MB | 19.8 MB |
| CLI, bare | 16.7 MB | 34.9 MB (x86_64 18.2 MB) |
| CLI tarball | 7.9 MB | 16.4 MB |
| dSYM zip | (not measured) | 79 MB |
| core static library in the XCFramework | 333 MB | 667 MB |

(Files in MB, bytes / 10^6; the app by `du -sk` and the NeoSCADCore
slices by `lipo -detailed_info`, in MiB.) The DMG grows by 8.1 MB (70%),
the CLI tarball by 8.5 MB. The build also needs a second Rust target
directory (`target/x86_64-apple-darwin`, 2.3 GB for the core and the
CLI).

### Debug symbols

No dSYM ships in the app or the tarball (the script fails if one is in
the app). The archive's dSYMs and the CLI's go into
`NeoSCAD-<version>-<build>-dSYMs.zip`. Symbolicate a user's crash report
with the zip of the same build:

    unzip NeoSCAD-0.1.0-37-dSYMs.zip -d syms
    atos -o syms/NeoSCADCore.framework.dSYM/Contents/Resources/DWARF/NeoSCADCore \
         -arch arm64 -l <load address> <address>   # -arch x86_64 for an Intel Mac's report

(or drop the dSYMs next to the `.ips` file and open it in Console or
Xcode). The stripped CLI's own panic backtraces lose function names; the
panic message and location are unaffected.

## The CLI, and MCP

    tar -xzf neoscad-0.1.0-99-macos-universal.tar.gz
    install -m 755 neoscad ~/.local/bin/     # or /usr/local/bin, or any dir on PATH
    neoscad --version
    claude mcp add neoscad -- neoscad mcp

See `docs/mcp.md` for roots and flags. A tarball downloaded by a browser
is quarantined; a Developer ID build is notarized and passes Gatekeeper
on first run (a bare Mach-O cannot hold a stapled ticket, so that first
check is online). An ad-hoc build runs only where it was built; elsewhere
`xattr -d com.apple.quarantine neoscad` is the workaround, and not one to
publish.

## The cross-platform release

`.github/workflows/release.yml` is generated by cargo-dist 0.33.0 from
`[workspace.metadata.dist]` in the root `Cargo.toml` (edit there, then run
`dist generate`; CI's lint job fails if the two disagree).

Windows is withheld from releases until the binaries can be
Authenticode-signed (owner decision 2026-09-29): no Windows zips, MSIs or
PowerShell installer, and no Scoop or winget manifests. CI still builds
and tests Windows on every PR and before every release. Restoring it is
the `installers` comment in `Cargo.toml`: the targets, installers and
`allow-dirty` go back, `dist generate` rewrites nothing else, and
`fill-manifests.sh` fills Scoop and winget again as soon as the release
has Windows checksums.

Pushing a tag such as `v0.1.0` runs, in order:

1. **plan**, and **CI** (`.github/workflows/ci.yml`, called as a plan
   job): nothing is built unless clippy, the tests and conformance pass
   on macOS arm64 and Linux x86_64 and aarch64, the Windows build and
   tests pass, and the WASM check passes.
2. **build**: `neoscad` for four targets with the `dist` profile
   (release, line tables stripped): macOS arm64 and x86_64 on `macos-15`;
   Linux x86_64 and aarch64 in `manylinux_2_28` containers (glibc 2.28)
   on `ubuntu-22.04` and `ubuntu-22.04-arm`. (With Windows restored, also
   Windows x86_64 on `windows-2025` and aarch64 on `windows-11-arm`.)
   Each archive holds the binary, `LICENSE`, `NOTICE` and `licenses/`,
   with a `.sha256` beside it.
3. **global**: the shell installer, the Homebrew formula, `sha256.sum`
   and the source tarball; GitHub artifact attestations. (With Windows
   restored, also the PowerShell installer and an MSI per Windows target.)
4. **host**: the GitHub Release.
5. **publish**: the `neoscad` formula pushed to `neoscad/homebrew-tap`;
   `publish-macos-app.yml` (this document's `release.sh` on `macos-15`,
   signed and notarized from the `NEOSCAD_*` secrets, and the DMG and
   dSYMs attached; with no secrets it builds ad hoc and uploads nothing);
   `publish-packages.yml` (`.deb` and `.rpm` for both Linux
   architectures, the vendored-dependency tarball, the filled AUR
   manifest (and Scoop and winget, once Windows is back) in
   `neoscad-package-manifests.tar.gz`, and the
   `ghcr.io/neoscad/neoscad` image).
6. **announce**.

A prerelease tag (`v0.2.0-beta.1`) skips the publish jobs.

| Secret | Used by |
|---|---|
| `HOMEBREW_TAP_TOKEN` | the formula push: a token with write access to `neoscad/homebrew-tap` |
| `NEOSCAD_DEVELOPER_ID_P12`, `NEOSCAD_DEVELOPER_ID_P12_PASSWORD` | the app job: the Developer ID Application certificate (base64 .p12) |
| `NEOSCAD_SIGN_IDENTITY`, `NEOSCAD_TEAM_ID` | the app job, as the local variables above |
| `NEOSCAD_NOTARY_KEY`, `NEOSCAD_NOTARY_KEY_ID`, `NEOSCAD_NOTARY_ISSUER` | the app job: an App Store Connect API key for `notarytool` |

Cutting one: bump `version`, commit, check that `dist plan` lists what you
expect, then `git tag v<version> && git push origin v<version>`. The
workflows have been linted (actionlint 1.7.12) and `dist plan` and a
local `dist build` of the Linux aarch64 archive have run, but no workflow
has run on GitHub yet: the first tag is their test.

**Locally**, the Linux half can be checked in Docker
(`packaging/docker/build.Dockerfile`: Rust 1.98.1 on Debian bookworm with
Mesa's software renderers):

    scripts/release/linux-docker.sh test                  # cargo test -p neoscad-cli
    scripts/release/linux-docker.sh conformance --cached-renderer
    scripts/release/linux-docker.sh png-smoke             # lavapipe, llvmpipe, no driver
    scripts/release/linux-docker.sh --platform linux/amd64 build   # emulated
    scripts/release/linux-packages.sh                     # .deb and .rpm in dist/linux
    scripts/release/source-tarballs.sh                    # dist/source
    scripts/release/fill-manifests.sh VERSION SUMS_DIR OUT_DIR

Windows builds happen only in CI: a local cross-build (`cargo xwin`)
would accept the Microsoft CRT and SDK licence, which is the owner's to
accept.

## Smoke test

`scripts/apple/smoke-release.sh`, run by the release script:

- mounts the DMG read-only and checks its layout and the mounted app's
  signature;
- launches the app from the mounted copy with `open -n -F` (a new
  instance, no restored windows) and `-ApplePersistenceIgnoreState YES`,
  opening a sample that exercises CSG, text with the bundled fonts and
  an MCAD include;
- after 10 s: the process is up, it has an on-screen window (counted
  through `CGWindowListCopyWindowInfo`, which needs no permission), idle
  footprint (`footprint`) and CPU, no error or fault from the
  `org.neoscad` subsystems in the unified log, no new crash report;
- quits it with SIGTERM (an Apple event would need automation access),
  unregisters the mounted copy from LaunchServices and detaches;
- runs the CLI from `dist/`: `--version`, a render to STL, and an MCP
  `initialize` whose reply must carry `serverInfo`.

It checks a window exists, not what it shows; the render's picture is
checked by the app tests and the conformance suite, not here.

## Not verified

- An Intel Mac. The universal CLI's x86_64 slice passed the conformance
  suite under Rosetta (1,773 of 1,773, `conformance run --binary` with a
  wrapper running `arch -x86_64 dist/neoscad`), but Rosetta is not an
  Intel CPU (the Linux x86_64 notes in `docs/packaging.md` say the same),
  its output differs from arm64's in last digits as Linux x86_64's does,
  and the app's x86_64 slice was only launched from the DMG under
  Rosetta (`arch -x86_64`; running and translated after 8 s, 58 MB
  footprint), not used.

- Everything under "The cross-platform release" on GitHub: the runners,
  the manylinux containers, the Windows and x86_64 macOS builds, the
  MSIs, the Homebrew push and the publish jobs.

- The Developer ID path: `-exportArchive`, notarization, stapling and an
  accepting Gatekeeper have never run, since no Developer ID identity or
  notary profile exists yet. The first signed run is its test.
- That `-exportArchive` accepts an archive whose Quick Look extensions
  (8h) are sandboxed without extra export options.
- The DMG and app on another Mac: see the checklist.

## Clean-machine checklist

On a Mac (or a fresh user account) that has never built NeoSCAD, with the
published files, downloaded through a browser so they are quarantined:

- [ ] `shasum -a 256 -c SHA256SUMS` passes.
- [ ] The DMG opens without a warning; the window shows NeoSCAD and
      Applications.
- [ ] Drag to Applications, eject the DMG, open NeoSCAD from Launchpad:
      no "cannot be opened" or "damaged" dialog; with the network off,
      too (stapled ticket).
- [ ] `spctl -a -vv /Applications/NeoSCAD.app` says
      `accepted source=Notarized Developer ID`.
- [ ] Double-click a `.scad` file in Finder: it opens in NeoSCAD; the
      preview renders; `text()` renders with the default font; an
      `include <MCAD/...>` resolves.
- [ ] Render (F6); export STL (once 8i lands); save, close, reopen.
- [ ] Quit and relaunch: windows restore; nothing about the build
      machine's paths appears.
- [ ] Activity Monitor: idle memory of one small document about 50-60 MB
      (54 MB on the build Mac), CPU near 0%.
- [ ] Console: no crash report for NeoSCAD.
- [ ] The CLI from the tarball: `neoscad --version`, a render to STL and
      PNG, then `claude mcp add neoscad -- neoscad mcp` and
      `claude mcp list` shows it connected.
- [ ] Quick Look (once 8h lands): space bar on a `.scad` file previews
      it; Finder shows thumbnails.
- [ ] Drag NeoSCAD to the Bin: nothing left running.
