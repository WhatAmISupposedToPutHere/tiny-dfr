{
  description = "The most basic dynamic function row daemon possible";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-23.11";
  };
  outputs = { self, nixpkgs }:
    let
      pkgs = import nixpkgs { system = "x86_64-linux"; };
    in
    rec {
      packages.x86_64-linux.default = pkgs.rustPlatform.buildRustPackage {
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
      devShells.x86_64-linux.default = pkgs.mkShell {
        inputsFrom = [
          packages.x86_64-linux.default
        ];
        packages = [
	        pkgs.rustfmt
          pkgs.rust-analyzer
        ];
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };
    };
}
