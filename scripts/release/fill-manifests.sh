#!/usr/bin/env bash
# Fill the package-manager templates in packaging/ for one release.
#
#   scripts/release/fill-manifests.sh VERSION SUMS_DIR OUT_DIR
#   scripts/release/fill-manifests.sh --winget VERSION MSI_DIR OUT_DIR
#
# The first form: SUMS_DIR holds cargo-dist's per-archive checksum files
# (neoscad-cli-<target>.<ext>.sha256, "HASH *NAME"). Each fills the
# placeholder @SHA256_<TARGET>@, the target upper-cased with `-` as `_`
# (@SHA256_X86_64_PC_WINDOWS_MSVC@). @VERSION@ is VERSION, and @PKGVER@
# is VERSION without hyphens (see below). Writes PKGBUILD (AUR
# neoscad-bin) and neoscad.json (Scoop) to OUT_DIR. Scoop installs the
# Windows CLI zips (unsigned; see Cargo.toml's [workspace.metadata.dist]);
# a release built without Windows targets has no Windows checksum in
# SUMS_DIR at all, and the Scoop manifest is then skipped with a note
# rather than failed. Run by publish-packages.yml.
#
# The second form writes the three NeoSCAD.NeoSCAD*.yaml (winget) from
# the desktop app's MSIs, NeoSCAD-VERSION-windows-{x64,arm64}.msi in
# MSI_DIR. winget installs the app, not the CLI zip: the CLI run with no
# arguments exits 2, the likely cause of 0.1.0's failed winget
# validation. Each MSI gives its SHA-256 (checked against the .sha256
# beside it, when there is one) and its ProductCode and UpgradeCode, read
# from its Property table with `msiinfo` (msitools: `apt-get install
# msitools`, `brew install msitools`). ProductCode can come from nowhere
# else: windows/installer/NeoSCAD.wxs sets none, so WiX makes a new one
# in every build, and a manifest whose ProductCode is not the MSI's
# leaves winget unable to match the install for upgrade and uninstall.
# @RELEASE_DATE@ is the environment's RELEASE_DATE, or today's UTC date.
# Run by windows-installer.yml once both MSIs are attached; it cannot run
# in publish-packages.yml, which starts alongside the MSI build rather
# than after it.
#
# The app's Homebrew cask is not here: it needs the DMG, and
# scripts/release/fill-cask.sh fills it in the macOS app job
# (.github/workflows/publish-macos-app.yml), which pushes it to the tap.
#
# Either form fails, naming them, if any placeholder is left unfilled, so
# a missing archive cannot produce a manifest that points at nothing.
set -euo pipefail

winget=0
if [[ ${1:-} == --winget ]]; then
    winget=1
    shift
