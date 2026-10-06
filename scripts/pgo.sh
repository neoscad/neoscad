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
#   scripts/pgo.sh [--profile release|dist] [--profile-only] [--ffi] [--target TRIPLE]
#
# --ffi trains the macOS app's core (neoscad-ffi) instead: the
# instrumented build is the crate's `pgo_train` example, which links the
# same library build (crates/ffi/examples/pgo_train.rs says why a CLI
# profile cannot serve), the training is `pgo-train.py --ffi`, and the
# optimised build is `-p neoscad-ffi --lib` (its static library's path is
# printed). --target trains another target than the host's, whose
# instrumented binary must still run here: x86_64-apple-darwin on an
# arm64 Mac, under Rosetta (scripts/apple/release.sh --pgo). The host's
# CLI keeps target/pgo/neoscad.profdata; the others get
# target/pgo/<package>-<target>/<package>.profdata.
#
# Prints the path of the optimised binary; with --profile-only, stops
# after step 3 and prints the path of the merged .profdata instead, for a
# caller that makes the optimised build itself (the release build,
# .github/build-setup.yml, builds it exactly as `dist build` will so that
# the binary it checks is the one dist ships). Needs `rustup component add
# llvm-tools-preview` (for the toolchain in rust-toolchain.toml), Python 3
# ($PYTHON, else python3, else python), and .reference with BOSL2's
# tests_x/examples_x (`conformance bosl2-corpus`). The instrumented
# binary has to run here, so a target this machine cannot run cannot be
# trained this way. Meant for macOS, Linux and Windows under Git Bash (as
# GitHub's Windows runners have it; .github/workflows/pgo.yml).
#
# Output is byte-identical to the normal build's on everything checked
# (perf-opportunities.md, P2). Recursion depth is too: it is a counted
# limit, not a measure of the stack, so PGO's larger frames no longer
# make a build recurse less deep, as they did while the evaluator
# recursed natively. Before shipping a PGO build, check that it reports
# the plain build's depths: `conformance depth --binary PATH`.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -f "$HOME/.cargo/env" ] && ! command -v cargo >/dev/null 2>&1; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi

profile=release
profile_only=false
ffi=false
target=
while [ $# -gt 0 ]; do
    case "$1" in
        --profile) profile=$2; shift 2 ;;
        --profile-only) profile_only=true; shift ;;
        --ffi) ffi=true; shift ;;
        --target) target=$2; shift 2 ;;
        -h|--help) sed -n '2,47p' "$0"; exit 0 ;;
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
target=${target:-$host}
case "$target" in *-windows-*) exe=.exe ;; *) exe= ;; esac
if $ffi; then
    package=neoscad-ffi
    gen_args=(--example pgo_train)
    use_args=(--lib)
    trained=examples/pgo_train$exe
    built=libneoscad_ffi.a
    train_args=(--ffi)
else
    package=neoscad-cli
    gen_args=()
    use_args=()
    trained=neoscad$exe
    built=neoscad$exe
    train_args=()
fi
if ! $ffi && [ "$target" = "$host" ]; then
    work=$root/pgo
    profdata=$work/neoscad.profdata
else
    work=$root/pgo/$package-$target
    profdata=$work/$package.profdata
fi
raw=$work/raw
rm -rf "$raw" "$work/train"
mkdir -p "$raw"

# `--target` keeps RUSTFLAGS off build scripts and proc macros: without
# it they are instrumented too, write their own profiles into $raw during
# the build and warn about value-profile counters.
# NEOSCAD_FEATURES builds both with those features of the package, so the
# profile is trained on the code it optimises.
echo "pgo.sh: instrumented build ($package, $target, $profile)" >&2
RUSTFLAGS="${RUSTFLAGS:-} -Cprofile-generate=$raw" CARGO_TARGET_DIR="$work/gen" \
    cargo build --quiet --locked --profile "$profile" --target "$target" -p "$package" \
    ${gen_args[@]+"${gen_args[@]}"} ${NEOSCAD_FEATURES:+--features "$NEOSCAD_FEATURES"}

echo "pgo.sh: training" >&2
"$python" scripts/pgo-train.py ${train_args[@]+"${train_args[@]}"} \
    "$work/gen/$target/$profile/$trained" "$work/train" >&2
"$profdata_tool" merge -o "$profdata" "$raw"
if $profile_only; then
    echo "$profdata"
    exit 0
fi

echo "pgo.sh: optimised build ($package, $target, $profile)" >&2
RUSTFLAGS="${RUSTFLAGS:-} -Cprofile-use=$profdata" CARGO_TARGET_DIR="$work/use" \
    cargo build --quiet --locked --profile "$profile" --target "$target" -p "$package" \
    ${use_args[@]+"${use_args[@]}"} ${NEOSCAD_FEATURES:+--features "$NEOSCAD_FEATURES"}
echo "$work/use/$target/$profile/$built"
