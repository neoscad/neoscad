#!/usr/bin/env bash
# End-to-end test of the update feed and the CLI's notice, on this machine:
#
#   scripts/release/test-update-feed.sh
#
# 1. writes a feed with scripts/release/update-feed.py, as the release
#    workflow does, from fake releases whose newest is 99.0.0;
# 2. signs it with the test key (crates/client/testdata/update/test.key;
#    minisign, or minisign in an alpine container);
# 3. serves it on 127.0.0.1;
# 4. builds neoscad trusting the test key (NEOSCAD_UPDATE_TEST_PUBLIC_KEY,
#    read at compile time) and runs it in a pseudo-terminal, where the
#    notice must appear once, and piped, redirected, with CI or with
#    NEOSCAD_NO_UPDATE_CHECK set, where it must not, and no check may run.
#
# Everything lives in a temporary directory (HOME and XDG_CACHE_HOME
# point there, so the real cache is untouched). Uses CARGO_TARGET_DIR if
# set.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
data=$root/crates/client/testdata/update
work=$(mktemp -d)
server=""
cleanup() {
    if [[ -n "$server" ]]; then
        kill "$server" 2>/dev/null || true
        wait "$server" 2>/dev/null || true
    fi
    rm -rf "$work"
}
trap cleanup EXIT

echo "== the feed"
python3 - "$work/releases.json" <<'EOF'
import json, sys
R = "https://github.com/neoscad/neoscad/releases"
name = "NeoSCAD-99.0.0-linux-x86_64.flatpak"
json.dump([{
    "tag_name": "v99.0.0", "draft": False, "prerelease": False,
    "published_at": "2099-01-01T00:00:00Z", "html_url": f"{R}/tag/v99.0.0",
    "assets": [{"name": name, "size": 1, "browser_download_url": f"{R}/download/v99.0.0/{name}",
                "digest": "sha256:" + "0" * 64}],
}], open(sys.argv[1], "w"))
EOF
feed=$work/site/updates/v1
python3 "$root/scripts/release/update-feed.py" --offline --releases "$work/releases.json" --feed-dir "$feed"
cp "$data/test.key" "$work/test.key"
if command -v minisign >/dev/null; then
    minisign -S -s "$work/test.key" -m "$feed/stable.json" >/dev/null
else
    docker run --rm -v "$work:/w" -w /w alpine:3.22 sh -c \
        'apk add -q minisign >/dev/null && minisign -S -s test.key -m site/updates/v1/stable.json >/dev/null'
fi
ls "$feed"

echo "== build neoscad trusting the test key"
key=$(sed -n 2p "$data/test.pub")
(cd "$root" && NEOSCAD_UPDATE_TEST_PUBLIC_KEY=$key cargo build -q -p neoscad-cli)
bin=${CARGO_TARGET_DIR:-$root/target}/debug/neoscad
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -1)
echo "neoscad $version"

echo "== serve it"
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
(cd "$work/site" && exec python3 -m http.server "$port" --bind 127.0.0.1 >/dev/null 2>&1) &
server=$!
for _ in $(seq 50); do curl -fs "http://127.0.0.1:$port/updates/v1/stable.json" >/dev/null && break; sleep 0.1; done

export HOME=$work/home XDG_CACHE_HOME=$work/cache
export NEOSCAD_UPDATE_FEED_URL=http://127.0.0.1:$port/updates/v1/
unset CI NEOSCAD_NO_UPDATE_CHECK
if [[ $(uname) == Darwin ]]; then
    state=$HOME/Library/Caches/neoscad/update-check.json
else
    state=$XDG_CACHE_HOME/neoscad/update-check.json
fi

# Runs neoscad in a pseudo-terminal (stdout and stderr both terminals);
# pty.spawn copies what it writes to this script's stdout.
in_pty() {
    python3 -c 'import pty, sys; pty.spawn(sys.argv[1:])' "$@" </dev/null | tr -d '\r'
}

fail() { echo "FAIL: $*" >&2; exit 1; }
want="neoscad 99.0.0 is available (you have $version): https://github.com/neoscad/neoscad/releases/tag/v99.0.0"

echo "== piped, redirected, CI, NEOSCAD_NO_UPDATE_CHECK: silent, and no check"
out=$("$bin" --version 2>&1 | cat)
[[ "$out" != *available* ]] || fail "notice when piped: $out"
"$bin" --version >"$work/out.txt" 2>"$work/err.txt"
! grep -q available "$work/err.txt" || fail "notice when redirected"
out=$(CI=true in_pty "$bin" --version)
[[ "$out" != *available* ]] || fail "notice with CI set"
out=$(NEOSCAD_NO_UPDATE_CHECK=1 in_pty "$bin" --version)
[[ "$out" != *available* ]] || fail "notice with NEOSCAD_NO_UPDATE_CHECK set"
[[ ! -e "$state" ]] || fail "a check ran: $(cat "$state")"
echo ok

echo "== a terminal: the check runs in the background"
in_pty "$bin" --version
for _ in $(seq 100); do grep -q '"notice"' "$state" 2>/dev/null && break; sleep 0.1; done
grep -q '"99.0.0"' "$state" || fail "no result stored: $(cat "$state" 2>/dev/null)"
echo "== the next command shows the notice"
out=$(in_pty "$bin" --version)
echo "$out"
grep -qxF "$want" <<<"$out" || fail "no notice"
echo "== and only once"
out=$(in_pty "$bin" --version)
[[ "$out" != *available* ]] || fail "notice shown twice: $out"

echo "== a tampered feed is ignored"
rm -f "$state"
sed -i.bak 's/99.0.0/98.0.0/' "$feed/stable.json"
in_pty "$bin" --version >/dev/null
sleep 3
out=$(in_pty "$bin" --version)
[[ "$out" != *available* ]] || fail "a tampered feed was believed: $out"
! grep -q '"notice"' "$state" || fail "a tampered feed was stored"
echo "PASS"
