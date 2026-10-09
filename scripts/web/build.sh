#!/usr/bin/env bash
# Builds the web demo bundle (docs/web-demo-plan.md, "Build and hosting"):
#
#   dist/web/neoscad-web-<version>-<sha>/     the self-contained bundle
#   dist/web/neoscad-web-<version>-<sha>.tar.gz
#   dist/web/neoscad-web-<version>-<sha>-source.tar.gz   (git archive of HEAD,
#       for a release to attach; SOURCE.txt points at the repository)
#   dist/web/SHA256SUMS
#
# The bundle holds index.html, app.js, app.css, the examples, bosl2.tar.gz
# (fetched by the page on first `include <BOSL2/...>`), fonts.tar.gz (the
# Liberation fonts, fetched when a model first draws text; with a core
# only, since the mock draws no text), build.json,
# THIRD-PARTY-LICENSES.txt and SOURCE.txt, plus the engine and the viewer
# when they have been built (this script packages them; it does not build
# them):
#
#   core/        the wasm core and its module worker (core/worker.js):
#                scripts/web/build-core.sh, which writes dist/web-core
#                ($NEOSCAD_WEB_CORE)
#   view/        the WebGPU-only viewer (view/web_view.js and its wasm):
#                scripts/web/build-view.sh --no-webgl --out dist/web-view/webgpu
#                ($NEOSCAD_WEB_VIEW)
#   view-webgl/  the viewer with wgpu's WebGL2 backend, which the page
#                fetches only when WebGPU is missing or fails:
#                scripts/web/build-view.sh --out dist/web-view/webgl
#                ($NEOSCAD_WEB_VIEW_WEBGL)
#
# Without a core the bundle uses the mock worker, and the page says so in
# a banner; without a viewer it draws with its canvas fallback. Every URL
# in the bundle is relative, so it can be unpacked under any path (the
# website's /try/: scripts/web/sync-website.sh).
#
# THIRD-PARTY-LICENSES.txt covers the npm packages in app.js, the Rust
# crates in both wasm modules (from `cargo metadata`, through
# scripts/web/rust-licenses.mjs), the embedded fonts, MCAD and colour
# schemes, NeoSCAD's NOTICE, and BOSL2.
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

# Archive timestamps: the commit's time (or SOURCE_DATE_EPOCH), so that
# the same commit gives the same tarball bytes.
epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct HEAD)}