fi
[[ $# -eq 3 ]] || { sed -n '4,5p' "$0" >&2; exit 2; }
version=$1
sums=$2
out=$3
root=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$out"

# Fill each template into OUT_DIR with the sed expressions in $subst;
# fail at the end if any placeholder is left. For winget the template's
# own comment above the schema line (which says it is a template) is
# dropped, since winget-pkgs takes the files as they are.
fill() {
    local status=0 t dest left
    for t in "$@"; do
        dest=$out/$(basename "$t")
        if [[ $winget -eq 1 ]]; then
            sed -n '/^# yaml-language-server:/,$p' "$root/$t" | sed "${subst[@]}" >"$dest"
        else
            sed "${subst[@]}" "$root/$t" >"$dest"
        fi
        if left=$(grep -o '@[A-Z0-9_]*@' "$dest" | sort -u | tr '\n' ' ') && [[ -n "$left" ]]; then
            echo "$dest: unfilled $left" >&2
            status=1
        fi
    done
    return $status
}

if [[ $winget -eq 1 ]]; then
    command -v msiinfo >/dev/null || { echo "msiinfo not found: install msitools" >&2; exit 1; }
    sha256() {
        if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1
    }
    subst=(-e "s|@VERSION@|$version|g" -e "s|@RELEASE_DATE@|${RELEASE_DATE:-$(date -u +%Y-%m-%d)}|g")
    # An MSI's ProductVersion is numeric only (build-msi.ps1 drops a
    # prerelease suffix), so 0.2.0-rc.1's MSIs say 0.2.0.
    numeric=${version%%-*}
    guid='^\{[0-9A-F]{8}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{4}-[0-9A-F]{12}\}$'
    for arch in x64 arm64; do
        msi=$sums/NeoSCAD-$version-windows-$arch.msi
        [[ -f "$msi" ]] || { echo "no $msi" >&2; exit 1; }
        key=$(tr 'a-z' 'A-Z' <<<"$arch")
        hash=$(sha256 "$msi")
        if [[ -f "$msi.sha256" ]] && [[ "$(awk '{print $1; exit}' "$msi.sha256")" != "$hash" ]]; then
            echo "$msi does not match $msi.sha256" >&2
            exit 1
        fi
        # `msiinfo export` writes the table as tab-separated, CRLF-ended
        # rows after a header of column names, types and the key.
        props=$(msiinfo export "$msi" Property | tr -d '\r')
        product=$(awk -F '\t' '$1 == "ProductCode" { print $2; exit }' <<<"$props")
        upgrade=$(awk -F '\t' '$1 == "UpgradeCode" { print $2; exit }' <<<"$props")
        msiversion=$(awk -F '\t' '$1 == "ProductVersion" { print $2; exit }' <<<"$props")
        [[ "$product" =~ $guid ]] || { echo "$msi: bad ProductCode '$product'" >&2; exit 1; }
        [[ "$upgrade" =~ $guid ]] || { echo "$msi: bad UpgradeCode '$upgrade'" >&2; exit 1; }
        # The file name is all that ties an MSI to its manifest entry, so
        # it is checked against what the MSI says of itself: an MSI of
        # another version or architecture would otherwise be published
        # under the wrong entry. The summary's Template is
        # "<platform>;<language>" (x64, Arm64).
        [[ "$msiversion" == "$numeric" ]] \
            || { echo "$msi: ProductVersion '$msiversion', expected $numeric" >&2; exit 1; }
        platform=$(msiinfo suminfo "$msi" | awk -F ': ' '$1 == "Template" { split($2, a, ";"); print a[1]; exit }')
        [[ "$(tr 'A-Z' 'a-z' <<<"$platform")" == "$arch" ]] \
            || { echo "$msi: platform '$platform', expected $arch" >&2; exit 1; }
        subst+=(
            -e "s|@SHA256_${key}@|$(tr 'a-f' 'A-F' <<<"$hash")|g"
            -e "s|@PRODUCT_CODE_${key}@|$product|g"
            -e "s|@UPGRADE_CODE_${key}@|$upgrade|g"
        )
    done
    fill packaging/winget/NeoSCAD.NeoSCAD.yaml \
        packaging/winget/NeoSCAD.NeoSCAD.installer.yaml \
        packaging/winget/NeoSCAD.NeoSCAD.locale.en-US.yaml
    exit
fi

# @PKGVER@ is the AUR's pkgver: makepkg rejects a hyphen there, and
# without it a prerelease still sorts first (vercmp: 0.1.0rc.1 < 0.1.0).
subst=(-e "s|@VERSION@|$version|g" -e "s|@PKGVER@|${version//-/}|g")
windows=0
shopt -s nullglob
for f in "$sums"/neoscad-cli-*.sha256; do
    name=$(basename "$f" .sha256)
    target=${name#neoscad-cli-}
    target=${target%.tar.xz}
    target=${target%.tar.gz}
    target=${target%.zip}
    target=${target%.msi}
    [[ "$name" == *.msi ]] && continue
    hash=$(awk '{print $1; exit}' "$f")
    [[ "$hash" =~ ^[0-9a-f]{64}$ ]] || { echo "bad checksum in $f" >&2; exit 1; }
    key=$(tr 'a-z-' 'A-Z_' <<<"$target")
    [[ "$target" == *-pc-windows-* ]] && windows=1
    subst+=(-e "s|@SHA256_${key}@|$hash|g")
done

templates=(packaging/aur/PKGBUILD)
if [[ $windows -eq 1 ]]; then
    templates+=(packaging/scoop/neoscad.json)
else
    echo "no Windows archives in $sums: skipping the Scoop manifest" >&2
fi
fill "${templates[@]}"
