#!/usr/bin/env bash
# End-to-end test of the macOS app's updates (Sparkle; docs/release.md,
# "The macOS app's updates"), on this machine:
#
#   scripts/apple/test-updates.sh [WORK_DIR]
#
# 1. makes a throwaway EdDSA key (scripts/release/sparkle-key.swift) in
#    WORK_DIR, never in the repository;
# 2. builds the app twice with scripts/apple/release.sh, ad hoc, trusting
#    that key and reading an appcast on 127.0.0.1 (the NEOSCAD_TEST_*
#    variables): build 1 is installed, build 2's DMG is the update;
# 3. writes and signs the appcast with scripts/release/appcast.py, as the
#    release workflow does, from a fake release whose DMG is served
#    locally, and serves it;
# 4. runs build 1 four times, each checked by what the local server saw,
#    what Sparkle logged and which build is installed afterwards:
#    1. against a tampered appcast (a word changed after signing): Sparkle
#       must refuse it, so the DMG is never requested;
#    2. against the real one, as a user would: Sparkle must offer the
#       update (a second window beside the document's) and download
#       nothing;
#    3. against a signed appcast whose DMG signature is for other bytes,
#       with automatic installation on: the DMG must be refused;
#    4. against the real one with automatic installation on
#       (-SUAutomaticallyUpdate YES, the choice Sparkle's own dialog
#       offers): Sparkle must download and verify the DMG, and install
#       build 2 in place when the app quits.
#
# release.sh replaces dist/ and apple/build/release/ on each build. Builds
# and the key are kept in WORK_DIR when one is given, so a rerun skips the
# builds (then the port, TEST_UPDATES_PORT or 8765, must stay the same:
# it is built into the apps). Without WORK_DIR everything is in a
# temporary directory removed at the end.
#
# The apps share the preferences domain org.neoscad.NeoSCAD with any
# NeoSCAD installed on this Mac, where Sparkle records its check times. So
# the script refuses to run while a NeoSCAD is running, saves the domain
# first and puts it back afterwards. Sparkle's download cache
# (~/Library/Caches/org.neoscad.NeoSCAD/org.sparkle-project.Sparkle) is
# removed afterwards if it wasn't there before.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

