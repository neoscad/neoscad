#!/usr/bin/env bash
# Packages @neoscad/viewer, the npm package that renders a SCAD model in any
# web page (web/package; docs/js-package.md):
#
#   dist/npm/neoscad-viewer-<version>/        the package directory, as packed
#   dist/npm/neoscad-viewer-<version>.tgz     `npm pack` of it
#
# Like build.sh it packages and does not build: the core and both viewers
# must be built first, and it fails without them (a package without its
# engine would publish nothing useful):
#
#   scripts/web/build-core.sh                                   dist/web-core
#   scripts/web/build-view.sh --no-webgl --out dist/web-view/webgpu
#   scripts/web/build-view.sh --out dist/web-view/webgl
#
# ($NEOSCAD_WEB_CORE, $NEOSCAD_WEB_VIEW and $NEOSCAD_WEB_VIEW_WEBGL name
# other directories.) It adds fonts.tar.gz and bosl2.tar.gz (BOSL2 from the
# reference checkout, required here), THIRD-PARTY-LICENSES.txt and
# SOURCE.txt (the GPL's source offer, with the commit and BOSL2's), checks
# that no file names a local path, and prints the sizes. Publishing is the
# owner's step (docs/release.md).
#
#   scripts/web/build-package.sh
set -euo pipefail
cd "$(dirname "$0")/../.."
root=$PWD

if [ -f "$HOME/.cargo/env" ] && ! command -v cargo >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
full_sha=$(git rev-parse HEAD)
dirty=""
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then dirty="-dirty"; fi
epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct HEAD)}
# shellcheck source=scripts/web/common.sh
source "$root/scripts/web/common.sh"

core=${NEOSCAD_WEB_CORE:-$root/dist/web-core}
view=${NEOSCAD_WEB_VIEW:-$root/dist/web-view/webgpu}
view_webgl=${NEOSCAD_WEB_VIEW_WEBGL:-$root/dist/web-view/webgl}
for f in "$core/worker.js" "$core/neoscad_web_bg.wasm" "$view/web_view_bg.wasm" "$view_webgl/web_view_bg.wasm"; do
    if [ ! -f "$f" ]; then
        echo "error: $f is missing: build the core and both viewers first (see the top of this script)" >&2
        exit 1
    fi
done
bosl2=$(find_bosl2)
if [ -z "$bosl2" ]; then
    echo "error: no .reference/BOSL2 (CLAUDE.md, \"Reference checkout\"), so no bosl2.tar.gz" >&2
    exit 1
fi
bosl2_sha=$(git -C "$bosl2" rev-parse HEAD)

npm_dir=$root/dist/npm
name=neoscad-viewer-$version
out=$npm_dir/$name
rm -rf "$out" "$npm_dir/$name.tgz"
mkdir -p "$npm_dir"

if [ ! -d "$root/web/package/node_modules" ] || [ "$root/web/package/package-lock.json" -nt "$root/web/package/node_modules" ]; then
    (cd "$root/web/package" && npm ci --no-audit --no-fund)
    touch "$root/web/package/node_modules"
fi

stage=$(mktemp -d)
fonts_tar "$stage/fonts.tar.gz"
bosl2_tar "$stage/bosl2.tar.gz" "$bosl2"
(cd "$root/web/package" && node build.mjs --out "$out" --version "$version" \
    --core "$core" --view "$view" --view-webgl "$view_webgl" \
    --fonts "$stage/fonts.tar.gz" --bosl2 "$stage/bosl2.tar.gz")
rm -rf "$stage"

# --- Licences ------------------------------------------------------------------
tp_title="Third-party software in the @neoscad/viewer npm package" \
    tp_license="NeoSCAD's own code in this package is GPL-2.0-or-later WITH Classpath-exception-2.0 (LICENSE); see SOURCE.txt for its source. The components below keep their own licences." \
    tp_npm_intro="The package's JavaScript (dist/index.js, dist/element.js) is NeoSCAD's own and bundles no npm packages." \
    tp_npm_file="" tp_prefix="dist/assets/" tp_engine=wasm tp_viewer=wasm tp_bosl2="$bosl2" \
    third_party_licenses >"$out/THIRD-PARTY-LICENSES.txt"

# --- The GPL source offer --------------------------------------------------------
source_url=${NEOSCAD_SOURCE_URL:-"https://github.com/neoscad/neoscad/tree/$full_sha"}
cat >"$out/SOURCE.txt" <<EOF
@neoscad/viewer $version, built from NeoSCAD commit $full_sha$dirty.

NeoSCAD is free software under the GNU General Public License, version 2
or (at your option) any later version; this package's NeoSCAD code is
also under the Classpath exception (LICENSE). The complete corresponding
source code of this package, including the scripts used to build it
(scripts/web/build-package.sh, web/package/build.mjs), is NeoSCAD at that
commit:

    $source_url

dist/assets/bosl2.tar.gz is BOSL2 at commit $bosl2_sha
(https://github.com/BelfrySCAD/BOSL2/tree/$bosl2_sha).

If you cannot obtain it there, write to source@neoscad.org, and the
NeoSCAD project will provide it for three years from the date of this
build, for no more than the cost of distribution.
EOF

# --- No local paths -----------------------------------------------------------------
no_local_paths "$out" "the package" || exit 1

# --- Pack -------------------------------------------------------------------------------
# npm pack stamps every entry with a fixed date, so packing the same
# directory again gives the same bytes (checked for 0.7.0).
(cd "$npm_dir" && npm pack --silent "$out" >/dev/null)
packed=$npm_dir/neoscad-viewer-$version.tgz
if [ ! -f "$packed" ]; then
    echo "error: npm pack wrote no $packed" >&2
    exit 1
fi

echo "sizes (raw, gzip -9):"
(cd "$out" && find . -type f | LC_ALL=C sort | while read -r f; do
    printf '  %-40s %10s %10s\n' "${f#./}" "$(wc -c <"$f" | tr -d ' ')" "$(gzip -9 -c "$f" | wc -c | tr -d ' ')"
done)
echo "unpacked: $(find "$out" -type f -exec cat {} + | wc -c | tr -d ' ') bytes in $(find "$out" -type f | wc -l | tr -d ' ') files"
echo "tarball:  $packed ($(wc -c <"$packed" | tr -d ' ') bytes)"
if [ -n "$dirty" ]; then
    echo "note: the working tree has uncommitted changes; SOURCE.txt names HEAD with -dirty" >&2
fi
