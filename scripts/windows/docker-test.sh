#!/usr/bin/env bash
# Tests the Windows app's C# binding and host logic off Windows, in Docker
# (docs/windows-app.md, "Testing off Windows"):
#
#   1. in rust:<pinned toolchain>: `cargo rustc -p neoscad-ffi --crate-type
#      cdylib` (the core as a Linux .so) and uniffi-bindgen-cs, pinned as
#      in build-core.ps1, generating windows/NeoSCAD.Bindings/Generated;
#   2. in mcr.microsoft.com/dotnet/sdk: `dotnet test windows/NeoSCAD.Tests`
#      against that .so (windows/native/<rid>/).
#
# The WinUI project itself needs Windows and is built by CI only. The
# checkout is mounted at /src (read-write: the binding and the .so are
# written into windows/, both gitignored); cargo's registry and target
# live under $NEOSCAD_DOCKER_DIR (default target/docker-windows), so reruns
# are incremental and `rm -rf` of it frees everything. Containers are
# capped at 8 GB (no extra swap), so a runaway dies inside the Docker VM.
#
# --with-cli also builds the `neoscad` command line and runs the agent
# end-to-end test against it (AgentEndToEndTests: the real `neoscad mcp`
# driving the C# host over the link's Unix socket); without it that test
# passes vacuously.
#
#   scripts/windows/docker-test.sh [--platform linux/amd64] [--with-cli]
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
platform=()
with_cli=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --platform) platform=(--platform "$2"); shift 2 ;;
        --with-cli) with_cli=1; shift ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

toolchain=$(sed -n 's/^channel = "\(.*\)"/\1/p' "$repo/rust-toolchain.toml")
bindgen_repo=$(sed -n 's/^\$BindgenRepo = "\(.*\)"/\1/p' "$repo/scripts/windows/build-core.ps1")
bindgen_rev=$(sed -n 's/^\$BindgenRev = "\(.*\)"/\1/p' "$repo/scripts/windows/build-core.ps1")
state=${NEOSCAD_DOCKER_DIR:-$repo/target/docker-windows}
mkdir -p "$state/cargo" "$state/target" "$state/nuget"

limits=(--memory 8g --memory-swap 8g)
# NEOSCAD_UPDATE_TEST_PUBLIC_KEY, when set, is passed to both containers:
# the core is then built trusting that key (crates/client/src/update.rs)
# and UpdateTests checks the signed fixtures against the real core. Unset,
# as in CI, those tests check only that the core refuses them.
keyenv=(-e NEOSCAD_UPDATE_TEST_PUBLIC_KEY)
clienv=()
if [[ $with_cli == 1 ]]; then
    clienv=(-e NEOSCAD_TEST_CLI=/src/windows/native/cli/neoscad)
fi

docker run --rm ${platform[@]+"${platform[@]}"} "${limits[@]}" "${keyenv[@]}" \
    -v "$repo":/src -v "$state/cargo":/usr/local/cargo/registry \
    -v "$state/target":/target -e CARGO_TARGET_DIR=/target \
    -e RUSTUP_TOOLCHAIN="$toolchain" -w /src "rust:$toolchain-bookworm" bash -euo pipefail -c "
        cargo rustc --locked --release -p neoscad-ffi --crate-type cdylib
        if [ $with_cli = 1 ]; then
            cargo build --locked --release -p neoscad-cli --bin neoscad
            mkdir -p windows/native/cli
            cp /target/release/neoscad windows/native/cli/
        fi
        if [ ! -x /target/bindgen/bin/uniffi-bindgen-cs ]; then
            cargo install --locked --git '$bindgen_repo' --rev '$bindgen_rev' \
                uniffi-bindgen-cs --root /target/bindgen
        fi
        /target/bindgen/bin/uniffi-bindgen-cs --library --no-format \
            --config windows/uniffi.toml --out-dir windows/NeoSCAD.Bindings/Generated \
            /target/release/libneoscad_ffi.so
        rid=linux-\$(uname -m | sed 's/aarch64/arm64/; s/x86_64/x64/')
        mkdir -p windows/native/\$rid
        cp /target/release/libneoscad_ffi.so windows/native/\$rid/
    "

docker run --rm ${platform[@]+"${platform[@]}"} "${limits[@]}" "${keyenv[@]}" \
    -v "$repo":/src -v "$state/nuget":/root/.nuget/packages -w /src \
    -e DOTNET_CLI_TELEMETRY_OPTOUT=1 -e DOTNET_NOLOGO=1 ${clienv[@]+"${clienv[@]}"} \
    mcr.microsoft.com/dotnet/sdk:10.0 \
    dotnet test windows/NeoSCAD.Tests/NeoSCAD.Tests.csproj -c Release \
        --logger "console;verbosity=normal"
