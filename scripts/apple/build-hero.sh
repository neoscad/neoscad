#!/usr/bin/env bash
# Render the hero image: apple/Icon/hero.scad (a BOSL2 planetary gearbox
# on a gyroid plinth) drawn by neoscad at 2400x1350, captioned with the
# full STL render time of neoscad and of the OpenSCAD nightly (Manifold),
# each the best of RUNS runs on this machine. Output in
# apple/Icon/build/hero/: hero.png (captioned), hero-plain.png, times.txt.
#
# Usage: scripts/apple/build-hero.sh [RUNS]     (default 3)
# Env:   NEOSCAD, OPENSCAD (default /Applications/OpenSCAD.app/...)
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
runs="${1:-3}"
neoscad="${NEOSCAD:-$root/target/release/neoscad}"
main="$(git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2; exit}')"
# A worktree has no target/ or .reference/ of its own; use the main
# checkout's.
[[ -x "$neoscad" ]] || neoscad="$main/target/release/neoscad"
openscad="${OPENSCAD:-/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD}"
libs="$root/.reference"
[[ -d "$libs/BOSL2" ]] || libs="$main/.reference"
[[ -d "$libs/BOSL2" ]] || { echo "build-hero: BOSL2 not found in .reference/" >&2; exit 1; }
export OPENSCADPATH="$libs"

scad="$root/apple/Icon/hero.scad"
out="$root/apple/Icon/build/hero"
mkdir -p "$out"
tool="$root/apple/Icon/build/icon-tool"
if [[ ! -x "$tool" || "$root/scripts/apple/icon-tool.swift" -nt "$tool" ]]; then
    swiftc -O "$root/scripts/apple/icon-tool.swift" -o "$tool"
fi
camera="$(sed -n 's|^// camera: *||p' "$scad" | head -1)"

# Best of $runs wall-clock seconds for a full render to STL. Both tools
# start cold each run (separate processes, no cache between them), and
# both write the same kind of file, so the numbers compare like for like.
best() {
    local best="" t
    for _ in $(seq "$runs"); do
        local s e
        s=$(perl -MTime::HiRes=time -e 'printf "%.3f", time')
        "$@" >/dev/null 2>&1
        e=$(perl -MTime::HiRes=time -e 'printf "%.3f", time')
        t=$(echo "$e - $s" | bc)
        if [[ -z "$best" ]] || (( $(echo "$t < $best" | bc) )); then best=$t; fi
    done
    printf '%.2f' "$best"
}
neo_t=$(best "$neoscad" "$scad" -o "$out/hero-neoscad.stl")
osc_t=$(best "$openscad" --backend=manifold "$scad" -o "$out/hero-openscad.stl")
osc_ver="$("$openscad" --version 2>&1 | head -1)"
neo_ver="$("$neoscad" --version 2>&1 | head -1)"
{
    echo "machine: $(sysctl -n machdep.cpu.brand_string), $(sysctl -n hw.ncpu) cores"
    echo "neoscad ($neo_ver): best of $runs: $neo_t s"
    echo "$osc_ver --backend=manifold: best of $runs: $osc_t s"
} | tee "$out/times.txt"
rm -f "$out/hero-neoscad.stl" "$out/hero-openscad.stl"

# 3x supersampled renders over two backgrounds for an alpha matte (see
# icon-tool.swift), then our own backdrop, downscale, caption.
for scheme in Starnight Nature; do
    "$neoscad" "$scad" -o "$out/$scheme.png" --render --imgsize 7200,4050 \
        --camera "$camera" --colorscheme "$scheme" -q
done
"$tool" matte "$out/Starnight.png" "$out/Nature.png" 250 "$out/matte.png"
"$tool" backdrop "$out/matte.png" "#23264a" "#07080f" "$out/backdrop.png"
"$tool" resize-to "$out/backdrop.png" 2400 1350 "$out/hero-plain.png"
rm "$out/Starnight.png" "$out/Nature.png" "$out/matte.png" "$out/backdrop.png"
"$tool" caption "$out/hero-plain.png" "$out/hero.png" \
    "Rendered in $neo_t s by NeoSCAD  —  OpenSCAD nightly: $osc_t s" \
    "BOSL2 herringbone planetary gearbox on a gyroid plinth; full render to STL, best of $runs, $(sysctl -n machdep.cpu.brand_string)"
echo "build-hero: $out/hero.png"
