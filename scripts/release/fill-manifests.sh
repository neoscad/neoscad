#!/usr/bin/env bash
# Fill the package-manager templates in packaging/ for one release.
#
#   scripts/release/fill-manifests.sh VERSION SUMS_DIR OUT_DIR
#
# SUMS_DIR holds cargo-dist's per-archive checksum files
# (neoscad-cli-<target>.<ext>.sha256, "HASH *NAME"). Each fills the
# placeholder @SHA256_<TARGET>@, the target upper-cased with `-` as `_`
# (@SHA256_X86_64_PC_WINDOWS_MSVC@). @VERSION@ is VERSION. The Homebrew
# cask for the app is filled too when NEOSCAD_BUILD (the DMG's build
# number) and NEOSCAD_DMG_SHA256 are set.
#
# Writes to OUT_DIR: PKGBUILD (AUR neoscad-bin), neoscad.json (Scoop),
# the three NeoSCAD.NeoSCAD*.yaml (winget) and neoscad-app.rb (cask).
# Fails, naming them, if any placeholder is left unfilled, so a missing
# archive cannot produce a manifest that points at nothing.
set -euo pipefail

[[ $# -eq 3 ]] || { sed -n '4p' "$0" >&2; exit 2; }
version=$1
sums=$2
out=$3
root=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$out"

subst=(-e "s|@VERSION@|$version|g")
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
    subst+=(-e "s|@SHA256_${key}@|$hash|g")
done

templates=(
    packaging/aur/PKGBUILD
    packaging/scoop/neoscad.json
    packaging/winget/NeoSCAD.NeoSCAD.yaml
    packaging/winget/NeoSCAD.NeoSCAD.installer.yaml
    packaging/winget/NeoSCAD.NeoSCAD.locale.en-US.yaml
)
if [[ -n "${NEOSCAD_BUILD:-}" && -n "${NEOSCAD_DMG_SHA256:-}" ]]; then
    subst+=(-e "s|@BUILD@|$NEOSCAD_BUILD|g" -e "s|@SHA256_DMG@|$NEOSCAD_DMG_SHA256|g")
    templates+=(packaging/homebrew/neoscad-app.rb)
fi

status=0
for t in "${templates[@]}"; do
    dest=$out/$(basename "$t")
    sed "${subst[@]}" "$root/$t" >"$dest"
    if left=$(grep -o '@[A-Z0-9_]*@' "$dest" | sort -u | tr '\n' ' ') && [[ -n "$left" ]]; then
        echo "$dest: unfilled $left" >&2
        status=1
    fi
done
exit $status
