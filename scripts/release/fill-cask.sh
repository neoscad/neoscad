#!/usr/bin/env bash
# Fill the Homebrew cask for the macOS app (packaging/homebrew/neoscad-app.rb)
# from the release's DMG.
#
#   scripts/release/fill-cask.sh VERSION DMG OUT_FILE
#
# VERSION is the release's version without the tag's `v` (0.1.0-rc.1). DMG
# is scripts/apple/release.sh's NeoSCAD-<version>-<build>.dmg: the build
# number is read from its name and the checksum from its contents, so the
# cask describes exactly the file that was attached to the release.
#
# The name must carry VERSION: the cask's URL is rebuilt from the version
# (tag v<version>, file NeoSCAD-<version>-<build>.dmg), so a DMG built from
# a different Cargo version than the tag would give a cask pointing at a
# file that does not exist. Fails, too, if any placeholder is left unfilled.
set -euo pipefail

[[ $# -eq 3 ]] || { sed -n '5p' "$0" >&2; exit 2; }
version=$1
dmg=$2
out=$3
root=$(cd "$(dirname "$0")/../.." && pwd)

[[ -f "$dmg" ]] || { echo "no DMG at $dmg" >&2; exit 1; }
name=$(basename "$dmg")
stem=${name%.dmg}
build=${stem##*-}
# The build number is `git rev-list --count HEAD`: digits only, which also
# keeps it from swallowing part of a hyphenated prerelease version.
if [[ "$name" != "NeoSCAD-$version-$build.dmg" || ! "$build" =~ ^[0-9]+$ ]]; then
    echo "$name is not NeoSCAD-$version-<build>.dmg" >&2
    exit 1
fi
sha=$(shasum -a 256 "$dmg" | awk '{print $1}')
[[ "$sha" =~ ^[0-9a-f]{64}$ ]] || { echo "bad checksum for $dmg" >&2; exit 1; }

mkdir -p "$(dirname "$out")"
sed -e "s|@VERSION@|$version|g" -e "s|@BUILD@|$build|g" -e "s|@SHA256_DMG@|$sha|g" \
    "$root/packaging/homebrew/neoscad-app.rb" >"$out"
if left=$(grep -o '@[A-Z0-9_]*@' "$out" | sort -u | tr '\n' ' ') && [[ -n "$left" ]]; then
    echo "$out: unfilled $left" >&2
    exit 1
fi
