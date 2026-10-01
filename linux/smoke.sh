#!/usr/bin/env bash
# Runs the Linux app headless and checks it works end to end: it opens a
# model and previews it (the document loop, the core, the wgpu view on
# Mesa's software Vulkan). With the editor bundle, the language server
# answers the page's `initialize` with real capabilities and a model with
# a warning shows markers in the editor (the page counts them), and with
# TYPE=1 typing in the editor previews the edited text again (the editor
# bridge) and the side panels are driven from the keyboard: a customizer
# edit runs the model again with the text unchanged, a check finds a known
# problem and marks it in the view, a measurement reports the volume,
# Export Again writes an STL beside the model, and rewriting a used file
# on disk runs the model again (file watching). CI's
# linux-app job runs it; docs/linux-app.md shows how to run it in Docker
# from macOS.
#
#   linux/smoke.sh BIN [MODEL]
#
# Environment:
#   SHOTS=DIR   also save screenshots (light.png, typed.png, dark.png,
#               and with TYPE=1 customizer-, check- and measure-light.png
#               and -dark.png; needs ImageMagick's `import`)
#   TYPE=1      type into the editor and require a second preview, and
#               drive the panels (needs the editor bundle and xdotool)
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

# The language server: a model with a warning, whose run's diagnostics
# must reach the page as markers. The app logs the capabilities the page's
# `initialize` got and, after each publication, the page's own count of
# its markers (`NeoSCADEditor.state()`).
lsp_check() {
    local log=$work/lsp.log
    printf 'use <MCAD/regular_shapes.scad>\noctagon(5);\necho(no_such_variable);\n' \
        >"$work/warning.scad"
    dbus-run-session -- "$bin" "$work/warning.scad" >"$log" 2>&1 &
    launcher=$!
    wait_for "run 1 (Preview)" "$log" 300
    wait_for "lsp: server -> page reply .*capabilities .*completionProvider" "$log" 60
    wait_for "editor: [1-9][0-9]* markers, language server connected" "$log" 60
    if [ "${TYPE:-}" = 1 ]; then
        # Go to definition (F12) on `octagon`, at the start of line 2:
        # MCAD's file opens read-only in a library viewer.
        xdotool mousemove 300 500 click 1
        sleep 0.5
        xdotool key ctrl+Home Down F12
        wait_for "library viewer: .*MCAD/regular_shapes.scad" "$log" 60
    fi
    grep "lsp: server -> page\|editor: .* markers\|definition at\|library viewer" "$log"
    pkill -x "$name" || true
    sleep 1
}

# The side panels, from the keyboard, in colour scheme $1. The model has
# customizer parameters, a plate thinner than check's 0.4 mm nozzle (a
# known error) and a used file beside it, which is rewritten at the end.
panels_check() {
    local log=$work/panels-$1.log model=$work/plate.scad
    cat >"$model" <<'EOF'
use <extra.scad>
/* [Plate] */
// Add a rim around the plate
rim = false;
// The plate's thickness
thickness = 0.2; // [0.2:0.1:3]
size = 30;
cube([size, size, thickness]);
if (rim) translate([0, 0, thickness]) difference() {
    cube([size, size, 2]);
    translate([1, 1, -1]) cube([size - 2, size - 2, 4]);
}
extra();
EOF
    printf 'module extra() translate([40, 0, 0]) cube(4);\n' >"$work/extra.scad"
    local before
    before=$(md5sum <"$model")
    ADW_DEBUG_COLOR_SCHEME=prefer-$1 dbus-run-session -- "$bin" "$model" >"$log" 2>&1 &
    launcher=$!
    wait_for "run 1 (Preview)" "$log" 300
    wait_for "watch: 1 files in 1 directories" "$log" 30
    # Alt+1: the customizer, its first parameter (the `rim` switch)
    # focused; Space turns it on, and the model runs with rim = true
    # while the text stays as it was.
    xdotool key alt+1
    wait_for "panels: customizer shown (focus true)" "$log" 30
    xdotool key space
    wait_for "customizer: rim = true" "$log" 30
    wait_for "run 2 (Preview)" "$log" 120
    wait_for "document: 1 customizer values, text unchanged" "$log" 30
    if [ "$(md5sum <"$model")" != "$before" ]; then
        echo "the customizer changed the file" >&2
        return 1
    fi
    sleep 2
    shot "customizer-$1"
    # Alt+2: the check panel, its Check button focused; Enter checks, the
    # first finding takes the focus and Enter marks it in the view.
    xdotool key alt+2
    wait_for "panels: check shown (focus true)" "$log" 30
    xdotool key Return
    wait_for "check: [1-9][0-9]* findings ([1-9][0-9]* errors" "$log" 180
    sleep 1
    xdotool key Return
    wait_for "check: finding [0-9]* selected" "$log" 30
    wait_for "overlay: [1-9][0-9]* markers, [1-9][0-9]* lines" "$log" 30
    sleep 2
    shot "check-$1"
    # Alt+3: measure.
    xdotool key alt+3
    wait_for "panels: measure shown (focus true)" "$log" 30
    xdotool key Return
    wait_for "measure: Volume" "$log" 180
    sleep 2
    shot "measure-$1"
    if [ "$1" = light ]; then
        # Export Again (Ctrl+Shift+E): a 3D model's suggestion is binary
        # STL, named after the model, beside it; Enter saves.
        xdotool key ctrl+shift+e
        sleep 3
        xdotool key Return
        wait_for "export: Exported plate.stl" "$log" 120
        test -s "$work/plate.stl"
    fi
    # Another program saves the used file (a write to a temporary file
    # renamed over it, as editors save): the model runs again.
    local runs
    runs=$(grep -c "run [0-9]* (Preview)" "$log")
    printf 'module extra() translate([40, 0, 0]) sphere(4);\n' >"$work/extra.scad.tmp"
    mv "$work/extra.scad.tmp" "$work/extra.scad"
    wait_for "watch: .*extra.scad changed" "$log" 30
    wait_for "run $((runs + 1)) (Preview)" "$log" 120
    grep "customizer:\|document:\|panels:\|check:\|overlay:\|measure:\|export:\|watch:" "$log"
    pkill -x "$name" || true
    sleep 1
}

if [ -f "$NEOSCAD_EDITOR_DIR/editor.html" ]; then
    lsp_check
fi
if [ "${TYPE:-}" = 1 ]; then
    panels_check light
    if [ -n "${SHOTS:-}" ]; then
        panels_check dark
    fi
fi
launch light
if [ -n "${SHOTS:-}" ]; then
    launch dark
fi
echo "ok"
