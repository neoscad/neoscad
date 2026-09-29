#!/usr/bin/env bash
# Builds the browser's 3D view (crates/web-view) into an ES module and its
# wasm: cargo for wasm32, then wasm-bindgen --target web, then wasm-opt
# when it is installed. Prints the sizes (the web demo's budget for the
# view is 2 MB; docs/web-demo-plan.md).
#
#   scripts/web/build-view.sh [--out DIR] [--no-webgl] [--demo]
#
#   --out DIR    where the module goes (default $TARGET/web-view/pkg)
#   --no-webgl   WebGPU only: leave out wgpu's WebGL2 fallback (smaller)
#   --demo       also write the demo page and its fixtures next to the
#                module (crates/web-view/demo/), ready to serve:
#                python3 -m http.server -d DIR/..
#
# wasm-bindgen must match the wasm-bindgen crate in Cargo.lock exactly. The
# CLI is taken from $WASM_BINDGEN, then $TARGET/tools/bin, then PATH; if
# none matches it is installed into $TARGET/tools (never globally).
# wasm-opt comes from $WASM_OPT or PATH (binaryen); without it the module
# is left as wasm-bindgen wrote it.
set -euo pipefail
cd "$(dirname "$0")/../.."

if [ -f "$HOME/.cargo/env" ] && ! command -v cargo >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi

target="${CARGO_TARGET_DIR:-target}"
out=""
features=()
demo=0
while [ $# -gt 0 ]; do
    case "$1" in
        --out) out="$2"; shift 2 ;;
        --no-webgl) features=(--no-default-features); shift ;;
        --demo) demo=1; shift ;;
        *) echo "build-view: unknown option $1" >&2; exit 2 ;;
    esac
done
out="${out:-$target/web-view/pkg}"

# The web profile (no debug info, stripped) when the workspace has one;
# release otherwise. Debug info is most of an unstripped module's size.
if grep -q '^\[profile\.web\]' Cargo.toml; then
    profile=web
else
    profile=release
fi
# No local paths in the module (scripts/web/remap-paths.sh).
# shellcheck source=scripts/web/remap-paths.sh
source scripts/web/remap-paths.sh
root=$PWD target_dir=$target neoscad_remap_paths
echo "build-view: cargo build --profile $profile ${features[*]+"${features[*]}"}"
cargo build --quiet --profile "$profile" --target wasm32-unknown-unknown \
    -p neoscad-web-view ${features[@]+"${features[@]}"}
wasm="$target/wasm32-unknown-unknown/$profile/web_view.wasm"

version=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/[^0-9.]/, ""); print; exit }' Cargo.lock)
bindgen=""
for candidate in "${WASM_BINDGEN:-}" "$target/tools/bin/wasm-bindgen" "$(command -v wasm-bindgen || true)"; do
    if [ -n "$candidate" ] && [ -x "$candidate" ] &&
        [ "$("$candidate" --version | awk '{print $2}')" = "$version" ]; then
        bindgen="$candidate"
        break
    fi
done
if [ -z "$bindgen" ]; then
    echo "build-view: installing wasm-bindgen-cli $version into $target/tools"
    cargo install --quiet wasm-bindgen-cli --version "$version" --locked --root "$target/tools"
    bindgen="$target/tools/bin/wasm-bindgen"
fi

rm -rf "$out"
mkdir -p "$out"
"$bindgen" --target web --no-typescript --out-dir "$out" "$wasm"
module="$out/web_view_bg.wasm"

opt="${WASM_OPT:-$(command -v wasm-opt || true)}"
if [ -n "$opt" ]; then
    before=$(wc -c <"$module" | tr -d ' ')
    "$opt" -O3 --enable-bulk-memory --enable-nontrapping-float-to-int \
        --enable-sign-ext --enable-mutable-globals --strip-debug \
        "$module" -o "$module.opt"
    mv "$module.opt" "$module"
    echo "build-view: wasm-opt -O3: $before -> $(wc -c <"$module" | tr -d ' ') bytes"
else
    echo "build-view: wasm-opt not found (install binaryen or set WASM_OPT); module not optimised"
fi

size() { wc -c <"$1" | tr -d ' '; }
gz() { gzip -9 -c "$1" | wc -c | tr -d ' '; }
echo "build-view: $module: $(size "$module") bytes, $(gz "$module") gzipped"
echo "build-view: $out/web_view.js: $(size "$out/web_view.js") bytes, $(gz "$out/web_view.js") gzipped"

if [ "$demo" = 1 ]; then
    site="$(dirname "$out")"
    cp crates/web-view/demo/index.html "$site/index.html"
    cargo run --quiet -p neoscad-web-view --example pack_fixtures -- "$site/fixtures"
    echo "build-view: demo in $site; serve it with: python3 -m http.server -d $site 8000"
fi
