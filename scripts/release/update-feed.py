#!/usr/bin/env python3
"""Write the update feeds (stable.json, rc.json) from the GitHub releases.

    update-feed.py --releases RELEASES.json --feed-dir DIR [--offline]

RELEASES.json is `gh api repos/neoscad/neoscad/releases` (newest first;
the first page is plenty). DIR holds the published feeds (the website
repository's updates/v1/); each is rewritten only when what it says
changes, with its serial raised by one. The names of the files that
changed are printed, one per line, for the workflow to sign and push.

The feeds follow the releases' tags, not GitHub's prerelease flag: a full
release is a prerelease for hours while its macOS app is notarized
(docs/release.md, "The macOS app after the release"), and the Windows and
Linux apps, and the CLI, should hear about it meanwhile. So:

- stable.json names the newest release whose tag has no prerelease part;
- rc.json names the newest release of any kind, so the rc channel also
  gets the final release after its candidates;
- each lists the apps' installers the release has so far. The DMG is
  missing until macos-notarize.yml attaches it and runs this again, and an
  app is offered only a release with its own installer
  (crates/client/src/update.rs), so a Mac waits for its DMG.

The whole feed is derived from the releases every time, so running it
again (a re-run, the notarize workflow's hourly runs) changes nothing, and
an older release's late DMG can't move a feed back. Feeds are never moved
to a lower version.

A sha256 comes from the asset's `digest` (GitHub computes it on upload)
or, failing that, from the `.sha256` file the workflows attach beside it.
An installer with neither is left out with a warning rather than listed
unchecked. --offline never downloads (the local test).
"""

import argparse
import json
import re
import sys
import urllib.request
from pathlib import Path

SCHEMA = 1

# The apps' installers, by the feed's platform key (Platform::key in
# crates/client/src/update.rs). {v} is the release's version.
PLATFORMS = {
    # macos-notarize.yml: NeoSCAD-<version>-<build>.dmg, universal.
    "macos": r"NeoSCAD-{v}-\d+\.dmg",
    # windows-installer.yml
    "windows-x64": r"NeoSCAD-{v}-windows-x64\.msi",
    "windows-arm64": r"NeoSCAD-{v}-windows-arm64\.msi",
    # flatpak.yml
    "linux-x86_64": r"NeoSCAD-{v}-linux-x86_64\.flatpak",
    "linux-aarch64": r"NeoSCAD-{v}-linux-aarch64\.flatpak",
}

# The checksum file macos-notarize.yml attaches for the DMG and dSYMs;
# the other installers have `<name>.sha256` beside them.
MACOS_SUMS = "NeoSCAD-macos-app.sha256"

SEMVER = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?$"
)


def warn(msg):
    print(f"::warning::{msg}", file=sys.stderr)


def version_key(v):
    """A sort key in semver precedence: 0.3.0-rc.1 < 0.3.0-rc.10 < 0.3.0."""
    m = SEMVER.match(v)
    if not m:
        return None
    major, minor, patch, pre = m.groups()
    core = (int(major), int(minor), int(patch))
    if pre is None:
        return (core, 1, ())
    ids = tuple(
        (0, int(p), "") if p.isdigit() else (1, 0, p) for p in pre.split(".")
    )
    return (core, 0, ids)


def is_prerelease(v):
    return "-" in v.split("+", 1)[0]


def release_version(release):
    tag = release.get("tag_name", "")
    v = tag[1:] if tag.startswith("v") else tag
    return v if version_key(v) else None


def fetch(url):
    with urllib.request.urlopen(url, timeout=60) as r:
        return r.read().decode("utf-8", "replace")


def sums_from(text):
    """`<hex>  <name>` lines (shasum and sha256sum both) as {name: hex}."""
    out = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) >= 2 and re.fullmatch(r"[0-9a-fA-F]{64}", parts[0]):
            out[parts[-1].lstrip("*")] = parts[0].lower()
        elif len(parts) == 1 and re.fullmatch(r"[0-9a-fA-F]{64}", parts[0]):
            out[""] = parts[0].lower()
    return out


