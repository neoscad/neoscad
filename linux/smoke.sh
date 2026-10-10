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
# Export Again writes an STL beside the model, rewriting a used file
# on disk runs the model again (file watching), and rewriting the model
# itself is taken into the editor and run, and with Preferences >
# Language's exact and fillet on File > Export > STEP writes a filleted
# part all exact, lists a hull's faceted region and refuses a fin. CI's
# linux-app job runs it; docs/linux-app.md shows how to run it in Docker
# from macOS.
#
#   linux/smoke.sh BIN [MODEL]
#
# Environment:
#   SHOTS=DIR   also save screenshots (light.png, typed.png, dark.png,
#               and with TYPE=1 customizer-, check- and measure-light.png
#               and -dark.png, and step-filleted, -faceted and
#               -refused.png; needs ImageMagick's `import`)
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
    wait_for "check: [1-9][0-9]* findings ([1-9][0-9]* error" "$log" 180
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
    # Another program (an agent through `neoscad mcp`) rewrites the model
    # itself while it has no unsaved changes: the window takes the change
    # in as one edit in the editor (`agentEdit`), and runs it.
    runs=$(grep -c "run [0-9]* (Preview)" "$log")
    sed 's/^size = 30;/size = 32;/' "$model" >"$model.tmp"
    mv "$model.tmp" "$model"
    wait_for "disk: reload (1 edits)" "$log" 30
    wait_for "run $((runs + 1)) (Preview)" "$log" 120
    grep "customizer:\|document:\|panels:\|check:\|overlay:\|measure:\|export:\|watch:\|disk:" "$log"
    pkill -x "$name" || true
    sleep 1
}

# STEP export, end to end: with Preferences > Language's `exact` and
# `fillet` on (the settings file its switches write), File > Export >
# STEP (exact surfaces) saves beside the model, through the save dialog,
# and the window says what it wrote. The menu item's action (`win.export`
# with "step") is sent over the session bus, where GTK publishes a
# window's actions (org.gtk.Actions): a popover menu has no stable keys
# or coordinates to click. A filleted part is all exact with one exact
# cylinder per blend; rewritten on disk to a part with a hull, its alert
# lists the hull as written in facets at its line; rewritten again to a
# fin of no thickness, the export is refused with the reason and leaves
# no file. Each report is an alert, which Escape closes.
step_check() {
    local log=$work/step.log dir=$work/step config=$work/step-config bus=$work/step-bus
    mkdir -p "$dir" "$config/neoscad"
    local part=$dir/part.scad out=$dir/part.step
    printf '{"exact": true, "fillet": true}\n' >"$config/neoscad/language.json"
    printf 'fillet_edges(r = 1, edges = "|y") cube([20, 10, 4]);\n' >"$part"
    # The launcher's bus, for gdbus.
    XDG_CONFIG_HOME=$config dbus-run-session -- \
        sh -c 'printf %s "$DBUS_SESSION_BUS_ADDRESS" >"$0"; exec "$@"' "$bus" "$bin" "$part" \
        >"$log" 2>&1 &
    launcher=$!
    wait_for "run 1 (Preview)" "$log" 300
    # File > Export > STEP: the app is the bus's only client (NON_UNIQUE,
    # so it owns no well-known name), at its application id's path.
    export_step() {
        local addr name
        addr=$(cat "$bus")
        for name in $(DBUS_SESSION_BUS_ADDRESS=$addr gdbus call --session \
            --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
            --method org.freedesktop.DBus.ListNames | grep -o "':[0-9.]*'" | tr -d "'"); do
            if DBUS_SESSION_BUS_ADDRESS=$addr gdbus call --session --dest "$name" \
                --object-path /org/neoscad/NeoSCAD/window/1 \
                --method org.gtk.Actions.Activate export "[<'step'>]" '{}' >/dev/null 2>&1; then
                sleep 3
                xdotool key Return
                return 0
            fi
        done
        echo "no window answered org.gtk.Actions on the bus" >&2
        return 1
    }
    # Rewrite the model on disk (the window has no unsaved changes, so
    # it takes the text in and runs it) and drop the last export, so the
    # save dialog does not ask to replace it.
    rewrite() {
        local runs
        runs=$(grep -c "run [0-9]* (Preview)" "$log")
        printf '%s' "$1" >"$part.tmp"
        mv "$part.tmp" "$part"
        rm -f "$out"
        wait_for "run $((runs + 1)) (Preview)" "$log" 120
    }
    export_step
    wait_for "export: Exported part.step (.*STEP: 10 of 10 faces exact (100%)" "$log" 120
    head -c 13 "$out" | grep -qx "ISO-10303-21;"
    [ "$(grep -o CYLINDRICAL_SURFACE "$out" | wc -l)" -eq 4 ]
    sleep 1
    shot step-filleted
    xdotool key Escape
    rewrite $'difference() {\n  cube(20);\n  translate([10, 10, -1]) cylinder(r = 4, h = 22);\n  hull() { cube(1); translate([2, 2, 2]) cube(1); }\n}\n'
    export_step
    wait_for "export: Exported part.step (.*STEP: 7 of 16 faces exact (43.8%)" "$log" 120
    wait_for "^Faceted: hull() at part.scad, line 4 " "$log" 10
    test -s "$out"
    sleep 1
    shot step-faceted
    xdotool key Escape
    rewrite $'cube(10); translate([10, 0, 0]) cube([10, 10, 0.000001]);\n'
    export_step
    wait_for "export: part.step failed: STEP export refused: .*No file was written." "$log" 120
    test ! -e "$out"
    sleep 1
    shot step-refused
    xdotool key Escape
    grep "export:\|^Faceted:" "$log"
    pkill -x "$name" || true
    sleep 1
}

