# Changelog

## Unreleased

- **Security** (findings of a full review):
  - Deleting or replacing a folder key of an imported profile could delete a folder
    outside the profile; page ids are now always checked.
  - Property inspectors, HTML plugins and icon pack files run on their own port without
    the API (sandboxed iframe, small bridge script); on the UI port these files can't run
    as pages. Plugin code can no longer use the UI's session.
  - `tuxdeck ui` and the app menu pass a one-time login code (30 s) to the browser
    instead of the API token; the browser keeps its own session cookie. Open UI windows
    need to be opened once more from the menu after the update. Settings that HTML
    plugins or inspectors kept in browser storage (not the usual plugin settings) start
    empty once, because the storage belongs to the old address.
  - The plugin SDK accepts plugins only with the secret they are started with
    (`-pluginUUID`, like the official app) and property inspectors only with an alias
    from the UI; other local programs can't pose as a plugin or read its settings.
  - Data and configuration folders are only accessible to the user (0700).
  - The Linux check sandbox has no network and sees only system folders; without
    bubblewrap the check doesn't run. Results are stored under the checked plugin.
  - `openUrl` from plugins can't open TuxDeck's own pages; `import-elgato` unpacks into a
    private temporary folder; plugin files are never installed group/world-writable;
    Hyprland sockets of other users are ignored; `POST /api/settings` changes nothing when
    a part is invalid.
- Programs started from keys get their own systemd scope and keep running when the
  service restarts.
- Hyprland 0.54 and older: hotkeys and text work again (fallback to the old syntax).
- `tuxdeck list`/`probe` show only the end of the serial number.
- The handbook explains autostart on desktops without `graphical-session.target`
  (Hyprland without uwsm, Sway, i3) and which network connections TuxDeck makes.
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
