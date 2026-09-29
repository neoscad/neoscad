{
  # `nix run github:neoscad/neoscad -- model.scad -o model.stl`, or
  # `nix profile install github:neoscad/neoscad`: the `neoscad` CLI built
  # from source with the toolchain rust-toolchain.toml pins (through
  # rust-overlay, since nixpkgs' rustc can lag the 1.98 this needs).
  #
  # Not built in CI yet (docs/packaging.md). The tests are not run in the
  # build: they need the OpenSCAD reference checkout and a GPU driver.
  description = "NeoSCAD: OpenSCAD-compatible programmable solid CAD";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      }));
    in
    {
      packages = forAll (pkgs:
        let
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform { cargo = toolchain; rustc = toolchain; };
          manifest = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package;
          # wgpu loads the Vulkan loader and EGL at run time (dlopen), so
          # they go on the binary's search path rather than being linked.
          gpuLibs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.vulkan-loader pkgs.libGL ];
        in
        rec {
          neoscad = rustPlatform.buildRustPackage {
            pname = "neoscad";
            inherit (manifest) version;
            src = pkgs.lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" "neoscad-cli" ];
            doCheck = false;
            nativeBuildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.makeWrapper ];
            postInstall = ''
              install -Dm644 -t $out/share/doc/neoscad LICENSE NOTICE
              install -Dm644 -t $out/share/doc/neoscad/licenses packaging/licenses/*
            '' + pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
              wrapProgram $out/bin/neoscad \
                --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath gpuLibs}
            '';
            meta = {
              description = "OpenSCAD-compatible programmable solid CAD";
              homepage = "https://neoscad.org";
              license = pkgs.lib.licenses.gpl2Plus;
              mainProgram = "neoscad";
            };
          };
          default = neoscad;
        });
    };
}
