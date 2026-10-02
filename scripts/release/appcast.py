#!/usr/bin/env python3
"""Write and sign the macOS app's Sparkle appcast from the GitHub releases.

    appcast.py --releases RELEASES.json --appcast FILE --sign-update PATH
               [--work DIR] [--require-notarized]

macOS only (hdiutil, codesign, and Sparkle's sign_update). The private
EdDSA key is read from the environment variable SPARKLE_ED_PRIVATE_KEY
(the text of the key file: a base64 32-byte seed) and handed to
sign_update on its standard input, never written to disk.

RELEASES.json is `gh api repos/neoscad/neoscad/releases` (newest first).
FILE is the published appcast (the website repository's
updates/macos/appcast.xml), which is rewritten only when what it says
changes; then "appcast.xml" is printed for the workflow to push.

What it lists (docs/release.md, "The macOS app's updates"):

- the newest release whose tag has no prerelease part and that has a DMG,
  in Sparkle's default channel, which every app sees;
- the newest release of any kind that has a DMG, when it is a release
  candidate newer than that, in the channel "rc", which only an app with
  "Receive release candidates" on sees (App/Updates/AppUpdater.swift).

As with the JSON feeds (update-feed.py), the tags decide, not GitHub's
prerelease flag, and the whole appcast is derived again every time.

Each DMG is downloaded, checked against the sha256 GitHub recorded for
the asset, and mounted read-only. The app inside must be org.neoscad.NeoSCAD
with a valid signature, and with --require-notarized (CI) Gatekeeper must
accept both the DMG and the app. sparkle:version is the app's own
CFBundleVersion, read from the DMG rather than its file name, because
Sparkle compares it with the installed app's: a mismatch would offer the
same update forever. Then sign_update signs the DMG (the enclosure's
sparkle:edSignature) and, last, the appcast itself (SURequireSignedFeed).
Ed25519 signatures are deterministic, so an unchanged release list gives
the same bytes and no commit.
"""

import argparse
import email.utils
import importlib.util
import json
import os
import plistlib
import re
import subprocess
import sys
import tempfile
import urllib.request
from datetime import datetime, timezone
from hashlib import sha256
from pathlib import Path
from xml.sax.saxutils import escape, quoteattr

HERE = Path(__file__).resolve().parent

# The JSON feeds' helpers: version order, tags, the DMG's name, checksums.
_spec = importlib.util.spec_from_file_location("update_feed", HERE / "update-feed.py")
feed = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(feed)

BUNDLE_ID = "org.neoscad.NeoSCAD"
RC_CHANNEL = "rc"  # UpdateSettings.releaseCandidateChannel
SIGNATURE_MARK = b"<!-- sparkle-signatures:\n"
SPARKLE_NS = "http://www.andymatuschak.org/xml-namespaces/sparkle"


def die(msg):
    print(f"::error::{msg}", file=sys.stderr)
    sys.exit(1)


def dmg_asset(release):
    """The release's DMG asset, or None (the same pattern as the feeds)."""
    version = feed.release_version(release)
    rx = re.compile("^" + feed.PLATFORMS["macos"].format(v=re.escape(version)) + "$")
    names = sorted(a["name"] for a in release.get("assets", []) if rx.match(a["name"]))
    if not names:
        return None
    name = max(names, key=lambda n: (len(n), n))
    return next(a for a in release["assets"] if a["name"] == name)


def newest_with_dmg(releases, stable_only):
    best = None
    for r in releases:
        if r.get("draft"):
            continue
        v = feed.release_version(r)
        if v is None or (stable_only and feed.is_prerelease(v)) or dmg_asset(r) is None:
            continue
        if best is None or feed.version_key(v) > feed.version_key(feed.release_version(best)):
            best = r
    return best


