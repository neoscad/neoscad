#!/usr/bin/env bash
# The source release: the tracked tree, and every crates.io dependency
# (`cargo vendor`) for building without network access.
#
#   scripts/release/source-tarballs.sh [OUT_DIR]      # default dist/source
#
# Writes:
#   neoscad-<version>.tar.gz         the tracked files, under neoscad-<version>/
#   neoscad-<version>-vendor.tar.gz  neoscad-<version>/cargo-vendor/ and
#                                    neoscad-<version>/.cargo/config.toml
#   SHA256SUMS
#
# Unpack both into the same directory, then
# `cargo build --release --offline --locked -p neoscad-cli`: the vendor
# tarball's .cargo/config.toml points crates.io at cargo-vendor/. (The
# tree's own vendor/ holds the patched manifold-rust, clipper2-rust and
# wgpu-core, which are path dependencies and so already in the source
# tarball.)
#
# The files come from `git ls-files` (tracked, plus untracked files that
# are not ignored), so a tag's clean checkout gives exactly the tag; a
# dirty tree is packed as it stands, with a warning.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-$root/dist/source}
cd "$root"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
name=neoscad-$version
if [[ -n "$(git status --porcelain)" ]]; then
    echo "warning: the working tree has uncommitted changes; they are included" >&2
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/$name" "$out"
git ls-files -z --cached --others --exclude-standard |
    while IFS= read -r -d '' f; do [[ -e "$f" ]] && printf '%s\0' "$f"; done |
    tar -c --null -T - -f - | tar -x -C "$work/$name" -f -
# Owner and group normalised, so the archive unpacks the same for anyone;
# no extended attributes (macOS's bsdtar stores com.apple.provenance,
# which GNU tar warns about on every file).
# GNU tar (the release runner's) and bsdtar (macOS) spell ownership
# differently: GNU tar has no --uid/--gid.
if tar --version | grep -q 'GNU tar'; then
    tar_args=(--owner=root:0 --group=root:0 --numeric-owner --no-xattrs)
else
    tar_args=(--uid 0 --gid 0 --uname root --gname root --no-xattrs --no-mac-metadata)
fi
COPYFILE_DISABLE=1 tar -C "$work" "${tar_args[@]}" -czf "$out/$name.tar.gz" "$name"

# Vendor from the unpacked copy, so the lockfile and manifests are exactly
# the tarball's.
mkdir -p "$work/vendor-root/$name/.cargo"
(cd "$work/$name" && cargo vendor --locked --versioned-dirs --quiet \
    "$work/vendor-root/$name/cargo-vendor" >/dev/null)
# Written here rather than taken from cargo's output, which names the
# absolute directory it vendored into (and which --quiet suppresses).
cat >"$work/vendor-root/$name/.cargo/config.toml" <<'EOF'
# From neoscad's vendor tarball: build from cargo-vendor/, offline.
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "cargo-vendor"
EOF
COPYFILE_DISABLE=1 tar -C "$work/vendor-root" "${tar_args[@]}" \
    -czf "$out/$name-vendor.tar.gz" "$name"

(cd "$out" && shasum -a 256 "$name.tar.gz" "$name-vendor.tar.gz" >SHA256SUMS)
ls -l "$out"
