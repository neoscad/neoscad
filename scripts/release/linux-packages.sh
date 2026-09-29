#!/usr/bin/env bash
# Build the Linux release binary (the `dist` profile) in Docker and wrap it
# as .deb and .rpm with nfpm (packaging/nfpm.yaml), then print each
# package's metadata from dpkg-deb and rpm as a check. Output: dist/linux/.
#
#   scripts/release/linux-packages.sh [--platform linux/arm64|linux/amd64]
#
# The binary comes from scripts/release/linux-docker.sh's bookworm image,
# so it needs glibc 2.36, not the 2.28 of the release workflow's
# manylinux build: these packages prove the packaging, and CI builds the
# ones that ship (docs/packaging.md). NFPM_VERSION pins the nfpm image.
set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
platform_args=()
arch=$(docker version --format '{{.Server.Arch}}')
if [[ "${1:-}" == "--platform" ]]; then
    platform_args=(--platform "$2")
    arch=${2#linux/}
fi
case "$arch" in
    arm64 | aarch64) arch=arm64 ;;
    amd64 | x86_64) arch=amd64 ;;
    *) echo "unsupported architecture $arch" >&2; exit 2 ;;
esac
dock=${NEOSCAD_DOCKER_DIR:-$repo/target/docker}
nfpm_image=goreleaser/nfpm:${NFPM_VERSION:-v2.47.0}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -1)
out=$repo/dist/linux
stage=$dock/$arch/stage

"$repo/scripts/release/linux-docker.sh" ${platform_args[@]+"${platform_args[@]}"} exec \
    "cargo build --locked --profile dist -p neoscad-cli"
bin=$dock/$arch/target/dist/neoscad
[[ -x "$bin" ]] || { echo "no binary at $bin" >&2; exit 1; }

rm -rf "$stage"
mkdir -p "$stage" "$out"
"$repo/scripts/release/licenses.sh" "$stage"
cp "$bin" "$stage/neoscad"

for fmt in deb rpm; do
    docker run --rm --memory 1g -v "$repo/packaging/nfpm.yaml:/work/nfpm.yaml:ro" \
        -v "$stage:/work/stage:ro" -v "$out:/out" -w /work \
        -e NEOSCAD_VERSION="$version" -e NEOSCAD_ARCH="$arch" \
        "$nfpm_image" package -f nfpm.yaml -p "$fmt" -t /out/
done

# Inspect them where the tools live: dpkg in Debian, rpm in Fedora.
docker run --rm --memory 1g -v "$out:/out:ro" debian:bookworm-slim \
    sh -c 'for f in /out/*.deb; do dpkg-deb --info "$f"; dpkg-deb --contents "$f"; done'
docker run --rm --memory 1g -v "$out:/out:ro" fedora:42 \
    sh -c 'for f in /out/*.rpm; do rpm -qip "$f"; rpm -qlp "$f"; rpm -qp --requires --recommends "$f"; done'
ls -l "$out"
