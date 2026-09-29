#!/usr/bin/env bash
# Check out OpenSCAD at the commit conformance/manifest.json was generated
# from, into .reference/openscad (with the MCAD submodule), for CI and for
# any machine without the developer's shallow clone.
#
#   scripts/release/fetch-reference.sh
#
# The root CLAUDE.md's `git clone --depth 1` takes OpenSCAD's latest
# master, which drifts from the manifest as upstream adds tests; this takes
# the pinned commit, so `conformance manifest --check` and the baseline
# agree with what is checked out. An existing checkout at that commit is
# left alone.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
commit=$(sed -n 's/.*"commit": *"\([0-9a-f]\{40\}\)".*/\1/p' "$root/conformance/manifest.json" | head -1)
[[ -n "$commit" ]] || { echo "no reference commit in conformance/manifest.json" >&2; exit 1; }
dest=$root/.reference/openscad

if [[ -d "$dest/.git" ]] && [[ "$(git -C "$dest" rev-parse HEAD)" == "$commit" ]]; then
    echo "reference: $dest already at $commit"
    exit 0
fi
mkdir -p "$dest"
git -C "$dest" init -q
git -C "$dest" remote add origin https://github.com/openscad/openscad.git 2>/dev/null || true
git -C "$dest" fetch -q --depth 1 origin "$commit"
git -C "$dest" checkout -q --detach FETCH_HEAD
git -C "$dest" submodule update -q --init --depth 1 libraries/MCAD
echo "reference: $dest at $commit"
