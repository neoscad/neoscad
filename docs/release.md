# Releases

How to cut a release of the macOS app and the `neoscad` command-line tool,
what the release script checks, and what still needs a person and another
Mac. Phase 8j (`docs/audits/macos-prep.md`). The command-line tool for
every platform, the Linux packages and the package-manager manifests come
from GitHub Actions on a version tag: see "The cross-platform release"
below and `docs/packaging.md`.

    scripts/apple/release.sh              # build, sign, package, verify, smoke test
    scripts/apple/release.sh --no-smoke   # the same without launching the app
    scripts/apple/release.sh --no-wait    # submit the app to Apple and stop (CI's first stage)
    scripts/apple/release.sh --pgo        # with the core and the CLI profile-guided (CI passes it; "PGO builds")
    scripts/apple/release.sh --staple-app DIR   # once Accepted: staple, make and submit the DMG
    scripts/apple/release.sh --staple-dmg DIR   # once Accepted: staple and check the DMG
    scripts/apple/smoke-release.sh DMG CLI [VERSION]   # the smoke test alone
    scripts/apple/test-updates.sh [DIR]   # the updater end to end: two builds, a local appcast

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
| `NeoSCAD-<version>-<build>.dmg` | the app (with the CLI in `Contents/Helpers`), an `Applications` link and a `Licenses` folder; HFS+, zlib |
| `neoscad-<version>-<build>-macos-universal.tar.gz` | the CLI (arm64 + x86_64), `LICENSE`, `NOTICE` and `licenses/` |
| `neoscad` | the same CLI, bare |
| `NeoSCAD-<version>-<build>-dSYMs.zip` | `NeoSCAD.app.dSYM`, `NeoSCADCore.framework.dSYM`, `neoscad.dSYM` |
| `BUILDINFO.txt` | version, commit, dirty flag, toolchains, signing mode, Gatekeeper verdicts |
| `SHA256SUMS` | `shasum -a 256` of all of the above (`shasum -a 256 -c SHA256SUMS`) |

**Versions.** `<version>` is `[workspace.package] version` in the root
`Cargo.toml`; it becomes `CFBundleShortVersionString` (passed to
`xcodebuild` as `MARKETING_VERSION`) and is what `neoscad --version`
prints (`env!("CARGO_PKG_VERSION")`, `crates/cli/src/main.rs:464`).
`CFBundleShortVersionString` must be numeric `x.y.z`, so a prerelease
such as `0.1.0-rc.1` goes there as `0.1.0`; the DMG's file and volume
names, and the About panel's "NeoSCAD core" line, keep the full version.
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
| `NEOSCAD_TEST_SPARKLE_PUBLIC_KEY`, `NEOSCAD_TEST_SPARKLE_FEED_URL`, `NEOSCAD_TEST_BUILD_NUMBER` | For `scripts/apple/test-updates.sh` only: a throwaway update key, a local appcast and a chosen `CFBundleVersion`. Refused with `NEOSCAD_SIGN_IDENTITY` set, so no signed build carries them ("The macOS app's updates" below). |

A notarized build (`NEOSCAD_NOTARY_PROFILE` set) also needs the update key,
`NEOSCAD_SPARKLE_PUBLIC_KEY` in `apple/project.yml`. The script refuses to
start without it: an app published without a key could never update itself
in place.

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

