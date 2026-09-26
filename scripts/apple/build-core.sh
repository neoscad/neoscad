#!/usr/bin/env bash
# Builds the Rust core for the macOS app (docs/audits/macos-prep.md, 8b):
#
#   1. `cargo build --release --target aarch64-apple-darwin -p neoscad-ffi`
#      -> target/aarch64-apple-darwin/release/libneoscad_ffi.a
#   2. `uniffi-bindgen-swift` (crates/uniffi-bindgen) on that library
#      -> apple/Core/Generated/neoscad_ffi.swift (compiled into
#         NeoSCADCore.framework) and a C header plus module map
#   3. `xcodebuild -create-xcframework`
#      -> apple/build/NeoSCADCore.xcframework (the library and headers)
#
# Steps 2 and 3 only run when the library changed, so a build with no
# Rust changes is cargo's no-op check plus a few stats.
#
# Xcode runs this as NeoSCADCore's first build phase, with
# apple/build/core-inputs.xcfilelist as its input file list: every file
# cargo reads. Xcode skips the phase when none is newer than the outputs,
# which is why the script touches its outputs even when nothing changed.
#
# `xcodegen generate` runs it first with `--prepare`
# (`options.preGenCommand` in apple/project.yml): Xcode reads the input
# list and resolves the XCFramework when it plans a build, before any
# build phase runs, so on a clean checkout both must exist already.
# `--prepare` writes the list and builds only when the outputs are missing.
#
#   scripts/apple/build-core.sh            build (release)
#   scripts/apple/build-core.sh --prepare  as xcodegen runs it
set -euo pipefail
cd "$(dirname "$0")/../.."
root=$PWD

apple=$root/apple
build=$apple/build
generated=$apple/Core/Generated
inputs=$build/core-inputs.xcfilelist
xcframework=$build/NeoSCADCore.xcframework
stamp=$build/core.stamp
target=aarch64-apple-darwin
lib=$root/target/$target/release/libneoscad_ffi.a
bindgen=$root/target/release/uniffi-bindgen-swift

# Every file cargo reads for the core: the workspace's manifests and lock
# file, each crate's sources and build scripts, the vendored crates and
# the assets compiled in. A superset is harmless (a change outside the
# core costs one cargo no-op); a missing file would leave a stale core.
write_inputs() {
    mkdir -p "$build"
    local tmp=$inputs.tmp
    {
        echo "$root/Cargo.toml"
        echo "$root/Cargo.lock"
        echo "$root/scripts/apple/build-core.sh"
        find "$root/crates" "$root/vendor" -type f \
            \( -name '*.rs' -o -name 'Cargo.toml' -o -name '*.toml' -o -name '*.wgsl' \) \
            -not -path '*/target/*' -not -path '*/tests/*' -not -path '*/benches/*'
        find "$root/assets" -type f -not -name '.DS_Store'
    } | LC_ALL=C sort -u >"$tmp"
    if ! cmp -s "$tmp" "$inputs" 2>/dev/null; then
        mv "$tmp" "$inputs"
    else
        rm "$tmp"
    fi
}

write_inputs
if [ "${1:-}" = "--prepare" ] && [ -f "$xcframework/Info.plist" ] &&
    [ -f "$generated/neoscad_ffi.swift" ]; then
    # The build phase keeps the core current from here on.
    exit 0
fi

# cargo from a clean environment: Xcode's build settings arrive as
# environment variables (SDKROOT, ARCHS, CC, ...), and build scripts that
# watch them would rebuild whenever the core is built from Xcode after the
# command line or the other way round. The deployment target is the
# app's, so the linker does not warn that the library needs a newer macOS.
cargo_env=(env -i
    HOME="$HOME"
    PATH="$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin"
    MACOSX_DEPLOYMENT_TARGET=15.0
    TERM="${TERM:-dumb}")
if [ -n "${RUSTUP_HOME:-}" ]; then cargo_env+=(RUSTUP_HOME="$RUSTUP_HOME"); fi
if [ -n "${CARGO_HOME:-}" ]; then cargo_env+=(CARGO_HOME="$CARGO_HOME"); fi

"${cargo_env[@]}" cargo build --quiet --release --target "$target" -p neoscad-ffi --lib
"${cargo_env[@]}" cargo build --quiet --release -p neoscad-uniffi-bindgen

if [ -f "$stamp" ] && [ -d "$xcframework" ] && [ -f "$generated/neoscad_ffi.swift" ] &&
    [ ! "$lib" -nt "$stamp" ] && [ ! "$bindgen" -nt "$stamp" ]; then
    # Nothing changed: tell Xcode the outputs are current.
    touch "$stamp" "$generated/neoscad_ffi.swift" "$xcframework/Info.plist"
    exit 0
fi

echo "build-core: generating bindings and $xcframework"
staging=$build/staging
rm -rf "$staging"
mkdir -p "$staging/swift" "$staging/include" "$generated"
"${cargo_env[@]}" "$bindgen" "$lib" "$staging/swift" --swift-sources --metadata-no-deps
"${cargo_env[@]}" "$bindgen" "$lib" "$staging/include" --headers --modulemap \
    --module-name neoscad_ffiFFI --modulemap-filename module.modulemap \
    --metadata-no-deps
# Replace the Swift source only when it changed, so the framework's Swift
# is not recompiled after a Rust change that left the interface alone.
if ! cmp -s "$staging/swift/neoscad_ffi.swift" "$generated/neoscad_ffi.swift"; then
    cp "$staging/swift/neoscad_ffi.swift" "$generated/neoscad_ffi.swift"
fi

rm -rf "$xcframework"
xcodebuild -create-xcframework \
    -library "$lib" -headers "$staging/include" \
    -output "$xcframework" >/dev/null
rm -rf "$staging"
touch "$stamp" "$generated/neoscad_ffi.swift" "$xcframework/Info.plist"
echo "build-core: done ($(du -sh "$lib" | cut -f1) static library)"
