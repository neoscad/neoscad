#!/usr/bin/env bash
# Cuts a release of the macOS app and the `neoscad` command-line tool
# (docs/release.md; docs/audits/macos-prep.md, 8j). Writes to dist/:
#
#   NeoSCAD-<version>-<build>.dmg                 the app and an Applications link
#   neoscad-<version>-<build>-macos-arm64.tar.gz  the CLI and its licence
#   neoscad                                       the same CLI, bare
#   NeoSCAD-<version>-<build>-dSYMs.zip           debug symbols (app, core, CLI)
#   BUILDINFO.txt                                 what was built, how, and checked
#   SHA256SUMS                                    of everything above
#
# <version> is the Cargo workspace version (CFBundleShortVersionString and
# `neoscad --version`); <build> is `git rev-list --count HEAD`
# (CFBundleVersion), so every commit on main gets a larger build number.
#
# Signing is chosen by the environment; nothing is prompted for or stored:
#
#   NEOSCAD_SIGN_IDENTITY   a "Developer ID Application" identity (its name
#                           or SHA-1) in the keychain. Unset: ad-hoc
#                           signing, which runs on this Mac only and which
#                           Gatekeeper rejects everywhere else.
#   NEOSCAD_TEAM_ID         its team; read from the identity's name if unset
#   NEOSCAD_NOTARY_PROFILE  a `notarytool store-credentials` keychain
#                           profile. Set (with an identity): notarize and
#                           staple the app, the DMG and the CLI.
#   CARGO_TARGET_DIR        honoured, as build-core.sh does
#
#   scripts/apple/release.sh              build, sign, package, verify, smoke test
#   scripts/apple/release.sh --no-smoke   without the smoke test
set -euo pipefail
cd "$(dirname "$0")/../.."
root=$PWD

smoke=1
for arg in "$@"; do
    case $arg in
        --no-smoke) smoke=0 ;;
        -h | --help)
            sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "release: unknown argument $arg" >&2
            exit 2
            ;;
    esac
done

say() { printf '\n==> %s\n' "$*"; }
die() {
    echo "release: error: $*" >&2
    exit 1
}

# --- Preflight -------------------------------------------------------------

[ "$(uname -s)" = Darwin ] || die "macOS only"
for tool in xcodebuild xcodegen hdiutil codesign spctl ditto dsymutil shasum; do
    command -v "$tool" >/dev/null || die "$tool not found"
done

# The core's static library alone is ~300 MB and the archive, its copy in
# the XCFramework and the dSYMs add as much again; a full disk mid-build
# leaves a half-written archive that looks like a result.
free_kb=$(df -k "$root" | awk 'NR == 2 { print $4 }')
if [ "$free_kb" -lt $((5 * 1024 * 1024)) ]; then
    die "under 5 GB free on $(df -h "$root" | awk 'NR == 2 { print $1 }'); clear target/ or DerivedData first"
fi

identity=${NEOSCAD_SIGN_IDENTITY:-}
team=${NEOSCAD_TEAM_ID:-}
notary=${NEOSCAD_NOTARY_PROFILE:-}
if [ -n "$identity" ]; then
    # Only a Developer ID Application certificate can be notarized and
    # passes Gatekeeper outside the Mac Developer Program's own devices.
    # An "Apple Development" or "Apple Distribution" identity would sign
    # without complaint and fail later, at notarization or on a user's
    # Mac, so refuse it here.
    line=$(security find-identity -v -p codesigning | grep -F "$identity" | head -1 || true)
    [ -n "$line" ] || die "NEOSCAD_SIGN_IDENTITY '$identity' is not a valid signing identity in the keychain"
    case $line in
        *"Developer ID Application"*) ;;
        *) die "NEOSCAD_SIGN_IDENTITY must be a 'Developer ID Application' identity; got: $line" ;;
    esac
    if [ -z "$team" ]; then
        team=$(sed -n 's/.*(\([A-Z0-9]\{10\}\))".*/\1/p' <<<"$line")
        [ -n "$team" ] || die "cannot read the team from '$line'; set NEOSCAD_TEAM_ID"
    fi
    mode="Developer ID ($identity, team $team)"
    sign_id=$identity
    timestamp=--timestamp
