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
- **Converting profiles from Windows/macOS:**
  - Only imported profiles (and keys with Windows/macOS paths or Elgato's own plugins) are
    offered the conversion; profiles made here never see it. Profiles imported before this
    version are checked once at the first start.
  - Without the plugin catalogue, or when a plugin can't be installed, the affected keys
    stay as they are and the conversion is offered again later.
  - A program on the Windows desktop or in Downloads is converted as the program, not as
    that folder; similar program names count only as whole words, and sample profiles use
    known names only ("Steam" no longer opens Steam Link).
  - Keys without a counterpart get the action's name as title and a note in the editor.
  - Smart profiles of imported profiles also work for OBS, Chrome, Edge and Teams.
- **Profiles:**
  - A profile for a model that isn't connected says so when it is imported.
  - Profiles a plugin brings along are installed once; deleted ones stay deleted.
  - The profile a deck shows can be deleted when another fitting profile exists (MK.2 and
    Original share theirs).
  - Sample profiles fit the deck: everything on one page on big decks, page keys on the
    Mini, three keys of their own on the Pedal.
  - Ptyxis and WezTerm are recognised as terminals.
- **Devices and several decks:**
  - Keys and dials held while a deck disconnects are released (no stuck modifiers, plugins
    get their keyUp) without running the key's action; plugins get willAppear only for
    connected decks, and again when a deck comes back.
  - Switch Profile can switch another deck (new "Deck" field; the device of profiles from
    the official app is recognised by its serial number).
  - A smart profile can be meant for one deck only; profiles from the official app keep
    their deck.
  - A second deck of the same model gets its own profile; decks with the same name are
    told apart by the end of their serial number.
  - After a system suspend the decks get their brightness again.
  - When several decks show the same page, the deck selected in the UI gets the test
    press, the property inspector and the preview; a settings change reaches the plugin
    for every deck.
  - `tuxdeck probe` refuses to run while the service uses the deck and works on the Pedal;
    `tuxdeck list` writes nothing to the decks and also lists Elgato devices TuxDeck
    doesn't support.
- **Installation:**
  - `make install-user` builds first; `sudo make install` refuses an outdated binary.
  - When another TuxDeck holds the port, the service stops instead of restarting every
    3 s; `tuxdeck ui` starts the installed service instead of a daemon of its own.
  - Enabling `/dev/uinput` also loads the module (and at every boot), so no reboot is needed.
  - `install.sh` no longer creates an icon cache in your home folder; all install routes
    (Makefile, install.sh, Nix) install the licence notices; the .rpm requires glibc 2.34.
  - NixOS: the handbook shows the `configuration.nix` lines for program, udev rule and
    service; the Nix package installs its documentation.
  - CI tests `install.sh` (install and uninstall) and builds the .deb and .rpm.
- **Robustness:** a page whose file can't be read stays in the profile (shown empty, the
  file is kept as `manifest.json.broken-…` before it is overwritten); files are flushed to
  disk before they replace the old ones; the `streamdeck://` registration never rewrites a
  `mimeapps.list` it can't read, a link or a read-only file, and writes it atomically.
- **Interface:**
  - Dates follow the chosen language; German wording is consistent ("Infoleiste",
    "Zustand", "Linux-Prüfung"); the "Amber" colour scheme is translated.
  - One failure shows one message; plugin names instead of ids in messages; icon pack
    import shows the pack's name and survives a cancelled file chooser.
  - Backups list shows date, kind and size in full; "Download" looks like a button.
  - Keyboard: arrow keys in "Add action…" no longer add an action (Enter does), focus
    stays in place when the settings form is rebuilt, and closing the plugins view returns
    focus to where it was.
  - Dragging the custom key colour no longer sends a request per step; edits of a key are
    saved in order.
  - When the first load fails because the service was restarting, the UI loads again as
    soon as it is back.
  - The Smart Profile dialog says when the desktop can't tell which window is active
    (GNOME/KDE Wayland) and lists X11 windows properly.
