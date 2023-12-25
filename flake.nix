{
  description = "The most basic dynamic function row daemon possible";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-23.11";
  };
  outputs = { self, nixpkgs }:
    let
      pkgs = import nixpkgs { system = "aarch64-linux"; };
    in
    rec {
      packages.aarch64-linux.default = pkgs.rustPlatform.buildRustPackage {
        name = "tiny-dfr";
        version = "0.2.0";
        src = ./.;
        cargoLock = {
          lockFile = ./Cargo.lock;
        };
        nativeBuildInputs = [
          pkgs.pkg-config
        ];
        buildInputs = [
          pkgs.cairo
          pkgs.libinput
          pkgs.freetype
          pkgs.fontconfig
          pkgs.glib
          pkgs.pango
          pkgs.gdk-pixbuf
          pkgs.libxml2
        ];
      };
      devShells.aarch64-linux.default = pkgs.mkShell {
        inputsFrom = [
          packages.aarch64-linux.default
        ];
        packages = [
	  pkgs.rustfmt
          pkgs.rust-analyzer
        ];
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };
    };
}