else
    [ -z "$notary" ] || die "NEOSCAD_NOTARY_PROFILE needs NEOSCAD_SIGN_IDENTITY (Apple notarizes Developer ID signatures only)"
    mode="ad hoc (no NEOSCAD_SIGN_IDENTITY; library validation off, this Mac only)"
    sign_id=-
    # An ad-hoc signature has no certificate for a timestamp to vouch for.
    timestamp=--timestamp=none
fi
if [ -n "$notary" ]; then
    # Fail before a long build, not after it, if the profile is missing.
    xcrun notarytool history --keychain-profile "$notary" >/dev/null 2>&1 ||
        die "notarytool cannot use keychain profile '$notary' (xcrun notarytool store-credentials)"
fi

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
[ -n "$version" ] || die "no version in Cargo.toml's [workspace.package]"
build_number=$(git rev-list --count HEAD)
commit=$(git rev-parse --short=12 HEAD)
dirty=no
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    dirty=yes
    echo "release: warning: the tree has uncommitted changes; build $build_number is not exactly $commit" >&2
fi
name=NeoSCAD-$version-$build_number
cli_name=neoscad-$version-$build_number-macos-arm64

target_dir=${CARGO_TARGET_DIR:-$root/target}
export CARGO_TARGET_DIR=$target_dir
work=$root/apple/build/release
dist=$root/dist
archive=$work/NeoSCAD.xcarchive
app_stage=$work/app
cli_stage=$work/cli
dsym_stage=$work/dSYMs

say "NeoSCAD $version (build $build_number, $commit, dirty: $dirty); signing: $mode"

# A clean Swift build every time: the archive's DerivedData and everything
# from the last run go. The Rust core is not cleaned: cargo's fingerprints
# rebuild whatever changed, and a from-scratch core costs minutes and a
# shared target directory's lock for nothing.
rm -rf "$work" "$dist"
mkdir -p "$work" "$dist" "$app_stage" "$cli_stage" "$dsym_stage"

# --- The app ----------------------------------------------------------------

say "Core, editor bundle and project"
scripts/apple/build-core.sh
scripts/apple/build-editor.sh
(cd apple && xcodegen generate --spec project.yml --quiet)

say "Archive (Release)"
sign_settings=(CODE_SIGN_IDENTITY="$sign_id" CODE_SIGN_STYLE=Manual)
if [ -n "$identity" ]; then
    sign_settings+=(DEVELOPMENT_TEAM="$team" OTHER_CODE_SIGN_FLAGS=--timestamp)
fi
log=$work/archive.log
if ! xcodebuild \
    -project apple/NeoSCAD.xcodeproj -scheme NeoSCAD -configuration Release \
    -destination generic/platform=macOS \
    -derivedDataPath "$work/DerivedData" -archivePath "$archive" \
    MARKETING_VERSION="$version" CURRENT_PROJECT_VERSION="$build_number" \
    "${sign_settings[@]}" \
    archive >"$log" 2>&1; then
    grep -E 'error:|\*\* ARCHIVE' "$log" | sort -u >&2 || tail -40 "$log" >&2
    die "xcodebuild archive failed; full log: $log"
fi
grep -E '(warning|error):' "$log" | grep -v 'Metadata extraction skipped' | sort -u || true
[ -d "$archive/Products/Applications/NeoSCAD.app" ] || die "the archive has no NeoSCAD.app; see $log"

app=$app_stage/NeoSCAD.app
entitlements=$root/apple/App/NeoSCAD.entitlements

# Sign nested code inside out (a bundle's signature seals its contents'
# signatures, so the inner ones must be final first), each with the
# hardened runtime. Nested bundles keep the entitlements Xcode gave them
# (the Quick Look extensions' sandbox); the app gets its own file.
sign_app() {
    local bundle=$1 item
    while IFS= read -r item; do
        codesign --force --options runtime $timestamp --preserve-metadata=entitlements \
            --sign "$sign_id" "$item"
    done < <(find "$bundle/Contents" -depth \
        \( -name '*.framework' -o -name '*.dylib' -o -name '*.appex' -o -name '*.xpc' \
        -o -name '*.app' \) -not -path '*/Versions/Current*' -print)
    codesign --force --options runtime $timestamp --entitlements "$entitlements" \
        --sign "$sign_id" "$bundle"
}