# repro_tar ARCHIVE DIR PATH...: a gzipped tar of the PATHs under DIR that
# depends only on their contents. Plain `tar -czf` records the builder's
# user and group names and ids, every file's local mtime, macOS extended
# attributes and readdir order, and gzip records the time; a published
# archive would then name the builder and change hash with every build.
# Here entries are sorted, owned by root:root (0:0), stamped $epoch, and
# gzip gets -n. This sets the mtimes of the files themselves, so DIR must
# be a copy this script owns. bsdtar (macOS) and GNU tar spell it
# differently.
repro_tar() {
    local archive=$1 dir=$2
    shift 2
    local list
    list=$(mktemp)
    (cd "$dir" && find "$@" -print | LC_ALL=C sort) >"$list"
    (cd "$dir" && node -e '
        const fs = require("fs");
        const t = Number(process.argv[1]);
        for (const f of fs.readFileSync(0, "utf8").split("\n").filter(Boolean)) fs.lutimesSync(f, t, t);
    ' "$epoch" <"$list")
    if tar --version 2>/dev/null | grep -q 'GNU tar'; then
        (cd "$dir" && tar --no-recursion --format=ustar --owner=root:0 --group=root:0 \
            --mtime="@$epoch" -cf - -T "$list")
    else
        (cd "$dir" && COPYFILE_DISABLE=1 tar --no-recursion --format ustar --uid 0 --gid 0 \
            --uname root --gname root --no-xattrs --no-acls --no-fflags -cf - -T "$list")
    fi | gzip -n -9 >"$archive"
    rm -f "$list"
}

if [ -f "$HOME/.cargo/env" ] && ! command -v cargo >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi

core=${NEOSCAD_WEB_CORE:-$root/dist/web-core}
view=${NEOSCAD_WEB_VIEW:-$root/dist/web-view/webgpu}
view_webgl=${NEOSCAD_WEB_VIEW_WEBGL:-$root/dist/web-view/webgl}

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
if [ -f "$core/worker.js" ] && [ -f "$core/neoscad_web_bg.wasm" ]; then
    engine=wasm
else
    echo "warning: no core in $core (scripts/web/build-core.sh), so the bundle uses the mock engine" >&2
fi
viewer=none
if [ -f "$view/web_view.js" ] && [ -f "$view_webgl/web_view.js" ]; then
    viewer=wasm
else
    echo "warning: no viewer in $view and $view_webgl (scripts/web/build-view.sh), so the page draws with its canvas fallback" >&2
fi

rm -rf "$out" "$out.tar.gz"
(cd "$root/web" && node build.mjs --out "$out" --engine "$engine" --view "$viewer" --version "$version" --sha "$sha$dirty")

if [ "$engine" = wasm ]; then
    mkdir "$out/core"
    # package.json only lets node import the glue (crates/web/test); the
    # browser needs the module, its glue and the worker.
    cp "$core/worker.js" "$core/neoscad_web.js" "$core/neoscad_web_bg.wasm" "$out/core/"
    # The fonts are not compiled into the core (crates/web, FONT_DIR): the
    # page adds them when a model first draws text. Their paths under
    # assets/fonts are kept, so the worker scans them in the order the
    # native builds index them (the last tie-break in font matching).
    fonts_stage=$(mktemp -d)
    cp -R "$root/assets/fonts/." "$fonts_stage/"
    repro_tar "$out/fonts.tar.gz" "$fonts_stage" Liberation-2.00.1
    rm -rf "$fonts_stage"
fi
if [ "$viewer" = wasm ]; then
    mkdir "$out/view" "$out/view-webgl"
    cp "$view/web_view.js" "$view/web_view_bg.wasm" "$out/view/"
    cp "$view_webgl/web_view.js" "$view_webgl/web_view_bg.wasm" "$out/view-webgl/"
fi

if [ -n "$bosl2" ]; then
    # Only the library: its .scad files and licence, not its docs, tests
    # or images. Copied first, because repro_tar stamps the files it packs
    # and the reference checkout is not ours to touch.
    bosl2_stage=$(mktemp -d)
    mkdir "$bosl2_stage/BOSL2"
    cp "$bosl2/LICENSE" "$bosl2"/*.scad "$bosl2_stage/BOSL2/"
    repro_tar "$out/bosl2.tar.gz" "$bosl2_stage" BOSL2
    rm -rf "$bosl2_stage"
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
    if [ "$engine" = wasm ] || [ "$viewer" = wasm ]; then
        echo
        echo "------------------------------------------------------------------------"
        echo "The engine (core/) and the viewer (view/, view-webgl/) are compiled from"
        echo "NeoSCAD's Rust source and these crates:"
        echo
        cargo metadata --format-version 1 --locked --filter-platform wasm32-unknown-unknown |
            node "$root/scripts/web/rust-licenses.mjs" neoscad-web neoscad-web-view
        echo
        echo "------------------------------------------------------------------------"
        echo "NeoSCAD's NOTICE (code ported from other projects):"
        echo
        cat "$root/NOTICE"
    fi
    if [ "$engine" = wasm ]; then
        echo
        echo "------------------------------------------------------------------------"
        echo "fonts.tar.gz is the Liberation fonts 2.00.1 (SIL Open Font License 1.1):"
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
source_url=${NEOSCAD_SOURCE_URL:-"https://github.com/neoscad/neoscad/tree/$full_sha"}
cat > "$out/SOURCE.txt" <<EOF
NeoSCAD web demo $version, built from commit $full_sha$dirty.

NeoSCAD is free software under the GNU General Public License, version 2
or (at your option) any later version. The complete corresponding source
code of this build, including the scripts used to build it
(scripts/web/build.sh), is NeoSCAD at that commit:

    $source_url

If you cannot obtain it there, write to source@neoscad.org, and the
NeoSCAD project will provide it for three years from the date of this
build, for no more than the cost of distribution.
EOF

# --- No local paths ---------------------------------------------------------
# The bundle is published: nothing in it may name the builder's home
# directory (scripts/web/remap-paths.sh maps the paths rustc writes).
if leaks=$(LC_ALL=C grep -rlaF "$HOME/" "$out"); then
    echo "error: the bundle contains local paths ($HOME/...):" >&2
    echo "$leaks" | sed "s|^$out/|  |" >&2
    exit 1
fi

# --- Archives and checksums ------------------------------------------------
git archive --format=tar.gz --prefix="$name-source/" -o "$dist/$name-source.tar.gz" HEAD
repro_tar "$dist/$name.tar.gz" "$dist" "$name"
(cd "$dist" && shasum -a 256 "$name.tar.gz" "$name-source.tar.gz" > SHA256SUMS)

# --- Sizes ------------------------------------------------------------------
echo "sizes (raw, gzip -9):"
(cd "$out" && find . -type f ! -path './examples/*' | sort | while read -r f; do
    printf '  %-34s %10s %10s\n' "${f#./}" "$(wc -c <"$f" | tr -d ' ')" "$(gzip -9 -c "$f" | wc -c | tr -d ' ')"
done)
printf '  %-34s %10s %10s\n' "examples/ (all)" "$(cat "$out"/examples/* | wc -c | tr -d ' ')" \
    "$(cat "$out"/examples/* | gzip -9 | wc -c | tr -d ' ')"

echo "bundle:   $out ($engine engine, $viewer view)"
echo "tarball:  $dist/$name.tar.gz"
echo "checksum: $(cat "$dist/SHA256SUMS" | head -n 1)"
if [ -n "$dirty" ]; then
    echo "note: the working tree has uncommitted changes; the source tarball is HEAD without them" >&2
fi
