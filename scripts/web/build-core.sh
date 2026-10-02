#!/usr/bin/env bash
# Builds the browser demo's worker core (crates/web; docs/web-protocol.md)
# into dist/web-core/:
#
#   1. `cargo build --profile web --target wasm32-unknown-unknown -p neoscad-web`
#      (no debug info, stripped, fat LTO, panic = abort; Cargo.toml)
#   2. `wasm-bindgen --target web` -> neoscad_web.js + neoscad_web_bg.wasm
#      The CLI must be exactly the version Cargo.lock pins for the
#      `wasm-bindgen` crate (their schemas must match); it is installed on
#      first use into $CARGO_TARGET_DIR/tools, not globally, so another
#      project's wasm-bindgen is never replaced.
#   3. `wasm-opt -O3` when binaryen is on PATH (or WASM_OPT names it, or
#      WASM_OPT=docker runs it in an Alpine container); skipped otherwise,
#      with a note. The module works either way.
#   4. The reference worker (crates/web/js/worker.js) beside them, and the
#      raw and gzipped sizes (the plan's target: core <= 4 MB gzipped).
#
#   scripts/web/build-core.sh               build
#   scripts/web/build-core.sh --out DIR     somewhere else
set -euo pipefail
cd "$(dirname "$0")/../.."
root=$PWD

if [ -f "$HOME/.cargo/env" ] && ! command -v cargo >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi

out=$root/dist/web-core
if [ "${1:-}" = "--out" ]; then
    out=$2
fi

if [ ! -d "$(rustc --print sysroot)/lib/rustlib/wasm32-unknown-unknown" ]; then
    echo "build-core: the wasm32-unknown-unknown target is not installed" \
        "(rustup target add wasm32-unknown-unknown)" >&2
    exit 1
fi

# Cargo writes to $CARGO_TARGET_DIR when it is set (per-worktree target
# directories); looking only in ./target would package a stale module.
target_dir=${CARGO_TARGET_DIR:-$root/target}
version=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/[^0-9.]/, ""); print; exit }' Cargo.lock)
if [ -z "$version" ]; then
    echo "build-core: no wasm-bindgen in Cargo.lock" >&2
    exit 1
fi
tools=$target_dir/tools
bindgen=$tools/bin/wasm-bindgen
if [ ! -x "$bindgen" ] || [ "$("$bindgen" --version)" != "wasm-bindgen $version" ]; then
    echo "build-core: installing wasm-bindgen-cli $version into $tools"
    cargo install --quiet wasm-bindgen-cli --version "$version" --locked --root "$tools"
fi

# No local paths in the module (scripts/web/remap-paths.sh).
# shellcheck source=scripts/web/remap-paths.sh
source "$root/scripts/web/remap-paths.sh"
neoscad_remap_paths
# NEOSCAD_FEATURES (e.g. `heap-eval`): features of neoscad-web to build with.
cargo build --quiet --profile web --target wasm32-unknown-unknown -p neoscad-web ${NEOSCAD_FEATURES:+--features "$NEOSCAD_FEATURES"}
wasm=$target_dir/wasm32-unknown-unknown/web/neoscad_web.wasm

rm -rf "$out"
mkdir -p "$out"
"$bindgen" --target web --no-typescript --out-dir "$out" "$wasm"

module=$out/neoscad_web_bg.wasm
# The module uses the features rustc enables by default for wasm32 (bulk
# memory, sign extension, mutable globals, non-trapping float-to-int,
# multi-value, reference types).
opt_args=(-O3 --enable-bulk-memory --enable-sign-ext --enable-mutable-globals
    --enable-nontrapping-float-to-int --enable-multivalue --enable-reference-types)
wasm_opt=${WASM_OPT:-$(command -v wasm-opt || true)}
if [ "$wasm_opt" = docker ]; then
    # binaryen from Alpine's packages, in a throwaway container: for a
    # machine without binaryen installed. Measured September 2026 (wasm-opt
    # 129): it saves about 14 KB of 4.4 MB gzipped, so it is opt-in.
    docker run --rm -v "$out":/w -w /w alpine:3 sh -c \
        "apk add --no-cache binaryen >/dev/null && wasm-opt ${opt_args[*]} neoscad_web_bg.wasm -o neoscad_web_bg.opt.wasm"
    mv "$out/neoscad_web_bg.opt.wasm" "$module"
elif [ -n "$wasm_opt" ]; then
    "$wasm_opt" "${opt_args[@]}" "$module" -o "$module.opt"
    mv "$module.opt" "$module"
else
    echo "build-core: wasm-opt not found (binaryen; WASM_OPT=docker runs it in a container); skipped -O3"
fi

cp crates/web/js/worker.js "$out/worker.js"
# The glue is ES modules; node (crates/web/test/run.mjs) needs this to
# load them. Browsers ignore it.
echo '{ "type": "module" }' >"$out/package.json"

size() { wc -c <"$1" | tr -d ' '; }
gz() { gzip -9 -c "$1" | wc -c | tr -d ' '; }
echo "build-core: $out"
for f in "$module" "$out/neoscad_web.js" "$out/worker.js"; do
    printf '  %-22s %10s bytes  %10s gzipped\n' "$(basename "$f")" "$(size "$f")" "$(gz "$f")"
done
