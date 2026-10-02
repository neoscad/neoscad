#!/usr/bin/env bash
# Sparkle's command-line tools (sign_update, generate_keys, ...) from
# Sparkle's release, checked against a pinned SHA-256:
#
#   scripts/release/sparkle-tools.sh DIR     # DIR/bin/sign_update, ...
#
# The version is the one apple/project.yml embeds in the app, so the tool
# that signs the appcast and the DMGs is the release the app verifies them
# with. Used by update-feed.yml's `appcast` job and by
# scripts/apple/test-updates.sh. macOS only (the tools are Mach-O).
set -euo pipefail

version=2.10.0
sha256=c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c

[[ $# -eq 1 ]] || { sed -n '5p' "$0" >&2; exit 2; }
dir=$1
root=$(cd "$(dirname "$0")/../.." && pwd)

# A version bump in one place and not the other would sign with tools the
# app's Sparkle never saw; cheap to refuse.
pinned=$(sed -n '/^packages:/,/^[a-z]/s/^ *exactVersion: *\([0-9.]*\) *$/\1/p' "$root/apple/project.yml")
[[ "$pinned" == "$version" ]] || {
    echo "sparkle-tools: apple/project.yml pins Sparkle $pinned, this script $version" >&2
    exit 1
}

if [[ -x "$dir/bin/sign_update" && "$(cat "$dir/VERSION" 2>/dev/null)" == "$version" ]]; then
    exit 0
fi
mkdir -p "$dir"
archive=$dir/Sparkle-$version.tar.xz
curl -fsSL -o "$archive" \
    "https://github.com/sparkle-project/Sparkle/releases/download/$version/Sparkle-$version.tar.xz"
echo "$sha256  $archive" | shasum -a 256 -c --quiet
tar -xf "$archive" -C "$dir" ./bin/sign_update ./bin/generate_keys
rm -f "$archive"
echo "$version" >"$dir/VERSION"
"$dir/bin/sign_update" --help >/dev/null
echo "Sparkle $version tools in $dir/bin"
