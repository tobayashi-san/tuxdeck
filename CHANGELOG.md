# Changelog

## 0.6.0-beta.1 – first public beta

The first release for everyone. TuxDeck configures Elgato Stream Deck devices on Linux,
runs plugins written for the Stream Deck SDK and imports profiles and backups from the
official app.

- **Devices:** Stream Deck Neo (keys, touch points, info bar) and MK.2 tested on real
  hardware; XL, Stream Deck +, Mini, Pedal and the first-generation models implemented and
  tested against simulated devices.
- **Starter profiles per device:** every deck starts with a media remote laid out for
  its size; 15- and 32-key decks add home folder, lock screen, timer, sleep and brightness.
- **Brightness:** a new "Cycle" option steps through 10 / 40 / 70 / 100 % with one key.
- **Plugins:** Node.js and HTML plugins run unmodified; open-source plugins install from
  the built-in catalogue (OpenAction registry), others from the file. Each plugin can be
  test-run in a sandbox ("Tested on Linux" badge).
- **Compatibility:** import/export of `.streamDeckProfile`, restore of the official app's
  backups, daily backups of your own setup.
- **Desktop:** Wayland and X11; Smart Profiles on Hyprland and X11; English and German.
- **Packages:** `.deb`, `.rpm` and a `.tar.gz` with an installer.
