# Sourced by build-core.sh and build-view.sh before `cargo build`.
#
# rustc writes source paths into a module: panic locations (`file!()` in
# every `unwrap`, `expect` and index check) and any debug info. Unmapped,
# a published wasm names the builder's home directory, user name and
# checkout layout (/Users/<name>/.cargo/registry/src/..., the repository
# root). `--remap-path-prefix` rewrites them to neutral prefixes, so the
# module is the same whoever builds it and says nothing about their
# machine. rustc applies the last matching mapping, so the most specific
# prefixes come last. std's own paths are already /rustc/<hash>/.
#
# The flags go into CARGO_ENCODED_RUSTFLAGS (separated by 0x1f), which
# keeps a path with spaces as one argument, and extend any RUSTFLAGS or
# CARGO_ENCODED_RUSTFLAGS the caller set rather than replacing them.
# Changing rustflags rebuilds the wasm target's crates once; the host
# build is unaffected (it is a different target, and build scripts and
# proc macros do not get target rustflags when --target is given).
#
# Requires $root (the repository root) and $target_dir (cargo's target
# directory) to be set.

neoscad_remap_paths() {
    local cargo_home=${CARGO_HOME:-$HOME/.cargo}
    local sep=$'\x1f'
    local flags=()
    local target_abs
    target_abs=$(mkdir -p "$target_dir" && cd "$target_dir" && pwd)
    flags+=("--remap-path-prefix=$root=/neoscad")
    flags+=("--remap-path-prefix=$cargo_home/registry/src=/cargo/registry/src")
    flags+=("--remap-path-prefix=$cargo_home/git/checkouts=/cargo/git/checkouts")
    # Build-script output (OUT_DIR, which `include!` may pull in) is under
    # the target directory: inside the repository by default, elsewhere
    # with a per-worktree CARGO_TARGET_DIR. Last, so it wins over $root.
    flags+=("--remap-path-prefix=$target_abs=/neoscad/target")

    local encoded=""
    if [ -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]; then
        encoded=$CARGO_ENCODED_RUSTFLAGS
    elif [ -n "${RUSTFLAGS:-}" ]; then
        # RUSTFLAGS is whitespace-separated; CARGO_ENCODED_RUSTFLAGS wins
        # over it, so carry its words across.
        local word
        for word in $RUSTFLAGS; do
            encoded=${encoded:+$encoded$sep}$word
        done
    fi
    local f
    for f in "${flags[@]}"; do
        encoded=${encoded:+$encoded$sep}$f
    done
    export CARGO_ENCODED_RUSTFLAGS=$encoded
    unset RUSTFLAGS
}
