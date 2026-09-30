#!/usr/bin/env bash
# Runs the Linux app headless and checks it works end to end: it opens a
# model and previews it (the document loop, the core, the wgpu view on
# Mesa's software Vulkan), and with the editor bundle, typing in the
# editor previews the edited text again (the editor bridge). CI's
# linux-app job runs it; docs/linux-app.md shows how to run it in Docker
# from macOS.
#
#   linux/smoke.sh BIN [MODEL]
#
# Environment:
#   SHOTS=DIR   also save screenshots (light.png, typed.png, dark.png;
#               needs ImageMagick's `import`)
#   TYPE=1      type into the editor and require a second preview (needs
#               the editor bundle and xdotool)
#
# Needs Xvfb, dbus-run-session and a Vulkan or GL driver (mesa-vulkan-
# drivers). The app is killed if it takes more than 2 GB of memory or
# does not finish a run in time: a runaway must not fill the machine.
set -euo pipefail
cd "$(dirname "$0")/.."

bin=${1:?usage: linux/smoke.sh BIN [MODEL]}
model=${2:-web/examples/CSG.scad}
name=$(basename "$bin")
max_kb=$((2 * 1024 * 1024))
work=$(mktemp -d)
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$work"' EXIT

export G_MESSAGES_DEBUG=neoscad GTK_A11Y=none NO_AT_BRIDGE=1 GDK_BACKEND=x11
# Containers and CI runners have no user namespaces for WebKit's
# bubblewrap sandbox; this is a test of the app, not of the sandbox.
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
export NEOSCAD_EDITOR_DIR=${NEOSCAD_EDITOR_DIR:-$PWD/apple/Editor/web/dist}
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-$work/xdg}
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"

Xvfb :97 -screen 0 1400x900x24 -nolisten tcp >/dev/null 2>&1 &
export DISPLAY=:97
sleep 1

# Wait up to $3 seconds for "$1" in log $2, failing if the app exits or
# grows past the memory limit.
wait_for() {
    for _ in $(seq 1 "$3"); do
        grep -q "$1" "$2" && return 0
        # The launcher (dbus-run-session) ends when the app does.
        if ! kill -0 "$launcher" 2>/dev/null; then
            echo "the app exited:" >&2
            cat "$2" >&2
            return 1
        fi
        pid=$(pgrep -n -x "$name" || true)
        rss=0
        if [ -n "$pid" ]; then
            rss=$(awk '/VmRSS/ {print $2}' "/proc/$pid/status" 2>/dev/null || echo 0)
        fi
        if [ "${rss:-0}" -gt "$max_kb" ]; then
            echo "the app took ${rss} kB (limit ${max_kb})" >&2
            return 1
        fi
        sleep 1
    done
    echo "timed out waiting for '$1':" >&2
    cat "$2" >&2
    return 1
}

shot() {
    if [ -n "${SHOTS:-}" ]; then
        mkdir -p "$SHOTS"
        import -window root "$SHOTS/$1.png"
    fi
}

# One launch in colour scheme $1 (libadwaita's debug override).
launch() {
    local log=$work/$1.log
    ADW_DEBUG_COLOR_SCHEME=prefer-$1 dbus-run-session -- "$bin" "$PWD/$model" >"$log" 2>&1 &
    launcher=$!
    wait_for "run 1 (Preview)" "$log" 300
    sleep 3
    shot "$1"
    if [ "$1" = light ] && [ "${TYPE:-}" = 1 ]; then
        # Into the editor, to the end, a new line: a preview follows.
        xdotool mousemove 300 500 click 1
        sleep 0.5
        xdotool key ctrl+End
        xdotool type --delay 30 $'\ntranslate([0,0,30]) sphere(8);'
        wait_for "run 2 (Preview)" "$log" 120
        sleep 3
        shot typed
    fi
    grep "neoscad-DEBUG" "$log"
    pkill -x "$name" || true
    sleep 1
}

launch light
if [ -n "${SHOTS:-}" ]; then
    launch dark
fi
echo "ok"
