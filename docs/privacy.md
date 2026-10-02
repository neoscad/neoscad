# Privacy: what NeoSCAD sends over the network

The `neoscad` command line reads, renders and exports files on your
machine without network requests, with two exceptions. One is the update
check, described here. The other is `neoscad bench`, which downloads its
kit and, only with `--submit` and your confirmation, files a public result
(`docs/community-bench.md`, "Privacy"). (`neoscad mcp --browser` listens
on the loopback address for the web demo's tab; it sends nothing out.)

## The update check

**What it asks for.** Once a day at most, NeoSCAD asks
`https://neoscad.org/updates/v1/stable.json` and its signature
`stable.json.minisig` whether a newer release exists. An app with "Receive
release candidates" turned on asks for `rc.json` instead. Each request is a
plain HTTPS `GET` of a static file. It has no query string, no cookies, no
install or user id, and the User-Agent is the bare word `neoscad`, without
the version or the operating system. Your NeoSCAD version, platform and
files never leave the machine, because the feed lists every platform and
the comparison is made locally (`crates/client/src/update.rs`).

**Who sees it.** neoscad.org is served by GitHub Pages, so GitHub sees the
request's IP address and time, as it would for any web page. Nothing else
is sent, and NeoSCAD keeps no record of its own beyond the cache file
described below.

**What it does with the answer.** It checks the feed's signature against a
key built into NeoSCAD. A feed that doesn't verify, or that is older than
one already seen, is ignored. When a verified feed names a newer version:

- the command line prints one line on stderr, for example
  `neoscad 0.3.0 is available (you have 0.2.0): https://github.com/neoscad/neoscad/releases/tag/v0.3.0`.
  It never downloads or installs anything; update through the package
  manager you installed with;
- the apps say that an update is available. Downloading and installing it
  is up to you.

### The command line

`neoscad` checks only when a person is likely to read the line:

- both stdout and stderr are terminals, so it is never active when output
  is piped or redirected, or when an editor, an agent or a script runs
  `neoscad`;
- `CI` is not set;
- the command is not `serve`, `mcp` or `lsp`.

The check runs in a separate background process, so it never delays a
command. Its result is shown at the end of the next command. The time of
the last check and its result are kept in `update-check.json` in the cache
directory: `~/Library/Caches/neoscad` on macOS,
`$XDG_CACHE_HOME/neoscad` or `~/.cache/neoscad` on Linux, and
`%LOCALAPPDATA%\neoscad\cache` on Windows.

**To turn it off**, set `NEOSCAD_NO_UPDATE_CHECK` to any value, for
example `export NEOSCAD_NO_UPDATE_CHECK=1` in your shell's profile. Nothing
is then fetched or written.

### The apps

The macOS, Windows and Linux apps check automatically, about once a day,
and a setting turns that off (owner decision,
`docs/audits/auto-update.md`, "Decisions").

**The macOS app** uses Sparkle, which reads its own signed file,
`https://neoscad.org/updates/macos/appcast.xml`, instead of the JSON feed.
It checks soon after the first launch and then about once a day. NeoSCAD >
Settings… has "Check for updates automatically" (on) and "Receive release
candidates" (off), and NeoSCAD > Check for Updates… checks at once either
way. The request is a plain `GET` of that file with the User-Agent
`neoscad`. Sparkle's system profile, which would add the Mac's model and
OS version to the request, is off. When an update is offered and you
accept it, the DMG is downloaded from GitHub Releases. Sparkle keeps the
time of the last check, and your choices, in the app's preferences
(`~/Library/Preferences/org.neoscad.NeoSCAD.plist`). A build without the
update key (development builds) makes no update request at all.

**The Windows and Linux apps** read the same feed as the command line. A
build with no feed key (any build before 0.2.2) makes no update request
at all.

- **Windows:** Help > "Check for Updates Automatically" (on by default)
  and "Receive Release Candidates" (off); Help > "Check for Updates…"
  works with the first off. The settings, the time of the last check and
  the last feed serial are kept in `%LOCALAPPDATA%\NeoSCAD\updates.json`.
  Install downloads the new MSI from GitHub Releases, which then sees
  that request too; nothing is downloaded until you choose Install.
- **Linux:** Preferences > Updates has the same two settings, and the main
  menu "Check for Updates". They are kept in `updates.json` under
  `~/.config/neoscad/` (`~/.var/app/org.neoscad.NeoSCAD/config/neoscad/`
  in the Flatpak). The app only says that a release exists; downloading
  it is up to you.

Both send the same plain request as the command line, and neither checks
automatically when `NEOSCAD_NO_UPDATE_CHECK` or `CI` is set.