1. **Core, CLI and editor.** `scripts/apple/build-cli.sh --universal`
   (the CLI the app carries; "The bundled command-line tool" below),
   `scripts/apple/build-core.sh --universal` (cargo
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
     Sparkle's helpers are then signed again explicitly (below) and the
     app after them.
   - *Ad hoc:* the app is copied out of the archive and re-signed inside
     out (nested frameworks, dylibs, app extensions and XPC services
     first, keeping their entitlements; the app last with
     `apple/App/NeoSCAD.entitlements`), all with `--options runtime`.
     Sparkle's framework is signed first, in the order Sparkle documents:
     `Installer.xpc`, `Downloader.xpc` (keeping its entitlements), the
     bare `Autoupdate` executable, `Updater.app`, then the framework.
     `Autoupdate` is neither a bundle nor a dylib, so the generic loop
     would leave it with the ad-hoc signature Sparkle ships it with, and
     notarization refuses that. Never `--deep`.
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
   `get-task-allow`; no dSYM inside the app; `Info.plist` versions and
   `SUPublicEDKey`; each dSYM's UUID equals its binary's. With Developer
   ID, every Mach-O must carry the team's signature (`TeamIdentifier`),
   which catches a Sparkle helper left signed ad hoc. Then
   `spctl -a -vv -t exec`.
5. **Notarize** (with a profile): the app is zipped, submitted with
   `notarytool submit --wait` and stapled, so a copy dragged out of the
   DMG opens offline; then the DMG is signed, submitted and stapled;
   the CLI is submitted in a zip. A rejection prints Apple's log and
   fails the run. With `--no-wait` it submits only the app and stops,
   and `--staple-app` and `--staple-dmg` pick up from there. That is
   how CI runs it ("The macOS app after the release" below).
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
   The app's own copy, `Contents/Helpers/neoscad`, must have the same
   UUIDs as this CLI, so the one `neoscad.dSYM` serves both and a stale
   bundled CLI fails the run.
8. **dSYM zip, `BUILDINFO.txt`, `SHA256SUMS`**, then the smoke test,
   which also runs the bundled CLI from the mounted DMG (`--version`,
   then an MCP `initialize` and `tools/list`) and checks that the app,
   launched from the read-only DMG, made no link to it.

### The bundled command-line tool

The app carries the universal `neoscad` CLI as
`NeoSCAD.app/Contents/Helpers/neoscad`, for AI agent clients
(`docs/mcp.md`, "Setup from the apps"). It is not on `PATH`, and the cask
has no `binary` stanza, so it does not conflict with the `neoscad`
formula (`packaging/homebrew/neoscad-app.rb`).

- **Where.** `Contents/Helpers`, the place for helper tools in a bundle,
  not `Contents/MacOS`: APFS is case-insensitive by default, and
  `Contents/MacOS/neoscad` would be the app's executable, `NeoSCAD`.
- **Build.** The `CommandLineTool` aggregate target runs
  `scripts/apple/build-cli.sh` (cargo for Xcode's `ARCHS`, lipo, `strip
  -x`, into `apple/build/cli/neoscad`), with the core's input file list
  so it is skipped when no Rust input changed. The app target's "Embed
  command-line tool" phase copies it into `Contents/Helpers` and signs it
  there with the build's identity, the hardened runtime and the
  identifier `org.neoscad.neoscad`, before Xcode signs the app: the
  app's signature seals its nested code and refuses an unsigned
  executable. Debug builds carry it too (arm64), which is what the hosted
  app tests run.
- **Signing.** `release.sh` signs it again (`sign_helper`) with the
  release identity, `--options runtime`, a secure timestamp with
  Developer ID, and no entitlements, after Sparkle's helpers and before
  the app, in both the ad-hoc and the Developer ID path (an export's
  signature is not relied on, as for Sparkle). It needs no entitlement:
  it loads no libraries and runs no JIT, and the ad-hoc app's
  library-validation exception stays the app's alone (the script fails
  if the CLI carries any entitlement). The verification's Mach-O loop
  covers it like any other: hardened runtime, both architectures, and
  with Developer ID the team. It is notarized inside the app's zip and
  covered by the app's stapled ticket.
- **The link.** At each launch the app points
  `~/Library/Application Support/NeoSCAD/bin/neoscad` at its own copy,
  and agent configs name that link, so they survive moving the app;
  Sparkle replaces the bundle in place, so updates keep the path anyway
  (`apple/App/Agents/CommandLineTool.swift`). Launched from the DMG, from
  App Translocation or under the tests, the app leaves the link alone.
  `-NeoSCADToolLinkDirectory DIR` as a launch argument moves it, which is
  how a local build is checked without touching the real one:

      open -n -a /path/to/NeoSCAD.app --args -NeoSCADToolLinkDirectory /tmp/link-check
      /tmp/link-check/neoscad --version

- **Size.** Measured on one ad-hoc `release.sh` build (0.3.1, build
  251, 2026-10-02), against a DMG made the same way from the same app
  with `Contents/Helpers` removed:

  | | without the CLI | with it |
  |---|---|---|
  | NeoSCAD.app, unpacked (`du -sk`) | 42.5 MB | 80.9 MB |
  | DMG | 21.8 MB | 39.7 MB (+17.8 MB, +82%) |

  The CLI itself is 38.4 MB (universal, `strip -x`), the same binary as
  the bare `neoscad` in `dist/`. Its tarball is 17.9 MB, so the DMG now
  costs about what the DMG and the tarball did together. A Debug app
  carries an arm64-only copy (18.3 MB).

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
`dist generate`; CI's lint job fails if the two disagree). Pushing a tag
such as `v0.1.0` runs, in order:

1. **plan**, and **CI** (`.github/workflows/ci.yml`, called as a plan
   job): nothing is built unless clippy, the tests and conformance pass
   on macOS arm64 and Linux x86_64 and aarch64, the Windows build and
   tests pass, and the WASM check passes.
2. **build**: `neoscad` for six targets with the `dist` profile (release,
   line tables stripped): macOS arm64 and x86_64 on `macos-15`; Linux
   x86_64 and aarch64 in `manylinux_2_28` containers (glibc 2.28) on
   `ubuntu-22.04` and `ubuntu-22.04-arm`; Windows x86_64 on `windows-2025`
   and aarch64 on `windows-11-arm`. Four of them are profile-guided
   builds, checked by the recursion-depth guard before packaging ("PGO
   builds" below). Each archive holds the binary,
   `LICENSE`, `NOTICE` and `licenses/`, with a `.sha256` beside it.
3. **global**: shell and PowerShell installers, the Homebrew formula,
   `sha256.sum` and the source tarball; MSIs for both Windows targets
   (unsigned; built by WiX 3.14.1, which `windows-2025` has preinstalled
   and `.github/build-setup.yml` installs on `windows-11-arm`, whose image
   has none); GitHub artifact attestations.
4. **host**: the GitHub Release.
5. **publish**: the `neoscad` formula pushed to `neoscad/homebrew-tap`;
   `publish-macos-app.yml` (this document's `release.sh --pgo --no-wait` on
   `macos-26` with Xcode 26.6, signed from the `NEOSCAD_*` secrets: it
   builds and smoke-tests the app, submits it to Apple without waiting
   and records the submission on the release, which stays a prerelease
   until `macos-notarize.yml` (which it starts, and which then restarts
   itself about every 15 minutes) has attached the notarized DMG
   and pushed the cask; see "The macOS app after the release" below;
   with no secrets it builds ad hoc and uploads and holds nothing);
   `publish-packages.yml` (`.deb` and `.rpm` for both Linux
   architectures, with the man page and shell completions the x86_64
   binary generates, installed with apt or dnf and run in Debian 10 and
   12, Ubuntu 22.04 and 24.04, Rocky Linux 8 and Fedora on an x86_64 and
   an arm64 runner, and attached only if all twelve pass; the
   vendored-dependency tarball, the filled AUR and Scoop manifests in
   `neoscad-package-manifests.tar.gz` (winget's come from
   `windows-installer.yml`, below), the Scoop manifest
   also pushed to the bucket `neoscad/scoop-bucket` as
   `bucket/neoscad.json`, and the `ghcr.io/neoscad/neoscad` image; the
   bench kit `neoscad-bench-kit-<version>.tar.gz` with its `.sha256`, and
   `neoscad-executables.sha256sums`, the SHA-256 of the `neoscad`
   executable in every archive, which `neoscad bench` checks itself
   against; then the release baseline, the release's own binaries
   benchmarked on four runners and committed to `neoscad/benchmarks`,
   never failing the release: `docs/community-bench.md`);
   `flatpak.yml` (the Linux app, `docs/linux-app.md` "Flatpak"):
   `NeoSCAD-<version>-linux-x86_64.flatpak` and
   `NeoSCAD-<version>-linux-aarch64.flatpak`, each with a `.sha256` and
   an artifact attestation, single-file bundles built in Flathub's
   `gnome-51` image on `ubuntu-24.04` and natively on `ubuntu-24.04-arm`,
   whose runtime repository is Flathub, so `flatpak install --user
   <file>` pulls `org.gnome.Platform` 51 from there. The x86_64 build
   blocks the release; the aarch64 one is `continue-on-error` until it
   has passed, and a release without it ships x86_64 alone with a
   warning. The job dates the metainfo's `<release>` for the version the
   day it builds, and fails a stable release that has none;
   `windows-installer.yml` (the Windows app's MSIs,
   `NeoSCAD-<version>-windows-x64.msi` on `windows-2025` and
   `NeoSCAD-<version>-windows-arm64.msi` on `windows-11-arm`, each with
   its `.sha256`: built by `scripts/windows/build-msi.ps1`
   (`docs/windows-app.md`, "Installer"), installed silently, checked,
   launched, opened a `.scad` through the association, uninstalled and
   checked again, and only then attested and attached; unsigned, like
   the CLI's MSIs from step 3, which are a separate installer; then its
   `winget` job reads both MSIs back from the release, fills the winget
   manifests from them with `fill-manifests.sh --winget`, which needs
   each MSI's ProductCode, and attaches
   `neoscad-winget-manifests.tar.gz`).
6. **announce**.
7. **update feed** (`update-feed.yml`, a cargo-dist post-announce job):
   the signed `stable.json` and `rc.json` on neoscad.org, from the
   release's assets ("The update feed" below), and the macOS app's
   Sparkle appcast ("The macOS app's updates"). It runs again from
   `macos-notarize.yml` once the DMG is attached.

A prerelease tag (`v0.2.0-beta.1`) makes a GitHub prerelease and skips
the publish jobs, unless `publish-prereleases = true` is set in
`[workspace.metadata.dist]` (set only for the `v0.1.0-rc.N` rehearsals,
then removed). With it set, a prerelease publishes everything:
the tap's `neoscad` formula and `neoscad-app` cask move to it (Homebrew
has no prerelease channel for either: a tap holds one version of each,
and `brew upgrade` takes whatever it holds), as does the Scoop bucket's
`neoscad` (one manifest per app, likewise), and the image is pushed as `:<version>` but not `:latest`.
The `.deb` and `.rpm` carry `0.1.0~rc.1` so they sort before `0.1.0`
(nfpm does this), and the AUR `pkgver` drops the hyphen (`0.1.0rc.1`).

| Secret | Used by |
|---|---|
| `HOMEBREW_TAP_TOKEN` | the formula and cask pushes: a token with write access to `neoscad/homebrew-tap` (the cask is pushed by `macos-notarize.yml`, which reads repository secrets directly) |
| `SCOOP_BUCKET_TOKEN` | `publish-packages.yml`'s `scoop` job, which pushes `bucket/neoscad.json` to `neoscad/scoop-bucket`: a fine-grained token, resource owner `neoscad`, only that repository, Contents read and write (through `secrets: inherit`, as above). Without it the job warns and the bucket stays on the previous release |
| `BENCHMARKS_TOKEN` | `publish-packages.yml`'s `baseline-submit` job, which commits the release baseline to `neoscad/benchmarks` as `results/<version>/ci-baseline-<target>.json`: a fine-grained token, resource owner `neoscad`, only that repository, Contents read and write (through `secrets: inherit`). Without it the job warns and nothing is submitted; the release is unaffected |
| `NEOSCAD_DEVELOPER_ID_P12`, `NEOSCAD_DEVELOPER_ID_P12_PASSWORD` | the app job and `macos-notarize.yml` (which signs the DMG): the Developer ID Application certificate (base64 .p12) |
| `NEOSCAD_SIGN_IDENTITY`, `NEOSCAD_TEAM_ID` | the same two, as the local variables above |
| `NEOSCAD_NOTARY_KEY`, `NEOSCAD_NOTARY_KEY_ID`, `NEOSCAD_NOTARY_ISSUER` | the same two: an App Store Connect API key for `notarytool` |
| `UPDATE_FEED_MINISIGN_KEY`, `UPDATE_FEED_MINISIGN_KEY_PASSWORD` | `update-feed.yml` (from `release.yml` through `secrets: inherit`, and from `macos-notarize.yml`): the text of the minisign secret key file that signs the update feeds, and its password. Its public half must be in `RELEASE_KEYS` (`crates/client/src/update.rs`), or the job fails. Without it the job warns, and the feeds it wrote are kept only as the run's artifact ("The update feed" below) |
| `WEBSITE_TOKEN` | `update-feed.yml`'s push of `updates/v1/` and `updates/macos/appcast.xml` to `neoscad/website`: a fine-grained token, resource owner `neoscad`, only that repository, Contents read and write. Without it the jobs warn and nothing is published |
| `SPARKLE_ED_PRIVATE_KEY` | `update-feed.yml`'s `appcast` job: the text of the Sparkle EdDSA private key file (`generate_keys -x`), which signs the appcast and the DMGs ("The macOS app's updates" below). Its public half must be `NEOSCAD_SPARKLE_PUBLIC_KEY` in `apple/project.yml`, or the job fails. Without it the job warns and writes nothing |

Cutting one: bump `version`; add (or date) the version's `<release
version="…" date="YYYY-MM-DD"/>` at the top of `<releases>` in
`linux/data/org.neoscad.NeoSCAD.metainfo.xml`, with the release day, so
the tagged source says what the Flatpak says (the Flatpak job refuses a
stable tag without that entry); commit, check that `dist plan` lists
what you expect, then `git tag v<version> && git push origin
v<version>`. The
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
    scripts/release/linux-packages.sh                     # .deb and .rpm in dist/linux, smoke-tested
    scripts/release/package-smoke.sh DIR [IMAGE...]       # install and run DIR's packages
    scripts/release/source-tarballs.sh                    # dist/source
    scripts/release/fill-manifests.sh VERSION SUMS_DIR OUT_DIR   # AUR, Scoop
    scripts/release/fill-manifests.sh --winget VERSION MSI_DIR OUT_DIR   # needs msiinfo (msitools)
    scripts/release/fill-cask.sh VERSION dist/NeoSCAD-*.dmg OUT_FILE   # the app's cask

`linux-packages.sh` builds on Debian bookworm (glibc 2.36), so it
smoke-tests only in Debian 12, Ubuntu 24.04 and Fedora; Debian 10 and
Rocky Linux 8 need the release's `manylinux_2_28` build.

Windows builds happen only in CI: a local cross-build (`cargo xwin`)
would accept the Microsoft CRT and SDK licence, which is the owner's to
accept.

### PGO builds

A profile-guided build (`scripts/pgo.sh`) makes the CLI faster on the
bench models (`docs/audits/perf-opportunities.md`, P2). The release's
`neoscad` is a PGO build on four targets: macOS arm64, Linux x86_64 and
aarch64, and Windows x86_64. Two ship plain builds:

- `x86_64-apple-darwin` is cross-built on the arm64 `macos-15` runner,
  whose host cannot run its instrumented binary (short of Rosetta, which
  the DMG's x86_64 slices now train under; see "The macOS DMG" below);
- `aarch64-pc-windows-msvc`: in `.github/workflows/pgo.yml`'s first run
  every instrumented run crashed with `0xC0000005` and `llvm-profdata`
  rejected the raw profile ("malformed instrumentation profile data:
  symbol name is empty", then "no profile can be merged"), the error
  rust-lang/rust#150123 reports for coverage instrumentation on that
  target. The plain build passed the depth guard there.

The step is `.github/build-setup.yml`'s second (cargo-dist copies it into
`release.yml`'s `build-local-artifacts` job, before `dist build`, with
`dist generate --mode=ci`). On the four targets it:

1. installs `llvm-tools-preview` for the pinned toolchain, fetches
   OpenSCAD and BOSL2 at their pinned commits (`fetch-reference.sh`,
   `bench-kit.sh`'s `BOSL2_COMMIT`), builds `conformance` and extracts
   BOSL2's tests and examples;
2. runs `scripts/pgo.sh --profile dist --profile-only`: the instrumented
   build, the training (`scripts/pgo-train.py`, about 1,350 bounded runs)
   and the merged profile. On Windows the instrumented build gets the
   `-Ctarget-feature=+crt-static` dist adds, so its IR matches;
3. makes the optimised build itself, with the command and `RUSTFLAGS` of
   dist's own cargo invocation (cargo-dist 0.33.0,
   `cargo-dist/src/build/cargo.rs`: it reads `RUSTFLAGS`, appends
   `-Ctarget-feature=+crt-static` on MSVC, and runs `cargo build
   --profile dist --target TRIPLE --package neoscad-cli`), in dist's
   target directory, and logs its SHA-256;
4. runs the recursion-depth guard on that binary, `conformance depth
   --binary target/TRIPLE/dist/neoscad`: exactly the depths of the
   counted recursion limit, the ones a plain build reports
   (`crates/conformance/src/depth.rs`), or the job fails before anything
   is packaged. While the evaluator recursed natively, PGO's larger
   frames made a PGO build recurse a third less deep than a plain one,
   and the guard asked for 1.25 times OpenSCAD's depth; on the heap the
   depth no longer depends on the build;
5. exports `RUSTFLAGS=-Cprofile-use=<profile>` for `dist build`, which
   then finds step 3's build fresh and packages that binary. The SHA-256
   logged in step 3 equals the one `neoscad-executables.sha256sums`
   lists for the target: checked for v0.4.2 (run 37324462422), where all
   four PGO targets' logged hashes match the release's, so `dist build`
   packed the guarded binary rather than rebuilding it.

Cost per PGO target: `conformance`, an instrumented build and the
training before the one optimised build dist would make anyway; in
`pgo.yml`'s first run, `pgo.sh` (two builds and the training) took 5 to
14 minutes against 2.5 to 6.5 for a plain release build. A profile is
only valid for the commit and compiler that made it, so it is trained in
the same job and never committed.

`.github/workflows/pgo.yml`, run by hand, builds PGO next to the plain
build on the four PGO targets and records the depth guard, a conformance
subset and an interleaved quick bench for both. Its first run
(2026-09-30): every PGO binary passed the guard (module recursion 1.42
to 1.55 times OpenSCAD's; Windows x86_64 lowest), the conformance counts
matched the plain build's on every target, and the bench's geometric
mean (PGO / plain, models of 30 ms or more) was 0.90 and 0.83 over two
rounds on macOS arm64 and, one round each, 0.91 on Linux x86_64 and
0.93 on Linux aarch64 (read from the job log: the bench then stopped
before writing a result in the containers, and opened no model on
Windows, both fixed since). Windows x86_64 has no bench numbers yet.
It trained on the `release` profile where releases train on `dist`,
which only adds `strip = "debuginfo"` (`Cargo.toml`, `[profile.dist]`).

**The macOS DMG.** `scripts/apple/release.sh --pgo` (which
`publish-macos-app.yml` runs) builds the app core and the app's bundled
CLI with PGO, both slices of both, from four profiles it trains first:

- **The core trains through itself.** It is `neoscad-ffi`, built as a
  static library, and a profile matches functions by their symbol
  names, which carry each crate's metadata hash. Built for the CLI and
  for the core, every shared crate (serde, eval, geom, manifold_rust,
  session, wgpu_core, ...) gets a different hash, so the CLI's profile
  leaves the core unmatched: with `-Cllvm-args=-pgo-warn-missing-function`,
  the arm64 core reported 14,614 functions without profile data under
  the CLI's profile (eval 958, geom 1,090, manifold_rust 2,534) against
  2,775 under its own (eval 90, geom 56, manifold_rust 301; the CLI
  itself, under its own, 1,576). `scripts/pgo.sh --ffi` trains it with
  the crate's `pgo_train` example (`crates/ffi/examples/pgo_train.rs`),
  which links the very library build that ships (an example of the
  crate compiles against the `--lib` unit; building the example leaves
  `--lib` fresh) and runs `scripts/pgo-train.py --ffi`: the CLI's
  workloads, through the calls the app makes (`run_document` previews
  and renders with a viewport and the editor's language server, exports,
  snapshot, check, measure and an edit loop), dealt to six processes.
- **x86_64 trains under Rosetta.** `pgo.sh --target x86_64-apple-darwin`
  runs the instrumented x86_64 binaries on the arm64 host; the macos-26
  runner has Rosetta (v0.4.2's app job ran the CLI's x86_64 slice under
  it), and `release.sh --pgo` stops before building without it.
- **The CLI is the CLI's own PGO build** (`pgo.sh --target`, release
  profile), so the app's CLI and core are built alike. It is not byte
  for byte the cargo-dist archive's (that is the `dist` profile).

The profiles reach `build-core.sh` and `build-cli.sh` through
`NEOSCAD_PGO_DIR`, exported so that the archive's own runs of those
build phases build the same way and find everything fresh; release.sh
checks that the archive left the core's libraries as built, and the
bundled CLI's UUIDs against the CLI's dSYM. Both CLI slices then pass
the recursion-depth guard (`conformance depth`; the x86_64 one under
Rosetta), with or without `--pgo` whenever `.reference/openscad` is
there, and BUILDINFO.txt records both.

Measured on an M4 Pro (2026-10-06, load average 3 to 7 from other
work), plain against PGO, best of five interleaved runs per model, the
geometric mean over runs of 30 ms or more: the core (through
`pgo_train --time`, one cold job per process, preview and render of
the 14 bench models) 0.927, and 0.998 under the CLI's profile; the
release CLI on the same models 0.928. On models held out of the
training (OpenSCAD's Old, Advanced and Parametric examples and every
8th BOSL2 documentation example from the 5th; 306 runs, best of three)
the core's mean was 0.939, so the profile is not fitted to its training
models alone. The x86_64 core, run under Rosetta on the same Mac (best
of three), measured 0.907; Rosetta's timings are not an Intel Mac's,
but its profile matches as the arm64 one does (eval 132 and geom 77
functions without data).

Cost: the four trainings (each an instrumented build and the training
run) took 88 s (core, arm64), 69 s (CLI, arm64), 153 s (core, x86_64)
and 118 s (CLI, x86_64) on that Mac, and the whole `release.sh --pgo
--no-smoke` 12 minutes; a hosted runner is several times slower (pgo.sh
took 5 to 14 minutes per target in `pgo.yml`), so the CI job's timeout
went from 120 to 180 minutes. The
Homebrew formula, the shell and PowerShell installers, the MSIs, the
`.deb`/`.rpm` packages and the container image all take the cargo-dist
archives, so they get PGO where the archive has it.

## The macOS app after the release

Apple's notary queue took 75 to 80 minutes per submission for the new
Developer ID, and a runner's network dropped twice while waiting, which
failed the release (owner decision, 2026-09-30). So the release does
not wait for Apple. The app is finished afterwards, and the release
stays a prerelease until it is, so `releases/latest` and every
`/releases/latest/download/…` link stay on the last release that has an
app. Everything else is published at release time as before: the
formula, Scoop, the packages, the Flatpaks and the Windows MSIs. Only
the DMG, its cask and the promotion wait.

There are two submissions. The app zip comes first, so its ticket can
be stapled to the `.app` and a copy dragged out of the DMG opens
offline on first launch. The DMG follows. CI doesn't submit the
universal CLI, because its tarball isn't published. A local
`release.sh` run without `--no-wait` still submits it.

1. **At release time**, `publish-macos-app.yml` runs as a publish job
   (after cargo-dist's `host` job has created and published the
   release):
   - `hold` (Linux, starts in seconds) marks a full release as a
     prerelease with `gh release edit --prerelease`. It then re-marks
     the newest remaining full release `--latest`, because GitHub's docs
     say that prereleases can't be latest but not what replaces one that
     turns into a prerelease. An rc tag is a prerelease already and is
     left alone. Without the signing secrets nothing is held, because no
     app will come. Between `host` and `hold` the new release is latest
     for the few seconds a runner takes to start. Closing that gap would
     mean editing the generated `release.yml`.
   - `app` runs `release.sh --pgo --no-wait`, which builds, signs and verifies
     the app, smoke-tests it from an unnotarized DMG of the same app,
     submits `NeoSCAD-<version>-<build>-app.zip` and stops. The zip,
     `notary-app.id`, the dSYMs and `BUILDINFO.txt` are kept as the
     workflow artifact `macos-app-<tag>` (30 days). Then the job uploads
     the state record `macos-app-state.json` to the release:

         {"tag": "v0.3.0", "prerelease": false, "stage": "app-submitted",
          "app_submission": "<uuid>", "dmg_submission": null,
          "artifact_run": "<run id>", "artifact": "macos-app-v0.3.0",
          "updated": "<UTC time>", "chain_started": "<UTC time>"}

     It's a release asset because later runs of another workflow must
     read it and rewrite it, and an artifact can't be rewritten. It's
     uploaded last, so it never names an artifact that doesn't exist
     yet. Then the job starts `macos-notarize.yml` with a
     `repository_dispatch` (event type `macos-notarize`). It takes as
     long as the build, not Apple's queue.
2. **About every 15 minutes while a release is pending**,
   `.github/workflows/macos-notarize.yml` runs (the chain is described
   below). A Linux job looks for releases among the last 20 that carry
   a state record in `app-submitted` or `dmg-submitted`. When there are
   none, the run ends there. For each pending release, a
   `macos-26` job with the same signing secrets checks out the tag, asks
   `notarytool info` for the stage's submission, and moves the release
   on by at most one stage:
   - **app Accepted**: `release.sh --staple-app` staples the app,
     checks Gatekeeper, and makes the DMG
     (`NeoSCAD-<version>-<build>.dmg`, the build number read from the
     zip's name). It then signs the DMG, submits it without waiting and
     keeps it as the artifact `macos-dmg-<tag>`. The state becomes
     `dmg-submitted`.
   - **DMG Accepted**: `release.sh --staple-dmg` staples the DMG and
     checks it (`stapler validate`, `hdiutil verify`, `spctl` must
     accept it). Then the job runs `actions/attest` on the DMG and
     dSYMs; attaches the DMG, the dSYMs, `NeoSCAD-macos-app.sha256` and
     `NeoSCAD-macos-app-BUILDINFO.txt`; and handles the cask as before:
     filled by `fill-cask.sh`, `brew style`, installed from the local
     tap, `spctl` on the installed app, and only then pushed. A full
     release then gets `gh release edit --prerelease=false --latest`
     (`--latest=false` if a newer full release was published in the
     meantime). An rc stays a prerelease. Last, the state record is
     deleted, so the finished release carries no internal file. The
     workflow's `feed` job then adds the DMG to the update feeds ("The
     update feed" below).
   - **In Progress** (or `notarytool` unreachable): nothing.
   - **Invalid or Rejected**: an issue titled "macOS notarization failed
     for `<tag>`" is opened, or commented on if one is already open,
     with `notarytool log`. The state becomes `failed`, which no run
     touches again, and the release stays a prerelease without its app.

   Every step can be repeated. Uploads use `--clobber`, the cask push
   does nothing when the cask is current, and the state is rewritten
   only after what it names exists. A run that dies part-way leaves the
   state where it was, and the next run redoes that stage (at worst one
   more DMG submission). The concurrency group `macos-notarize` keeps
   two runs from advancing a release at once.

**The chain.** GitHub's cron is best effort. On 2026-10-01 an hourly
cron ran once in four hours, and after it moved to every 15 minutes
(`8,23,38,53 * * * *`) no scheduled run fired for at least 45 minutes,
so v0.2.1-rc.2 and v0.2.1 sat for that long after Apple had accepted
them. So the workflow keeps itself going, and the cron is only a
backstop:

- The release's `app` job starts the first run as soon as it has
  recorded `app-submitted`.
- Each run that found a pending release ends with a `next` job (Linux).
  `next` reads every state record again, sleeps 15 minutes, and sends a
  `repository_dispatch` for a new untagged run, which checks every
  pending release. If a run of the workflow is already queued or
  waiting (the cron's, or one started by hand), `next` dispatches
  nothing and leaves the chain to that run. With the workflow-wide
  concurrency group, at most one run goes and one waits, so a
  re-dispatch never makes a second, parallel chain. Even if two
  dispatches raced, GitHub would cancel the older waiting run.
- The wait is a `sleep` in a Linux job because GitHub has no delayed
  dispatch. An environment wait timer would need repository settings
  and would hold the group just the same. Actions minutes are free in
  this public repository, so the sleeping job costs nothing. While a
  release is pending, each check is a few minutes of Linux and macOS
  runner time, about four times an hour.
- A `repository_dispatch` and not `gh workflow run`, because the `app`
  job runs inside `release.yml`, which cargo-dist generates and which
  grants its publish jobs `contents: write` but not `actions: write`.
  The dispatches endpoint needs only `contents: write`, and GitHub lets
  the `GITHUB_TOKEN` start runs with either dispatch event.
- **Bound**: when a release is still pending 48 hours after its
  `chain_started` (`updated` for a record written before that field
  existed), `next` adds `chain_stopped` to its state, opens or comments
  on an issue titled "macOS notarization stalled for `<tag>`", and stops
  dispatching for it. The cron and runs started by hand still check it,
  and it is finished normally if Apple answers.

**Stopping the chain.** Cancel the run whose `next` job is sleeping
(`gh run cancel <run id>`). `next` runs only if the run wasn't
cancelled, so no new run is dispatched. The cron still advances the
release. To stop everything, also run
`gh workflow disable macos-notarize.yml`, and later `enable` it.

**Restarting it.** Any run that finds a pending release starts the
chain again (`gh workflow run macos-notarize.yml`, or just wait for
the cron). After the 48-hour bound, run
`gh workflow run macos-notarize.yml -f tag=<tag> -f restart=true`.
That run resets `chain_started` to now and removes `chain_stopped`.
Re-running the release's `app` job also writes a new `chain_started`.

**Watching it.** The release's assets show the stage
(`gh release download <tag> -p macos-app-state.json -O -`). The
"macOS notarization" workflow's run summaries show each pending release
and Apple's answer. `xcrun notarytool history` lists the submissions.

**Forcing it.** Running "macOS notarization" from the Actions tab
(`gh workflow run macos-notarize.yml [-f tag=v0.3.0]`) checks for one
tag or for every pending release. If the chain's `next` job is
sleeping, the new run waits for it (up to 15 minutes), because they
share the concurrency group. To check at once, cancel the sleeping run
first; the new run carries the chain on. It can't skip Apple: a stage
still in progress stays put.

**After a rejection**, read the issue's log, fix the cause on main and
cut a new patch release. Re-running the release's own `app` job for the
same tag (from the release run, "Re-run jobs") rebuilds from the tagged
commit and rewrites the state to `app-submitted`, which only helps when
the rejection was Apple's error, not the build's. Don't re-run it while
a `macos-notarize.yml` run is advancing that tag. To publish a release without its
app, run `gh release edit <tag> --prerelease=false --latest` and
`gh release delete-asset <tag> macos-app-state.json`.

**Locally**, the same three stages are `release.sh --no-wait`, then
`--staple-app dist` and `--staple-dmg dist` once `xcrun notarytool info
"$(cat dist/notary-app.id)"` (then `notary-dmg.id`) says Accepted. Each
stage refuses to run before that. Plain `release.sh` still notarizes
the app, the DMG and the CLI in one run and waits for each.

The download page on neoscad.org links each file by version
(`/releases/download/v<version>/…`), not through `releases/latest`.
Update its DMG link only once `macos-notarize.yml` has attached the DMG.
Before then, the link returns 404.

## The update feed

The apps and the CLI learn about new releases from two small signed files
on the website, not from the GitHub API (whose unauthenticated limit is 60
requests an hour per IP, and whose "latest" can't serve an rc channel;
`docs/audits/auto-update.md`):

    https://neoscad.org/updates/v1/stable.json   (+ stable.json.minisig)
    https://neoscad.org/updates/v1/rc.json       (+ rc.json.minisig)

    {
      "schema": 1,
      "channel": "stable",
      "serial": 7,
      "version": "0.3.0",
      "date": "2026-10-01",
      "url": "https://github.com/neoscad/neoscad/releases/tag/v0.3.0",
      "artifacts": {
        "macos":         {"name": "NeoSCAD-0.3.0-412.dmg", "url": "…", "sha256": "…", "size": 51234567},
        "windows-x64":   {"name": "NeoSCAD-0.3.0-windows-x64.msi", …},
        "windows-arm64": {…}, "linux-x86_64": {…}, "linux-aarch64": {…}
      }
    }

- `stable.json` names the newest release whose tag has no prerelease
  part, and `rc.json` the newest release of any kind. An rc therefore
  goes to `rc.json` only, and a full release to both, so the rc channel
  moves on to the final release.
- `artifacts` lists the apps' installers that the release has so far: the
  DMG, the two MSIs of `windows-installer.yml` and the two Flatpaks. It
  never lists the CLI's archives or cargo-dist's CLI MSIs. Package
  managers update the CLI, which only uses the version and URL.
- `serial` goes up by one whenever the file changes. Clients refuse a
  lower serial than one they have accepted, which stops replay of an old,
  validly signed feed. They refuse a feed whose `channel` isn't the one
  they asked for, and they never offer a version that isn't newer than
  their own (`crates/client/src/update.rs`).
- A change that old clients would misread goes to `/updates/v2/`. A v1
  file always says `"schema": 1`, and clients ignore fields they don't
  know.

**How it's made.** `update-feed.yml` runs
`scripts/release/update-feed.py` on the repository's last 30 releases
(`gh api …/releases`) and the website's current feeds. It rewrites a feed
only when what it says has changed, and never moves one to a lower
version. Each sha256 is the asset's GitHub `digest`, or else the value in
the `.sha256` file beside the asset. The job signs the changed files with
`minisign -S` and checks them with `minisign -V`, then commits
`updates/v1/` to `neoscad/website`. Every run derives everything again, so
re-running it is safe.

**When.** The feeds follow tags, not GitHub's prerelease flag. A full
release is held as a prerelease until its DMG is notarized ("The macOS app
after the release"), yet the Windows and Linux apps and the CLI hear about
it as soon as it is published:

1. After `announce`, `release.yml`'s `custom-update-feed` job publishes the
   release without `macos`. A Mac app is offered a release only when the
   feed has its DMG (a client given a platform needs that platform's
   installer), so Macs keep their current version until then.
2. Once `macos-notarize.yml` has attached the DMG, its `feed` job runs the
   same workflow, and the feed gets `macos` and the next serial. The `feed`
   job runs after every run that found a pending release. When
   nothing changed, it pushes nothing.

If notarization fails, the feed stays without `macos` until a later
release. An rc tag without `publish-prereleases` has no app installers
(their publish jobs are skipped), so `rc.json` names it without
installers. Only the version and URL reach the CLI, and the CLI follows
`stable.json`. `gh workflow run update-feed.yml` runs it by hand.

**Without the secrets** (a fork, say), the job writes the feeds, warns
that it can't sign or publish them, and uploads them as the run's
artifact `update-feed-<run>-<attempt>`, so they can be checked. The
release key was created on 2026-10-02; its public half is in
`RELEASE_KEYS`, and builds before 0.3.0 trust no key and so never offer
an update.

**The key.** Create it once, offline:

    minisign -G -p neoscad-update.pub -s neoscad-update.key    # with a password

- The password manager holds the secret key file and its password; the
  repository secrets `UPDATE_FEED_MINISIGN_KEY` (the file's text) and
  `UPDATE_FEED_MINISIGN_KEY_PASSWORD` hold working copies.
- The public key's second line (`RW…`) goes into `RELEASE_KEYS` in
  `crates/client/src/update.rs`, and ships with the next release. The job
  refuses to sign with a key whose public half isn't there.
- Never commit the secret key. `crates/client/testdata/update/test.key`
  is a throwaway key for the tests, and `RELEASE_KEYS` must never hold its
  public half (a unit test checks).

**Rotating the key.** Installed clients trust only the keys they were
built with, so a new key must ship before the feed uses it:

1. Make the new key. Add its public line to `RELEASE_KEYS` next to the old
   one, and release.
2. Wait until most installs have that release (or later), since older ones
   stop seeing updates at the switch. Then replace the two secrets with
   the new key. The next feed is signed with it.
3. In a later release, remove the old key from `RELEASE_KEYS`.

If the key leaks, do steps 1 and 2 at once and remove the old key in the
same release. Installs that predate it then stop hearing about updates and
need a manual update, which the release notes should say.

**Testing it locally.** `scripts/release/test-update-feed.sh` writes a
feed from fake releases, signs it with the test key and serves it on
127.0.0.1. It then builds `neoscad` trusting the test key (the
compile-time `NEOSCAD_UPDATE_TEST_PUBLIC_KEY`, never set for a release)
and pointed at that server (`NEOSCAD_UPDATE_FEED_URL`). It checks that the
notice appears once in a pseudo-terminal, and never when output is piped
or redirected, with `CI` or `NEOSCAD_NO_UPDATE_CHECK` set, or for a
tampered feed. `crates/client/testdata/update/make.sh` regenerates the
unit tests' signed fixtures. `docs/privacy.md` says what the check sends
and how to turn it off.

## The macOS app's updates

The app updates itself with [Sparkle](https://sparkle-project.org) 2.10.0
(`apple/project.yml`, `packages:`), following
`docs/audits/auto-update.md` and the owner's decisions there: it checks
about once a day from the first launch, with "Check for updates
automatically" in Settings to turn that off; NeoSCAD > Check for
Updates… checks at once; and "Receive release candidates" (off by
default) lets the app see release candidates.

    https://neoscad.org/updates/macos/appcast.xml

The appcast is separate from the JSON feeds above because it is
Sparkle's own format and signature, and Sparkle does the download, the
verification, the install and the relaunch. It lists at most two items:

- the newest release whose tag has no prerelease part and that has a
  DMG, which every app sees;
- the newest release of any kind with a DMG, when that is a release
  candidate newer than the first, with `<sparkle:channel>rc</sparkle:channel>`.
  Only apps with "Receive release candidates" on ask for that channel
  (`App/Updates/AppUpdater.swift`).

Sparkle orders items by `sparkle:version`, the app's `CFBundleVersion`
(`git rev-list --count HEAD`, "Versions" above), so a final release
always follows its candidates. `sparkle:shortVersionString` is the full
version (`0.3.0-rc.1`), which is what the update dialog shows. The DMG is
the notarized one attached to the release; there is no second archive and
no delta.

**Signatures.** One EdDSA (Ed25519) key signs both the appcast and each
DMG. The app's `Info.plist` sets `SURequireSignedFeed` (the appcast must
carry a valid signature, so a replaced appcast is ignored) and
`SUVerifyUpdateBeforeExtraction` (the DMG's signature is checked before it
is mounted), and Sparkle also checks that the new app's code signature is
valid. The public half is in one place:

    apple/project.yml, target NeoSCAD:  NEOSCAD_SPARKLE_PUBLIC_KEY: ""

It becomes `SUPublicEDKey`. While it is empty (the state until the owner
creates the key), the app has no updater at all: no menu item, no request,
and Settings says the build doesn't update itself. Sparkle given no key
would alert the user to "contact the developer", so the app never starts
it. Development builds stay that way. The pipeline refuses to go on
without it: `release.sh` won't build a notarized app with an empty key,
and the `appcast` job fails when the key is empty or isn't the public
half of its secret.

**How it's made.** `update-feed.yml`'s `appcast` job runs on `macos-15`
next to the JSON feeds' job, from the same three triggers (after
`announce`, after `macos-notarize.yml` advanced a release, and by hand).
`scripts/release/appcast.py` reads the last 30 releases. For each item it
downloads the DMG, checks it against the asset's GitHub `digest`, checks
that Gatekeeper accepts the DMG and the app in it (`--require-notarized`),
and mounts it to read the app's own `CFBundleVersion`,
`LSMinimumSystemVersion` and `SUPublicEDKey`. An app built without the key
is refused, because Sparkle won't install an update that drops it. Then
it signs the DMG with Sparkle's `sign_update` (from Sparkle's release,
pinned by SHA-256 in `scripts/release/sparkle-tools.sh`), writes the
appcast, and signs that last (the signature goes in a closing XML
comment). Ed25519 signatures are deterministic, so an unchanged list of
releases gives the same bytes, and nothing is pushed. A changed appcast
goes to `updates/macos/appcast.xml` in `neoscad/website`, and is kept as
the run's artifact `appcast-<run>-<attempt>`. The private key reaches
`sign_update` on standard input only.

The first run after a release finds no new DMG (it isn't attached until
notarization), so Macs hear about a release once `macos-notarize.yml`
has attached its DMG, as with the JSON feed.

**The key.** Create it once, on a Mac, with Sparkle's tool:

    scripts/release/sparkle-tools.sh .cache/sparkle      # gitignored
    .cache/sparkle/bin/generate_keys --account neoscad    # prints the public key
    .cache/sparkle/bin/generate_keys --account neoscad -x neoscad-sparkle.key

- The password manager holds `neoscad-sparkle.key`. The repository secret
  `SPARKLE_ED_PRIVATE_KEY` holds a working copy (the file's text). Then
  delete the file. `generate_keys` also leaves the key in the login
  keychain; remove it from there too if that Mac isn't the backup.
- The public key goes in `NEOSCAD_SPARKLE_PUBLIC_KEY` in
  `apple/project.yml`, and ships with the next release.
  `xcrun swift scripts/release/sparkle-key.swift public < neoscad-sparkle.key`
  prints it from the file too, which is how the `appcast` job checks the
  secret.
- Losing the private key strands every installed app: it then trusts no
  appcast, and users must download the next release by hand.

**Rotating the key.** Sparkle accepts an update signed with the old key
whose app carries a new one (it checks that the new app's code signature
matches the old app's team). So: put the new public key in
`apple/project.yml` and release, with the appcast still signed by the old
key. Once that release has been out long enough, replace the secret with
the new key. Apps that skipped the transition release then need a manual
update. Not tried yet.

**Testing it locally.** `scripts/apple/test-updates.sh [WORK_DIR]` builds
the app twice with `release.sh` (ad hoc, build numbers 1 and 2), trusting
a throwaway key it makes with `sparkle-key.swift generate` and reading an
appcast on `127.0.0.1`. It writes and signs that appcast with
`appcast.py` from a fake release, serves it, and runs build 1 three
times. A tampered appcast must be refused (the DMG is never requested).
The real one must be offered (a second window). With automatic
installation on, build 2 must be downloaded, verified, and installed in
place when the app quits. It saves the `org.neoscad.NeoSCAD` preferences
first and restores them, and refuses to run while any NeoSCAD is running.
The key never enters the repository.

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
  MSIs, the Homebrew pushes and the publish jobs (the package smoke
  test ran locally on arm64 only, around a local `manylinux_2_28`
  build; its x86_64 half and the artifact hand-off between the
  packages, smoke and attach jobs are untested). The cask template
  passes `brew style` and `brew audit --cask --strict` locally (Homebrew
  7.0.1, filled with a stand-in DMG); its URL, checksum and `livecheck`
  meet a real release for the first time on the first tag. The
  Flatpak's x86_64 build has passed in `flatpak.yml` on main; its
  aarch64 build, the bundle naming, attestation and upload are first
  exercised by a dispatch (build only) and the first tag.

- The Developer ID path: `-exportArchive`, notarization, stapling and an
  accepting Gatekeeper have never run, since no Developer ID identity or
  notary profile exists yet. The first signed run is its test.
- The staged notarization ("The macOS app after the release"): the
  hold, the state record, `macos-notarize.yml` and the promotion have
  never run on GitHub. `release.sh --staple-app` and `--staple-dmg` ran
  locally only against stand-ins for `notarytool`, `stapler`, `spctl`
  and `codesign`, with a placeholder app. The first rc tag after the
  change is their test.
- That `-exportArchive` accepts an archive whose Quick Look extensions
  (8h) are sandboxed without extra export options.
- The DMG and app on another Mac: see the checklist.
- The Windows app's MSIs as a release publish job: attesting and
  attaching them have never run (dispatched runs skip both), nor has the
  job under `release.yml`'s `workflow_call`.

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

## Windows is unsigned

Windows zips and MSIs (the CLI's, and the app's
`NeoSCAD-<version>-windows-<arch>.msi`) are not Authenticode-signed (owner decision
2026-09-29: no paid signing service). Double-clicking a downloaded `.exe`
or `.msi` shows SmartScreen's "Windows protected your PC"; **More info →
Run anyway** proceeds. The PowerShell installer (`irm … | iex`), winget and
Scoop install without that prompt, and running `neoscad` from a terminal
normally doesn't show it (Windows 11's Smart App Control, where enabled, may
still block unsigned programs). Every file can be checked against `sha256.sum` (the app's MSIs,
attached after it is made, against their own `.sha256`), and its
origin with `gh attestation verify <file> -R neoscad/neoscad` (GitHub
artifact attestations, `github-attestations = true`). The SignPath
Foundation offers free signing to open-source projects if that changes.

## The Windows app in the release

`.github/workflows/windows-installer.yml` is a publish job
(`"./windows-installer"` in `publish-jobs`, with `contents`, `id-token`
and `attestations` write in `github-custom-job-permissions`), so
`release.yml` has a `custom-windows-installer` job that `announce` waits
for. Like the other publish jobs it runs for a prerelease only when
`publish-prereleases` is set.

Dispatching the workflow by hand (`gh workflow run windows-installer.yml`)
runs the same build and install test as a dry run. The MSIs and the
licence page's RTF become workflow artifacts. Nothing is attested or
attached, because both steps need the release's `plan` input.

The app's MSI carries the `neoscad` CLI as `bin\neoscad.exe` in the
install folder, for AI agent clients (`docs/mcp.md`, "Setup from the
apps"). `build-msi.ps1` builds it as cargo-dist builds the released CLI
(`--profile dist`, `+crt-static`, so it needs no Visual C++ runtime in
`bin\`), without the PGO profile, checks its `--version`, and stages it
for the WiX harvest. It is in `bin\` because `neoscad.exe` beside
`NeoSCAD.exe` would be the same file, and it is not added to `PATH` (the
CLI's own MSI and scoop do that). The install check runs it from the
install folder (`--version` and an MCP `initialize`) and checks that the
machine `PATH` does not name the install. The MSI is unsigned like the
rest (below), so the CLI in it is too. Its size in the MSI was not
measured: it is built on Windows only.

Before the build, the job runs `scripts/windows/test-scripts.ps1`, which
parses every script in `scripts/windows` and checks the licence page's
RTF. Being plain pwsh, it runs anywhere, e.g. in the
`mcr.microsoft.com/dotnet/sdk` image, which ships pwsh for arm64. Don't
use `mcr.microsoft.com/powershell:latest` for this on an Apple Silicon
Mac: that tag has only amd64 and 32-bit arm images, and pwsh under
emulation produced wrong output and crashes.
