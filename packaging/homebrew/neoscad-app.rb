# Template for Casks/neoscad-app.rb in the neoscad/homebrew-tap repository:
# the macOS app. (The `neoscad` formula, for the command-line tool, is
# generated and pushed to the same tap by cargo-dist's release workflow.)
#
# scripts/release/fill-manifests.sh fills @VERSION@, @BUILD@ (the DMG's
# build number) and @SHA256_DMG@. Only a notarized DMG belongs here: Homebrew
# disabled OpenSCAD's own cask in September 2026 because it failed
# Gatekeeper (docs/packaging.md).
cask "neoscad-app" do
  version "@VERSION@,@BUILD@"
  sha256 "@SHA256_DMG@"

  url "https://github.com/neoscad/neoscad/releases/download/v#{version.csv.first}/NeoSCAD-#{version.csv.first}-#{version.csv.second}.dmg"
  name "NeoSCAD"
  desc "OpenSCAD-compatible programmable solid CAD"
  homepage "https://neoscad.org/"

  depends_on arch: :arm64
  depends_on macos: ">= :sequoia"

  app "NeoSCAD.app"

  zap trash: [
    "~/Library/Caches/org.neoscad.NeoSCAD",
    "~/Library/Preferences/org.neoscad.NeoSCAD.plist",
    "~/Library/Saved Application State/org.neoscad.NeoSCAD.savedState",
  ]
end
