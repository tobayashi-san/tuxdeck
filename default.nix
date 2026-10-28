{ pkgs ? import <nixpkgs> { } }:
pkgs.rustPlatform.buildRustPackage {
  pname = "tuxdeck";
  version = "0.5.0";
  src = pkgs.lib.cleanSourceWith {
    src = ./.;
    filter = path: type:
      let base = baseNameOf path; in
      !(builtins.elem base [ "target" "ui-mock" "result" ]) && pkgs.lib.cleanSourceFilter path type;
  };
  cargoLock.lockFile = ./Cargo.lock;
  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = [ pkgs.udev ];
  postInstall = ''
    install -Dm644 packaging/70-tuxdeck.rules $out/lib/udev/rules.d/70-tuxdeck.rules
    install -Dm644 packaging/tuxdeck.desktop $out/share/applications/tuxdeck.desktop
    install -Dm644 packaging/tuxdeck.service $out/lib/systemd/user/tuxdeck.service
    substituteInPlace $out/lib/systemd/user/tuxdeck.service --replace-fail '%h/.local/bin/tuxdeck' "$out/bin/tuxdeck"
  '';
  meta = {
    description = "Linux software for Elgato Stream Deck devices";
    mainProgram = "tuxdeck";
    platforms = pkgs.lib.platforms.linux;
  };
}