def run(*cmd, stdin=None):
    p = subprocess.run(cmd, input=stdin, capture_output=True, text=True)
    if p.returncode != 0:
        die(f"{' '.join(cmd)} failed ({p.returncode}): {p.stderr.strip() or p.stdout.strip()}")
    return p.stdout


def download(url, dest, size):
    with urllib.request.urlopen(url, timeout=300) as r, open(dest, "wb") as f:
        while chunk := r.read(1 << 20):
            f.write(chunk)
            if f.tell() > size:
                break
    got = dest.stat().st_size
    if got != size:
        die(f"{url}: {got} bytes, the release says {size}")


def inspect_dmg(path, require_notarized):
    """The app's Info.plist from a DMG, after checking its signatures."""
    if require_notarized:
        out = subprocess.run(
            ["spctl", "-a", "-vv", "-t", "open", "--context", "context:primary-signature", str(path)],
            capture_output=True, text=True,
        )
        if "accepted" not in out.stdout + out.stderr:
            die(f"Gatekeeper rejects {path.name}: {(out.stdout + out.stderr).strip()}")
    with tempfile.TemporaryDirectory() as mnt:
        run("hdiutil", "attach", "-readonly", "-nobrowse", "-noautoopen", "-mountpoint", mnt, str(path))
        try:
            app = Path(mnt) / "NeoSCAD.app"
            if not app.is_dir():
                die(f"{path.name} holds no NeoSCAD.app")
            info = plistlib.loads((app / "Contents" / "Info.plist").read_bytes())
            run("codesign", "--verify", "--deep", "--strict", str(app))
            if require_notarized:
                out = subprocess.run(
                    ["spctl", "-a", "-vv", "-t", "exec", str(app)], capture_output=True, text=True
                )
                if "accepted" not in out.stdout + out.stderr:
                    die(f"Gatekeeper rejects the app in {path.name}: {(out.stdout + out.stderr).strip()}")
        finally:
            subprocess.run(["hdiutil", "detach", "-quiet", mnt], check=False)
    return info


def three_part(v):
    """Sparkle wants minimumSystemVersion as major.minor.patch."""
    parts = (v or "0").split(".")
    return ".".join((parts + ["0", "0"])[:3])


