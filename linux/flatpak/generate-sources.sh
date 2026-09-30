#!/usr/bin/env bash
# Writes the Flatpak's offline sources from the lock files, with
# flatpak-builder-tools' generators (Flathub's documented way to build
# Rust and npm projects without network):
#
#   linux/flatpak/cargo-sources.json   every crate in Cargo.lock
#   linux/flatpak/node-sources.json    the editor bundle's npm packages
#                                      (apple/Editor/web/package-lock.json)
#
# Both are generated, not committed (.gitignore): rerun after a lock file
# changes. Needs python3 3.11+ with pip, and network.
#
#   linux/flatpak/generate-sources.sh
set -euo pipefail
cd "$(dirname "$0")/../.."
root=$PWD

# The generators at a known commit, so the same lock files give the same
# sources.
tools_rev=74697c75b630d7330e77250fc13cb5ea688d9479
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

curl -sSfL "https://codeload.github.com/flatpak/flatpak-builder-tools/tar.gz/$tools_rev" \
    | tar xz -C "$work"
tools=$work/flatpak-builder-tools-$tools_rev
python3 -m venv "$work/venv"
"$work/venv/bin/pip" install --quiet aiohttp tomlkit PyYAML "$tools/node"

"$work/venv/bin/python" "$tools/cargo/flatpak-cargo-generator.py" \
    "$root/Cargo.lock" -o "$root/linux/flatpak/cargo-sources.json"
"$work/venv/bin/flatpak-node-generator" npm \
    "$root/apple/Editor/web/package-lock.json" -o "$root/linux/flatpak/node-sources.json"
echo "wrote linux/flatpak/cargo-sources.json and linux/flatpak/node-sources.json"
