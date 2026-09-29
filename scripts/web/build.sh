#!/usr/bin/env bash
# Builds the web demo bundle (docs/web-demo-plan.md, "Build and hosting"):
#
#   dist/web/neoscad-web-<version>-<sha>/     the self-contained bundle
#   dist/web/neoscad-web-<version>-<sha>.tar.gz
#   dist/web/neoscad-web-<version>-<sha>-source.tar.gz   (git archive of HEAD)
#   dist/web/SHA256SUMS
#
# The bundle holds index.html, app.js, app.css, the examples, bosl2.tar.gz
# (fetched by the page on first `include <BOSL2/...>`), build.json,
# THIRD-PARTY-LICENSES.txt and SOURCE.txt, plus the engine and the viewer
# when they have been built:
#
#   core/  the wasm core and its module worker (core/worker.js), from
#          scripts/web/build-core.sh; its output directory is
#          $NEOSCAD_WEB_CORE, default target/web/core
#   view/  the WebGPU viewer (view/neoscad_web_view.js and its wasm), from
#          scripts/web/build-view.sh; $NEOSCAD_WEB_VIEW, default
#          target/web/view
#
# Without a core the bundle uses the mock worker, and the page says so in
# a banner; without a viewer it draws with its canvas fallback. Every URL
# in the bundle is relative, so it can be unpacked under any path (the
# website's /try/: scripts/web/sync-website.sh).
#
# Node 18 or newer. `npm ci` runs (in apple/Editor/web and web) only when
# a lockfile is newer than its node_modules: the only step that needs the
# network.
#
#   scripts/web/build.sh
set -euo pipefail
cd "$(dirname "$0")/../.."
root=$PWD

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
sha=$(git rev-parse --short=10 HEAD)
full_sha=$(git rev-parse HEAD)
dirty=""
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then dirty="-dirty"; fi
name="neoscad-web-$version-$sha$dirty"
dist=$root/dist/web
out=$dist/$name

core=${NEOSCAD_WEB_CORE:-$root/target/web/core}
view=${NEOSCAD_WEB_VIEW:-$root/target/web/view}

# BOSL2: the reference checkout, which is gitignored and so lives only in
# the main checkout; a worktree finds it through the shared git directory.
bosl2=""
for dir in "$root/.reference/BOSL2" "$(cd "$(git rev-parse --git-common-dir)/.." && pwd)/.reference/BOSL2"; do
    if [ -f "$dir/std.scad" ]; then bosl2=$dir; break; fi
done

npm_ci() {
    local dir=$1
    if [ ! -d "$dir/node_modules" ] || [ "$dir/package-lock.json" -nt "$dir/node_modules" ]; then
        (cd "$dir" && npm ci --no-audit --no-fund)
        touch "$dir/node_modules"
    fi
}
npm_ci "$root/apple/Editor/web"
npm_ci "$root/web"

engine=mock
if [ -f "$core/worker.js" ]; then engine=wasm; fi
viewer=none
if [ -f "$view/neoscad_web_view.js" ]; then viewer=wasm; fi

rm -rf "$out" "$out.tar.gz"
(cd "$root/web" && node build.mjs --out "$out" --engine "$engine" --view "$viewer" --version "$version" --sha "$sha$dirty")

if [ "$engine" = wasm ]; then cp -R "$core" "$out/core"; fi
if [ "$viewer" = wasm ]; then cp -R "$view" "$out/view"; fi

