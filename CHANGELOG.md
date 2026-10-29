# Changelog

## Unreleased

- **Convert profiles from Windows and macOS:** after importing one, TuxDeck offers to
  turn it into a regular Linux profile (a backup is made first). Open keys get the Linux
  program (desktop entries incl. Flatpak, your default browser/editor), the web version
  or the folder; actions of missing plugins become built-in actions, catalogue plugins
  (OBS Tools, Discord, Linux Media, System Information, …) or keyboard shortcuts (Teams).
  Keys without a counterpart keep title and icon and wait for a command of your own.
- **Sample profiles** (profile menu → "Add a sample profile…"): Stream (OBS Tools),
  Work and Play (Discord, music, game launchers), with our own icons; programs are taken
  from the computer, plugins installed from the catalogue.
- Plugin archives packed on Windows (`\` in paths) install now.
- Fixes: two backups within the same second (e.g. converting a profile twice) no longer
  overwrite each other; `tuxdeck import-profile` gives the copy new action ids and leaves
  nothing behind on errors, like importing in the UI; `tuxdeck list`/`probe` leave real
  devices alone in test instances (`TUXDECK_VIRTUAL_DEVICE`); new profile names lose
  control characters like renamed ones; the API answers 404 for unknown pages, plugins,
  profiles to convert, sample profiles and API routes, validates key positions of
  `…/press`, and no longer logs request errors as "Installation failed".

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
