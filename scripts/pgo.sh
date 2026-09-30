#!/usr/bin/env bash
# A profile-guided (PGO) build of the neoscad command-line binary, for the
# host target (docs/audits/perf-opportunities.md, P2):
#
#   1. build neoscad instrumented (-Cprofile-generate) in its own target
#      directory, since changing RUSTFLAGS rebuilds every crate anyway and
#      would otherwise throw away the normal build;
#   2. run scripts/pgo-train.py with it (bench models, the BOSL2 tests,
#      BOSL2 and OpenSCAD examples, snapshot/check/measure, a served edit
#      loop);
#   3. merge the raw profiles with llvm-profdata from the pinned
#      toolchain's llvm-tools component (its LLVM must match rustc's: a
#      profile from another LLVM version is rejected or, worse, misread);
#   4. build again with -Cprofile-use, same profile settings as the
#      normal build (thin LTO, one codegen unit).
#
#   scripts/pgo.sh [--profile release|dist]
#
# Prints the path of the optimised binary. Needs `rustup component add
# llvm-tools-preview` (for the toolchain in rust-toolchain.toml), python3,
# and .reference with BOSL2's tests_x/examples_x (`conformance
# bosl2-corpus`). Host target only: the instrumented binary has to run
# here, so a cross-built target cannot be trained this way.
#
# Output is byte-identical to the normal build's on everything checked
# (perf-opportunities.md, P2), with one known difference: PGO inlines
# more into the recursive evaluator, whose frames grow, so recursion
# reaches the stack budget (eval's DEFAULT_STACK_LIMIT) at a smaller depth
# and `*** Excluding N frames ***` counts change. Check deep-recursion
# models before shipping a PGO build.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -f "$HOME/.cargo/env" ] && ! command -v cargo >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi

profile=release
while [ $# -gt 0 ]; do
    case "$1" in
        --profile) profile=$2; shift 2 ;;
        -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
        *) echo "pgo.sh: unknown argument $1" >&2; exit 2 ;;
    esac
done

host=$(rustc -vV | sed -n 's/^host: //p')
profdata_tool=$(rustc --print sysroot)/lib/rustlib/$host/bin/llvm-profdata
if [ ! -x "$profdata_tool" ]; then
    echo "pgo.sh: no llvm-profdata; run: rustup component add llvm-tools-preview" >&2
    exit 1
fi

root=${CARGO_TARGET_DIR:-$PWD/target}
work=$root/pgo
raw=$work/raw
profdata=$work/neoscad.profdata
rm -rf "$raw" "$work/train"
mkdir -p "$raw"

# `--target $host` keeps RUSTFLAGS off build scripts and proc macros:
# without it they are instrumented too, write their own profiles into
# $raw during the build and warn about value-profile counters.
echo "pgo.sh: instrumented build ($profile)" >&2
RUSTFLAGS="${RUSTFLAGS:-} -Cprofile-generate=$raw" CARGO_TARGET_DIR="$work/gen" \
    cargo build --quiet --locked --profile "$profile" --target "$host" -p neoscad-cli

echo "pgo.sh: training" >&2
python3 scripts/pgo-train.py "$work/gen/$host/$profile/neoscad" "$work/train" >&2
"$profdata_tool" merge -o "$profdata" "$raw"

echo "pgo.sh: optimised build ($profile)" >&2
RUSTFLAGS="${RUSTFLAGS:-} -Cprofile-use=$profdata" CARGO_TARGET_DIR="$work/use" \
    cargo build --quiet --locked --profile "$profile" --target "$host" -p neoscad-cli
echo "$work/use/$host/$profile/neoscad"
