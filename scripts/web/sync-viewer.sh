#!/usr/bin/env bash
# Copies a built @neoscad/viewer package into the website's /viewer/
# directory, where the home page's live hero loads it from, and the hero
# model beside it, as ../neoscad-website/README.md ("The live hero")
# describes:
#
#   scripts/web/sync-viewer.sh PKG_DIR [WEBSITE_DIR]
#
# PKG_DIR is the package directory scripts/web/build-package.sh makes,
# dist/npm/neoscad-viewer-<version>. WEBSITE_DIR defaults to
# ../neoscad-website (beside this repository). Its viewer/ is replaced by
# the package's dist/ (the bundles and every asset: core, both viewers,
# fonts, BOSL2) with the package's licences and source offer, and
# viewer/VIEWER.txt records the pin: the package's name and version, and
# the sha256 of its tarball when one is beside PKG_DIR. apple/Icon/hero.scad
# goes to assets/hero.scad, so the page renders the model this checkout
# has (its `body` and `hero_kinematics` are what hero.js relies on).
#
# It changes files only; review and commit in the website repository.
set -euo pipefail
cd "$(dirname "$0")/../.."
root=$PWD

pkg=${1:?usage: scripts/web/sync-viewer.sh PKG_DIR [WEBSITE_DIR]}
site=${2:-$root/../neoscad-website}
pkg=$(cd "$pkg" && pwd)
name=$(basename "$pkg")

if [ ! -f "$site/site.json" ] || [ ! -f "$site/theme.css" ]; then
    echo "error: $site does not look like the website (no site.json and theme.css)" >&2
    exit 1
fi
for f in package.json LICENSE THIRD-PARTY-LICENSES.txt SOURCE.txt dist/index.js \
    dist/assets/core/worker.js dist/assets/core/neoscad_web_bg.wasm \
    dist/assets/view/web_view_bg.wasm dist/assets/view-webgl/web_view_bg.wasm \
    dist/assets/bosl2.tar.gz; do
    if [ ! -f "$pkg/$f" ]; then
        echo "error: $pkg has no $f (build it with scripts/web/build-package.sh)" >&2
        exit 1
    fi
done
# The mock build (web/package/build.mjs --mock) has no viewer and a fake
# engine; it must never reach the site.
if grep -q '"0.0.0-mock"' "$pkg/dist/index.js" "$pkg/dist/assets/core/worker.js" 2>/dev/null; then
    echo "error: $pkg is the mock build" >&2
    exit 1
fi

version=$(sed -n 's/^  "version": "\(.*\)",$/\1/p' "$pkg/package.json" | head -n 1)
tarball="$pkg.tgz"
sha="-"
if [ -f "$tarball" ]; then
    sha=$(shasum -a 256 "$tarball" | cut -d ' ' -f 1)
fi

# Fill a staging directory beside viewer/ first, so a failure leaves the
# old one in place.
staging=$(mktemp -d "$site/.viewer-staging.XXXXXX")
trap 'rm -rf "$staging"' EXIT
cp -R "$pkg/dist/." "$staging/"
cp "$pkg/LICENSE" "$pkg/THIRD-PARTY-LICENSES.txt" "$pkg/SOURCE.txt" "$pkg/package.json" "$staging/"
cp "$root/apple/Icon/hero.scad" "$staging/.hero.scad"
# The site is public: refuse a package that names a home directory.
if leaks=$(LC_ALL=C grep -rlaE "/(Users|home)/[^/]+/" "$staging"); then
    echo "error: the package contains local paths:" >&2
    echo "$leaks" | sed "s|^$staging/|  |" >&2
    exit 1
fi
echo "@neoscad/viewer $version ($name.tgz sha256 $sha)" > "$staging/VIEWER.txt"
mv "$staging/.hero.scad" "$site/assets/hero.scad"
rm -rf "$site/viewer"
mv "$staging" "$site/viewer"
trap - EXIT
chmod 755 "$site/viewer"

echo "$site/viewer <- $name ($sha)"
echo "$site/assets/hero.scad <- apple/Icon/hero.scad"
