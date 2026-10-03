# Template for Casks/neoscad-app.rb in the neoscad/homebrew-tap repository:
# the macOS app. (The `neoscad` formula, for the command-line tool, is
# generated and pushed to the same tap by cargo-dist's release workflow.)
#
# scripts/release/fill-cask.sh fills @VERSION@, @BUILD@ (the DMG's build
# number) and @SHA256_DMG@, and .github/workflows/publish-macos-app.yml
# pushes the result to the tap once the notarized DMG is attached to the
# release. Only a notarized DMG belongs here: Homebrew disabled OpenSCAD's
# own cask in September 2026 because it failed Gatekeeper
# (docs/packaging.md).
#
# No `binary` stanza, on purpose, and so no `conflicts_with formula:
# "neoscad"`. The app does carry the CLI (NeoSCAD.app/Contents/Helpers/
# neoscad), but for AI agent clients only: the app writes their configs
# with the absolute path of a link it keeps at ~/Library/Application
# Support/NeoSCAD/bin/neoscad, so nothing depends on PATH (docs/mcp.md,
# "Setup from the apps"). Exposing it as a `binary` would put a second
# `neoscad` in Homebrew's bin, which conflicts with the `neoscad` formula
# (the owner's decision, 2026-10-02): `brew install neoscad/tap/neoscad`
# remains the way to get the command line in a terminal, and the cask and
# the formula still install nothing in common.
#
# `auto_updates true`: the app updates itself with Sparkle (docs/release.md,
# "The macOS app's updates"), so `brew upgrade` leaves it alone unless
# given --greedy, and Homebrew and Sparkle don't both replace the app.
# Every cask this template fills is for an app with the update key:
# scripts/apple/release.sh refuses to notarize one without it.
cask "neoscad-app" do
  version "@VERSION@,@BUILD@"
  sha256 "@SHA256_DMG@"

  url "https://github.com/neoscad/neoscad/releases/download/v#{version.csv.first}/NeoSCAD-#{version.csv.first}-#{version.csv.second}.dmg"
  name "NeoSCAD"
  desc "OpenSCAD-compatible programmable solid CAD"
  homepage "https://neoscad.org/"

  # The build number is only in the DMG's name, so it is read from the
  # latest release's asset list rather than from its tag.
  livecheck do
    url :url
    regex(/^NeoSCAD[._-]v?(\d+(?:\.\d+)+)[._-](\d+)\.dmg$/i)
    strategy :github_latest do |json, regex|
      json["assets"]&.map do |asset|
        match = asset["name"]&.match(regex)
        next if match.blank?

        "#{match[1]},#{match[2]}"
      end
    end
  end

  # auto_updates: see the top. depends_on: apple/project.yml's
  # deploymentTarget, macOS 15.0. A bare symbol is a minimum: current
  # Homebrew (rubocop Homebrew/OSDependsOn) rewrites ">= :sequoia" to it
  # and spells a maximum `depends_on maximum_macos:`.
  auto_updates true
  depends_on macos: :sequoia

  app "NeoSCAD.app"

  # The Quick Look preview and thumbnail extensions are sandboxed, so they
  # keep their own containers under their own bundle ids; the app's editor
  # is a WKWebView, which keeps WebKit data under the app's. Application
  # Support/NeoSCAD holds the CLI link agent configs name (see the top).
  zap trash: [
    "~/Library/Application Scripts/org.neoscad.NeoSCAD.QuickLook",
    "~/Library/Application Scripts/org.neoscad.NeoSCAD.Thumbnail",
    "~/Library/Application Support/NeoSCAD",
    "~/Library/Caches/org.neoscad.NeoSCAD",
    "~/Library/Containers/org.neoscad.NeoSCAD.QuickLook",
    "~/Library/Containers/org.neoscad.NeoSCAD.Thumbnail",
    "~/Library/HTTPStorages/org.neoscad.NeoSCAD",
    "~/Library/Preferences/org.neoscad.NeoSCAD.plist",
    "~/Library/Saved Application State/org.neoscad.NeoSCAD.savedState",
    "~/Library/WebKit/org.neoscad.NeoSCAD",
  ]
end
