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
# llvm-tools-preview` (for the toolchain in rust-toolchain.toml), Python 3
# ($PYTHON, else python3, else python), and .reference with BOSL2's
# tests_x/examples_x (`conformance bosl2-corpus`). Host target only: the
# instrumented binary has to run here, so a cross-built target cannot be
# trained this way. Meant for macOS, Linux and Windows under Git Bash (as
# GitHub's Windows runners have it; .github/workflows/pgo.yml).
#
# Output is byte-identical to the normal build's on everything checked
# (perf-opportunities.md, P2), with one known difference: PGO inlines
# more into the recursive evaluator, whose frames grow, so recursion
# reaches the stack budget (eval's DEFAULT_STACK_LIMIT) at a smaller depth
# and `*** Excluding N frames ***` counts change. Before shipping a PGO
# build, run the recursion-depth guard on it:
# `conformance depth --binary PATH`.
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
        -h|--help) sed -n '2,33p' "$0"; exit 0 ;;
        *) echo "pgo.sh: unknown argument $1" >&2; exit 2 ;;
    esac
done

# On Windows, rustc and cargo are native programs that cannot read Git
# Bash's /d/a/... paths, and a -Cprofile-generate directory they cannot
# resolve would put the profiles somewhere else: every path handed to them
# is converted to C:/... form first. Elsewhere this is the identity.
native() {
    if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s\n' "$1"; fi
}
python=${PYTHON:-$(command -v python3 || command -v python || true)}
if [ -z "$python" ]; then
    echo "pgo.sh: no Python 3 found; set PYTHON" >&2
    exit 1
fi

host=$(rustc -vV | sed -n 's/^host: //p')
exe=
case "$host" in *-windows-*) exe=.exe ;; esac
profdata_tool=$(native "$(rustc --print sysroot)")/lib/rustlib/$host/bin/llvm-profdata$exe
if [ ! -x "$profdata_tool" ]; then
    echo "pgo.sh: no llvm-profdata; run: rustup component add llvm-tools-preview" >&2
    exit 1
fi

root=$(native "${CARGO_TARGET_DIR:-$PWD/target}")
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
"$python" scripts/pgo-train.py "$work/gen/$host/$profile/neoscad$exe" "$work/train" >&2
"$profdata_tool" merge -o "$profdata" "$raw"

echo "pgo.sh: optimised build ($profile)" >&2
RUSTFLAGS="${RUSTFLAGS:-} -Cprofile-use=$profdata" CARGO_TARGET_DIR="$work/use" \
    cargo build --quiet --locked --profile "$profile" --target "$host" -p neoscad-cli
echo "$work/use/$host/$profile/neoscad$exe"
