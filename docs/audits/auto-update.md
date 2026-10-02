# Spike: auto-update for the desktop apps

Scope: the macOS, Windows and Linux apps against today's release pipeline
(`docs/release.md`, `publish-macos-app.yml`, `windows-installer.yml`,
`flatpak.yml`, `scripts/apple/release.sh`, `crates/client`). Read
2026-09-30 at d001758. Nothing was changed. External claims were checked
against the URLs cited on that date.

## Recommendation

| Platform | Minimum viable | Later | Effort |
|---|---|---|---|
| Shared | A signed feed (`stable.json`, `rc.json`) on neoscad.org, checked by pure logic in `client` | | S |
| macOS | Sparkle 2 with the existing notarized DMG as the update archive, appcast on neoscad.org | Delta updates | M |
| Windows | In-app check against the shared feed, then download, verify and launch the MSI | winget manifest for the app; Velopack only if the per-machine decision changes | S-M (plus a WiX fix, below) |
| Linux | A GPG-signed static Flatpak repo so `flatpak update` and GNOME Software work | | M |

Don't use the GitHub API as the feed. Unauthenticated calls are limited
to "60 requests per hour", counted per originating IP
([REST rate limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)).
"Latest" is "the most recent non-prerelease, non-draft release"
([Releases API](https://docs.github.com/en/rest/releases/releases#get-the-latest-release)),
so neither the API nor `/releases/latest/download/…` can serve an rc
channel. The API also returns nothing signed.

## Firm ground: what the code does today

1. **The macOS app is not sandboxed** (`apple/App/NeoSCAD.entitlements`).
   Sparkle's XPC and sandbox setup is optional for non-sandboxed apps
   ([Sparkle sandboxing](https://sparkle-project.org/documentation/sandboxing/)).
2. **`CFBundleVersion` is `git rev-list --count HEAD`** (`docs/release.md`,
   "Versions"; checked in `release.sh:278`). Sparkle compares
   `sparkle:version` against it
   ([Sparkle publishing](https://sparkle-project.org/documentation/publishing/)),
   so it keeps increasing, and an rc and its final release (both
   `CFBundleShortVersionString` 0.2.0) still order correctly.
3. **`release.sh`'s re-signing loop would miss Sparkle's `Autoupdate`.**
   `sign_app` (`scripts/apple/release.sh:194-204`) signs only
   `*.framework`, `*.dylib`, `*.appex`, `*.xpc` and `*.app`. Sparkle
   documents re-signing `Sparkle.framework/Versions/B/Autoupdate`, a bare
   executable, with `-o runtime` and without `--deep`
   ([Sparkle sandboxing, code signing](https://sparkle-project.org/documentation/sandboxing/)).
   Unless it is added explicitly, notarization is likely to fail.
4. **The Flatpak has no network access.** The `finish-args` in
   `linux/flatpak/org.neoscad.NeoSCAD.yml:21-31` have no `--share=network`,
   so a Linux in-app check needs a new sandbox permission.
5. **A Windows rc and its final release install side by side (existing
   bug, independent of auto-update).** `build-msi.ps1:43` keeps only the
   numeric `x.y.z`, so 0.2.0-rc.1, rc.2 and 0.2.0 all have ProductVersion
   0.2.0. `<MajorUpgrade>` (`windows/installer/NeoSCAD.wxs:48`) leaves
   `AllowSameVersionUpgrades` at its default. WiX's documentation for that
   default: "installing a product with the same version and upgrade code
   (but different product code) is allowed and treated by MSI as two
   products" ([WiX MajorUpgrade](https://docs.firegiant.com/wix/schema/wxs/majorupgrade/)).
   `docs/windows-app.md:99-101` claims that "any other version is a major
   upgrade", which is not true within a version's rcs. This comes from the
   documentation and was not tested on Windows.
6. **`publish-prereleases = true` is set** (`Cargo.toml:147`), so an rc
   moves the cask and the Scoop bucket. The release doc says to remove it
   before the real tag. An in-app channel makes that more visible: while
   it is set, cask users get rcs through `brew upgrade`.
7. **`client` is a library crate:** no clock, no `std::env`, no network
   (`crates/client/src/lib.rs:13-16`; nothing in `Cargo.lock` provides an
   HTTP client). The shared part of an update check must therefore be pure
   logic, with each host doing the fetch and the scheduling.

## Shared: the feed and the check

**Feed.** The release workflow's last step writes
`https://neoscad.org/updates/v1/stable.json` and `rc.json`, each with a
detached minisign signature (`.minisig`). Each entry gives the version,
the build number, the publication date, a monotonic `serial`, a
release-notes URL, and, per platform and architecture, the asset URL on
GitHub Releases with its sha256, size and minimum OS. The files are text
and tiny, so the website repo (63 MB, Pages "legacy" branch build) can
hold them.

**Check, in `client`:** `update::evaluate(feed, sig, current, channel,
platform, last_serial) -> Option<Offer>`. It:
- verifies the signature against a public key compiled into the crate,
  using [`minisign-verify`](https://github.com/jedisct1/rust-minisign-verify)
  0.3.0 (MIT, "no external dependencies" per `cargo info`, pure Rust, so
  it stays WASM-clean);
- orders versions by semver with prereleases, so an rc user on the rc
  channel is offered the final release;
- refuses a lower version, and a `serial` lower than the last one seen.
  That blocks replay of an old, validly signed feed, which gives downgrade
  protection.

A second call, `verify_download(bytes, offer)`, checks the sha256 from
the signed feed. Attestations (`gh attestation verify`) stay a manual
audit tool, because verifying them in-app needs Sigstore and network
access. The hosts (Swift, C#, Rust/GTK) do the GET, the schedule and the
setting.

**Frequency and privacy.** At most once per 24 h, at launch or when idle,
plus a menu item. The request is a plain GET with no query string, no
cookies and no install id. It sends a generic User-Agent without the
version or OS, and filters the platform locally. GitHub Pages still sees
the IP, which a privacy note should say. There is a "Check for updates
automatically" setting, and the menu check works with it off.

## macOS: Sparkle 2

The current release is Sparkle 2.10.0 (13 Sep 2026, per `gh release
view`). Sparkle accepts a `.dmg` as the archive
([publishing](https://sparkle-project.org/documentation/publishing/)), so
the existing notarized, stapled DMG is reused and no second artifact is
needed.
- **Info.plist:** `SUFeedURL=https://neoscad.org/appcast.xml` and
  `SUPublicEDKey`. By default Sparkle asks permission to check only on
  the second launch
  ([docs](https://sparkle-project.org/documentation/)). Turn on
  `SURequireSignedFeed` with `SUVerifyUpdateBeforeExtraction`, which are
  validated from 2.9 on.
- **Channels:** rc items carry `<sparkle:channel>beta</sparkle:channel>`,
  and only updaters allowed that channel see them
  ([publishing](https://sparkle-project.org/documentation/publishing/)).
  A "Include release candidates" setting controls this.
- **Workflow:** after "Attach to the release" in
  `publish-macos-app.yml`, sign the DMG and the appcast with the EdDSA key
  from a secret (`generate_appcast`, run over the previous appcast plus the
  new DMG; exact flags to settle when building), then push `appcast.xml`
  to the website repo.
- **`release.sh`:** sign Sparkle's `Autoupdate`, `Updater.app` and XPC
  services inside out (finding 3). The existing Gatekeeper/`spctl` checks
  then cover them.
- **Cask:** add `auto_updates true` (for apps whose "Check for Updates…"
  downloads and installs;
  [Cask Cookbook](https://docs.brew.sh/Cask-Cookbook)). `brew upgrade`
  then skips the cask unless the user passes `--greedy` or
  `--greedy-auto-updates` ([manpage](https://docs.brew.sh/Manpage)). The
  tap push stays as it is.
- **Custom in-app check instead:** it could share the feed, but it would
  only open a download page. Sparkle does the install, the relaunch and
  the verification for M effort, so a custom check isn't worth it on this
  platform.

## Windows

**Minimum viable: in-app check plus MSI.** The app uses `client`'s check
through the ffi and downloads the MSI to `%TEMP%`. It verifies the sha256
from the signed feed, runs `msiexec /i <file>` and exits. Per-machine
means a UAC prompt naming an unknown publisher, every time.
`MajorUpgrade` replaces the old version once finding 5 is fixed. SmartScreen
acts on the Mark of the Web, which a downloader adds only if it calls
`IAttachmentExecute::SetSource` or writes a zone identifier
([IAttachmentExecute](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-iattachmentexecute)).
An `HttpClient` download therefore probably shows no SmartScreen prompt.
The brief's "SmartScreen every time" is plausible only for browser
downloads. **Untested**; check it on a Windows machine.

**Prerequisite:** set `AllowSameVersionUpgrades="yes"` in `NeoSCAD.wxs`,
or put the rc number into the MSI version. Without one of them, rc to
final leaves two NeoSCADs in Apps & Features.

**Velopack** reads GitHub Releases or any static host
([docs](https://docs.velopack.io/)). Its default Setup.exe installs per
user into `%LocalAppData%\{packId}`. Its optional MSI is built with
Velopack's own WiX v7 fork and can be `PerMachine` (Program Files,
"requires elevation") ([installers](https://docs.velopack.io/packaging/installer),
[Windows](https://docs.velopack.io/packaging/operating-systems/windows)).
Signing is "very recommended (but not required)". Adopting it would
replace `build-msi.ps1` and WiX 5, and either reverse the per-machine
decision (`docs/windows-app.md:82-87`) or rely on per-machine updates,
whose elevation behaviour the docs don't describe. The gain is delta
updates on 78-82 MB MSIs. Effort L; I don't recommend it now.

**MSIX / App Installer** needs a trusted signature, so it is out
(`docs/windows-app.md:77`). **winget:** adding the app's MSI to the
existing manifest fill is S effort and complementary. `winget upgrade`
is user-driven, not automatic.

## Linux

**Minimum viable: a static, GPG-signed Flatpak repo.** Flatpak repos can
be served by any web server and must be GPG-signed (`flatpak
build-update-repo --gpg-sign`). `.flatpakrepo`/`.flatpakref` carry `Url`,
`GPGKey` and `RuntimeRepo` (Flathub for the GNOME runtime). `--prune`
keeps only the latest commit, and static deltas trade space for speed
([hosting a repository](https://docs.flatpak.org/en/latest/hosting-a-repository.html)).
- **Size:** the bundles are 5.6-6.2 MB per architecture (v0.2.0-rc.1
  assets). A pruned repo is far below Pages' 1 GB site limit, 100 GB/month
  soft bandwidth and 10-minute deploy timeout
  ([Pages limits](https://docs.github.com/en/pages/getting-started-with-github-pages/github-pages-limits)).
  Pages "may" rate-limit with 429, and the repo format makes "a lot of
  HTTP requests" per pull. **Unmeasured.**
- **Where:** deploy the repo as a Pages *artifact* from Actions, in its
  own repository, rather than committing OSTree objects to the website
  repo, whose git history would grow on every release. Serving it under
  neoscad.org needs DNS/Pages work (open question 3).
- **Workflow:** `flatpak.yml` also exports to the repo (both
  architectures), signs it, runs `build-update-repo --prune
  --generate-static-deltas` and deploys. Bundles built with `flatpak
  build-bundle --repo-url` then point installs at that repo, so bundle
  users also get `flatpak update`.
- **In-app notice only:** needs `--share=network` (finding 4). With the
  repo in place, GNOME Software and KDE Discover already notify. Skip it.
- **AppImage + zsync:** a new format to build and test. AppImages "do
  not update themselves automatically"; AppImageUpdate is a separate tool
  ([AppImage updates](https://docs.appimage.org/packaging-guide/optional/updates.html)).
  Not recommended.

## New secrets and keys (the owner creates these)

| Secret | What | Notes |
|---|---|---|
| `SPARKLE_ED_PRIVATE_KEY` | Sparkle EdDSA key (`generate_keys`, export with `-x`) | Public half goes in Info.plist. Losing it strands installed apps; keep an offline backup |
| `UPDATE_FEED_MINISIGN_KEY`, `…_PASSWORD` | minisign key for `stable.json`/`rc.json` | Public half compiled into `client` |
| `FLATPAK_GPG_KEY`, `…_PASSPHRASE` | Flatpak repo signing key | Public half in `.flatpakrepo`/`.flatpakref` and the bundles |
| `WEBSITE_TOKEN` | fine-grained, `neoscad/website` Contents write | Appcast and feed pushes, like `HOMEBREW_TAP_TOKEN` |

## Open questions for the owner

1. Should the automatic check be on by default (Sparkle's second-launch
   prompt, or the same prompt on all three platforms), or off until the
   user turns it on? Either way, what does the site's privacy text say?
2. Should rc builds reach anyone automatically? Recommended: only users
   who opt into the rc channel. This interacts with `publish-prereleases`
   (finding 6).
3. Where should the Flatpak repo live: a subpath of neoscad.org, or a
   subdomain on its own Pages repo?
4. Is a separate feed-signing key per purpose acceptable (Sparkle EdDSA,
   minisign, GPG)? Where are offline backups kept?
5. Should the Windows app install silently (`msiexec /passive`) after one
   UAC prompt, or show the MSI's licence page on every update?
6. Is the CLI in scope? It updates through its package managers today;
   this spike did not look at cargo-dist's updater.

## Checked and found fine

- Every app artifact already has a sha256 and a GitHub attestation. The
  feed can reuse the sha256 values the workflows already compute.
- `MajorUpgrade` refuses downgrades and replaces any *different* version
  (`NeoSCAD.wxs:43-48`).
- Sparkle's hardened-runtime requirements match what `release.sh`
  already does (`--options runtime`, no `--deep`), apart from finding 3.

## Not verified

- SmartScreen behaviour for an MSI that the app downloads itself.
- How Velopack updates a `PerMachine` MSI install (elevation).
- `generate_appcast`'s exact flags for signed feeds without deltas, and
  Sparkle key rotation.
- Pages rate limiting under Flatpak's per-object pull pattern.
- The WiX same-version behaviour (finding 5): taken from WiX's docs, not
  run on Windows.

## Decisions (owner, 2026-09-30)

- **Default:** the apps check for updates automatically, about once a day,
  with a setting to turn it off. The check is a plain request for the
  signed feed and sends nothing identifying.
- **Release candidates:** opt-in only. A "Receive release candidates"
  setting switches an app to the rc feed; everyone else gets stable.
- **Flatpak repository:** a new repository on GitHub Pages, not the
  website repository.
- **Windows:** silent after one UAC prompt. The app shows "Update
  available", then Install, then runs `msiexec /qn` and restarts on the
  new version. The licence was accepted at first install.
- **Command line:** an automatic notice. `neoscad` occasionally prints that
  a newer release exists, only in an interactive terminal: never when
  output is piped or redirected, never in CI (`CI` set), and never when
  `NEOSCAD_NO_UPDATE_CHECK` is set. It uses the same signed feed and never
  downloads or installs anything. Package managers still do the updating.
- **Keys:** the Sparkle EdDSA key, the feed's minisign key and the Flatpak
  GPG key are kept in the owner's password manager. The GitHub secrets
  hold working copies.

## Built so far, and next steps

The shared foundation exists (`docs/release.md`, "The update feed"):

- the feeds, `stable.json` and `rc.json`, written from the releases by
  `scripts/release/update-feed.py`, signed with minisign and pushed to the
  website by `update-feed.yml` (after `announce`, and again from
  `macos-notarize.yml` once the DMG is attached);
- the check, `client::update::check` (signature, channel, serial,
  version, platform), and the apps' entry point `check_for_update` and
  `update_feed_url` in `crates/ffi/src/update.rs`;
- the CLI's notice (`crates/cli/src/update.rs`; `docs/privacy.md`);
- the Windows app's check, download and silent install
  (`windows/NeoSCAD.Host/Updates.cs`, `windows/NeoSCAD.App/MainWindow.Updates.cs`;
  `docs/windows-app.md`, "Updates"): the shared feed through
  `check_for_update`, the MSI checked against the feed's size and
  sha256, then an unelevated helper that waits for the app to exit,
  runs `msiexec /i … /qn` after one UAC prompt and starts the app again.
  Help has "Check for Updates…", "Check for Updates Automatically" and
  "Receive Release Candidates". Built and unit-tested off Windows; not
  yet run on Windows;
- the Linux app's check and notice (`crates/linux-app/src/update.rs`,
  `src/app/update.rs`; `docs/linux-app.md`, "Updates"): a banner and a
  dialog that, for the Flatpak, offers the new bundle (the bundles carry
  no repository, so `flatpak update` doesn't reach it) and otherwise
  the release page, with the same two preferences. This needed
  `--share=network` in the Flatpak (finding 4); the Flatpak repository
  below remains the way to updates without a download.

Nothing is live until the owner creates the minisign key, adds its public
half to `RELEASE_KEYS`, and sets `UPDATE_FEED_MINISIGN_KEY`,
`UPDATE_FEED_MINISIGN_KEY_PASSWORD` and `WEBSITE_TOKEN`.

The macOS app's updater is built too (`docs/release.md`, "The macOS
app's updates"), as recommended above with two departures:

- the appcast is at `https://neoscad.org/updates/macos/appcast.xml`, beside
  the JSON feeds, not at `/appcast.xml`;
- the rc channel is called `rc`, not `beta`, to match `rc.json` and the
  setting's name.

It is made of Sparkle 2.10.0 (still the newest release on 2026-10-01) in
`apple/project.yml`; the menu item and Settings (`apple/App/Updates`);
`release.sh` signing Sparkle's helpers explicitly (finding 3) and checking
that every Mach-O carries the team's signature; the `appcast` job in
`update-feed.yml` with `scripts/release/appcast.py`; `auto_updates true` in
the cask; and `scripts/apple/test-updates.sh`, which installs one local
build over another from a signed local appcast. The public key is
`NEOSCAD_SPARKLE_PUBLIC_KEY` in `apple/project.yml`. Until the owner sets
it, apps have no updater, and `release.sh` refuses to notarize, so **no
release can be published until the key exists**. The owner must:

1. make the key (`generate_keys`, `docs/release.md`), keep it in the
   password manager, and put the public half in `apple/project.yml`;
2. set the secret `SPARKLE_ED_PRIVATE_KEY` (and `WEBSITE_TOKEN`, shared
   with the feeds).

Next steps, in order:

1. **Keys and the first live feed.** Create the key, add the public half
   to `RELEASE_KEYS`, set the three secrets, then run `gh workflow run
   update-feed.yml` and check
   `https://neoscad.org/updates/v1/stable.json` and its `.minisig`.
2. **Windows app.** Built (above; finding 5 was already fixed by
   `AllowSameVersionUpgrades="yes"`). Left: run it on Windows against a
   test-signed feed and an older installed MSI, and look at the UAC
   prompt, SmartScreen (expected none for an `HttpClient` download) and
   the restart (`docs/windows-app.md`, "Updates").
3. **macOS app.** Built and keyed: the public EdDSA key is in
   `apple/project.yml` (Release only) and `SPARKLE_ED_PRIVATE_KEY` is set.
   Left: the `appcast` job's first real run, with `WEBSITE_TOKEN`. The
   shared feed's `macos` entry is for other readers: the CLI, the website,
   a future custom check.
4. **Linux app.** The in-app notice is built (above). Still to do: the
   GPG-signed Flatpak repository on its own Pages site (owner decision),
   so GNOME Software, Discover and `flatpak update` update the app; the
   app's notice should then tell installs from that repository to use
   them rather than offering a bundle.
5. **Website.** A privacy page from `docs/privacy.md`, and download links
   that could read the feed instead of being edited by hand.
6. **Later.** Each artifact's minimum OS version in the feed (the field
   can be added to schema 1, since clients ignore unknown fields), and a
   `verify_download(bytes, artifact)` helper in `client` once a Rust host
   downloads installers.
