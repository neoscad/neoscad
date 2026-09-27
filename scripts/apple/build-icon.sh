#!/usr/bin/env bash
# Render an app icon concept with neoscad and emit every asset a macOS
# app icon can be built from, into apple/Icon/build/CONCEPT/:
#
#   art-4096.png          the model on transparent, framed, 4096 px
#   art-{1024,512,128,32,16}.png   downscaled from it
#   sheet.png             contact sheet of those sizes (docs/icon.md)
#   AppIcon.appiconset/   classic macOS icon: tile + art, all ten sizes
#   AppIcon.icon/         Icon Composer document: gradient fill (light and
#                         dark) plus the art as one glass layer
#   preview-*.png         the .icon rendered by Icon Composer's ictool
#   actool/               the .icon and .appiconset compiled by actool
#
# Usage: scripts/apple/build-icon.sh a|b|c|path/to/concept.scad
#
# The concept's .scad carries its own camera and tile colours in header
# comments ("// camera: ..." and "// tile: #top #bottom"), so a concept
# is one self-describing file. Nothing here touches the Xcode project:
# the app's icon is a committed copy of concept C's AppIcon.icon, updated
# by hand (docs/icon.md, "The app icon").
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
neoscad="${NEOSCAD:-$root/target/release/neoscad}"
# A worktree has no target/ of its own; fall back to the main checkout's.
if [[ ! -x "$neoscad" ]]; then
    main="$(git -C "$root" worktree list --porcelain | awk '/^worktree /{print $2; exit}')"
    neoscad="$main/target/release/neoscad"
fi
[[ -x "$neoscad" ]] || { echo "build-icon: no neoscad at $neoscad (cargo build --release, or set NEOSCAD)" >&2; exit 1; }

case "${1:-}" in
    a|b|c) scad="$root/apple/Icon/concept-$1.scad" ;;
    *.scad) scad="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")" ;;
    *) echo "usage: $0 a|b|c|FILE.scad" >&2; exit 2 ;;
esac
name="$(basename "$scad" .scad)"
out="$root/apple/Icon/build/$name"
mkdir -p "$out"

tool="$root/apple/Icon/build/icon-tool"
if [[ ! -x "$tool" || "$root/scripts/apple/icon-tool.swift" -nt "$tool" ]]; then
    swiftc -O "$root/scripts/apple/icon-tool.swift" -o "$tool"
fi

camera="$(sed -n 's|^// camera: *||p' "$scad" | head -1)"
read -r tile_top tile_bottom <<<"$(sed -n 's|^// tile: *||p' "$scad" | head -1)"
: "${camera:?$scad has no '// camera:' line}"
: "${tile_top:?$scad has no '// tile:' line}"

# Two renders that differ only in background (black Starnight, #fafafa
# Nature) give an exact alpha matte; see icon-tool.swift. 4096 px is 4x
# supersampling for the 1024 master, since neoscad's PNG export draws
# without MSAA.
render() {
    "$neoscad" "$scad" -o "$1" --render --imgsize 4096,4096 --camera "$camera" \
        --colorscheme "$2" -q
}
start=$(date +%s)
render "$out/dark.png" Starnight
render "$out/light.png" Nature
echo "$name: two 4096 px renders in $(( $(date +%s) - start )) s"
"$tool" matte "$out/dark.png" "$out/light.png" 250 "$out/matte.png"
"$tool" fit "$out/matte.png" 0.92 "$out/art-4096.png"
rm "$out/dark.png" "$out/light.png" "$out/matte.png"
for s in 1024 512 128 32 16; do
    "$tool" resize "$out/art-4096.png" "$s" "$out/art-$s.png"
done
"$tool" sheet "$out/sheet.png" "NeoSCAD icon $name (transparent; 1:1, then 8x)" \
    "$out"/art-{1024,512,128,32,16}.png

# Classic asset catalog icon. macOS draws .appiconset images as they are
# (no mask), so the tile is baked in; the small sizes are downscaled from
# the 1024 tile rather than re-rendered.
set_dir="$out/AppIcon.appiconset"
rm -rf "$set_dir" && mkdir -p "$set_dir"
"$tool" tile "$out/art-1024.png" "$tile_top" "$tile_bottom" "$out/tile-1024.png"
entries=()
for pt in 16 32 128 256 512; do
    for scale in 1 2; do
        px=$(( pt * scale ))
        suffix=""
        [[ $scale == 2 ]] && suffix="@2x"
        file="icon_${pt}x${pt}${suffix}.png"
        "$tool" resize "$out/tile-1024.png" "$px" "$set_dir/$file"
        entries+=("{\"filename\":\"$file\",\"idiom\":\"mac\",\"scale\":\"${scale}x\",\"size\":\"${pt}x${pt}\"}")
    done
