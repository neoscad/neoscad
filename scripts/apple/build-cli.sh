#!/usr/bin/env bash
# Builds the `neoscad` command-line tool that the macOS app carries in
# NeoSCAD.app/Contents/Helpers/neoscad (docs/release.md, "The bundled
# command-line tool"; docs/mcp.md, "Setup from the apps"):
#
#   `cargo build --release --target <triple> -p neoscad-cli --bin neoscad`
#   for each architecture, joined with lipo when there are two, and
#   stripped of local symbols -> apple/build/cli/neoscad
#
# Xcode runs it as the CommandLineTool aggregate target's build phase
# (apple/project.yml), with the core's input file list, so it is skipped
# when no Rust input changed. The app target then copies the result into
# Contents/Helpers and signs it there.
#
# Architectures as build-core.sh: `--universal`, else Xcode's ARCHS, else
# arm64. The environment and target triples are exactly those of
# release.sh's own CLI build, so in a release both are the same cargo
# outputs (release.sh checks the bundled copy against the CLI's dSYM).
#
#   scripts/apple/build-cli.sh              build (release profile)
#   scripts/apple/build-cli.sh --universal  arm64 + x86_64
set -euo pipefail
cd "$(dirname "$0")/../.."
root=$PWD

out_dir=$root/apple/build/cli
out=$out_dir/neoscad

universal=0
for arg in "$@"; do
    case $arg in
        --universal) universal=1 ;;
        *)
            echo "build-cli: unknown argument $arg" >&2
            exit 2
            ;;
    esac
done
if [ $universal = 1 ]; then
    archs="arm64 x86_64"
else
    archs=${ARCHS:-arm64}
fi
triples=()
for arch in $archs; do
    case $arch in
        arm64) triples+=(aarch64-apple-darwin) ;;
        x86_64) triples+=(x86_64-apple-darwin) ;;
        *)
            echo "build-cli: no Rust target for architecture $arch" >&2
            exit 1
            ;;
    esac
done
target_dir=${CARGO_TARGET_DIR:-$root/target}
# The same clean environment as build-core.sh and release.sh: Xcode's
# build settings arrive as environment variables, and build scripts that
# watch them would rebuild the CLI whenever it is built from the other
# side.
cargo_env=(env -i
    HOME="$HOME"
    PATH="$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin"
    MACOSX_DEPLOYMENT_TARGET=15.0
    CARGO_TARGET_DIR="$target_dir"
    TERM="${TERM:-dumb}")
if [ -n "${RUSTUP_HOME:-}" ]; then cargo_env+=(RUSTUP_HOME="$RUSTUP_HOME"); fi
if [ -n "${CARGO_HOME:-}" ]; then cargo_env+=(CARGO_HOME="$CARGO_HOME"); fi

thin=()
for triple in "${triples[@]}"; do
    "${cargo_env[@]}" cargo build --quiet --release --target "$triple" -p neoscad-cli --bin neoscad
    thin+=("$target_dir/$triple/release/neoscad")
done

mkdir -p "$out_dir"
# Current when it holds every requested architecture and no requested
# thin binary is newer; the stamp records the architectures. As in
# build-core.sh, a Debug build (arm64) after a Release one keeps a current
# universal binary, so the two configurations don't rebuild it in turn;
# any change makes it again from exactly the requested architectures.
stamp=$out_dir/archs
current=0
if [ -f "$out" ] && [ -f "$stamp" ]; then
    current=1
    have=" $(cat "$stamp") "
    for arch in $archs; do
        case $have in *" $arch "*) ;; *) current=0 ;; esac
    done
    for t in "${thin[@]}"; do
        if [ "$t" -nt "$out" ]; then current=0; fi
    done
fi
if [ $current = 0 ]; then
    tmp=$out.tmp
    if [ ${#thin[@]} = 1 ]; then
        cp "${thin[0]}" "$tmp"
    else
        lipo -create "${thin[@]}" -output "$tmp"
    fi
    # Local symbols only serve a debugger; release.sh keeps the dSYM.
    # Stripping changes no UUID, so the dSYM still matches.
    strip -x "$tmp"
    mv "$tmp" "$out"
    echo "$archs" >"$stamp"
    echo "build-cli: $out ($(du -sh "$out" | cut -f1), $archs)"
fi
# Xcode's dependency analysis compares against these outputs.
touch "$out"
if [ -n "${CONFIGURATION:-}" ]; then touch "$out_dir/cli-$CONFIGURATION.stamp"; fi
