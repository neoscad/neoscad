# shellcheck shell=bash
# Sourced by build-core.sh, build-cli.sh and release.sh: the PATH for the
# clean environment (`env -i`) they run cargo in.
#
# rustup's cargo is not always in ~/.cargo/bin (GitHub's macOS images from
# 2026-11 install it elsewhere), so the directory of the cargo the caller
# would run, and $CARGO_HOME/bin, come first.
neoscad_cargo_path() {
    local path="$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin"
    if [ -n "${CARGO_HOME:-}" ]; then path="$CARGO_HOME/bin:$path"; fi
    local cargo
    if cargo=$(command -v cargo 2>/dev/null) && [ -n "$cargo" ]; then
        path="$(dirname "$cargo"):$path"
    fi
    printf '%s\n' "$path"
}