done
(IFS=,; printf '{"images":[%s],"info":{"author":"xcode","version":1}}\n' "${entries[*]}") \
    | python3 -m json.tool >"$set_dir/Contents.json"

# Icon Composer document. The system supplies the rounded-square mask,
# the Liquid Glass material, and the dark, tinted and clear variants, so
# the document holds only a fill and an untiled, transparent layer. The
# layer is the art reframed smaller (72% of the canvas, not 92%): Icon
# Composer draws a layer across the whole canvas and the mask clips it,
# so full-bleed art loses its corners to the rounded square.
doc="$out/AppIcon.icon"
rm -rf "$doc" && mkdir -p "$doc/Assets"
"$tool" fit "$out/art-4096.png" 0.72 "$out/layer-4096.png"
"$tool" resize "$out/layer-4096.png" 1024 "$doc/Assets/art.png"
rm "$out/layer-4096.png"
srgb() { # #rrggbb -> "srgb:r,g,b,1" with five decimals, as Icon Composer writes
    echo "$((16#${1:1:2})) $((16#${1:3:2})) $((16#${1:5:2}))" \
        | awk '{printf "srgb:%.5f,%.5f,%.5f,1.00000", $1/255, $2/255, $3/255}'
}
cat >"$doc/icon.json" <<EOF
{
  "fill-specializations" : [
    {
      "value" : {
        "linear-gradient" : [ "srgb:0.94118,0.95294,1.00000,1.00000", "srgb:0.78431,0.81961,0.94902,1.00000" ],
        "orientation" : { "start" : { "x" : 0.5, "y" : 0 }, "stop" : { "x" : 0.5, "y" : 0.7 } }
      }
    },
    {
      "appearance" : "dark",
      "value" : {
        "linear-gradient" : [ "$(srgb "$tile_top")", "$(srgb "$tile_bottom")" ],
        "orientation" : { "start" : { "x" : 0.5, "y" : 0 }, "stop" : { "x" : 0.5, "y" : 0.7 } }
      }
    }
  ],
  "groups" : [
    {
      "layers" : [
        {
          "image-name" : "art.png",
          "name" : "art",
          "glass" : true
        }
      ],
      "shadow" : { "kind" : "neutral", "opacity" : 0.5 },
      "translucency" : { "enabled" : true, "value" : 0.2 },
      "specular" : true
    }
  ],
  "supported-platforms" : {
    "squares" : [ "macOS" ]
  }
}
EOF

# Check the document the way Xcode will read it: Icon Composer's own
# renderer for the appearances, and actool for the compiled catalog.
ictool="$(dirname "$(xcode-select -p)")/Applications/Icon Composer.app/Contents/Executables/ictool"
if [[ -x "$ictool" ]]; then
    for r in Default Dark TintedDark ClearLight; do
        "$ictool" "$doc" --export-image --output-file "$out/preview-$r.png" \
            --platform macOS --rendition "$r" --width 512 --height 512 --scale 1 \
            || echo "build-icon: ictool could not render $r" >&2
    done
fi
rm -rf "$out/actool" && mkdir -p "$out/actool/icon" "$out/actool/appiconset"
xcrun actool "$doc" --compile "$out/actool/icon" --platform macosx \
    --minimum-deployment-target 26.0 --app-icon AppIcon \
    --output-partial-info-plist "$out/actool/icon/partial.plist" >"$out/actool/icon/log.plist"
mkdir -p "$out/actool/catalog.xcassets"
cp -R "$set_dir" "$out/actool/catalog.xcassets/"
xcrun actool "$out/actool/catalog.xcassets" --compile "$out/actool/appiconset" --platform macosx \
    --minimum-deployment-target 14.0 --app-icon AppIcon \
    --output-partial-info-plist "$out/actool/appiconset/partial.plist" >"$out/actool/appiconset/log.plist"
echo "$name: built $out"
