#!/usr/bin/env bash
# Checks that every vendored crate is exactly its crates.io release plus
# the patch series kept next to it (vendor/README.md, "The convention").
#
# For each directory vendor/<crate> (other than vendor/patches) it reads
# the package name and version from vendor/<crate>/Cargo.toml, downloads
# that release's .crate from static.crates.io, checks its SHA-256 against
# the checksum in the crates.io index (the one Cargo.lock would record),
# unpacks it, deletes the paths listed in vendor/patches/<crate>/removed
# (if that file exists), applies vendor/patches/<crate>/NNNN-*.patch in
# order with `patch -p1`, and compares the result with vendor/<crate>
# byte for byte. A vendored crate without a vendor/patches/<crate>
# directory, a patches directory without a crate, a patch that does not
# apply exactly, and any file that differs are all failures. Without this,
# an edit to the vendored tree that never made it into a patch (or a patch
# that no longer matches the tree) goes unnoticed until someone tries to
# rebase the patches onto a new upstream release and finds the series
# does not reproduce what we ship.
#
#   scripts/vendor-check.sh                 check every vendored crate
#   scripts/vendor-check.sh CRATE...        check only these
#   scripts/vendor-check.sh --refresh CRATE
#       rewrite the body of the crate's last patch from the difference
#       between (release + removed + every earlier patch) and
#       vendor/<crate>, keeping its header, then check the crate. Use it
#       after editing the vendored tree: for a new change, first create
#       the next NNNN-name.patch holding only its header (see the README).
#   scripts/vendor-check.sh --diff OLD NEW
#       print the patch body between two directory trees, as --refresh
#       writes it (unified diff, a/ and b/ paths, bytes kept as they are,
#       CRLF included).
#
# Needs bash, curl, tar, patch, diff and sha256sum or shasum; works with
# GNU and BSD patch and diff (Linux and macOS).
set -euo pipefail
export LC_ALL=C
caller_dir=$PWD
cd "$(dirname "$0")/.."
root=$PWD

die() {
    echo "vendor-check: $*" >&2
    exit 1
}

usage() {
    sed -n '/^#   scripts/,/^#$/p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# GNU patch may decide a patch has DOS line endings and strip the CR from
# its lines ("Stripping trailing CRs from patch"), which would break the
# CRLF sources of manifold-rust; --binary stops that (BSD patch, on macOS,
# never does it and has no such option). -F0 allows no fuzz, -E deletes a
# file a patch empties, -f never stops to ask. A hunk that applies only at
# an offset fails the check (apply_patch): it means the patch was made
# against a different tree than the series gives it, and both patches
# then leave a .orig backup, which could even overwrite a crate's own
# Cargo.toml.orig.
patch_flags=(-p1 -f -F0 -E)
if patch --version 2>/dev/null | grep -q 'GNU patch'; then
    patch_flags+=(--binary)
fi

# Applies patch $2 in tree $1, failing on any hunk that needs an offset or
# fuzz, with patch's own messages in $3. GNU patch reports an offset in
# its messages; BSD patch says nothing and only leaves the .orig backup.
backups() {
    find "$1" -type f \( -name '*.orig' -o -name '*.rej' \) | sort
}
apply_patch() {
    local before
    before=$(backups "$1")
    (cd "$1" && patch "${patch_flags[@]}" -i "$2" </dev/null) >"$3" 2>&1 || return 1
    if grep -q -i -E 'offset|fuzz' "$3" || [ "$(backups "$1")" != "$before" ]; then
        echo "a hunk applied only at an offset or with fuzz" >>"$3"
        return 1
    fi
}

# `diff -u` exits 1 when files differ; only 2 and up is an error.
udiff() {
    local rc=0
    diff -u --label "$1" --label "$2" "$3" "$4" || rc=$?
    [ "$rc" -le 1 ] || die "diff failed on $3 and $4"
}

is_binary() {
    # A NUL byte is what diff and patch cannot carry.
    [ "$(tr -cd '\000' <"$1" | wc -c)" -gt 0 ]
}

# The patch body that turns tree $1 into tree $2, one file at a time in
# byte order of their paths, so regenerating an unchanged patch gives the
# same bytes.
tree_diff() {
    local old=$1 new=$2 f
    {
        (cd "$old" && find . -type f)
        (cd "$new" && find . -type f)
    } | sed 's|^\./||' | sort -u | while IFS= read -r f; do
        if [ -f "$old/$f" ] && [ -f "$new/$f" ]; then
            cmp -s "$old/$f" "$new/$f" && continue
            if is_binary "$old/$f" || is_binary "$new/$f"; then
                die "$f is binary; patch cannot carry it (list a removal in 'removed' instead)"
            fi
            udiff "a/$f" "b/$f" "$old/$f" "$new/$f"
        elif [ -f "$old/$f" ]; then
            is_binary "$old/$f" && die "$f is binary and removed: list it in 'removed' instead"
            udiff "a/$f" "b/$f" "$old/$f" /dev/null
        else
            is_binary "$new/$f" && die "$f is binary and new; patch cannot carry it"
            udiff "a/$f" "b/$f" /dev/null "$new/$f"
        fi
    done
}

# Path of a crate in the sparse index (https://doc.rust-lang.org/cargo/reference/registry-index.html#index-files).
index_path() {
    local n
    n=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')
    case ${#n} in
        1) echo "1/$n" ;;
        2) echo "2/$n" ;;
        3) echo "3/${n:0:1}/$n" ;;
        *) echo "${n:0:2}/${n:2:2}/$n" ;;
    esac
}

