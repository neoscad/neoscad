#!/usr/bin/env bash
# The licence files every NeoSCAD artifact carries, and a check that the
# copies under packaging/licenses/ still match what they were copied from.
#
#   scripts/release/licenses.sh --check     # exit 1 if a copy is stale
#   scripts/release/licenses.sh DEST        # DEST/LICENSE, DEST/NOTICE, DEST/licenses/
#
# The binary embeds the Liberation fonts (SIL OFL 1.1) and MCAD (LGPL 2.1),
# and links patched copies of manifold-rust (Apache 2.0) and clipper2-rust
# (Boost 1.0); NOTICE covers the libtess2 port. Their licences require the
# notices to travel with the binary, and a release that shipped only
# LICENSE (as scripts/apple/release.sh once did) did not meet them.
#
# The copies are committed, rather than staged at build time, so that
# cargo-dist's `include` (Cargo.toml, [workspace.metadata.dist]) can name a
# directory that exists when `dist plan` runs; --check (run by CI) keeps
# them from drifting when assets/ or vendor/ are updated.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)

# Each copy and its source, relative to the repository root.
pairs=(
    "packaging/licenses/Liberation-Fonts-OFL-1.1.txt assets/fonts/Liberation-2.00.1/LICENSE"
    "packaging/licenses/MCAD-LGPL-2.1.txt assets/libraries/MCAD/lgpl-2.1.txt"
    "packaging/licenses/manifold-rust-Apache-2.0.txt vendor/manifold-rust/LICENSE"
    "packaging/licenses/clipper2-rust-BSL-1.0.txt vendor/clipper2-rust/LICENSE"
)

case "${1:-}" in
    "")
        sed -n '5,6p' "$0" >&2
        exit 2
        ;;
    --check)
        status=0
        for p in "${pairs[@]}"; do
            read -r copy src <<<"$p"
            if ! cmp -s "$root/$copy" "$root/$src"; then
                echo "stale: $copy differs from $src (copy it again)" >&2
                status=1
            fi
        done
        exit $status
        ;;
    *)
        dest=$1
        mkdir -p "$dest/licenses"
        cp "$root/LICENSE" "$root/NOTICE" "$dest/"
        for p in "${pairs[@]}"; do
            read -r copy _ <<<"$p"
            cp "$root/$copy" "$dest/licenses/"
        done
        cp "$root/packaging/licenses/README.md" "$dest/licenses/"
        ;;
esac