- **Icons:** TuxDeck now uses Elgato's open icon set (`@elgato/icons`, MIT): the icons of
  the built-in actions, the starter and sample profile keys, the bundled icon pack
  ("Elgato Icons", 866 icons, outline and filled; the set's brand logos are left out) and
  the icon font of the configuration UI. Keys that still show an icon of the earlier
  starter or sample profiles get the new one once at the first start (a backup "Before the
  new icons" is made first).
- **Gradient key backgrounds:** besides colours, keys can have a linear or radial gradient:
  ready-made ones, an editor (type, angle, up to six colours) and CSS pasted from
  cssgradient.io (`linear-gradient(…)`, `radial-gradient(…)`).
- **Tests:** plugins built on Elgato's own `@elgato/streamdeck` library (1.x, 2.x, 3.x)
  and a property inspector made of `sdpi-components` run against TuxDeck in the test
  suites.
- **API and documentation:** every API error is JSON; API.md documents all routes; the
  README's images and links also work in the packages; it names the supported models;
  `TUXDECK_VIRTUAL_DEVICE` accepts model numbers (Original v2 is simulated too) and
  rejects unknown values.
- **Keyboard input:**
  - Hotkeys are recorded and shown with your keyboard layout (QWERTZ Ctrl+Z is "Ctrl + Z",
    not "Ctrl + Y"), and tools that send characters (wtype, xdotool) get the right one.
  - "Type instead of paste" types over uinput with your keyboard layout, also on GNOME
    and KDE (Wayland). Where nothing can type, the key reports it instead of quietly using
    the clipboard; characters the layout lacks are reported too.
  - wtype is no longer chosen on GNOME/KDE Wayland, xdotool only in X11 sessions, xclip
    only with an X server.
  - When the clipboard tool fails, nothing is pasted (it used to paste what was on the
    clipboard before).
  - On Hyprland without uinput, the hotkey editor and the handbook say that Hyprland's own
    key bindings don't react.
  - Ptyxis, Yakuake, Guake and Tilda paste with Ctrl+Shift+V like other terminals.
  - Input error messages are translated.
- **Plugins and SDK:**
  - Every installation unpacks into a folder of its own and replaces the old version only
    when complete; installs running at the same time no longer break each other. Plugin
    folders spelled `.sdplugin` are accepted.
  - Node.js is checked before a plugin starts; a missing or too old one is reported with
    what to do (handbook: "Node.js"). The .deb suggests Node.js 20 or newer.
  - SDK: `applicationDidLaunch`/`applicationDidTerminate` for monitored applications,
    answers to `getSecrets`, `getResources`/`setResources`, request ids in settings
    answers; a property inspector opened while the deck is unplugged works.
  - Backups keep file modes: restored plugin programs stay executable.
  - `tuxdeck import-profile` and `install-plugin` hand the file to the running service,
    which shows it at once.
  - Catalogue: certificates installed on the system are trusted; slow downloads are no
    longer cut off after two minutes; the size limit applies while downloading; a removed
    GitHub repository and a refused token are reported clearly (a bad token is dropped).
  - Plugin logs are kept small while plugins run.
- **Images and editor:**
  - Pictures inside SVG images (plugin images, uploads, icons) show again.
  - WebP and BMP images are stored as PNG and no longer show as a black key; animated GIFs
    stay animated when picked in the editor, whatever their size.
  - Plugin images are decoded and scaled without blocking the rest of TuxDeck, and the
    touch strip no longer decodes its images on every refresh.
  - An imported info bar layout with the name of a built-in one is listed as its own
    entry ("(Elgato)"), so the colour schemes offered are the ones that are drawn.
  - Fonts of info bar layouts imported while the service runs are used without a restart.
  - Multi Actions and Key Logic offer only built-in actions (a nested plugin action could
    not be set up).
  - A plugin updating a key while you drag one no longer leaves the editor in drag mode.
  - Pedal: no image, title or brightness controls, and no info bar actions.
- **Actions:**
  - Open Application's long press quits exactly the program the key started (its scope or
    process group). Otherwise it goes by the program's name, but never by an interpreter
    or launcher name (`python3`, `sh`, …); Flatpaks are quit with `flatpak kill`.
  - Keys show the warning sign when their program fails right away (command not found,
    no application for a link) and when an entry of a Multi Action or Key Logic fails.
  - The Open key accepts a quoted program path followed by arguments.
  - `.desktop` entries also start without `gtk-launch` (`gio launch` or the entry's command).
  - Stopped soundboard sounds no longer leave zombie processes behind.
  - Web links in the UI (Marketplace, plugin sources) open in your default browser.
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
