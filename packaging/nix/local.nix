# package.nix built from this source tree instead of the release tag, for
# the flake and for testing package.nix before a tag exists:
#
#   nix-build packaging/nix        # default.nix, with <nixpkgs>
#   nix build .#neoscad            # the flake, with its locked nixpkgs
#
# The crates come from Cargo.lock through importCargoLock, which needs no
# cargoHash: every dependency is from crates.io (the patched ones are
# path dependencies under vendor/) and none is a git dependency. The
# version is the workspace's, so versionCheckHook checks the binary that
# was actually built rather than package.nix's release number.
{
  lib,
  callPackage,
  rustPlatform,
}:

let
  root = ../..;
  # Only what the build and its tests read, so an edit to the docs, the
  # web demo or the macOS app does not rebuild the package, and a local
  # target/ or .reference/ checkout never reaches the store. The CLI's
  # build script reads Cargo.lock, the assets crate's reads assets/, cargo
  # needs every workspace member's manifest under crates/, and the CLI's
  # tests run `neoscad test examples/tests`.
  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (root + "/Cargo.toml")
      (root + "/Cargo.lock")
      (root + "/LICENSE")
      (root + "/NOTICE")
      (root + "/assets")
      # The extensions' golden models and cases (sketch, query, exact),
      # which the session and geom tests read.
      (root + "/conformance/extensions")
      # The two reference pages whose examples the session tests evaluate.
      (root + "/docs/geometry-queries.md")
      (root + "/docs/sketch.md")
      (root + "/crates")
      (root + "/examples")
      (root + "/packaging/licenses")
      (root + "/vendor")
      # The examples the app core embeds (crates/client), which the CLI
      # links since its update notice.
      (root + "/web/examples")
    ];
  };
in
(callPackage ./package.nix { }).overrideAttrs {
  inherit ((lib.importTOML (root + "/Cargo.toml")).workspace.package) version;
  inherit src;
  cargoDeps = rustPlatform.importCargoLock { lockFile = root + "/Cargo.lock"; };
}
