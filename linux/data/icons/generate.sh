#!/usr/bin/env bash
# Regenerates the Linux app's hicolor icons from the macOS app's icon art
# (apple/App/AppIcon.icon/Assets/art.png, 1024 px on transparent; how it
# is made is docs/icon.md). The art is the shape alone, without the macOS
# tile, which is what a GNOME app icon is. Run after the art changes and
# commit the PNGs: the Flatpak build installs them as they are, so it
# needs no image tools.
#
#   linux/data/icons/generate.sh
#
# Uses `sips` on macOS, else ImageMagick's `magick` or `convert`.
set -euo pipefail
cd "$(dirname "$0")/../../.."

art=apple/App/AppIcon.icon/Assets/art.png
for size in 32 48 64 128 256 512; do
    dir=linux/data/icons/hicolor/${size}x${size}/apps
    out=$dir/org.neoscad.NeoSCAD.png
    mkdir -p "$dir"
    if command -v sips >/dev/null; then
        sips -z "$size" "$size" "$art" --out "$out" >/dev/null
    elif command -v magick >/dev/null; then
        magick "$art" -resize "${size}x${size}" -strip "$out"
    else
        convert "$art" -resize "${size}x${size}" -strip "$out"
    fi
done
