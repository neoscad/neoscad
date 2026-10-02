#!/usr/bin/env bash
# Builds the Rust core for the macOS app (docs/audits/macos-prep.md, 8b):
#
#   1. `cargo build --release --target <triple> -p neoscad-ffi` for each
#      architecture -> target/<triple>/release/libneoscad_ffi.a
#   2. `uniffi-bindgen-swift` (crates/uniffi-bindgen) on the arm64 library
#      (the interface is the same on both)
#      -> apple/Core/Generated/neoscad_ffi.swift (compiled into
#         NeoSCADCore.framework) and a C header plus module map
#   3. `lipo -create` of the libraries when there are two, and
#      `xcodebuild -create-xcframework`
#      -> apple/build/NeoSCADCore.xcframework (the library and headers;
#         one macOS slice, arm64 or arm64_x86_64)
#
# Steps 2 and 3 only run when a library changed, so a build with no
# Rust changes is cargo's no-op check plus a few stats.
#
# Architectures: `--universal`, else Xcode's ARCHS (the project's Release
# configuration is arm64 + x86_64, Debug arm64 alone, so everyday builds
# and `xcodebuild test` compile the core once), else arm64. The stamp
# records which the XCFramework holds. A request for fewer (a Debug build
# after a Release one) keeps a current universal XCFramework rather than
# rebuild it; any library change makes it again from exactly the requested
# architectures, so an XCFramework never pairs a current arm64 slice with
# a stale x86_64 one.
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
#   scripts/apple/build-core.sh              build (release profile)
#   scripts/apple/build-core.sh --universal  arm64 + x86_64 (release.sh)
#   scripts/apple/build-core.sh --prepare    as xcodegen runs it
set -euo pipefail
cd "$(dirname "$0")/../.."
root=$PWD

apple=$root/apple
build=$apple/build
generated=$apple/Core/Generated
inputs=$build/core-inputs.xcfilelist
xcframework=$build/NeoSCADCore.xcframework
stamp=$build/core.stamp

prepare=0
universal=0
for arg in "$@"; do
    case $arg in
        --prepare) prepare=1 ;;
        --universal) universal=1 ;;
        *)
            echo "build-core: unknown argument $arg" >&2
            exit 2
            ;;
    esac
done
if [ $universal = 1 ]; then
    archs="arm64 x86_64"
else
    # Xcode's own list, as the aggregate target's configuration sets it.
    archs=${ARCHS:-arm64}
fi
triples=()
for arch in $archs; do
    case $arch in
        arm64) triples+=(aarch64-apple-darwin) ;;
        x86_64) triples+=(x86_64-apple-darwin) ;;
        *)
            echo "build-core: no Rust target for architecture $arch" >&2
            exit 1
            ;;
    esac
done
# Cargo's output directory: `CARGO_TARGET_DIR` when set (several checkouts
# of the repository can share one, so the dependencies compile once), else
# the workspace's `target`. It has to be read here and handed to cargo
# explicitly, because cargo runs under `env -i` below and would otherwise
# build into `target` while this script looked for the library elsewhere.
target_dir=${CARGO_TARGET_DIR:-$root/target}
lib_of() { echo "$target_dir/$1/release/libneoscad_ffi.a"; }
bindgen=$target_dir/release/uniffi-bindgen-swift
# Xcode's dependency analysis skips the build phase when no input is newer
# than its outputs; this per-configuration output (apple/project.yml) makes
# the first Release build after a Debug one run the phase, which the
# arm64-only XCFramework would otherwise not survive (its x86_64 link
# fails).
config_stamp=${CONFIGURATION:+$build/core-$CONFIGURATION.stamp}

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
if [ $prepare = 1 ] && [ -f "$xcframework/Info.plist" ] &&
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
cargo_env+=(CARGO_TARGET_DIR="$target_dir")

for triple in "${triples[@]}"; do
    "${cargo_env[@]}" cargo build --quiet --release --target "$triple" -p neoscad-ffi --lib ${NEOSCAD_FEATURES:+--features "$NEOSCAD_FEATURES"}
done
"${cargo_env[@]}" cargo build --quiet --release -p neoscad-uniffi-bindgen

finish() {
    touch "$stamp" "$generated/neoscad_ffi.swift" "$xcframework/Info.plist"
    if [ -n "$config_stamp" ]; then touch "$config_stamp"; fi
}

# Current when the XCFramework holds every requested architecture and no
# requested library (nor the binding generator) is newer than it.
current=0
if [ -f "$stamp" ] && [ -d "$xcframework" ] && [ -f "$generated/neoscad_ffi.swift" ] &&
    [ ! "$bindgen" -nt "$stamp" ]; then
    current=1
    have=" $(cat "$stamp") "
    for arch in $archs; do
        case $have in *" $arch "*) ;; *) current=0 ;; esac
    done
    for triple in "${triples[@]}"; do
        if [ "$(lib_of "$triple")" -nt "$stamp" ]; then current=0; fi
    done
fi
if [ $current = 1 ]; then
    # Nothing changed: tell Xcode the outputs are current.
    finish
    exit 0
fi

echo "build-core: generating bindings and $xcframework ($archs)"
staging=$build/staging
rm -rf "$staging"
mkdir -p "$staging/swift" "$staging/include" "$staging/lib" "$generated"
# The bindings come from the first library's metadata; every architecture
# is built from the same source, so they are the same for all of them.
first=$(lib_of "${triples[0]}")
"${cargo_env[@]}" "$bindgen" "$first" "$staging/swift" --swift-sources --metadata-no-deps
"${cargo_env[@]}" "$bindgen" "$first" "$staging/include" --headers --modulemap \
    --module-name neoscad_ffiFFI --modulemap-filename module.modulemap \
    --metadata-no-deps
# Replace the Swift source only when it changed, so the framework's Swift
# is not recompiled after a Rust change that left the interface alone.
if ! cmp -s "$staging/swift/neoscad_ffi.swift" "$generated/neoscad_ffi.swift"; then
    cp "$staging/swift/neoscad_ffi.swift" "$generated/neoscad_ffi.swift"
fi

if [ ${#triples[@]} = 1 ]; then
    lib=$first
else
    # One universal archive: an XCFramework takes a single library per
    # platform, and macOS's one slice then covers both architectures.
    lib=$staging/lib/libneoscad_ffi.a
    libs=()
    for triple in "${triples[@]}"; do libs+=("$(lib_of "$triple")"); done
    lipo -create "${libs[@]}" -output "$lib"
fi

rm -rf "$xcframework"
xcodebuild -create-xcframework \
    -library "$lib" -headers "$staging/include" \
    -output "$xcframework" >/dev/null
size=$(du -sh "$lib" | cut -f1)
rm -rf "$staging"
echo "$archs" >"$stamp"
finish
echo "build-core: done ($size static library, $archs)"
