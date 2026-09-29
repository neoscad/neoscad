#!/usr/bin/env bash
# Build and test NeoSCAD for Linux in Docker, on the host's architecture
# (linux/arm64 on Apple silicon) or, with --platform linux/amd64, under
# emulation (slow: use it to prove the build, and run x86_64 conformance in
# CI instead).
#
#   scripts/release/linux-docker.sh [--platform P] build
#   scripts/release/linux-docker.sh [--platform P] test          # cargo test -p neoscad-cli
#   scripts/release/linux-docker.sh [--platform P] conformance [ARGS...]
#   scripts/release/linux-docker.sh [--platform P] png-smoke     # lavapipe, then llvmpipe GL
#   scripts/release/linux-docker.sh [--platform P] exec 'COMMAND'  # in /src
#   scripts/release/linux-docker.sh [--platform P] shell
#
# The checkout is mounted read-only at /src; cargo's registry and the
# target directory live under $NEOSCAD_DOCKER_DIR (default target/docker),
# so a rerun is incremental and `rm -rf` of that directory frees it all.
# The reference checkout is mounted read-only at its own absolute path (so
# a `.reference` symlink into another checkout still resolves), with a
# scratch tmpfs over `openscad/build`, where the harness runs the tests.
#
# Tier 3 geometry cases need the pinned OpenSCAD nightly to draw neoscad's
# meshes, and there is none for Linux aarch64. `conformance --cached-renderer`
# stands a stub in for it that answers `--version` as the macOS nightly
# does and fails every draw, so a case passes only when its mesh is
# byte-identical to one the macOS run already drew (seed the image cache by
# copying target/conformance/image-cache into $NEOSCAD_DOCKER_DIR/<arch>/
# target/conformance/). A failing case there means "differs from macOS",
# not necessarily "wrong".
#
# Every container is capped at 8 GB (--memory, no extra swap), so a
# runaway is killed inside the Docker VM instead of swapping the host.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
platform=""
if [[ "${1:-}" == "--platform" ]]; then
    platform=$2
    shift 2
fi
cmd=${1:-}
[[ -n "$cmd" ]] || { sed -n '2,13p' "$0"; exit 2; }
shift

case "${platform:-$(docker version --format '{{.Server.Arch}}')}" in
    linux/amd64 | amd64) arch=amd64; platform=linux/amd64 ;;
    linux/arm64 | arm64 | aarch64) arch=arm64; platform=linux/arm64 ;;
    *) echo "unsupported platform: $platform" >&2; exit 2 ;;
esac

image="neoscad-linux-build:$arch"
dock=${NEOSCAD_DOCKER_DIR:-$repo/target/docker}
mkdir -p "$dock/cargo-registry" "$dock/cargo-git" "$dock/$arch/target" "$repo/target"

if ! docker image inspect "$image" >/dev/null 2>&1; then
    docker build --platform "$platform" -t "$image" \
        -f "$repo/packaging/docker/build.Dockerfile" "$repo/packaging/docker"
fi

mounts=(
    -v "$repo:/src:ro"
    -v "$dock/$arch/target:/src/target"
    -v "$dock/cargo-registry:/usr/local/cargo/registry"
    -v "$dock/cargo-git:/usr/local/cargo/git"
)
if [[ -e "$repo/.reference" ]]; then
    ref=$(cd "$repo/.reference" && pwd -P)
    # The tmpfs needs an existing mount point under the read-only mount.
    mkdir -p "$ref/openscad/build"
    if [[ -L "$repo/.reference" ]]; then
        # /src/.reference is a symlink to $ref: mount it at that path.
        mounts+=(-v "$ref:$ref:ro" --tmpfs "$ref/openscad/build:exec,size=2g")
    else
        mounts+=(--tmpfs "/src/.reference/openscad/build:exec,size=2g")
    fi
fi

tty=()
[[ -t 0 && -t 1 ]] && tty=(-it)

run() {
    # ${a[@]+...}: macOS's bash 3.2 calls an empty array unbound under -u.
    docker run --rm ${tty[@]+"${tty[@]}"} --platform "$platform" \
        --memory 8g --memory-swap 8g \
        -e CARGO_TARGET_DIR=/src/target -e CARGO_TERM_COLOR=never \
        -w /src "${mounts[@]}" "$image" bash -euo pipefail -c "$1"
}

stub='mkdir -p /Applications/OpenSCAD.app/Contents/MacOS
cat > /Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD <<"EOF"
#!/bin/sh
if [ "$1" = "--version" ]; then echo "OpenSCAD version 2026.09.23" >&2; exit 0; fi
echo "stub renderer: mesh not in the macOS image cache" >&2
exit 1
EOF
chmod +x /Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD'

case "$cmd" in
    build)
        run "cargo build --release --locked -p neoscad-cli -p neoscad-conformance"
        ;;
    test)
        run "cargo test --locked -p neoscad-cli $*"
        ;;
    conformance)
        pre=":"
        args=()
        for a in ${@+"$@"}; do
            if [[ "$a" == "--cached-renderer" ]]; then pre=$stub; else args+=("$a"); fi
        done
        run "$pre
cargo build --release --locked -p neoscad-cli -p neoscad-conformance
target/release/conformance run ${args[*]+${args[*]}}"
        ;;
    png-smoke)
        run 'cargo build --release --locked -p neoscad-cli
cd /tmp
echo "cube(10); translate([15,0,0]) sphere(5);" > smoke.scad
check() {
    /src/target/release/neoscad smoke.scad -o "$1" --imgsize=320,240
    head -c 8 "$1" | od -An -c | grep -q "P   N   G" || { echo "$1 is not a PNG" >&2; exit 1; }
    echo "$1: $(stat -c %s "$1") bytes"
}
echo "== Vulkan (lavapipe)"
check vulkan.png
echo "== OpenGL (llvmpipe; no Vulkan ICD)"
VK_ICD_FILENAMES=/nonexistent.json VK_DRIVER_FILES=/nonexistent.json check gl.png
echo "== no driver at all: the error names Mesa"
if VK_ICD_FILENAMES=/nonexistent.json VK_DRIVER_FILES=/nonexistent.json \
    LIBGL_ALWAYS_SOFTWARE=1 __EGL_VENDOR_LIBRARY_FILENAMES=/nonexistent.json \
    /src/target/release/neoscad smoke.scad -o none.png 2>err.txt; then
    echo "expected a failure without drivers" >&2; exit 1
fi
cat err.txt
grep -q mesa-vulkan-drivers err.txt'
        ;;
    exec)
        run "$*"
        ;;
    shell)
        run "bash -i"
        ;;
    *)
        echo "unknown command: $cmd" >&2
        exit 2
        ;;
esac
