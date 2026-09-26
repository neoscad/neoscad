#!/usr/bin/env bash
# Builds the pipeline for wasm32-unknown-unknown (crates/wasm-check, which
# pulls in lang, eval, geom with default features, io, text and the bundled
# assets) and runs its cases in node: primitives, a boolean, minkowski,
# text in the bundled font, include <MCAD/...>, import() and dxf_dim() from
# an in-memory file system, a host-supplied rands() seed, recursion that
# must end in OpenSCAD's error rather than a trap, and a preview (CSG
# products with `#` and `%`, their booleans, the preview scene), and a
# `session::Session` taking an edit to an open document (synchronous on
# wasm32, the second render reusing cached subtrees). Each result also
# goes through the renderer's CPU side (scene, colour scheme, camera fit).
# The renderer's GPU side (wgpu on WebGPU) is only built, not run: it needs
# a browser and wasm-bindgen glue, which this plain module has neither of.
#
#   scripts/wasm-check.sh            run the cases
#   scripts/wasm-check.sh --depths   also report the deepest recursion
#
# Skips (exit 0) when node or the wasm32 target is not installed.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -f "$HOME/.cargo/env" ] && ! command -v cargo >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi
if ! command -v node >/dev/null 2>&1; then
    echo "wasm-check: skipped, node is not installed"
    exit 0
fi
if ! command -v cargo >/dev/null 2>&1; then
    echo "wasm-check: skipped, cargo is not installed"
    exit 0
fi
if [ ! -d "$(rustc --print sysroot)/lib/rustlib/wasm32-unknown-unknown" ]; then
    echo "wasm-check: skipped, the wasm32-unknown-unknown target is not installed" \
        "(rustup target add wasm32-unknown-unknown)"
    exit 0
fi

cargo build --quiet --release --target wasm32-unknown-unknown -p neoscad-wasm-check
cargo build --quiet --release --target wasm32-unknown-unknown -p neoscad-render
echo "wasm-check: neoscad-render (wgpu, WebGPU backend) builds for wasm32"
wasm=target/wasm32-unknown-unknown/release/wasm_check.wasm
echo "wasm-check: $(node --version), $wasm ($(wc -c <"$wasm" | tr -d ' ') bytes)"
node crates/wasm-check/run.js "$wasm" "$@"