# AI agents, end to end: with agents allowed (the consent kept in the
# settings file, as a user's earlier "Allow" leaves it), the real
# `neoscad mcp` finds the running app by itself (NEOSCAD_AGENT_DIR keeps the
# test's socket apart from the user's), reads the open document, edits it
# as one change in the editor (which runs it, and leaves the file alone),
# is refused an edit on the version it read before, moves the camera,
# marks the view (the marks chip shows) and captures it; with TYPE=1 the
# chip's clear button takes the marks away. Needs the command line beside
# the app (or NEOSCAD_CLI); with SHOTS it saves agent-light.png and
# agent-dark.png, the popover, the Agents dialog with each client picked
# (the pick kept in agents.json, and the page opening on it again), and
# its usage section.
agent_check() {
    local cli=${NEOSCAD_CLI:-$(dirname "$bin")/neoscad}
    if [ ! -x "$cli" ]; then
        echo "agent_check: no $cli; skipped"
        return 0
    fi
    # Absolute: the server runs from the model's folder.
    cli=$(cd "$(dirname "$cli")" && pwd)/$(basename "$cli")
    local log=$work/agent-$1.log model=$work/agent/gear.scad
    local config=$work/agent-config-$1 dir=$work/agent-sockets-$1 err=$work/mcp-$1.err
    mkdir -p "$work/agent" "$config/neoscad" "$dir"
    chmod 700 "$dir"
    printf 'cube(10);\n' >"$model"
    printf '{"allowed": true}\n' >"$config/neoscad/agents.json"
    XDG_CONFIG_HOME=$config NEOSCAD_AGENT_DIR=$dir ADW_DEBUG_COLOR_SCHEME=prefer-$1 \
        dbus-run-session -- "$bin" "$model" >"$log" 2>&1 &
    launcher=$!
    wait_for "run 1 (Preview)" "$log" 300
    wait_for "agent: listening at" "$log" 30

    coproc MCP { cd "$work/agent" && NEOSCAD_AGENT_DIR=$dir exec "$cli" mcp 2>"$err"; }
    local to=${MCP[1]} from=${MCP[0]}
    # One request; its reply, skipping notifications (the tool list
    # changing when the app connects).
    call() {
        printf '{"jsonrpc":"2.0","id":%s,"method":"%s","params":%s}\n' "$1" "$2" "$3" >&"$to"
        local line
        while IFS= read -r -t 60 line <&"$from"; do
            case $line in *"\"id\":$1,"*) printf '%s\n' "$line"; return 0 ;; esac
        done
        echo "no reply to $2" >&2
        cat "$err" >&2
        return 1
    }
    tool() { call "$1" tools/call "{\"name\":\"$2\",\"arguments\":$3}"; }
    call 1 initialize '{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"Smoke Test","version":"1"}}' >/dev/null
    printf '{"jsonrpc":"2.0","method":"notifications/initialized"}\n' >&"$to"

    local r version
    r=$(tool 2 editor_read '{}')
    echo "editor_read: ${r:0:200}"
    version=$(grep -o 'version [0-9]*' <<<"$r" | head -1 | cut -d' ' -f2)
    [ -n "$version" ] && grep -q 'cube(10);' <<<"$r"
    wait_for "agent: Smoke Test connected" "$log" 30
    r=$(tool 3 editor_edit "{\"version\":$version,\"edits\":[{\"old\":\"cube(10);\",\"new\":\"cube(12);\"}]}")
    echo "editor_edit: ${r:0:200}"
    if grep -q '"isError":true' <<<"$r"; then
        echo "the agent's edit failed" >&2
        return 1
    fi
    wait_for "agent: edit of 1 changes: Applied" "$log" 30
    wait_for "run 2 (Preview)" "$log" 120
    r=$(tool 4 editor_read '{}')
    grep -q 'cube(12);' <<<"$r"
    # The edit is in the buffer, not saved: the file is as it was.
    grep -qx 'cube(10);' "$model"
    # An edit on the version read before is refused (by the command line,
    # which reads first, or the app), and changes nothing.
    r=$(tool 5 editor_edit "{\"version\":$version,\"edits\":[{\"old\":\"cube(12);\",\"new\":\"cube(1);\"}]}")
    grep -q '"isError":true' <<<"$r"
    grep -q 'version' <<<"$r"
    r=$(tool 6 view_camera '{"view":"diagonal","fit":true}')
    grep -q 'vpr' <<<"$r"
    r=$(tool 7 view_annotate '{"markers":[{"point":[12,12,12],"label":"corner"}]}')
    wait_for "overlay: 1 markers" "$log" 30
    wait_for "agent: marks chip: 1 agent mark" "$log" 30
    r=$(tool 8 view_capture '{"size":256}')
    grep -q '"type":"image"' <<<"$r"
    sleep 1
    shot "agent-$1"
    if [ "${TYPE:-}" = 1 ]; then
        # The chip's clear button (the chip is at the view's top left;
        # the window at the screen's) takes the agent's marks away, and
        # the chip with them.
        xdotool mousemove 711 78 click 1
        wait_for "agent: marks cleared" "$log" 30
        wait_for "agent: marks chip: hidden" "$log" 30
        grep "overlay:" "$log" | tail -1 | grep -q "overlay: 0 markers"
    fi
    if [ -n "${SHOTS:-}" ]; then
        # The header button (left of the main menu; the window is at the
        # screen's top left) opens the popover; its Set Up Agents… opens
        # the Agents dialog.
        xdotool mousemove 1100 27 click 1
        sleep 1.5
        shot "agent-popover-$1"
        xdotool mousemove 1043 190 click 1
        wait_for "agent: setup page opens on claude-code" "$log" 30
        sleep 1.5
        shot "agent-dialog-$1"
        # The client selector (its buttons' centres in the dialog, which
        # GTK centres over the 1280-wide window): each pick shows only
        # that client's setup, and the last one is kept in agents.json.
        local pick
        for pick in cursor:567 vs-code:710 other:853; do
            xdotool mousemove "${pick#*:}" 507 click 1
            wait_for "agent: setup for ${pick%:*}" "$log" 30
            sleep 1
            shot "agent-dialog-${pick%:*}-$1"
        done
        grep -q '"setupClient": "other"' "$config/neoscad/agents.json"
        # Opened again, the page starts on the last pick.
        xdotool key Escape
        sleep 0.5
        xdotool mousemove 1100 27 click 1
        sleep 1.5
        xdotool mousemove 1043 190 click 1
        wait_for "agent: setup page opens on other" "$log" 30
        sleep 1.5
        # Back to Claude Code, and down to "Using NeoSCAD with your agent".
        xdotool mousemove 425 507 click 1
        wait_for "agent: setup for claude-code" "$log" 30
        xdotool mousemove 640 600 click --repeat 20 --delay 50 5
        sleep 1.5
        shot "agent-usage-$1"
        xdotool key Escape
        sleep 0.5
        # The popover's Disconnect ends the agent's connection.
        xdotool mousemove 1100 27 click 1
        sleep 1.5
        xdotool mousemove 1112 134 click 1
        wait_for "agent: disconnect" "$log" 30
        sleep 1
        grep "agent: [0-9]* connected, listening" "$log" | tail -1 | grep -q "agent: 0 connected"
    fi
    exec {to}>&-
    wait "$MCP_PID" 2>/dev/null || true
    grep "agent:" "$log"
    pkill -x "$name" || true
    sleep 1
}

if [ -f "$NEOSCAD_EDITOR_DIR/editor.html" ]; then
    lsp_check
    agent_check light
    if [ -n "${SHOTS:-}" ]; then
        agent_check dark
    fi
fi
if [ "${TYPE:-}" = 1 ]; then
    step_check
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
