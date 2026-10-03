#!/usr/bin/env bash
# Smoke test of release artifacts (docs/release.md), run by release.sh:
#
#   1. mounts the DMG read-only and launches the app from it, as a user
#      would before dragging it to Applications, with a sample .scad and
#      window restoration off (a fresh launch must not depend on saved
#      state);
#   2. checks that it stays up, shows a window, logs no fault and leaves no
#      crash report; measures its idle footprint and CPU; quits it;
#   3. runs the CLI: --version, a render to STL, and an MCP initialize.
#
#   scripts/apple/smoke-release.sh DMG CLI [VERSION]
#
# It never prompts: no Apple events (quitting through AppleScript would ask
# for automation access), so the app is quit with SIGTERM, which AppKit
# treats as an immediate, clean exit when no document has changes.
set -euo pipefail
[ $# -ge 2 ] || {
    echo "usage: $0 DMG CLI [VERSION]" >&2
    exit 2
}
dmg=$1
cli=$2
version=${3:-}

failures=0
pass() { echo "  ok    $*"; }
fail() {
    echo "  FAIL  $*"
    failures=$((failures + 1))
}

# The physical path: $TMPDIR is under /var, a symlink to /private/var,
# and the process's executable path (what pgrep matches) is the real one.
tmp=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/neoscad-smoke.XXXXXX")" && pwd -P)
mnt=$tmp/mnt
pid=
cleanup() {
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then kill -KILL "$pid" 2>/dev/null || true; fi
    if [ -d "$mnt" ]; then
        hdiutil detach -quiet "$mnt" 2>/dev/null || hdiutil detach -quiet -force "$mnt" 2>/dev/null || true
    fi
    rm -rf "$tmp"
}
trap cleanup EXIT

# Runs a command with a time limit (macOS has no `timeout`).
with_timeout() {
    local secs=$1
    shift
    perl -e 'alarm shift; exec @ARGV or die "exec: $!"' "$secs" "$@"
}

cat >"$tmp/sample.scad" <<'EOF'
// Smoke test: both render paths, text (the bundled fonts) and a library.
include <MCAD/units.scad>
difference() {
    cube(20, center = true);
    sphere(r = 12, $fn = 48);
}
translate([0, 0, 15]) linear_extrude(2) text("Neo", size = 5, halign = "center");
EOF

echo "App (from the DMG)"
mkdir -p "$mnt"
hdiutil attach -quiet -readonly -nobrowse -noautoopen -mountpoint "$mnt" "$dmg"
app=$mnt/NeoSCAD.app
if [ -d "$app" ] && [ "$(readlink "$mnt/Applications")" = /Applications ]; then
    pass "DMG holds NeoSCAD.app and an Applications link"
else
    fail "DMG layout: $(ls "$mnt")"
fi
codesign --verify --deep --strict "$app" 2>/dev/null && pass "signature intact on the mounted copy" ||
    fail "signature on the mounted copy"

exe=$app/Contents/MacOS/NeoSCAD
reports=$HOME/Library/Logs/DiagnosticReports
marker=$tmp/marker
touch "$marker"
start=$(date '+%Y-%m-%d %H:%M:%S')
# -n: a new instance even if a development build is running; -F: no
# restored windows. The document arrives as an open-documents event, as
# from Finder. LaunchServices applies the app's LSEnvironment this way.
open -n -F -a "$app" "$tmp/sample.scad" --args -ApplePersistenceIgnoreState YES
for _ in $(seq 1 40); do
    pid=$(pgrep -f "^$exe" | head -1 || true)
    [ -n "$pid" ] && break
    sleep 0.25
done
if [ -z "$pid" ]; then
    fail "the app did not start"
else
    pass "launched (pid $pid)"
    # Long enough to load the editor, evaluate, render and settle.
    sleep 10
    if kill -0 "$pid" 2>/dev/null; then
        pass "still running after 10 s"
    else
        fail "exited within 10 s"
        pid=
    fi
fi

if [ -n "$pid" ]; then
    # On-screen windows of the process, from the window server. Titles
    # need screen-recording access, so this counts windows at the normal
    # level only; a document window is the only one the app shows.
    cat >"$tmp/windows.swift" <<'EOF'