# The value of KEY in the [package] table of Cargo.toml $1.
package_field() {
    awk -v key="$2" '
        /^\[/ { table = $0 }
        table == "[package]" && $1 == key && $2 == "=" {
            v = $3; gsub(/"/, "", v); print v; exit
        }' "$1"
}

curl_get() {
    curl --proto '=https' --tlsv1.2 -fsSL --retry 3 --max-time 300 \
        -A 'neoscad-vendor-check (https://github.com/neoscad/neoscad)' "$@"
}

work=$(mktemp -d "${TMPDIR:-/tmp}/vendor-check.XXXXXX")
trap 'rm -rf "$work"' EXIT

# Builds <release> - removed + the first $2 patches of crate $1 (all of
# them when $2 is empty) in $work/$1/tree and prints that path.
build_tree() {
    local crate=$1 upto=${2:-} dir="$root/vendor/$1" pdir="$root/vendor/patches/$1"
    local name version file want got out tree path p n=0
    [ -f "$dir/Cargo.toml" ] || die "vendor/$crate has no Cargo.toml"
    name=$(package_field "$dir/Cargo.toml" name)
    version=$(package_field "$dir/Cargo.toml" version)
    [ -n "$name" ] && [ -n "$version" ] || die "no [package] name and version in vendor/$crate/Cargo.toml"

    out="$work/$crate"
    rm -rf "$out"
    mkdir -p "$out"
    file="$out/$name-$version.crate"
    curl_get -o "$file" "https://static.crates.io/crates/$name/$name-$version.crate" ||
        die "could not download $name $version from crates.io"
    want=$(curl_get "https://index.crates.io/$(index_path "$name")" |
        grep -F "\"vers\":\"$version\"" |
        sed -n 's/.*"cksum":"\([0-9a-f]\{64\}\)".*/\1/p' | head -n 1) || true
    [ -n "$want" ] || die "no checksum for $name $version in the crates.io index"
    got=$(sha256_of "$file")
    [ "$got" = "$want" ] || die "$name-$version.crate has SHA-256 $got, the index says $want"

    tar -xzf "$file" -C "$out" || die "could not unpack $name-$version.crate"
    tree="$out/$name-$version"
    [ -d "$tree" ] || die "$name-$version.crate did not unpack to $name-$version/"

    if [ -f "$pdir/removed" ]; then
        while IFS= read -r path || [ -n "$path" ]; do
            path=${path%%#*}
            path=$(printf '%s' "$path" | sed 's/[[:space:]]*$//')
            [ -n "$path" ] || continue
            [ -e "$tree/$path" ] || die "vendor/patches/$crate/removed lists $path, which $name $version does not have"
            rm -rf "${tree:?}/$path"
        done <"$pdir/removed"
    fi

    for p in "$pdir"/[0-9][0-9][0-9][0-9]-*.patch; do
        [ -e "$p" ] || break
        if [ -n "$upto" ] && [ "$n" -ge "$upto" ]; then
            break
        fi
        n=$((n + 1))
        apply_patch "$tree" "$p" "$out/patch.log" || {
            cat "$out/patch.log" >&2
            die "vendor/patches/$crate/${p##*/} does not apply exactly (no offset, no fuzz) to $name $version plus the patches before it"
        }
    done
    echo "$tree"
}

check_crate() {
    local crate=$1 tree n
    tree=$(build_tree "$crate") || return 1
    n=$(find "$root/vendor/patches/$crate" -name '[0-9][0-9][0-9][0-9]-*.patch' | wc -l | tr -d ' ')
    if diff -rq "$tree" "$root/vendor/$crate" >"$work/$crate.diff" 2>&1; then
        [ "$n" = 1 ] && n="1 patch" || n="$n patches"
        echo "vendor/$crate: ok ($(basename "$tree") + $n)"
        return 0
    fi
    {
        echo "vendor/$crate differs from $(basename "$tree") plus vendor/patches/$crate:"
        sed "s|$tree|<release+patches>|g; s|$root/||g" "$work/$crate.diff"
        echo "First differences (<release+patches> -> vendor/$crate):"
        # Into a file first: `head` closing the pipe would kill diff.
        tree_diff "$tree" "$root/vendor/$crate" >"$work/$crate.patch" || true
        head -n 60 "$work/$crate.patch"
        echo "If the vendored tree is what you want, put the change in a patch:"
        echo "scripts/vendor-check.sh --refresh $crate (see vendor/README.md)."
    } >&2
    return 1
}

refresh_crate() {
    local crate=$1 pdir="$root/vendor/patches/$1" last count tree header
    [ -d "$root/vendor/$crate" ] || die "no vendor/$crate"
    last=$(find "$pdir" -name '[0-9][0-9][0-9][0-9]-*.patch' 2>/dev/null | sort | tail -n 1)
    [ -n "$last" ] || die "vendor/patches/$crate has no patch; create NNNN-name.patch with a header first"
    count=$(find "$pdir" -name '[0-9][0-9][0-9][0-9]-*.patch' | wc -l | tr -d ' ')
    tree=$(build_tree "$crate" $((count - 1))) || exit 1
    # The header is everything before the first file header; it must not
    # itself contain a line starting with "--- ".
    header="$work/header"
    awk '/^--- / { exit } { print }' "$last" >"$header"
    [ -s "$header" ] || die "${last#"$root"/} has no header; describe the change above the diff"
    {
        cat "$header"
        tree_diff "$tree" "$root/vendor/$crate"
    } >"$work/new.patch"
    mv "$work/new.patch" "$last"
    echo "rewrote ${last#"$root"/}"
}

case ${1:-} in
    -h | --help) usage ;;
    --diff)
        [ $# -eq 3 ] || usage
        # The trees are named relative to where the script was run from.
        cd "$caller_dir"
        tree_diff "$2" "$3"
        exit 0
        ;;
    --refresh)
        [ $# -eq 2 ] || usage
        refresh_crate "$2"
        set -- "$2"
        ;;
    -*) usage ;;
esac

# Every vendored crate has a patch directory, and every patch directory a
# crate, so a new vendored copy cannot skip the convention.
status=0
for d in vendor/*/; do
    c=$(basename "$d")
    [ "$c" = patches ] && continue
    if [ ! -d "vendor/patches/$c" ]; then
        echo "vendor-check: vendor/$c has no vendor/patches/$c (every vendored crate needs one; see vendor/README.md)" >&2
        status=1
    fi
done
for d in vendor/patches/*/; do
    [ -d "$d" ] || continue
    c=$(basename "$d")
    if [ ! -d "vendor/$c" ]; then
        echo "vendor-check: vendor/patches/$c has no vendor/$c" >&2
        status=1
    fi
done

if [ $# -gt 0 ]; then
    crates=("$@")
else
    crates=()
    for d in vendor/patches/*/; do
        [ -d "$d" ] && [ -d "vendor/$(basename "$d")" ] && crates+=("$(basename "$d")")
    done
fi
for c in "${crates[@]}"; do
    [ -d "vendor/$c" ] && [ -d "vendor/patches/$c" ] || die "no vendor/$c with vendor/patches/$c"
    check_crate "$c" || status=1
done
exit "$status"