if [ -n "$identity" ]; then
    say "Export (Developer ID)"
    options=$work/ExportOptions.plist
    cat >"$options" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>method</key><string>developer-id</string>
    <key>signingStyle</key><string>manual</string>
    <key>signingCertificate</key><string>$identity</string>
    <key>teamID</key><string>$team</string>
</dict>
</plist>
EOF
    xcodebuild -quiet -exportArchive -archivePath "$archive" \
        -exportPath "$work/export" -exportOptionsPlist "$options"
    ditto "$work/export/NeoSCAD.app" "$app"
else
    say "Sign (ad hoc, hardened runtime)"
    ditto "$archive/Products/Applications/NeoSCAD.app" "$app"
    # The hardened runtime's library validation loads only libraries
    # signed by the process's own team, and an ad-hoc signature has no
    # team: the first ad-hoc release build died in dyld ("Library not
    # loaded: @rpath/NeoSCADCore.framework ... mapping process and mapped
    # file (non-platform) have different Team IDs"). Xcode's Debug builds
    # never met this because they sign ad hoc without the runtime. The
    # local build keeps the runtime (so its other restrictions are what
    # the smoke test runs under) and lifts library validation alone; a
    # Developer ID build signs both with one team and needs no exception,
    # which the verification below insists on.
    entitlements=$work/NeoSCAD-adhoc.entitlements
    cp "$root/apple/App/NeoSCAD.entitlements" "$entitlements"
    /usr/libexec/PlistBuddy -c "Add :com.apple.security.cs.disable-library-validation bool true" \
        "$entitlements" >/dev/null
    sign_app "$app"
fi

# --- Verification -------------------------------------------------------

say "Verify the app"
codesign --verify --deep --strict --verbose=2 "$app"
# Every Mach-O must carry the hardened runtime, or notarization refuses the
# whole submission; and none may carry get-task-allow (a debug build's
# entitlement, which notarization also refuses).
while IFS= read -r macho; do
    flags=$(codesign -dv "$macho" 2>&1 | sed -n 's/.*flags=\(0x[0-9a-f]*\)(\(.*\)).*/\2/p')
    case $flags in
        *runtime*) ;;
        *) die "no hardened runtime on ${macho#"$app"/} (flags: ${flags:-none})" ;;
    esac
done < <(find "$app" -type f -perm -u+x -print | while IFS= read -r f; do
    if file -b "$f" | grep -q Mach-O; then echo "$f"; fi
done)
if codesign -d --entitlements - --xml "$app" 2>/dev/null | grep -q get-task-allow; then
    die "the app carries com.apple.security.get-task-allow"
fi
if [ -n "$identity" ] &&
    codesign -d --entitlements - --xml "$app" 2>/dev/null | grep -q disable-library-validation; then
    die "a Developer ID build must not disable library validation"
fi
# dSYMs ship in dist/, never in the app (they are larger than it).
if [ -n "$(find "$app" -name '*.dSYM' -print -quit)" ]; then
    die "a dSYM is inside the app"
