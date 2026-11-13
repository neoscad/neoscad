# shellcheck shell=bash
# shellcheck disable=SC2154 # root, epoch and the tp_ variables come from the caller
# Shared by the scripts that package the web engine (scripts/web/build.sh
# for the /try bundle, scripts/web/build-package.sh for the npm package),
# so both ship the same archives and the same licence notices. Source it
# after setting:
#
#   root    the repository
#   epoch   the archive timestamp (the commit's time, or SOURCE_DATE_EPOCH)
#
# Bash only (arrays, local).

# repro_tar ARCHIVE DIR PATH...: a gzipped tar of the PATHs under DIR that
# depends only on their contents. Plain `tar -czf` records the builder's
# user and group names and ids, every file's local mtime, macOS extended
# attributes and readdir order, and gzip records the time; a published
# archive would then name the builder and change hash with every build.
# Here entries are sorted, owned by root:root (0:0), stamped $epoch, and
# gzip gets -n. This sets the mtimes of the files themselves, so DIR must
# be a copy the caller owns. bsdtar (macOS) and GNU tar spell it
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

# find_bosl2: print the BOSL2 checkout, or nothing. It is the reference
# checkout, which is gitignored and so lives only in the main checkout; a
# worktree finds it through the shared git directory.
find_bosl2() {
    local dir
    for dir in "$root/.reference/BOSL2" "$(cd "$(git rev-parse --git-common-dir)/.." && pwd)/.reference/BOSL2"; do
        if [ -f "$dir/std.scad" ]; then
            echo "$dir"
            return
        fi
    done
}

# fonts_tar ARCHIVE: the Liberation fonts the page adds when a model first
# draws text (the core does not compile them in: crates/web, FONT_DIR).
# Their paths under assets/fonts are kept, so the worker scans them in the
# order the native builds index them (the last tie-break in font matching).
fonts_tar() {
    local stage
    stage=$(mktemp -d)
    cp -R "$root/assets/fonts/." "$stage/"
    repro_tar "$1" "$stage" Liberation-2.00.1
    rm -rf "$stage"
}

# bosl2_tar ARCHIVE BOSL2_DIR: only the library, its .scad files and
# licence, not its docs, tests or images. Copied first, because repro_tar
# stamps the files it packs and the reference checkout is not ours to
# touch.
bosl2_tar() {
    local stage
    stage=$(mktemp -d)
    mkdir "$stage/BOSL2"
    cp "$2/LICENSE" "$2"/*.scad "$stage/BOSL2/"
    repro_tar "$1" "$stage" BOSL2
    rm -rf "$stage"
}

# third_party_licenses: print the third-party notices for what was
# shipped. Named arguments, as variables set for the call:
#
#   tp_title      the first line
#   tp_rule       the line under it (default: as long as the title)
#   tp_license    the line saying NeoSCAD's own licence
#   tp_npm_intro  the line before the npm packages' licences
#   tp_npm_file   those licences (build.mjs's metafile walk), or empty
#   tp_prefix     where the assets are ("" in the bundle, "assets/" in
#                 the package)
#   tp_engine     "wasm" when the core is in
#   tp_viewer     "wasm" when the viewers are in
#   tp_bosl2      the BOSL2 checkout when bosl2.tar.gz is in, or empty
third_party_licenses() {
    local p=${tp_prefix:-}
    echo "$tp_title"
    echo "${tp_rule:-${tp_title//?/=}}"
    echo
    echo "$tp_license"
    echo
    echo "$tp_npm_intro"
    if [ -n "${tp_npm_file:-}" ]; then
        echo
        cat "$tp_npm_file"
    fi
    if [ "$tp_engine" = wasm ] || [ "$tp_viewer" = wasm ]; then
        echo
        echo "------------------------------------------------------------------------"
        echo "The engine (${p}core/) and the viewer (${p}view/, ${p}view-webgl/) are compiled from"
        echo "NeoSCAD's Rust source and these crates:"
        echo
        (cd "$root" && cargo metadata --format-version 1 --locked --filter-platform wasm32-unknown-unknown) |
            node "$root/scripts/web/rust-licenses.mjs" neoscad-web neoscad-web-view
        echo
        echo "------------------------------------------------------------------------"
        echo "NeoSCAD's NOTICE (code ported from other projects):"
        echo
        cat "$root/NOTICE"
    fi
    if [ "$tp_engine" = wasm ]; then
        echo
        echo "------------------------------------------------------------------------"
        echo "${p}fonts.tar.gz is the Liberation fonts 2.00.1 (SIL Open Font License 1.1):"
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
    if [ -n "${tp_bosl2:-}" ]; then
        echo
        echo "------------------------------------------------------------------------"
        echo "${p}bosl2.tar.gz is BOSL2 (https://github.com/BelfrySCAD/BOSL2):"
        echo
        cat "$tp_bosl2/LICENSE"
    fi
}

# no_local_paths DIR WHAT: fail if any file under DIR, binaries included,
# names the builder's home directory, a user directory or a macOS
# temporary path (scripts/web/remap-paths.sh maps the paths rustc writes).
no_local_paths() {
    local dir=$1 what=$2 leaks="" needle
    # Not "/private/" alone: serde's own source path (src/private/de.rs)
    # is in the core's panic strings, and is no one's machine.
    for needle in "$HOME/" "/Users/" "/private/tmp/" "/private/var/"; do
        leaks+=$(LC_ALL=C grep -rlaF "$needle" "$dir" || true)$'\n'
    done
    if [ -n "${leaks//$'\n'/}" ]; then
        echo "error: $what contains local paths (\$HOME/, /Users/, /private/tmp/ or /private/var/):" >&2
        echo "$leaks" | grep . | LC_ALL=C sort -u | sed "s|^$dir/|  |" >&2
        return 1
    fi
}