import CoreGraphics
let pid = Int(CommandLine.arguments[1])!
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
print(list.filter { ($0[kCGWindowOwnerPID as String] as? Int) == pid && ($0[kCGWindowLayer as String] as? Int) == 0 }.count)
EOF
    windows=$(with_timeout 60 xcrun swift "$tmp/windows.swift" "$pid" 2>/dev/null || echo "?")
    if [ "$windows" = "?" ]; then
        echo "  skip  window count (swift unavailable)"
    elif [ "$windows" -ge 1 ]; then
        pass "$windows document window(s) on screen"
    else
        fail "no window on screen"
    fi

    # Idle: after the render, the display link should only draw on change.
    cpu=$(ps -o %cpu= -p "$pid" | tr -d ' ')
    sleep 3
    cpu=$(ps -o %cpu= -p "$pid" | tr -d ' ')
    footprint_line=$(footprint -p "$pid" 2>/dev/null | grep -m1 -E '^[[:space:]]*(Footprint|phys_footprint)' ||
        footprint -p "$pid" 2>/dev/null | grep -m1 -i footprint || true)
    echo "  info  idle: $(sed 's/^[[:space:]]*//' <<<"$footprint_line"), CPU ${cpu}%"
    faults=$(log show --style compact --start "$start" \
        --predicate "processID == $pid AND (messageType == fault OR messageType == error) AND (subsystem BEGINSWITH \"org.neoscad\" OR eventMessage CONTAINS \"NeoSCAD:\")" \
        2>/dev/null | grep -c . || true)
    # The first line of `log show` output is a header.
    if [ "${faults:-0}" -le 1 ]; then pass "no NeoSCAD errors or faults logged"; else fail "$((faults - 1)) NeoSCAD error/fault log lines"; fi

    kill -TERM "$pid"
    for _ in $(seq 1 40); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.25
    done
    if kill -0 "$pid" 2>/dev/null; then
        fail "did not quit on SIGTERM within 10 s"
        kill -KILL "$pid" || true
    else
        pass "quit"
    fi
    pid=
fi
if [ -n "$(find "$reports" -name 'NeoSCAD*' -newer "$marker" -print -quit 2>/dev/null)" ]; then
    fail "crash report: $(find "$reports" -name 'NeoSCAD*' -newer "$marker" | head -1)"
else
    pass "no crash report"
fi
# The CLI the app carries (Contents/Helpers/neoscad), run from the mounted
# DMG as an agent client would run it through the app's link: the version,
# and an MCP session that lists the tools. The app launched above from a
# read-only volume, so it must not have made a link to this copy.
helper=$app/Contents/Helpers/neoscad
got=$("$helper" --version 2>/dev/null || true)
if [ -n "$got" ] && { [ -z "$version" ] || [ "$got" = "neoscad $version" ]; }; then
    pass "bundled CLI --version: $got"
else
    fail "bundled CLI --version: ${got:-did not run}"
fi
session='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"neoscad-smoke","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
replies=$(printf '%s\n' "$session" | with_timeout 30 "$helper" mcp 2>/dev/null || true)
if grep -q '"serverInfo"' <<<"$replies" && grep -q '"id":2' <<<"$replies" &&
    grep -q '"name":"render"' <<<"$replies"; then
    pass "bundled CLI mcp: initialize and tools/list ($(grep -o '"inputSchema"' <<<"$replies" | wc -l | tr -d ' ') tools)"
else
    fail "bundled CLI mcp: ${replies:-no reply}"
fi
link=$HOME/Library/Application\ Support/NeoSCAD/bin/neoscad
if [ -L "$link" ] && [ "$(readlink "$link")" = "$helper" ]; then
    fail "the app linked $link to the DMG's copy"
else
    pass "no command-line tool link to the DMG's copy"
fi

# The launch registered the DMG's copy with LaunchServices; forget it, so
# the unmounted path does not linger as a handler for .scad files.
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
    -u "$app" 2>/dev/null || true
if hdiutil detach -quiet "$mnt" 2>/dev/null || hdiutil detach -quiet -force "$mnt"; then
    rmdir "$mnt" 2>/dev/null || true
else
    fail "could not detach $mnt"
fi

echo "CLI ($cli)"
got=$("$cli" --version)
if [ -z "$version" ] || [ "$got" = "neoscad $version" ]; then pass "--version: $got"; else fail "--version: $got (expected neoscad $version)"; fi
if with_timeout 120 "$cli" -o "$tmp/sample.stl" "$tmp/sample.scad" 2>"$tmp/render.err" &&
    [ "$(head -c 5 "$tmp/sample.stl")" = solid ]; then
    pass "render to STL: $(grep -c 'facet normal' "$tmp/sample.stl") facets"
else
    fail "render to STL: $(tail -3 "$tmp/render.err")"
fi
request='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"neoscad-smoke","version":"0"}}}'
reply=$(printf '%s\n' "$request" | with_timeout 30 "$cli" mcp 2>/dev/null | head -1 || true)
if grep -q '"id":1' <<<"$reply" && grep -q '"serverInfo"' <<<"$reply"; then
    pass "mcp initialize: $(grep -o '"serverInfo":{[^}]*}' <<<"$reply")"
else
    fail "mcp initialize: ${reply:-no reply}"
fi

if [ $failures -gt 0 ]; then
    echo "smoke test: $failures failure(s)"
    exit 1
fi
echo "smoke test: passed"