fi
shipped_version=$(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist")
shipped_build=$(plutil -extract CFBundleVersion raw "$app/Contents/Info.plist")
[ "$shipped_version" = "$version" ] && [ "$shipped_build" = "$build_number" ] ||
    die "Info.plist says $shipped_version ($shipped_build), expected $version ($build_number)"

# The dSYMs must match what ships, or a crash report cannot be symbolicated
# with them: the UUIDs of each binary and its dSYM agree.
for dsym in "$archive"/dSYMs/*.dSYM; do
    ditto "$dsym" "$dsym_stage/$(basename "$dsym")"
done
check_uuid() {
    local binary=$1 dsym=$2 want got
    want=$(dwarfdump --uuid "$binary" | awk '{ print $2 }')
    got=$(dwarfdump --uuid "$dsym" | awk '{ print $2 }')
    [ -n "$want" ] && [ "$want" = "$got" ] || die "dSYM $dsym ($got) does not match $binary ($want)"
}
check_uuid "$app/Contents/MacOS/NeoSCAD" "$dsym_stage/NeoSCAD.app.dSYM"
check_uuid "$app/Contents/Frameworks/NeoSCADCore.framework/NeoSCADCore" \
    "$dsym_stage/NeoSCADCore.framework.dSYM"

# --- Notarization -------------------------------------------------------

# Submit, wait, and fail with Apple's log unless accepted.
notarize() {
    local file=$1 out id status
    out=$(xcrun notarytool submit "$file" --keychain-profile "$notary" --wait \
        --output-format plist)
    id=$(plutil -extract id raw - <<<"$out")
    status=$(plutil -extract status raw - <<<"$out")
    if [ "$status" != Accepted ]; then
        xcrun notarytool log "$id" --keychain-profile "$notary" >&2 || true
        die "notarization of $(basename "$file") ended $status (submission $id)"
    fi
    echo "notarized $(basename "$file") ($id)"
}

if [ -n "$notary" ]; then
    say "Notarize and staple the app"
    # The app on its own first, so its ticket can be stapled to it before
    # it goes into the DMG: a copy dragged out of the DMG then opens
    # offline on first launch.
    ditto -c -k --keepParent "$app" "$work/NeoSCAD.zip"
    notarize "$work/NeoSCAD.zip"
    xcrun stapler staple "$app"
    xcrun stapler validate "$app"
fi

say "Gatekeeper assessment of the app"
app_spctl=$(spctl -a -vv -t exec "$app" 2>&1 || true)
echo "$app_spctl"
if [ -n "$notary" ]; then
    grep -q accepted <<<"$app_spctl" || die "Gatekeeper rejects the notarized app"
elif [ -z "$identity" ]; then
    echo "(expected: ad-hoc signatures are rejected; set NEOSCAD_SIGN_IDENTITY and NEOSCAD_NOTARY_PROFILE for a distributable build)"
fi

# --- DMG --------------------------------------------------------------------

say "DMG"
dmg_root=$work/dmg
mkdir -p "$dmg_root"
ditto "$app" "$dmg_root/NeoSCAD.app"
ln -s /Applications "$dmg_root/Applications"
dmg=$dist/$name.dmg
# HFS+ and zlib: mountable on every macOS the app supports. No background
# image or window layout: those need Finder scripting (a prompt for
# automation access), and the two icons are the whole instruction.
hdiutil create -quiet -volname "NeoSCAD $version" -srcfolder "$dmg_root" \
    -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$dmg"
if [ -n "$identity" ]; then
    codesign --force --timestamp --sign "$sign_id" "$dmg"
fi
if [ -n "$notary" ]; then
    notarize "$dmg"
    xcrun stapler staple "$dmg"
    xcrun stapler validate "$dmg"
fi
hdiutil verify -quiet "$dmg"
dmg_spctl=$(spctl -a -vv -t open --context context:primary-signature "$dmg" 2>&1 || true)
echo "$dmg_spctl"

# --- The CLI ----------------------------------------------------------------

say "CLI (neoscad, arm64)"
# The same clean environment and target triple as build-core.sh, so the
# CLI shares the core's compiled dependencies and its deployment target,
# and a release build leaves the developer's target/release alone.
cargo_env=(env -i
    HOME="$HOME"
    PATH="$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin"
    MACOSX_DEPLOYMENT_TARGET=15.0
    CARGO_TARGET_DIR="$target_dir"
    TERM="${TERM:-dumb}")
if [ -n "${RUSTUP_HOME:-}" ]; then cargo_env+=(RUSTUP_HOME="$RUSTUP_HOME"); fi
if [ -n "${CARGO_HOME:-}" ]; then cargo_env+=(CARGO_HOME="$CARGO_HOME"); fi
"${cargo_env[@]}" cargo build --quiet --release --target aarch64-apple-darwin \
    -p neoscad-cli --bin neoscad
built=$target_dir/aarch64-apple-darwin/release/neoscad
cli=$cli_stage/neoscad
cp "$built" "$cli"
# The symbols first, then strip the copy that ships: the symbol table and
# the line tables' debug map only serve a debugger or a crash
# symbolicator, and the dSYM serves both. rustc writes the dSYM itself
# when the profile has debug info (split-debuginfo "packed", macOS's
# default); dsymutil makes one when it has not.
if [ -d "$built.dSYM" ]; then
    ditto "$built.dSYM" "$dsym_stage/neoscad.dSYM"
else
    dsymutil "$cli" -o "$dsym_stage/neoscad.dSYM"
fi
strip -x "$cli"
check_uuid "$cli" "$dsym_stage/neoscad.dSYM"
codesign --force --options runtime $timestamp --identifier org.neoscad.neoscad \
    --sign "$sign_id" "$cli"
codesign --verify --strict --verbose=2 "$cli"
cli_version=$("$cli" --version)
[ "$cli_version" = "neoscad $version" ] || die "the CLI says '$cli_version', expected 'neoscad $version'"
if [ -n "$notary" ]; then
    # A bare Mach-O cannot hold a stapled ticket; Gatekeeper finds the
    # notarization online on first run.
    ditto -c -k "$cli" "$work/neoscad.zip"
    notarize "$work/neoscad.zip"
fi
cli_spctl=$(spctl -a -vv -t exec "$cli" 2>&1 || true)
echo "$cli_spctl"
cp "$root/LICENSE" "$cli_stage/LICENSE"
cp "$cli" "$dist/neoscad"
# Owner and group normalised, and no AppleDouble files for the extended
# attributes, so the tarball unpacks the same for anyone.
COPYFILE_DISABLE=1 tar -C "$cli_stage" --uid 0 --gid 0 --uname root --gname wheel \
    -czf "$dist/$cli_name.tar.gz" neoscad LICENSE

# --- Symbols, record and checksums -----------------------------------------

say "dSYMs, BUILDINFO, SHA256SUMS"
(cd "$dsym_stage" && ditto -c -k --keepParent . "$dist/$name-dSYMs.zip")

app_bytes=$(du -sk "$app" | cut -f1)
# spctl's verdicts on one line, without this checkout's paths.
oneline() { sed -e "s|$work/||g" -e "s|$dist/||g" <<<"$1" | tr '\n' ' '; }
{
    echo "NeoSCAD $version, build $build_number"
    echo "commit:        $commit (uncommitted changes: $dirty)"
    echo "built:         $(date -u +%Y-%m-%dT%H:%M:%SZ) on macOS $(sw_vers -productVersion)"
    echo "toolchains:    $(xcodebuild -version | tr '\n' ' ')/ $("${cargo_env[@]}" rustc -V)"
    echo "signing:       $mode"
    echo "notarized:     $([ -n "$notary" ] && echo "yes ($notary)" || echo no)"
    echo "app size:      $((app_bytes / 1024)) MB unpacked"
    echo
    echo "codesign --verify --deep --strict: passed; hardened runtime on every Mach-O"
    echo "spctl (app):   $(oneline "$app_spctl")"
    echo "spctl (dmg):   $(oneline "$dmg_spctl")"
    echo "spctl (cli):   $(oneline "$cli_spctl")"
} >"$dist/BUILDINFO.txt"
(cd "$dist" && shasum -a 256 "$name.dmg" "$cli_name.tar.gz" neoscad "$name-dSYMs.zip" \
    BUILDINFO.txt >SHA256SUMS)

if [ $smoke = 1 ]; then
    say "Smoke test"
    scripts/apple/smoke-release.sh "$dmg" "$dist/neoscad" "$version"
fi

# The next run starts from a fresh DerivedData anyway, and this one is
# ~500 MB; the archive stays for Xcode's Organizer and for inspection.
rm -rf "$work/DerivedData" "$dsym_stage"

say "Done: $dist"
(cd "$dist" && ls -l)
echo "app: $((app_bytes / 1024)) MB unpacked; signing: $mode"
