{
  description = "vynl — Cross-platform Rust TUI Spotify client with native terminal cover art";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        linuxBuildDeps = pkgs.lib.optionals pkgs.stdenv.isLinux [
          pkgs.pkg-config
          pkgs.alsa-lib
        ];
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "vynl";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.pkg-config ];
          buildInputs = linuxBuildDeps;
          meta = with pkgs.lib; {
            description = "Cross-platform Rust TUI Spotify client with native terminal cover art";
            homepage = "https://github.com/OWNER/vynl";
            license = licenses.gpl3Plus;
            mainProgram = "vynl";
          };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer
            just
            cargo-nextest
            cargo-deny
            cargo-insta
            cargo-hack
            cargo-zigbuild
            zig
            mdbook
            vhs
          ] ++ linuxBuildDeps;
        };
      });
}
