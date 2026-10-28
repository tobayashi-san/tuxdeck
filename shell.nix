{ pkgs ? import <nixpkgs> { } }:
pkgs.mkShell {
  nativeBuildInputs = with pkgs; [ cargo rustc rustfmt clippy pkg-config gnumake nodejs_22 ];
  buildInputs = with pkgs; [ udev ];
}
