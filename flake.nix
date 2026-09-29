{
  # `nix run github:neoscad/neoscad -- model.scad -o model.stl`, or
  # `nix profile install github:neoscad/neoscad`: the `neoscad` CLI built
  # from source with nixpkgs' own Rust (1.98.1 on nixos-unstable, the
  # version rust-toolchain.toml pins).
  #
  # The package is packaging/nix/package.nix, the file submitted to
  # nixpkgs, built from this tree by packaging/nix/local.nix; its tests run
  # in the build. Built in CI by .github/workflows/nix.yml.
  description = "NeoSCAD: OpenSCAD-compatible programmable solid CAD";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      # Not x86_64-darwin: nixpkgs 26.11 (and so nixos-unstable) dropped
      # it, and evaluating its package set throws. Intel Macs have the
      # universal DMG and the Homebrew formula.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        neoscad = pkgs.callPackage ./packaging/nix/local.nix { };
        default = neoscad;
      });

      # `nix flake check` builds the package, and with it its tests.
      checks = forAll (pkgs: {
        neoscad = self.packages.${pkgs.stdenv.hostPlatform.system}.neoscad;
      });
    };
}