if [ $# -ge 1 ]; then
    mkdir -p "$1"
    work=$(cd "$1" && pwd -P)
    keep=1
else
    work=$(mktemp -d)
    keep=0
fi
port=${TEST_UPDATES_PORT:-8765}
feed_url=http://127.0.0.1:$port/appcast.xml
domain=org.neoscad.NeoSCAD
version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
short=${version%%-*}

say() { printf '\n==> %s\n' "$*"; }
fail() {
    echo "test-updates: FAIL: $*" >&2
    exit 1
}

if pgrep -x NeoSCAD >/dev/null; then
    fail "a NeoSCAD is running; quit it first (the test shares its preferences)"
fi

server=
prefs_saved=
sparkle_cache=$HOME/Library/Caches/$domain/org.sparkle-project.Sparkle
had_cache=no
if [ -e "$sparkle_cache" ]; then had_cache=yes; fi
cleanup() {
    pkill -f "$work/install/NeoSCAD.app/Contents/MacOS/NeoSCAD" 2>/dev/null || true
    if [ -n "$server" ]; then
        kill "$server" 2>/dev/null || true
        wait "$server" 2>/dev/null || true
    fi
    # `defaults import` adds to a domain rather than replacing it, so the
    # domain goes first, or Sparkle's keys from the test would stay.
    if [ "$prefs_saved" = yes ]; then
        defaults delete "$domain" >/dev/null 2>&1 || true
        defaults import "$domain" "$work/prefs-before.plist"
    elif [ "$prefs_saved" = none ]; then
        defaults delete "$domain" >/dev/null 2>&1 || true
    fi
    if [ $had_cache = no ]; then rm -rf "$sparkle_cache"; fi
    if [ -d "$work/install/NeoSCAD.app" ]; then
        /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
            -u "$work/install/NeoSCAD.app" 2>/dev/null || true
    fi
    if [ $keep = 0 ]; then rm -rf "$work"; else rm -f "$work/test-sparkle.key.tmp"; fi
}
trap cleanup EXIT

say "Sparkle's tools and a throwaway key"
scripts/release/sparkle-tools.sh "$work/sparkle"
sign_update=$work/sparkle/bin/sign_update
if [ ! -f "$work/test-sparkle.key" ]; then
    xcrun swift scripts/release/sparkle-key.swift generate "$work/test-sparkle.key" >/dev/null
fi
pub=$(xcrun swift scripts/release/sparkle-key.swift public <"$work/test-sparkle.key")
echo "test public key $pub"

# Each build records the key and feed it was made for; a kept build made
# for another key or port is made again.
build() {
    local n=$1 stamp="$pub $feed_url"
    if [ -d "$work/build-$n/NeoSCAD.app" ] && [ "$(cat "$work/build-$n/stamp" 2>/dev/null)" = "$stamp" ]; then
        echo "build $n: kept from an earlier run"
        return
    fi
    say "Build $n (release.sh, ad hoc)"
    rm -rf "$work/build-$n"
    NEOSCAD_TEST_SPARKLE_PUBLIC_KEY=$pub NEOSCAD_TEST_SPARKLE_FEED_URL=$feed_url \
        NEOSCAD_TEST_BUILD_NUMBER=$n scripts/apple/release.sh --no-smoke
    mkdir -p "$work/build-$n"
    ditto apple/build/release/app/NeoSCAD.app "$work/build-$n/NeoSCAD.app"
    cp "dist/NeoSCAD-$version-$n.dmg" "$work/build-$n/"
    echo "$stamp" >"$work/build-$n/stamp"
}
build 1
build 2

say "Appcast"
dmg_name=NeoSCAD-$version-2.dmg
site=$work/site
rm -rf "$site"
mkdir -p "$site"
cp "$work/build-2/$dmg_name" "$site/"
# Served before the appcast exists: appcast.py downloads the DMG from its
# release URL, which here is this server, as the job downloads from GitHub.
python3 -m http.server --bind 127.0.0.1 --directory "$site" "$port" >>"$work/server.log" 2>&1 &
server=$!
for _ in $(seq 50); do curl -fs "http://127.0.0.1:$port/$dmg_name" -o /dev/null && break; sleep 0.1; done
curl -fs "http://127.0.0.1:$port/$dmg_name" -o /dev/null || fail "the server on port $port did not start"
python3 - "$work/releases.json" "$site/$dmg_name" "$port" "$version" <<'EOF'
import hashlib, json, os, sys
out, dmg, port, version = sys.argv[1:]
name = os.path.basename(dmg)
json.dump([{
    "tag_name": f"v{version}", "draft": False, "prerelease": "-" in version,
    "published_at": "2026-10-01T00:00:00Z",
    "html_url": f"https://github.com/neoscad/neoscad/releases/tag/v{version}",
    "assets": [{
        "name": name, "size": os.path.getsize(dmg),
        "digest": "sha256:" + hashlib.sha256(open(dmg, "rb").read()).hexdigest(),
        "browser_download_url": f"http://127.0.0.1:{port}/{name}",
    }],
}], open(out, "w"))
EOF
SPARKLE_ED_PRIVATE_KEY=$(cat "$work/test-sparkle.key") python3 scripts/release/appcast.py \
    --releases "$work/releases.json" --appcast "$site/appcast.xml" \
    --sign-update "$sign_update" --work "$work/dmgs" >/dev/null
cat "$site/appcast.xml"
grep -q "<sparkle:version>2</sparkle:version>" "$site/appcast.xml" || fail "the appcast does not offer build 2"
# The signature is checked here with the public key alone (CryptoKit), not
# with sign_update, which derives it from the private key it is given.
python3 - "$site/appcast.xml" "$work/appcast.body" "$work/appcast.sig" <<'EOF'
import sys
data = open(sys.argv[1], "rb").read()
cut = data.rfind(b"<!-- sparkle-signatures:\n")
open(sys.argv[2], "wb").write(data[:cut])
block = data[cut:].decode().splitlines()
open(sys.argv[3], "w").write(next(l.split(": ", 1)[1] for l in block if l.startswith("edSignature: ")))
EOF
cat >"$work/verify.swift" <<'EOF'
import CryptoKit
import Foundation
let a = CommandLine.arguments
let key = try! Curve25519.Signing.PublicKey(rawRepresentation: Data(base64Encoded: a[1])!)
let sig = Data(base64Encoded: try! String(contentsOfFile: a[2], encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines))!
exit(key.isValidSignature(sig, for: FileManager.default.contents(atPath: a[3])!) ? 0 : 1)
EOF
xcrun swift "$work/verify.swift" "$pub" "$work/appcast.sig" "$work/appcast.body" ||
    fail "the appcast's signature does not verify with the test public key"
echo "appcast signature verifies with the public key"

if defaults export "$domain" "$work/prefs-before.plist" 2>/dev/null; then
    prefs_saved=yes
else
    prefs_saved=none
fi
# Sparkle's own record of earlier checks would postpone the first one by a
# day; the real preferences come back at the end.
reset_sparkle() {
    local k
    for k in SULastCheckTime SUHasLaunchedBefore SUSkippedVersion SUSkippedMinorVersion \
        SUSkippedMajorVersion SUUpdateGroupIdentifier SUEnableAutomaticChecks SUAutomaticallyUpdate; do
        defaults delete "$domain" "$k" >/dev/null 2>&1 || true
    done
}

app=$work/install/NeoSCAD.app
exe=$app/Contents/MacOS/NeoSCAD
install_old() {
    rm -rf "$work/install"
    mkdir -p "$work/install"
    ditto "$work/build-1/NeoSCAD.app" "$app"
}
pid=
launched_at=
launch() {
    : >"$work/server.log"
    launched_at=$(date '+%Y-%m-%d %H:%M:%S')
    open -n -F -a "$app" --args -ApplePersistenceIgnoreState YES "$@"
    pid=
    for _ in $(seq 100); do
        pid=$(pgrep -f "^$exe" | head -1 || true)
        [ -n "$pid" ] && break
        sleep 0.1
    done
    [ -n "$pid" ] || fail "the app did not start"
}
# Quit the way a user does (an Apple event, as Command-Q's terminate:
# would), so Sparkle sees the app terminate and installs; SIGTERM would end
# the process without that.
quit_app() {
    cat >"$work/quit.swift" <<'EOF'
import AppKit
if let app = NSRunningApplication(processIdentifier: pid_t(CommandLine.arguments[1])!) {
    _ = app.terminate()
}
EOF
    xcrun swift "$work/quit.swift" "$pid"
    for _ in $(seq 300); do
        kill -0 "$pid" 2>/dev/null || return 0
        sleep 0.1
    done
    kill -KILL "$pid" 2>/dev/null || true
    fail "the app did not quit within 30 s"
}
requested() { grep -qa "\"GET /$1 " "$work/server.log"; }
# What Sparkle itself logged in this launch.
sparkle_log() {
    /usr/bin/log show --style compact --start "$launched_at" \
        --predicate "processID == $pid AND subsystem == \"org.sparkle-project.Sparkle\"" 2>/dev/null
}
wait_for() {
    local what=$1 seconds=$2
    for _ in $(seq $((seconds * 10))); do
        requested "$what" && return 0
        sleep 0.1
    done
    return 1
}
windows() {
    cat >"$work/windows.swift" <<'EOF'
import CoreGraphics
let pid = Int(CommandLine.arguments[1])!
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
print(list.filter { ($0[kCGWindowOwnerPID as String] as? Int) == pid && ($0[kCGWindowLayer as String] as? Int) == 0 }.count)
EOF
    xcrun swift "$work/windows.swift" "$pid"
}
build_of() { plutil -extract CFBundleVersion raw "$app/Contents/Info.plist"; }

say "1. A tampered appcast is refused"
cp "$site/appcast.xml" "$work/appcast.good"
sed -i '' 's|<title>NeoSCAD</title>|<title>NeoSCAX</title>|' "$site/appcast.xml"
install_old
reset_sparkle
launch -SUAutomaticallyUpdate YES
wait_for appcast.xml 60 || fail "the app never asked for the appcast"
sleep 15
requested "$dmg_name" && fail "the app downloaded the DMG from a tampered appcast"
sparkle_log | grep -q 'EdDSA signature does not match' ||
    fail "Sparkle did not log a bad appcast signature: $(sparkle_log | tail -5)"
quit_app
sleep 5
[ "$(build_of)" = 1 ] || fail "the app changed after a tampered appcast"
echo "PASS: appcast fetched; Sparkle: \"EdDSA signature does not match\"; no DMG request; app still build 1"
cp "$work/appcast.good" "$site/appcast.xml"

say "2. The update is offered"
install_old
reset_sparkle
launch
wait_for appcast.xml 60 || fail "the app never asked for the appcast"
# Sparkle shows a scheduled check's update to a regular app only when the
# app is active, and macOS doesn't let `open -n` take the focus from the
# terminal, so the app is brought forward as a user clicking it would.
sleep 3
open -a "$app"
offered=
for _ in $(seq 30); do
    n=$(windows)
    if [ "$n" -ge 2 ]; then
        offered=$n
        break
    fi
    sleep 1
done
[ -n "$offered" ] || fail "no update window appeared (windows: $(windows))"
requested "$dmg_name" && fail "the DMG was downloaded before the user agreed"
sparkle_log | grep -q 'OK: EdDSA signature is correct for appcast' ||
    fail "Sparkle did not log a verified appcast: $(sparkle_log | tail -5)"
quit_app
echo "PASS: Sparkle: \"OK: EdDSA signature is correct for appcast\"; an update window appeared ($offered on screen); nothing downloaded"

say "3. A DMG whose signature doesn't match is refused"
# A correctly signed appcast whose enclosure carries a valid signature of
# other bytes: only the DMG check (SUVerifyUpdateBeforeExtraction) can
# catch it.
other_sig=$("$sign_update" -p --ed-key-file - "$work/appcast.body" <"$work/test-sparkle.key")
python3 - "$work/appcast.good" "$site/appcast.xml" "$other_sig" <<'EOF'
import re, sys
data = open(sys.argv[1], "rb").read()
body = data[: data.rfind(b"<!-- sparkle-signatures:\n")].decode()
body, n = re.subn(r'sparkle:edSignature="[^"]*"', f'sparkle:edSignature="{sys.argv[3]}"', body)
assert n == 1
open(sys.argv[2], "w").write(body)
EOF
"$sign_update" --disable-signing-warning --ed-key-file - "$site/appcast.xml" <"$work/test-sparkle.key" >/dev/null
install_old
reset_sparkle
launch -SUAutomaticallyUpdate YES
wait_for appcast.xml 60 || fail "the app never asked for the appcast"
wait_for "$dmg_name" 60 || fail "the app never downloaded the DMG"
sleep 15
quit_app
sleep 5
[ "$(build_of)" = 1 ] || fail "a DMG with a bad signature was installed"
/usr/bin/log show --style compact --start "$launched_at" \
    --predicate 'subsystem == "org.sparkle-project.Sparkle"' 2>/dev/null >"$work/sparkle-3.log"
grep -q 'OK: EdDSA signature is correct for appcast' "$work/sparkle-3.log" ||
    fail "Sparkle did not accept the appcast in step 3"
grep -q 'EdDSA signature does not match' "$work/sparkle-3.log" ||
    fail "Sparkle did not log a bad DMG signature: $(tail -5 "$work/sparkle-3.log")"
echo "PASS: appcast accepted, DMG downloaded; Sparkle: \"EdDSA signature does not match\"; app still build 1"
cp "$work/appcast.good" "$site/appcast.xml"

say "4. The update installs"
install_old
reset_sparkle
launch -SUAutomaticallyUpdate YES
wait_for appcast.xml 60 || fail "the app never asked for the appcast"
wait_for "$dmg_name" 60 || fail "the app never downloaded the DMG"
# Sparkle verifies the DMG's EdDSA signature (SUVerifyUpdateBeforeExtraction),
# extracts it and stages the installer; then it waits for the app to quit.
staged=
for _ in $(seq 120); do
    if pgrep -f 'Sparkle.framework/Versions/B/Autoupdate' >/dev/null; then
        staged=yes
        break
    fi
    sleep 0.5
done
[ -n "$staged" ] || fail "Sparkle's installer never started"
sleep 3
quit_app
installed=
for _ in $(seq 120); do
    if [ "$(build_of 2>/dev/null)" = 2 ]; then
        installed=yes
        break
    fi
    sleep 0.5
done
[ -n "$installed" ] || fail "build 2 was not installed (the app is build $(build_of))"
codesign --verify --deep --strict "$app" || fail "the installed app's signature does not verify"
[ "$(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist")" = "$short" ] ||
    fail "the installed app's version is not $short"
for _ in $(seq 100); do pgrep -f 'Sparkle.framework/Versions/B/Autoupdate' >/dev/null || break; sleep 0.1; done
# The installer is Sparkle's Autoupdate, a process of its own.
/usr/bin/log show --style compact --start "$launched_at" \
    --predicate 'process == "Autoupdate" AND subsystem == "org.sparkle-project.Sparkle"' 2>/dev/null |
    grep -q 'OK: EdDSA signature is correct for update' ||
    fail "Sparkle's installer did not log a verified DMG"
echo "PASS: DMG downloaded; Autoupdate: \"OK: EdDSA signature is correct for update\"; build 2 installed in place on quit"

say "All passed"
