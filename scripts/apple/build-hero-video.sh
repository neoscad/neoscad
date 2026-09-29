#!/usr/bin/env bash
# Render the hero video: apple/Icon/hero.scad driven through one full $t
# cycle by neoscad's --animate, framed and captioned like the still
# (scripts/apple/build-hero.sh), as a seamless 1920x1080, 30 fps loop.
# Output in apple/Icon/build/hero-video/: hero-gearbox.webm (AV1) and
# hero-gearbox.mp4 (H.264), no audio.
#
# The caption's render times are read from build-hero.sh's times.txt,
# not measured again: the video shows the same model at $t = 0, and those
# are the published numbers. Run build-hero.sh first.
#
# Usage: scripts/apple/build-hero-video.sh [JOBS]   (default 8)
# Env:   NEOSCAD; WORK (frame directory, kept, and reused if it already
#        holds every frame; default a temporary directory, removed after)
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
jobs="${1:-8}"
neoscad="${NEOSCAD:-$root/target/release/neoscad}"
main="$(git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2; exit}')"
# A worktree has no target/, .reference/ or build output of its own; use
# the main checkout's.
[[ -x "$neoscad" ]] || neoscad="$main/target/release/neoscad"
libs="$root/.reference"
[[ -d "$libs/BOSL2" ]] || libs="$main/.reference"
[[ -d "$libs/BOSL2" ]] || { echo "build-hero-video: BOSL2 not found in .reference/" >&2; exit 1; }
export OPENSCADPATH="$libs"
command -v ffmpeg >/dev/null || { echo "build-hero-video: needs ffmpeg (brew install ffmpeg)" >&2; exit 1; }

scad="$root/apple/Icon/hero.scad"
out="$root/apple/Icon/build/hero-video"
mkdir -p "$out"
times="$root/apple/Icon/build/hero/times.txt"
[[ -f "$times" ]] || times="$main/apple/Icon/build/hero/times.txt"
[[ -f "$times" ]] || { echo "build-hero-video: no times.txt; run scripts/apple/build-hero.sh first" >&2; exit 1; }
tool="$root/apple/Icon/build/icon-tool"
if [[ ! -x "$tool" || "$root/scripts/apple/icon-tool.swift" -nt "$tool" ]]; then
    mkdir -p "$(dirname "$tool")"
    swiftc -O "$root/scripts/apple/icon-tool.swift" -o "$tool"
fi
camera="$(sed -n 's|^// camera: *||p' "$scad" | head -1)"
# The loop length lives in hero.scad beside sun_turns, since the two
# together set the speed; 30 fps, and frame N (= frame 0 again) is never
# rendered, so the loop has no repeated frame at the seam.
loop="$(sed -n 's|^// loop: *\([0-9]*\) s.*|\1|p' "$scad" | head -1)"
fps=30
frames=$((loop * fps))

# The caption, word for word the still's: build-hero.sh wrote these lines.
neo_t="$(sed -n 's|^neoscad .*: \([0-9.]*\) s$|\1|p' "$times")"
osc_t="$(sed -n 's|^OpenSCAD .*: \([0-9.]*\) s$|\1|p' "$times")"
runs="$(sed -n 's|^neoscad .*best of \([0-9]*\):.*|\1|p' "$times")"
cpu="$(sed -n 's|^machine: \(.*\), [0-9]* cores$|\1|p' "$times")"
[[ -n "$neo_t" && -n "$osc_t" && -n "$runs" && -n "$cpu" ]] ||
    { echo "build-hero-video: can't read $times" >&2; exit 1; }
title="Rendered in $neo_t s by NeoSCAD  —  OpenSCAD nightly: $osc_t s"
subtitle="BOSL2 herringbone planetary gearbox on a gyroid plinth; full render to STL, best of $runs, $cpu"

if [[ -n "${WORK:-}" ]]; then
    work="$WORK"
    mkdir -p "$work"
