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
# No `binary` stanza and so no `conflicts_with formula: "neoscad"`: the app
# bundle carries no command-line tool (the DMG's universal CLI ships
# beside it, not inside it), so the cask and the formula install nothing
# in common. No `auto_updates`: the app does not update itself.
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

  # apple/project.yml's deploymentTarget, macOS 15.0. A bare symbol is a
  # minimum: current Homebrew (rubocop Homebrew/OSDependsOn) rewrites
  # ">= :sequoia" to it and spells a maximum `depends_on maximum_macos:`.
  depends_on macos: :sequoia

  app "NeoSCAD.app"

  # The Quick Look preview and thumbnail extensions are sandboxed, so they
  # keep their own containers under their own bundle ids; the app's editor
  # is a WKWebView, which keeps WebKit data under the app's.
  zap trash: [
    "~/Library/Application Scripts/org.neoscad.NeoSCAD.QuickLook",
    "~/Library/Application Scripts/org.neoscad.NeoSCAD.Thumbnail",
    "~/Library/Caches/org.neoscad.NeoSCAD",
    "~/Library/Containers/org.neoscad.NeoSCAD.QuickLook",
    "~/Library/Containers/org.neoscad.NeoSCAD.Thumbnail",
    "~/Library/Preferences/org.neoscad.NeoSCAD.plist",
    "~/Library/Saved Application State/org.neoscad.NeoSCAD.savedState",
    "~/Library/WebKit/org.neoscad.NeoSCAD",
  ]
end