def item(release, channel, args, key):
    version = feed.release_version(release)
    asset = dmg_asset(release)
    name = asset["name"]
    expected = feed.sha256_of(asset, {a["name"]: a for a in release["assets"]}, offline=False)
    if not expected:
        die(f"{name}: no sha256 from GitHub (no digest and no checksum file)")
    dmg = args.work / name
    download(asset["browser_download_url"], dmg, int(asset["size"]))
    got = sha256(dmg.read_bytes()).hexdigest()
    if got != expected:
        die(f"{name}: sha256 {got}, the release says {expected}")
    info = inspect_dmg(dmg, args.require_notarized)
    if info.get("CFBundleIdentifier") != BUNDLE_ID:
        die(f"{name}: the app is {info.get('CFBundleIdentifier')}, not {BUNDLE_ID}")
    build = str(info.get("CFBundleVersion", ""))
    short = str(info.get("CFBundleShortVersionString", ""))
    if not build.isdigit():
        die(f"{name}: CFBundleVersion {build!r} is not a build number")
    if short != version.split("-", 1)[0]:
        die(f"{name}: the app says {short}, the tag says {version}")
    if not info.get("SUPublicEDKey"):
        # Sparkle refuses an update that drops the key ("Sparkle only
        # supports rotation, but not removal"), so this would be offered
        # and then fail on every Mac.
        die(f"{name}: the app has no SUPublicEDKey; it was built without the update key")
    signature = run(args.sign_update, "-p", "--ed-key-file", "-", str(dmg), stdin=key).strip()
    dmg.unlink()

    published = release.get("published_at") or release.get("created_at")
    when = datetime.strptime(published, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    url = release["html_url"]
    notes = f"<h2>NeoSCAD {escape(version)}</h2>"
    if channel:
        notes += "<p>A release candidate: a preview of the next version, for testing.</p>"
    notes += f'<p><a href="{escape(url)}">Release notes</a></p>'
    lines = [
        "    <item>",
        f"      <title>NeoSCAD {escape(version)}</title>",
        f"      <pubDate>{email.utils.format_datetime(when)}</pubDate>",
        f"      <link>{escape(url)}</link>",
        f"      <sparkle:version>{build}</sparkle:version>",
        f"      <sparkle:shortVersionString>{escape(version)}</sparkle:shortVersionString>",
        f"      <sparkle:minimumSystemVersion>{three_part(info.get('LSMinimumSystemVersion'))}"
        "</sparkle:minimumSystemVersion>",
    ]
    if channel:
        lines.append(f"      <sparkle:channel>{channel}</sparkle:channel>")
    lines += [
        f"      <description><![CDATA[{notes}]]></description>",
        f"      <enclosure url={quoteattr(asset['browser_download_url'])} length=\"{asset['size']}\""
        f' type="application/x-apple-diskimage" sparkle:edSignature={quoteattr(signature)}/>',
        "    </item>",
    ]
    print(f"{channel or 'stable'}: {version} (build {build}) {name}", file=sys.stderr)
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--releases", required=True, type=Path)
    ap.add_argument("--appcast", required=True, type=Path)
    ap.add_argument("--sign-update", required=True)
    ap.add_argument("--work", type=Path)
    ap.add_argument("--require-notarized", action="store_true")
    args = ap.parse_args()

    key = os.environ.get("SPARKLE_ED_PRIVATE_KEY", "").strip()
    if not key:
        die("SPARKLE_ED_PRIVATE_KEY is not set")
    key += "\n"
    releases = json.loads(args.releases.read_text())
    tmp = None
    if args.work is None:
        tmp = tempfile.TemporaryDirectory()
        args.work = Path(tmp.name)
    args.work.mkdir(parents=True, exist_ok=True)

    stable = newest_with_dmg(releases, stable_only=True)
    newest = newest_with_dmg(releases, stable_only=False)
    items = []
    if stable is not None:
        items.append(item(stable, None, args, key))
    if newest is not None and newest is not stable and feed.is_prerelease(feed.release_version(newest)):
        items.append(item(newest, RC_CHANNEL, args, key))
    if not items:
        print("no release has a DMG yet; the appcast is left alone", file=sys.stderr)
        return

    body = "\n".join(
        [
            '<?xml version="1.0" encoding="utf-8"?>',
            f'<rss version="2.0" xmlns:sparkle="{SPARKLE_NS}">',
            "  <channel>",
            "    <title>NeoSCAD</title>",
            "    <link>https://neoscad.org/</link>",
            "    <description>Updates to the NeoSCAD app for macOS</description>",
            "    <language>en</language>",
            *items,
            "  </channel>",
            "</rss>",
            "",
        ]
    ).encode()

    if args.appcast.exists():
        old = args.appcast.read_bytes()
        cut = old.rfind(SIGNATURE_MARK)
        if cut >= 0 and old[:cut] == body:
            print("appcast: unchanged", file=sys.stderr)
            return
    args.appcast.parent.mkdir(parents=True, exist_ok=True)
    args.appcast.write_bytes(body)
    # Embeds the feed's signature as a comment at the end. No warning
    # comment at the top, so the signed part is exactly `body` and the
    # comparison above stays a byte comparison.
    run(args.sign_update, "--disable-signing-warning", "--ed-key-file", "-", str(args.appcast), stdin=key)
    run(args.sign_update, "--verify", "--ed-key-file", "-", str(args.appcast), stdin=key)
    if not args.appcast.read_bytes().startswith(body):
        die("sign_update changed the appcast beyond appending its signature")
    print("appcast: written and signed", file=sys.stderr)
    print(args.appcast.name)


if __name__ == "__main__":
    main()
