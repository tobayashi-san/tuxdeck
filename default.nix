{ pkgs ? import <nixpkgs> { } }:
pkgs.rustPlatform.buildRustPackage {
  pname = "tuxdeck";
  version = "0.6.0-beta.1";
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
    install -Dm644 assets/icon/tuxdeck.svg $out/share/icons/hicolor/scalable/apps/tuxdeck.svg
    for s in 16 24 32 48 64 128 256; do install -Dm644 assets/icon/tuxdeck-$s.png $out/share/icons/hicolor/"$s"x"$s"/apps/tuxdeck.png; done
    install -Dm644 packaging/tuxdeck.service $out/lib/systemd/user/tuxdeck.service
    substituteInPlace $out/lib/systemd/user/tuxdeck.service --replace-fail '%h/.local/bin/tuxdeck' "$out/bin/tuxdeck"
  '';
  meta = {
    description = "Linux software for Elgato Stream Deck devices";
    mainProgram = "tuxdeck";
    platforms = pkgs.lib.platforms.linux;
  };
}
