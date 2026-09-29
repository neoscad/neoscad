#!/usr/bin/env bash
# Install the .deb or .rpm from DIR in a clean container of each supported
# distribution, with its own package manager (so the declared dependencies
# must resolve there), then run the installed neoscad: --version, an STL
# export of a small model, and the man page and completion scripts in
# place. Fails if any distribution fails.
#
#   scripts/release/package-smoke.sh [--arch amd64|arm64] DIR [IMAGE...]
#
# DIR holds nfpm's packages (neoscad_<ver>_<arch>.deb,
# neoscad-<ver>.<rpmarch>.rpm). --arch picks the architecture (default:
# Docker's); run it where that architecture is native, as the release
# workflow does on its x86_64 and arm64 runners, since emulation makes
# dnf's metadata step take minutes. The images default to the oldest and
# newest releases the packages claim (glibc 2.28 and later): Debian 10
# and 12, Ubuntu 22.04 and 24.04, Rocky Linux 8 and the current Fedora.
#
# The containers run in parallel, each capped at 1 GB, and each one's log
# is printed when all have finished. Recommended packages (Mesa, for PNG
# export) are not installed: they are large, optional, and not needed for
# an STL.
set -euo pipefail

arch=$(docker version --format '{{.Server.Arch}}')
if [[ "${1:-}" == "--arch" ]]; then
    arch=$2
    shift 2
fi
case "$arch" in
    arm64 | aarch64) arch=arm64 ;;
    amd64 | x86_64) arch=amd64 ;;
    *) echo "unsupported architecture $arch" >&2; exit 2 ;;
esac
[[ $# -ge 1 ]] || { sed -n '8p' "$0" >&2; exit 2; }
dir=$(cd "$1" && pwd)
shift
images=("$@")
if [[ ${#images[@]} -eq 0 ]]; then
    images=(debian:10 debian:12 ubuntu:22.04 ubuntu:24.04 rockylinux:8 fedora:latest)
fi

# Runs as root in each container, with the packages at /pkg.
# shellcheck disable=SC2016 # expanded in the container, not here
inner='set -eu
arch=$1
. /etc/os-release
if command -v apt-get >/dev/null; then
    if [ "$ID" = debian ] && [ "$VERSION_ID" = 10 ]; then
        # Buster is archived: its suites moved to archive.debian.org
        # (and buster-updates is gone).
        echo "deb http://archive.debian.org/debian buster main" >/etc/apt/sources.list
        echo "deb http://archive.debian.org/debian-security buster/updates main" >>/etc/apt/sources.list
    fi
    # The minimized Ubuntu images skip /usr/share/man and most of
    # /usr/share/doc when installing; the check below needs the man page.
    rm -f /etc/dpkg/dpkg.cfg.d/excludes
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends /pkg/neoscad_*_"$arch".deb
    dpkg -s neoscad | grep "^Version:"
    zsh_dir=/usr/share/zsh/vendor-completions
else
    case "$arch" in amd64) rpmarch=x86_64 ;; arm64) rpmarch=aarch64 ;; esac
    # The images set tsflags=nodocs, which would skip the man page.
    dnf install -y -q --setopt=install_weak_deps=False --setopt=tsflags= \
        /pkg/neoscad-*."$rpmarch".rpm
    rpm -q neoscad
    zsh_dir=/usr/share/zsh/site-functions
fi
neoscad --version
echo "cube(10); translate([15, 0, 0]) sphere(5, \$fn = 12);" >/tmp/smoke.scad
neoscad -o /tmp/smoke.stl /tmp/smoke.scad
head -n 1 /tmp/smoke.stl | grep -q "^solid"
facets=$(grep -c "facet normal" /tmp/smoke.stl)
[ "$facets" -gt 12 ] || { echo "only $facets facets" >&2; exit 1; }
echo "smoke.stl: $facets facets"
gzip -cd /usr/share/man/man1/neoscad.1.gz | grep -q "^\.TH NEOSCAD 1"
bash -c ". /usr/share/bash-completion/completions/neoscad && complete -p neoscad"
grep -q "^#compdef neoscad" "$zsh_dir/_neoscad"
grep -q "complete -c neoscad" /usr/share/fish/vendor_completions.d/neoscad.fish
echo "man page and completions in place"'

logs=$(mktemp -d)
trap 'rm -rf "$logs"' EXIT
pids=()
for image in "${images[@]}"; do
    log=$logs/${image//[:\/]/_}.log
    docker run --rm --platform "linux/$arch" --memory 1g --memory-swap 1g \
        -v "$dir:/pkg:ro" "$image" sh -c "$inner" sh "$arch" >"$log" 2>&1 &
    pids+=($!)
done

failed=()
for i in "${!images[@]}"; do
    image=${images[$i]}
    if wait "${pids[$i]}"; then status=ok; else status=FAILED; failed+=("$image"); fi
    [[ -n "${GITHUB_ACTIONS:-}" ]] && echo "::group::$image ($arch): $status"
    echo "== $image ($arch): $status"
    cat "$logs/${image//[:\/]/_}.log"
    [[ -n "${GITHUB_ACTIONS:-}" ]] && echo "::endgroup::"
done

if [[ ${#failed[@]} -gt 0 ]]; then
    echo "package smoke test failed ($arch): ${failed[*]}" >&2
    [[ -n "${GITHUB_ACTIONS:-}" ]] && echo "::error::package smoke test failed ($arch): ${failed[*]}"
    exit 1
fi
echo "package smoke test passed ($arch): ${images[*]}"
