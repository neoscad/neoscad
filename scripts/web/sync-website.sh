#!/usr/bin/env bash
# Copies a built web demo bundle into the website's /try/ directory, as
# ../neoscad-website/README.md ("The /try bundle") describes:
#
#   scripts/web/sync-website.sh TARBALL [WEBSITE_DIR]
#
# TARBALL is dist/web/neoscad-web-<version>-<sha>.tar.gz; its checksum is
# checked against the SHA256SUMS beside it. WEBSITE_DIR defaults to
# ../neoscad-website (beside this repository). Its try/ is replaced by the
# unpacked bundle, and try/BUNDLE.txt records the pin: the bundle's name
# and sha256. The website keeps its own site.json and theme.css at its
# root; the bundle reads them from there.
#
# It changes files only; review and commit in the website repository.
set -euo pipefail
cd "$(dirname "$0")/../.."
root=$PWD

tarball=${1:?usage: scripts/web/sync-website.sh TARBALL [WEBSITE_DIR]}
site=${2:-$root/../neoscad-website}
tarball=$(cd "$(dirname "$tarball")" && pwd)/$(basename "$tarball")
name=$(basename "$tarball" .tar.gz)

if [ ! -f "$site/site.json" ] || [ ! -f "$site/theme.css" ]; then
    echo "error: $site does not look like the website (no site.json and theme.css)" >&2
    exit 1
fi

sums=$(dirname "$tarball")/SHA256SUMS
actual=$(shasum -a 256 "$tarball" | cut -d ' ' -f 1)
expected=$(awk -v f="$name.tar.gz" '$2 == f { print $1 }' "$sums" 2>/dev/null || true)
if [ -z "$expected" ]; then
    echo "error: $name.tar.gz is not listed in $sums" >&2
    exit 1
fi
if [ "$actual" != "$expected" ]; then
    echo "error: $name.tar.gz has sha256 $actual, SHA256SUMS says $expected" >&2
    exit 1
fi

# Unpack beside try/ first, so a failed unpack leaves the old one in place.
staging=$(mktemp -d "$site/.try-staging.XXXXXX")
trap 'rm -rf "$staging"' EXIT
tar -xzf "$tarball" -C "$staging" --strip-components 1
for f in index.html SOURCE.txt THIRD-PARTY-LICENSES.txt build.json; do
    if [ ! -f "$staging/$f" ]; then
        echo "error: the bundle has no $f" >&2
        exit 1
    fi
done
# The site is public: refuse a bundle that names a home directory (a
# bundle made before scripts/web/remap-paths.sh mapped every path rustc
# writes, or built by hand).
if leaks=$(LC_ALL=C grep -rlaE "/(Users|home)/[^/]+/" "$staging"); then
    echo "error: the bundle contains local paths:" >&2
    echo "$leaks" | sed "s|^$staging/|  |" >&2
    exit 1
fi
echo "$name $actual" > "$staging/BUNDLE.txt"
rm -rf "$site/try"
mv "$staging" "$site/try"
trap - EXIT
chmod 755 "$site/try"

echo "$site/try <- $name ($actual)"