def sha256_of(asset, assets, offline):
    digest = asset.get("digest") or ""
    if digest.startswith("sha256:"):
        return digest.split(":", 1)[1].lower()
    if offline:
        return None
    name = asset["name"]
    for sums_name in (f"{name}.sha256", MACOS_SUMS):
        sums = assets.get(sums_name)
        if not sums:
            continue
        try:
            table = sums_from(fetch(sums["browser_download_url"]))
        except OSError as e:
            warn(f"cannot read {sums_name}: {e}")
            continue
        found = table.get(name) or (table.get("") if sums_name != MACOS_SUMS else None)
        if found:
            return found
    return None


def entry(release, channel, offline):
    version = release_version(release)
    assets = {a["name"]: a for a in release.get("assets", [])}
    artifacts = {}
    for platform, pattern in PLATFORMS.items():
        rx = re.compile("^" + pattern.format(v=re.escape(version)) + "$")
        matches = sorted(n for n in assets if rx.match(n))
        if not matches:
            continue
        # Two DMGs of one version would be two builds; the later name
        # (higher build number at equal length) is the newer one.
        name = max(matches, key=lambda n: (len(n), n))
        asset = assets[name]
        sha = sha256_of(asset, assets, offline)
        if not sha:
            warn(f"{name}: no sha256 (no digest and no checksum file); left out of {channel}.json")
            continue
        artifacts[platform] = {
            "name": name,
            "url": asset["browser_download_url"],
            "sha256": sha,
            "size": int(asset["size"]),
        }
    return {
        "schema": SCHEMA,
        "channel": channel,
        "version": version,
        "date": (release.get("published_at") or release.get("created_at") or "")[:10],
        "url": release["html_url"],
        "artifacts": artifacts,
    }


def newest(releases, stable_only):
    best = None
    for r in releases:
        if r.get("draft"):
            continue
        v = release_version(r)
        if v is None or (stable_only and is_prerelease(v)):
            continue
        if best is None or version_key(v) > version_key(release_version(best)):
            best = r
    return best


def write_feed(path, feed):
    # Sorted keys and a final newline: the same feed is the same bytes, so
    # an unchanged feed makes no commit on the website.
    path.write_text(json.dumps(feed, indent=2, sort_keys=True) + "\n")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--releases", required=True, type=Path)
    ap.add_argument("--feed-dir", required=True, type=Path)
    ap.add_argument("--offline", action="store_true")
    args = ap.parse_args()

    releases = json.loads(args.releases.read_text())
    args.feed_dir.mkdir(parents=True, exist_ok=True)
    changed = []
    for channel, stable_only in (("stable", True), ("rc", False)):
        release = newest(releases, stable_only)
        if release is None:
            print(f"{channel}: no release", file=sys.stderr)
            continue
        new = entry(release, channel, args.offline)
        path = args.feed_dir / f"{channel}.json"
        old = json.loads(path.read_text()) if path.exists() else None
        if old is not None:
            serial = int(old.get("serial", 0))
            if {k: v for k, v in old.items() if k != "serial"} == new:
                print(f"{channel}: {new['version']} unchanged (serial {serial})", file=sys.stderr)
                continue
            old_key = version_key(str(old.get("version", "")))
            if old_key is not None and old_key > version_key(new["version"]):
                warn(f"{channel}.json names {old['version']}, newer than {new['version']}; left alone")
                continue
        else:
            serial = 0
        new["serial"] = serial + 1
        write_feed(path, new)
        print(
            f"{channel}: {new['version']} serial {new['serial']}, "
            f"installers: {', '.join(new['artifacts']) or 'none'}",
            file=sys.stderr,
        )
        changed.append(path.name)
    for name in changed:
        print(name)


if __name__ == "__main__":
    main()
