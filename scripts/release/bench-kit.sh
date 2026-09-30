#!/usr/bin/env bash
# Builds the bench kit, the release asset `neoscad bench` times
# (docs/community-bench.md):
#
#   scripts/release/bench-kit.sh [--out DIR] [--bosl2 DIR] [--openscad DIR] [--unpinned]
#
# writes DIR/neoscad-bench-kit-<version>.tar.gz and its .sha256 (default
# DIR: dist/bench-kit). The kit holds conformance/bench.json's models, each
# as a file under models/ (inline sources written out, `file` models
# copied from OpenSCAD's examples or BOSL2), the sources of the files
# models import under inputs/, BOSL2 at a pinned commit under
# libraries/BOSL2 (every run's OPENSCADPATH), their licences, and kit.json
# (crates/bench-core/src/kit.rs reads it). Two users' results are
# comparable because both ran these exact bytes.
#
# BOSL2 comes from --bosl2, else .reference/BOSL2 (in this checkout or the
# main one), else a fetch of BOSL2_COMMIT; OpenSCAD's examples from
# --openscad, else .reference/openscad (scripts/release/fetch-reference.sh
# checks it out at conformance/manifest.json's commit). Both must be at
# their pinned commits, so a kit never silently carries other models;
# --unpinned (for tests) accepts any tree and records "unpinned".
#
# The archive is reproducible: the same commit (or SOURCE_DATE_EPOCH) and
# the same inputs give the same bytes on macOS and Linux. Entries are
# sorted, owned by root, stamped with the commit time, their modes
# normalised, and gzip gets -n; see repro_tar in scripts/web/build.sh for
# why each matters.
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

# BOSL2's commit, moved deliberately: a new BOSL2 changes the BOSL2
# models' times, so results from kits with different commits are not
# comparable (the kit version records it).
BOSL2_COMMIT=9948313433166f58701a41f6e03cf19d6a70f42a
BOSL2_URL=https://github.com/BelfrySCAD/BOSL2.git

# The models --quick runs: the fast ones, a few BOSL2 among them.
QUICK="bosl_gears__003 bosl_screws__001 csg_deep_union ex_csg_basic extrude_twist mink_convex text_30lines"

out=$root/dist/bench-kit
bosl2=""
openscad=""
unpinned=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --out) out=$2; shift 2 ;;
        --bosl2) bosl2=$2; shift 2 ;;
        --openscad) openscad=$2; shift 2 ;;
        --unpinned) unpinned=1; shift ;;
        -h|--help) sed -n '2,5p' "$0"; exit 0 ;;
        *) echo "bench-kit: unknown argument $1" >&2; exit 2 ;;
    esac
done

command -v jq >/dev/null || { echo "bench-kit: needs jq" >&2; exit 1; }

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
neoscad_commit=$(git rev-parse HEAD 2>/dev/null || echo unknown)
epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct HEAD 2>/dev/null || echo 0)}
openscad_pin=$(sed -n 's/.*"commit": *"\([0-9a-f]\{40\}\)".*/\1/p' conformance/manifest.json | head -1)

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

common=""
if git rev-parse --git-common-dir >/dev/null 2>&1; then
    common=$(cd "$(git rev-parse --git-common-dir)/.." && pwd)
fi
find_ref() { # find_ref NAME MARKER: .reference/NAME here or in the main checkout
    local d
    for d in "$root/.reference/$1" "${common:+$common/.reference/$1}"; do
        if [[ -n "$d" && -e "$d/$2" ]]; then echo "$d"; return; fi
    done
}

# commit_of DIR PIN: the tree's commit, checked against PIN.
commit_of() {
    local dir=$1 pin=$2 got
    got=$(git -C "$dir" rev-parse HEAD 2>/dev/null || true)
    if [[ "$got" == "$pin" ]]; then
        echo "$pin"
    elif [[ $unpinned == 1 ]]; then
        echo unpinned
    else
        echo "bench-kit: $dir is at '${got:-not a git checkout}', not the pinned $pin" >&2
        exit 1
    fi
}

[[ -n "$bosl2" ]] || bosl2=$(find_ref BOSL2 std.scad)
if [[ -z "$bosl2" ]]; then
    bosl2=$scratch/BOSL2
    git init -q "$bosl2"
    git -C "$bosl2" fetch -q --depth 1 "$BOSL2_URL" "$BOSL2_COMMIT"
    git -C "$bosl2" checkout -q --detach FETCH_HEAD
fi
bosl2_commit=$(commit_of "$bosl2" "$BOSL2_COMMIT")

[[ -n "$openscad" ]] || openscad=$(find_ref openscad examples)
[[ -n "$openscad" ]] || { echo "bench-kit: no OpenSCAD checkout; run scripts/release/fetch-reference.sh or pass --openscad" >&2; exit 1; }
openscad_commit=$(commit_of "$openscad" "$openscad_pin")

name=neoscad-bench-kit-$version
stage=$scratch/stage
kit=$stage/$name
mkdir -p "$kit/models" "$kit/inputs" "$kit/libraries/BOSL2" "$kit/licenses"

