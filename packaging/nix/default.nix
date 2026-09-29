# `nix-build packaging/nix`: package.nix built from this checkout with the
# nixpkgs on NIX_PATH (or `--arg pkgs`), without flakes. See local.nix.
{
  pkgs ? import <nixpkgs> { },
}:

pkgs.callPackage ./local.nix { }
