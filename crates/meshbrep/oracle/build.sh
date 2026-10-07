#!/usr/bin/env bash
# Builds the OCCT read-back oracle (check.cpp) against a prebuilt OCCT 8.0.1.
# A test tool only: meshbrep never depends on OCCT.
#
#   oracle/build.sh DIR      download OCCT into DIR (about 140 MB unpacked)
#                            and build DIR/check
#
# Then run the read-back test with it:
#
#   MESHBREP_OCCT_CHECK=DIR/check cargo test -p meshbrep --release --test occt
#
# The prebuilt static libraries are cadrum's release `occt-8_0_1_rev2`
# (github.com/lzpel/cadrum), built from OCCT's V8_0_1 tag. Supported
# hosts: aarch64/x86_64 macOS and Linux.
set -euo pipefail
dir=${1:?usage: oracle/build.sh DIR}
here=$(cd "$(dirname "$0")" && pwd)
case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) target=aarch64_apple_darwin ;;
    Darwin-x86_64) target=x86_64_apple_darwin ;;
    Linux-x86_64) target=x86_64_unknown_linux_gnu ;;
    Linux-aarch64) target=aarch64_unknown_linux_gnu ;;
    *) echo "unsupported host" >&2; exit 1 ;;
esac
name=occt-8_0_1_rev2-$target
mkdir -p "$dir"
if [ ! -d "$dir/$name" ]; then
    curl -fsSL -o "$dir/occt.tar.gz" \
        "https://github.com/lzpel/cadrum/releases/download/occt-8_0_1_rev2/$name.tar.gz"
    tar xzf "$dir/occt.tar.gz" -C "$dir"
    rm "$dir/occt.tar.gz"
fi
occt=$dir/$name
# Static libraries in dependency order (users before what they use).
libs="TKDESTEP TKDE TKXSBase TKMesh TKShHealing TKBool TKBO TKPrim TKTopAlgo TKGeomAlgo
      TKBRep TKGeomBase TKG3d TKG2d TKMath TKernel"
args=()
for l in $libs; do args+=("$occt/lib/lib$l.a"); done
extra=()
[ "$(uname -s)" = Linux ] && extra=(-lpthread -ldl)
c++ -std=c++17 -O1 -Wno-deprecated-declarations -o "$dir/check" "$here/check.cpp" -I "$occt/include/opencascade" \
    "${args[@]}" ${extra[@]+"${extra[@]}"}
echo "built $dir/check"
