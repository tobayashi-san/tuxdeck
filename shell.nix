{ pkgs ? import <nixpkgs> { } }:
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ cargo rustc rustfmt clippy pkg-config gnumake nodejs_22 ];
  buildInputs = with pkgs; [ udev ];
  # Loaded at runtime (keymap.rs); the tests use it when it is there.
  LD_LIBRARY_PATH = "${pkgs.libxkbcommon}/lib";
}
