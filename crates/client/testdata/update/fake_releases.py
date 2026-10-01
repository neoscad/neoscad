#!/usr/bin/env python3
"""Two fake GitHub release lists, as `gh api repos/R/releases` returns them,
for the update-feed fixtures (make.sh) and scripts/release/test-update-feed.sh:

    fake_releases.py DIR    # DIR/releases-a.json, DIR/releases-b.json

In both, 0.4.0-rc.1 is the newest release (an rc with only the Linux x86_64
Flatpak), 0.3.0 a full release and 0.2.0 the one before it. In
releases-a.json 0.3.0 is held as a GitHub prerelease without its DMG (Apple
still has it); in releases-b.json the DMG is attached. A draft must be
ignored. Digests are the sha256 of each file's name: they only need to be
stable.
"""

import hashlib
import json
import sys
from pathlib import Path

R = "https://github.com/neoscad/neoscad/releases"


def asset(tag, name, size):
    return {
        "name": name,
        "size": size,
        "browser_download_url": f"{R}/download/{tag}/{name}",
        "digest": "sha256:" + hashlib.sha256(name.encode()).hexdigest(),
    }


def release(v, prerelease, date, names):
    tag = "v" + v
    return {
        "tag_name": tag,
        "draft": False,
        "prerelease": prerelease,
        "published_at": date + "T12:00:00Z",
        "html_url": f"{R}/tag/{tag}",
        "assets": [asset(tag, n, 1000 + i) for i, n in enumerate(names)],
    }


def apps(v, dmg):
    names = [
        f"NeoSCAD-{v}-windows-x64.msi",
        f"NeoSCAD-{v}-windows-x64.msi.sha256",
        f"NeoSCAD-{v}-windows-arm64.msi",
        f"NeoSCAD-{v}-linux-x86_64.flatpak",
        f"NeoSCAD-{v}-linux-aarch64.flatpak",
        # The CLI's own MSI and the source: not the apps' installers.
        "neoscad-x86_64-pc-windows-msvc.msi",
        "source.tar.gz",
    ]
    if dmg:
        names += [
            f"NeoSCAD-{v}-1234.dmg",
            f"NeoSCAD-{v}-1234-dSYMs.zip",
            "NeoSCAD-macos-app.sha256",
        ]
    return names


def releases(dmg_030):
    # Newest first, as the API lists them.
    return [
        release("0.4.0-rc.1", True, "2026-10-20", ["NeoSCAD-0.4.0-rc.1-linux-x86_64.flatpak"]),
        release("0.3.0", not dmg_030, "2026-10-01", apps("0.3.0", dmg_030)),
        release("0.2.0", False, "2026-09-01", apps("0.2.0", True)),
        {"tag_name": "v9.9.9", "draft": True, "assets": []},
    ]


out = Path(sys.argv[1])
for suffix, dmg in (("a", False), ("b", True)):
    (out / f"releases-{suffix}.json").write_text(json.dumps(releases(dmg), indent=1) + "\n")
