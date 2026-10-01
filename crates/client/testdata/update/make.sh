#!/usr/bin/env bash
# Regenerates the signed update-feed fixtures that src/update_tests.rs
# reads, with scripts/release/update-feed.py as the release workflow runs
# it:
#
#   crates/client/testdata/update/make.sh
#
# test.key and other.key are throwaway minisign keys with no password,
# made for these tests (`minisign -G -W`). Neither is trusted by any
# build: RELEASE_KEYS in src/update.rs holds only the release key, and
# test.pub is trusted only by the tests and by a CLI built with
# NEOSCAD_UPDATE_TEST_PUBLIC_KEY set (scripts/release/test-update-feed.sh).
#
# Needs minisign (`brew install minisign`, `apt install minisign`); without
# it, runs it in an alpine container.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
cd "$here"

sign() { # KEY FILE SIG
    if command -v minisign >/dev/null; then
        minisign -S -s "$1" -m "$2" -x "$3" -t "neoscad update feed (test)" >/dev/null
    else
        docker run --rm -v "$here:/w" -w /w alpine:3.22 sh -c \
            "apk add -q minisign >/dev/null && minisign -S -s '$1' -m '$2' -x '$3' -t 'neoscad update feed (test)' >/dev/null"
    fi
}

python3 fake_releases.py .
rm -rf feed && mkdir feed
# The release, held without its DMG: stable 0.3.0 without macOS, serial 1.
python3 "$root/scripts/release/update-feed.py" --offline --releases releases-a.json --feed-dir feed >/dev/null
cp feed/stable.json stable-1.json
cp feed/rc.json rc-1.json
# Nothing changed: nothing is rewritten.
test -z "$(python3 "$root/scripts/release/update-feed.py" --offline --releases releases-a.json --feed-dir feed)"
# The DMG lands: stable gets macOS and serial 2; rc (0.4.0-rc.1) is unchanged.
test "$(python3 "$root/scripts/release/update-feed.py" --offline --releases releases-b.json --feed-dir feed)" = stable.json
cp feed/stable.json stable-2.json
rm -rf feed

for f in stable-1.json stable-2.json rc-1.json; do
    sign test.key "$f" "$f.minisig"
done
sign other.key stable-2.json stable-2.json.other.minisig