if [ -n "$bosl2" ]; then
    # Only the library: its .scad files and licence, not its docs, tests
    # or images. COPYFILE_DISABLE keeps macOS's ._ files out of the tar.
    (cd "$bosl2/.." && COPYFILE_DISABLE=1 tar -czf "$out/bosl2.tar.gz" BOSL2/LICENSE BOSL2/*.scad)
else
    echo "warning: no .reference/BOSL2, so bosl2.tar.gz is missing and the BOSL2 examples will fail" >&2
fi

# --- Licences ------------------------------------------------------------
{
    echo "Third-party software in this NeoSCAD web bundle"
    echo "================================================"
    echo
    echo "NeoSCAD itself is GPL-2.0-or-later; see SOURCE.txt for its source."
    echo
    echo "The front end bundles these npm packages (app.js):"
    echo
    cat "$out/THIRD-PARTY-LICENSES-web.txt"
    if [ -f "$out/core/THIRD-PARTY-LICENSES.txt" ]; then
        echo
        echo "------------------------------------------------------------------------"
        echo "The engine (core/) includes:"
        echo
        cat "$out/core/THIRD-PARTY-LICENSES.txt"
    fi
    if [ -f "$out/view/THIRD-PARTY-LICENSES.txt" ]; then
        echo
        echo "------------------------------------------------------------------------"
        echo "The viewer (view/) includes:"
        echo
        cat "$out/view/THIRD-PARTY-LICENSES.txt"
    fi
    if [ "$engine" = wasm ]; then
        echo
        echo "------------------------------------------------------------------------"
        echo "The engine embeds the Liberation fonts 2.00.1 (SIL Open Font License 1.1):"
        echo
        cat "$root/assets/fonts/Liberation-2.00.1/LICENSE"
        echo
        echo "------------------------------------------------------------------------"
        echo "The engine embeds the MCAD library (GNU LGPL 2.1; some files allow more"
        echo "permissive terms, as their comments say) and OpenSCAD's colour schemes"
        echo "(GNU GPL 2 or later). See assets/README.md in the source."
        echo
        cat "$root/assets/libraries/MCAD/lgpl-2.1.txt"
    fi
    if [ -f "$out/bosl2.tar.gz" ]; then
        echo
        echo "------------------------------------------------------------------------"
        echo "bosl2.tar.gz is BOSL2 (https://github.com/BelfrySCAD/BOSL2):"
        echo
        cat "$bosl2/LICENSE"
    fi
    echo
    echo "------------------------------------------------------------------------"
    echo "examples/: the OpenSCAD examples (CSG, sign, GEB, example024) are"
    echo "dedicated to the public domain under CC0 1.0 (examples/COPYING-CC0.txt);"
    echo "box-lid.scad is CC0 too; helical-gear.scad is a BOSL2 example"
    echo "(BSD-2-Clause, above); threaded-ring.scad and gearbox.scad are NeoSCAD's"
    echo "(GPL-2.0-or-later)."
} > "$out/THIRD-PARTY-LICENSES.txt"
rm "$out/THIRD-PARTY-LICENSES-web.txt"

# --- The GPL source offer --------------------------------------------------
source_url=${NEOSCAD_SOURCE_URL:-"(the NeoSCAD repository; not yet public)"}
cat > "$out/SOURCE.txt" <<EOF
NeoSCAD web demo $version, built from commit $full_sha$dirty.

NeoSCAD is free software under the GNU General Public License, version 2
or (at your option) any later version. The complete corresponding source
code of this build, including the scripts used to build it
(scripts/web/build.sh), is NeoSCAD at that commit:

    $source_url

It is also published beside this bundle as $name-source.tar.gz.
If you cannot obtain it there, write to the NeoSCAD project, which will
provide it for three years from the date of this build, for no more than
the cost of distribution.
EOF

# --- Archives and checksums ------------------------------------------------
git archive --format=tar.gz --prefix="$name-source/" -o "$dist/$name-source.tar.gz" HEAD
(cd "$dist" && COPYFILE_DISABLE=1 tar -czf "$name.tar.gz" "$name")
(cd "$dist" && shasum -a 256 "$name.tar.gz" "$name-source.tar.gz" > SHA256SUMS)

echo "bundle:   $out ($engine engine, $viewer view)"
echo "tarball:  $dist/$name.tar.gz"
echo "checksum: $(cat "$dist/SHA256SUMS" | head -n 1)"
if [ -n "$dirty" ]; then
    echo "note: the working tree has uncommitted changes; the source tarball is HEAD without them" >&2
fi
