#!/usr/bin/env bash
# Builds the editor bundle for the macOS app (docs/audits/macos-prep.md, 8d):
#
#   1. `npm ci` in apple/Editor/web, when package-lock.json changed since
#      node_modules was installed (the only step that needs the network)
#   2. `node build.mjs`: the Lezer grammar -> src/lang/parser.js, then
#      esbuild -> apple/Editor/web/dist/editor.js, editor.html and
#      THIRD-PARTY-LICENSES.txt, which the app copies into
#      Resources/Editor and serves offline
#
# Xcode runs this as the EditorBundle target's build phase, with
# apple/build/editor-inputs.xcfilelist as its input file list (every file
# the bundle is built from). Xcode skips the phase when none is newer than
# the outputs, which is why the script touches its outputs even when
# build.mjs left them unchanged.
#
# `xcodegen generate` runs it first with `--prepare` (apple/project.yml's
# `preGenCommand`), because Xcode reads the input list when it plans a
# build, before any phase runs. `--prepare` writes the list and builds
# only when the bundle is missing.
#
# Node: $NODE if set, else `node` on PATH, else the newest nvm install
# (preferring nvm's default alias), else Homebrew's. Xcode's build
# environment has a minimal PATH, so the fallbacks matter there. Node 18 or
# newer; the app itself needs no node.
#
#   scripts/apple/build-editor.sh            build
#   scripts/apple/build-editor.sh --prepare  as xcodegen runs it
set -euo pipefail
cd "$(dirname "$0")/../.."
root=$PWD

web=$root/apple/Editor/web
build=$root/apple/build
inputs=$build/editor-inputs.xcfilelist
stamp=$build/editor.stamp
outputs=("$web/dist/editor.html" "$web/dist/editor.js" "$web/dist/THIRD-PARTY-LICENSES.txt")

# Every file the bundle is built from; not the parser the grammar
# generates (an output), nor node_modules (package-lock.json stands for it).
write_inputs() {
    mkdir -p "$build"
    local tmp=$inputs.tmp
    {
        echo "$root/scripts/apple/build-editor.sh"
        echo "$web/package.json"
        echo "$web/package-lock.json"
        echo "$web/build.mjs"
        find "$web/src" -type f -not -name 'parser.js' -not -name 'parser.terms.js' \
            -not -name '.DS_Store'
    } | LC_ALL=C sort -u >"$tmp"
    if ! cmp -s "$tmp" "$inputs" 2>/dev/null; then
        mv "$tmp" "$inputs"
    else
        rm "$tmp"
    fi
}

find_node() {
    if [ -n "${NODE:-}" ]; then
        echo "$NODE"
        return
    fi
    if command -v node >/dev/null 2>&1; then
        command -v node
        return
    fi
    local nvm=${NVM_DIR:-$HOME/.nvm}
    if [ -d "$nvm/versions/node" ]; then
        local want="" candidate
        if [ -f "$nvm/alias/default" ]; then want=$(tr -d '[:space:]' <"$nvm/alias/default"); fi
        want=${want#v}
        # Newest first, so the first match is the newest of the default's
        # line.
        for candidate in $(ls "$nvm/versions/node" | sort -t. -k1,1Vr -k2,2nr -k3,3nr); do
            if [ -z "$want" ] || [ "${candidate#v}" = "$want" ] ||
                [[ "${candidate#v}" == "$want".* ]]; then
                echo "$nvm/versions/node/$candidate/bin/node"
                return
            fi
        done
        candidate=$(ls "$nvm/versions/node" | sort -t. -k1,1Vr -k2,2nr -k3,3nr | head -1)
        if [ -n "$candidate" ]; then
            echo "$nvm/versions/node/$candidate/bin/node"
            return
        fi
    fi
    for candidate in /opt/homebrew/bin/node /usr/local/bin/node; do
        if [ -x "$candidate" ]; then
            echo "$candidate"
            return
        fi
    done
    return 1
}

write_inputs
if [ "${1:-}" = "--prepare" ]; then
    missing=0
    for f in "${outputs[@]}"; do [ -f "$f" ] || missing=1; done
    # The build phase keeps the bundle current from here on.
    if [ $missing = 0 ]; then exit 0; fi
fi

if ! node=$(find_node) || [ ! -x "$node" ]; then
    echo "error: build-editor: node (18 or newer) not found; set NODE=/path/to/node" >&2
    exit 1
fi
# node and npm from a clean environment, as build-core.sh runs cargo:
# Xcode's build settings arrive as environment variables, and npm reads
# npm_config_* ones.
node_env=(env -i
    HOME="$HOME"
    PATH="$(dirname "$node"):/usr/bin:/bin:/usr/sbin:/sbin"
    TERM="${TERM:-dumb}")

cd "$web"
if [ ! -f node_modules/.package-lock.json ] ||
    [ package-lock.json -nt node_modules/.package-lock.json ]; then
    echo "build-editor: npm ci"
    "${node_env[@]}" npm ci --no-audit --no-fund --loglevel=error
fi
"${node_env[@]}" node build.mjs
# Tell Xcode the outputs are current (build.mjs rewrites only what changed).
touch "${outputs[@]}"
touch "$stamp"