cfg=conformance/bench.json
expand() { # {REF} and {BOSL2} of a bench.json path
    local p=$1
    p=${p//\{REF\}/$openscad}
    p=${p//\{BOSL2\}/$bosl2}
    echo "$p"
}

jq -j '.cold_start.source' "$cfg" >"$kit/models/cold_start.scad"
for id in $(jq -r '.models | keys[]' "$cfg"); do
    file=$(jq -r --arg id "$id" '.models[$id].file // empty' "$cfg")
    if [[ -n "$file" ]]; then
        cp "$(expand "$file")" "$kit/models/$id.scad"
    else
        jq -j --arg id "$id" '.models[$id].source' "$cfg" >"$kit/models/$id.scad"
    fi
    for input in $(jq -r --arg id "$id" '.models[$id].inputs // {} | keys[]' "$cfg"); do
        jq -j --arg id "$id" --arg i "$input" '.models[$id].inputs[$i]' "$cfg" >"$kit/inputs/$input.scad"
    done
done

cp "$bosl2"/*.scad "$kit/libraries/BOSL2/"
cp "$bosl2/LICENSE" "$kit/libraries/BOSL2/LICENSE"
cp "$bosl2/LICENSE" "$kit/licenses/BOSL2-BSD-2-Clause.txt"
cp "$openscad/examples/COPYING-CC0.txt" "$kit/licenses/OpenSCAD-examples-CC0.txt"

# kit.json: bench.json's settings and models, with kit-relative paths.
jq -S --arg version "$version" --arg nc "$neoscad_commit" --arg bc "$bosl2_commit" \
    --arg oc "$openscad_commit" --arg quick "$QUICK" '
    ($quick | split(" ")) as $q |
    {
      kit_schema: 1,
      version: $version,
      sources: {neoscad_commit: $nc, bosl2_commit: $bc, openscad_commit: $oc},
      runs: .runs,
      single_run_over_s: .single_run_over_s,
      timeout_s: .timeout_s,
      quick: {runs: 1, cold_start_runs: 5},
      library_path: "libraries",
      cold_start: {description: .cold_start.description, file: "models/cold_start.scad", runs: .cold_start.runs},
      models: (.models | to_entries | map(.key as $id | {
        key: $id,
        value: {
          description: .value.description,
          file: ("models/" + $id + ".scad"),
          requires: (.value.requires // []),
          inputs: ((.value.inputs // {}) | with_entries(.value = ("inputs/" + .key + ".scad"))),
          quick: ($q | index($id) != null)
        }
      }) | from_entries)
    }' "$cfg" >"$kit/kit.json"

cat >"$kit/README.txt" <<EOF
The bench kit of neoscad $version: the models \`neoscad bench\` times.
Run it with \`neoscad bench --kit <this directory or its .tar.gz>\`;
docs/community-bench.md in the neoscad repository explains the benchmark.

kit.json      settings and models (built from conformance/bench.json at $neoscad_commit)
models/       each model's source; cold_start.scad is cube(1)
inputs/       sources of files models import, exported by neoscad before the first run
libraries/    BOSL2 at $bosl2_commit (https://github.com/BelfrySCAD/BOSL2), BSD 2-Clause
licenses/     BOSL2's licence; OpenSCAD's examples (ex_csg_basic, ex_menger, from
              OpenSCAD $openscad_commit) are CC0 1.0

The inline models are neoscad's own benchmark cases (GPL-2.0-or-later, as neoscad).
EOF

# Normalised modes and times: the builder's umask and clock must not
# reach the archive.
find "$stage" -type d -exec chmod 755 {} +
find "$stage" -type f -exec chmod 644 {} +
list=$scratch/list
(cd "$stage" && find "$name" -print | LC_ALL=C sort) >"$list"
if tar --version 2>/dev/null | grep -q 'GNU tar'; then
    (cd "$stage" && tar --no-recursion --format=ustar --owner=root:0 --group=root:0 \
        --mtime="@$epoch" -cf - -T "$list")
else
    stamp=$(date -u -r "$epoch" +%Y%m%d%H%M.%S)
    (cd "$stage" && while IFS= read -r f; do TZ=UTC0 touch -h -t "$stamp" "$f"; done <"$list")
    (cd "$stage" && COPYFILE_DISABLE=1 tar --no-recursion --format ustar --uid 0 --gid 0 \
        --uname root --gname root --no-xattrs --no-acls --no-fflags -cf - -T "$list")
fi | gzip -n -9 >"$scratch/$name.tar.gz"

mkdir -p "$out"
mv "$scratch/$name.tar.gz" "$out/$name.tar.gz"
if command -v sha256sum >/dev/null; then
    (cd "$out" && sha256sum "$name.tar.gz" >"$name.tar.gz.sha256")
else
    (cd "$out" && shasum -a 256 "$name.tar.gz" >"$name.tar.gz.sha256")
fi
echo "bench kit: $out/$name.tar.gz ($(wc -c <"$out/$name.tar.gz" | tr -d ' ') bytes; BOSL2 $bosl2_commit, OpenSCAD $openscad_commit)"