else
    work="$(mktemp -d)"
    trap 'rm -rf "$work"' EXIT
fi

# One shard is one second of video: both colour schemes rendered by
# --animate for its 30 frames, then each frame through the still's image
# steps, with the full-size renders deleted as soon as a frame is done,
# so the disk holds only finished frames plus JOBS shards in flight.
# 2x supersampling (3840x2160 renders for 1920x1080 frames), not the
# still's 3x: 3x has 2.25 times the pixels to render and to matte, for
# every one of thousands of frames, and each neoscad already peaks near
# 1.5 GB at 2x; the video encoder then smooths away the finer edge
# antialiasing 3x would buy.
shard() {
    local k="$1" d="$work/shard$1" f
    mkdir -p "$d"
    for scheme in Starnight Nature; do
        "$neoscad" "$scad" -o "$d/$scheme.png" --render --imgsize 3840,2160 \
            --camera "$camera" --colorscheme "$scheme" -q \
            --animate "$frames" --animate_sharding "$k/$loop"
    done
    for s in "$d"/Starnight*.png; do
        f="${s##*/Starnight}"
        f="${f%.png}"
        "$tool" matte "$s" "$d/Nature$f.png" 250 "$d/matte$f.png" >/dev/null
        "$tool" backdrop "$d/matte$f.png" "#23264a" "#07080f" "$d/backdrop$f.png"
        "$tool" resize-to "$d/backdrop$f.png" 1920 1080 "$d/plain$f.png"
        "$tool" caption "$d/plain$f.png" "$work/frame$f.png" "$title" "$subtitle"
    done
    rm -r "$d"
}
export -f shard
export neoscad scad camera frames loop tool title subtitle work
# A WORK directory that already holds every frame (a finished earlier
# run) goes straight to encoding, so the encoder settings can be tuned
# without the hour of rendering.
count() { find "$work" -name 'frame*.png' | wc -l | tr -d ' '; }
if [[ "$(count)" != "$frames" ]]; then
    start=$(date +%s)
    seq "$loop" | xargs -P "$jobs" -I{} bash -c 'shard {}'
    n="$(count)"
    [[ "$n" == "$frames" ]] || { echo "build-hero-video: $n of $frames frames rendered" >&2; exit 1; }
    echo "build-hero-video: $frames frames in $(($(date +%s) - start)) s"
fi

# The frames are sRGB PNGs. ffmpeg's default RGB-to-YUV matrix is BT.601
# while browsers decode untagged HD video as BT.709, which would shift
# every colour against the PNG poster shown until playback starts; so
# convert with BT.709 and tag it (setparams: output options alone let the
# PNGs' sRGB transfer tag through, which not every player honours). yuv420p so every decoder takes it; a
# keyframe every 20 s rather than every 8, since nothing seeks in a
# looping hero and each keyframe costs about as much as a second of
# motion. No audio track: the page autoplays it muted.
#
# Two files: AV1 in WebM, the smaller at equal quality, which the page
# offers first to browsers that can decode it, and H.264 in MP4 (with
# faststart, so playback starts before the download ends) for the rest.
# The loop is two minutes long, so the CRFs decide the download size;
# these are the highest found that keep tooth edges clean at 1080p
# (luma SSIM about 0.99 against the frames for both).
yuv=(-vf "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p,setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709:range=tv"
     -g $((20 * fps)))
ffmpeg -v error -y -framerate "$fps" -i "$work/frame%05d.png" -an "${yuv[@]}" \
    -c:v libx264 -preset slow -tune animation -crf 30 -movflags +faststart \
    "$out/hero-gearbox.mp4"
ffmpeg -v error -y -framerate "$fps" -i "$work/frame%05d.png" -an "${yuv[@]}" \
    -c:v libsvtav1 -preset 6 -crf 46 -svtav1-params tune=0:svt-log-level=1 \
    "$out/hero-gearbox.webm"
ls -l "$out"/hero-gearbox.*
echo "build-hero-video: $out"
