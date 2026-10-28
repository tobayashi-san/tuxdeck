# TuxDeck

**Linux software for Elgato Stream Deck devices.** Set up your keys, run plugins made for
the Stream Deck SDK and bring over your existing profiles – no Windows, no Mac, no VM.

![The UI: Stream Deck Neo keys, action list and editor](docs/images/editor.png)

- **Familiar:** actions, folders, pages and profiles like in the official app, configured
  by drag & drop in a browser-based UI.
- **Stream Deck plugins:** Node.js and HTML plugins made for the Stream Deck SDK run
  unmodified, including their settings pages.
- **Plugin catalogue built in:** browse and install open-source plugins straight from their
  authors' releases. Plugins downloaded elsewhere (for example from the Elgato Marketplace
  website) are installed from the file.
- **Compatible:** import and export profiles (`.streamDeckProfile`) of the Windows or Mac
  app, and restore its backups (`.streamDeckProfilesBackup`).
- **Tested for Linux:** every plugin can be checked automatically in an isolated test
  instance; the result shows up as a badge ("Tested on Linux"), and results are shared in
  [tuxdeck-plugin-checks](https://github.com/tobayashi-san/tuxdeck-plugin-checks).
- **Backups:** download and restore your whole setup; a backup is also made every day.
- **Smart Profiles:** the profile switches automatically with the application in focus.
- **Any distribution:** Wayland and X11, a single binary written in Rust, starts with your
  session.
- **Your language:** English and German, following your system language (switchable in the
  settings). More translations are welcome.

> TuxDeck is an independent project. It is not affiliated with, endorsed or sponsored by
> Elgato or Corsair. "Elgato" and "Stream Deck" are trademarks of Corsair Memory, Inc.; they
> are used here only to name the hardware TuxDeck works with.

## Supported devices

| Device | Status |
|---|---|
| Stream Deck Neo (keys, touch points, info bar) | tested on real hardware |
| Stream Deck MK.2, Stream Deck (2nd gen), XL | implemented, same protocol as the Neo |
| Stream Deck + (dials, touch strip) | implemented, tested with a simulated device |
| Stream Deck Mini, Mini MK.2, Original (1st gen) | implemented, tested with a simulated device |
| Stream Deck Pedal | implemented |

Do you own one of the devices that have not been tested on real hardware yet? Feedback is
very welcome; `tuxdeck probe` shows test images on all keys and prints every key press.

## Installation

### Download a ready-made package (recommended)

Download the file for your distribution from
[Releases](https://github.com/tobayashi-san/tuxdeck/releases/latest) and install it:

| Distribution | File | Install |
|---|---|---|
| Debian, Ubuntu, Mint, Pop!_OS | `tuxdeck_<version>_amd64.deb` | `sudo apt install ./tuxdeck_*_amd64.deb` |
| Fedora | `tuxdeck-<version>-1.x86_64.rpm` | `sudo dnf install ./tuxdeck-*.x86_64.rpm` |
| openSUSE | `tuxdeck-<version>-1.x86_64.rpm` | `sudo zypper install --allow-unsigned-rpm ./tuxdeck-*.x86_64.rpm` |
| everything else (Arch, …) | `tuxdeck-<version>-x86_64.tar.gz` | extract, then run `./install.sh` in the extracted folder |

Then start it with your session:

```sh
systemctl --user enable --now tuxdeck
```

The packages include the udev rule for device access; if the Stream Deck is not detected
right away, unplug it and plug it back in. Requirements: a 64-bit PC (x86_64) with a
distribution from about 2022 or newer (glibc ≥ 2.35: Ubuntu 22.04, Debian 12, Fedora 36 and
later). Uninstall with your package manager or `./install.sh --uninstall`.

### Build from source

For other architectures (e.g. Raspberry Pi) or older distributions. You need Rust ≥ 1.85, a
C compiler, `make`, `pkg-config` and the libudev headers:

| Distribution | Command |
|---|---|
| Debian 13, Raspberry Pi OS | `sudo apt install build-essential pkg-config libudev-dev rustc cargo` |
| Ubuntu 24.04, Mint, Pop!_OS | `sudo apt install build-essential pkg-config libudev-dev curl`<br>plus Rust via [rustup](https://rustup.rs) (Ubuntu only ships Rust 1.75) |
| Fedora | `sudo dnf install gcc make pkgconf-pkg-config systemd-devel rust cargo` |
| Arch, Manjaro, EndeavourOS | `sudo pacman -S --needed base-devel rust` |
| openSUSE | `sudo zypper install gcc make pkg-config libudev-devel rust cargo` |
| NixOS | `nix-build` (see [Advanced](#advanced)) |

Then:

```sh
git clone https://github.com/tobayashi-san/tuxdeck.git
cd tuxdeck
make                                    # build (takes a few minutes the first time)
make install-user                       # install to ~/.local, no root needed
sudo make install-udev                  # allow access to the device (once)
systemctl --user enable --now tuxdeck   # start with your session
```

Unplug the Stream Deck and plug it back in – done. Open the UI from **"TuxDeck" in your
application menu** or with `tuxdeck ui` (it runs at <http://127.0.0.1:47800>).

## Getting started

1. **Assign keys:** drag an action from the list on the right onto a key. Its settings
   appear below, together with title, image and icon.
2. **Pages and folders:** "+ Page" below the device or the "Create Folder" action give you
   more room.
3. **Plugins:** top right. The catalogue lists open-source plugins; "Install" downloads
   the newest release from the author's repository. Plugin files you downloaded yourself
   (`.streamDeckPlugin`) are installed with "Install from file…" or by dropping them onto
   the window.
4. **Bring over existing profiles:** export them in the Elgato app (`.streamDeckProfile`)
   and import them here via the profile menu (`⋯`).

### Plugins from the Elgato Marketplace

TuxDeck does not sign in to the Elgato Marketplace. You can still use many of its plugins:
sign in on [marketplace.elgato.com](https://marketplace.elgato.com), click "Get", download
the file in your account under "Products" and install it with "Install from file…". The
plugins view explains the steps as well ("Elgato Marketplace…").

Plugins protected with DRM (an encrypted manifest) only run in the official app and cannot
be installed. Plugins that only contain a macOS or Windows program do not run on Linux.

## What works, what doesn't

| | |
|---|---|
| ✅ Built-in actions | Hotkey, Text, Open, Open/Close application, Website, Multimedia, Folders, Pages, Switch profile, Brightness, Sleep, Multi actions, Key Logic (press/double press/hold), Soundboard, Timer |
| ✅ Neo info bar | clock and page indicator |
| ✅ Plugins | Node.js and HTML plugins from the catalogue or as `.streamDeckPlugin` |
| ✅ Icon packs | as `.streamDeckIconPack`, picked in the image editor via "Icon…" |
| ✅ Smart Profiles | on Hyprland and X11 |
| ✅ Linux check | badge per plugin: starts, actions, keys/dials, settings page and log tested in an isolated instance (automatic after installing, or "Test on Linux") |
| ✅ Backups | Settings → Backups: download/restore (also the official app's backups), daily automatic backups |
| ❌ Mac/Windows-only plugins | plugins that ship a Mac or Windows program do not run; the UI marks them |
| ❌ DRM-protected plugins | plugins with an encrypted manifest only run in the official app |
| ❌ Not implemented yet | Virtual Stream Deck, dial stacks |

## Troubleshooting

<details>
<summary><b>The device is not detected</b></summary>

Run `sudo make install-udev` (not needed with the .deb/.rpm packages) and plug the Stream
Deck back in. `tuxdeck list` shows what is found. If the official app runs in a VM with
USB passthrough, the device belongs to that VM.
</details>

<details>
<summary><b>Hotkeys or text do not arrive</b></summary>

The most reliable way is the virtual keyboard `/dev/uinput`. Its udev rule is **optional
and not installed by default**, because it lets every program you run inject keystrokes
into other applications. Enable it only if you need it:

| Installed with | Enable `/dev/uinput` |
|---|---|
| `./install.sh` | `./install.sh --with-uinput` |
| `make` | `sudo make install-uinput` |
| `.deb` / `.rpm` | `sudo cp /usr/share/tuxdeck/udev/71-tuxdeck-uinput.rules /etc/udev/rules.d/`<br>`sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=misc --sysname-match=uinput` |
| NixOS | add the uinput line from `packaging/70-tuxdeck.rules` to `services.udev.extraRules` |

Then restart the app (`systemctl --user restart tuxdeck`). To disable it again, delete
`71-tuxdeck-uinput.rules` from `/etc/udev/rules.d/` (or `sudo make uninstall-uinput`)
and reboot.

Without uinput the program falls back to Hyprland, `wtype` (Sway and other wlroots
desktops) or `xdotool` (X11). GNOME and KDE on Wayland usually need uinput. Text
additionally needs `wl-copy` (`wl-clipboard`) or `xclip`. The settings (gear icon) show
which method is active.
</details>

<details>
<summary><b>A key shows a yellow warning sign</b></summary>

The action failed, usually because a helper program is missing. The message appears in the
UI and in the log (`journalctl --user -u tuxdeck -f`). Required programs are listed
under [Advanced → Helper programs](#advanced). Profiles from a Mac often point to paths
like `/Applications/…`; you need to adjust those.
</details>

<details>
<summary><b>A plugin does not start</b></summary>

Node.js plugins need `node` (≥ 20), HTML plugins a Chromium-based browser (Chromium,
Chrome, Brave, Edge or Vivaldi). Plugins with Mac or Windows programs do not run on Linux.
Plugin logs are in `~/.local/share/tuxdeck/logs/`.
</details>

<details>
<summary><b>It does not start with the session</b></summary>

Check `systemctl --user status tuxdeck`. Some compositors (Hyprland, Sway) do not pass
their environment to systemd on their own; add
`dbus-update-activation-environment --systemd --all` to the compositor's autostart.
Alternatively, without systemd: use `tuxdeck run --no-browser` as an autostart command.
</details>

## Advanced

<details>
<summary><b>Other ways to install, uninstalling</b></summary>

```sh
make && sudo make install          # system-wide to /usr/local
make install PREFIX=/usr DESTDIR=… UDEVRULESDIR=/usr/lib/udev/rules.d   # for packagers
make uninstall-user                # or: sudo make uninstall
```

`./install.sh --uninstall` and `make uninstall-user` stop and disable the service and
also remove the `streamdeck://` link handler the app registers
(`tuxdeck-url-handler.desktop` and its line in `mimeapps.list`); `sudo make
uninstall` removes the system-wide files only, so each user should run `make
uninstall-user` (or `./install.sh --uninstall --no-udev`) as well. Profiles and settings
are kept when uninstalling. `XDG_DATA_HOME` and `XDG_CONFIG_HOME` are respected.

**NixOS:** `nix-build` (see `default.nix`) builds a package with the program, udev rule,
desktop file and user service. Add the udev rule to your system configuration via
`services.udev.packages` or the snippet in `packaging/70-tuxdeck.rules` (the
`/dev/uinput` line there is optional, see above). For development: `nix-shell`.
</details>

<details>
<summary><b>Helper programs</b></summary>

All optional; if one is missing, only the affected key reports an error.

| Purpose | Programs (Debian/Ubuntu package in parentheses) |
|---|---|
| Hotkeys, text | `/dev/uinput`, otherwise Hyprland, `wtype` or `xdotool` |
| Pasting text | `wl-copy` (`wl-clipboard`) or `xclip` |
| Multimedia | `playerctl`, `wpctl` (`wireplumber`) or `pactl` (`pulseaudio-utils`) |
| Soundboard, timer alarm | `pw-play` (`pipewire-bin`), `ffplay`, `mpv` or `paplay` |
| Opening websites and files | `xdg-open` (`xdg-utils`) |
| Node.js plugins | `node` (Node.js ≥ 20) |
| HTML plugins | Chromium, Chrome, Brave, Edge or Vivaldi |
| Linux check of plugins | `node`; `bwrap` (`bubblewrap`) isolates the plugin under test from your files |
| Smart Profiles on X11 | `xprop` (`x11-utils`) |
| Notifications | `notify-send` (`libnotify-bin`) |
</details>

<details>
<summary><b>Commands</b></summary>

```text
tuxdeck run [--no-browser]      start the daemon and UI (default)
tuxdeck ui                      open the UI (starts the daemon if needed)
tuxdeck list                    list connected devices
tuxdeck probe [sec]             hardware test: fill keys, print input
tuxdeck import-profile <file.streamDeckProfile>
tuxdeck install-plugin <file.streamDeckPlugin>
tuxdeck install-icon-pack <file.streamDeckIconPack>
tuxdeck import-elgato <pkg|app> [--default-profile 20GBJ9901]
tuxdeck udev                    print the udev rules
```

`import-elgato` is optional: if you have the Mac installer of the official app
(`Stream_Deck_*.pkg`) or its `Stream Deck.app`, it copies the icons, info bar designs and
default profiles from it into your data folder, so imported profiles look like in the
original. These files are not part of TuxDeck. Requires `bsdtar` (Debian/Ubuntu package
`libarchive-tools`).
</details>

<details>
<summary><b>Data, settings and environment variables</b></summary>

| Path | Contents |
|---|---|
| `~/.local/share/tuxdeck/ProfilesV2/` | profiles (Elgato format) |
| `~/.local/share/tuxdeck/Plugins/` | installed plugins (`*.sdPlugin`) |
| `~/.local/share/tuxdeck/IconPacks/` | installed icon packs (`*.sdIconPack`) |
| `~/.local/share/tuxdeck/elgato/` | resources imported with `import-elgato` |
| `~/.local/share/tuxdeck/logs/` | plugin logs |
| `~/.local/share/tuxdeck/plugin-data/` | data the plugins store (logins, caches) |
| `~/.local/share/tuxdeck/Backups/` | automatic backups (daily, before restores) |
| `~/.local/share/tuxdeck/linux-checks.json` | results of the Linux checks |
| `~/.config/tuxdeck/api-token` | secret for the local API (mode 0600) |
| `~/.config/tuxdeck/settings.json` | settings, brightness, profile assignment, global plugin settings |

Versions before 0.5 used the folders `streamdeck-linux`; they are moved on the first start.

Environment variables: `TUXDECK_DATA_DIR`, `TUXDECK_CONFIG_DIR`, `TUXDECK_NODE`
(Node.js for plugins), `TUXDECK_BROWSER` (Chromium for HTML plugins), `GITHUB_TOKEN`
(optional, for more than 60 catalogue downloads per hour), `RUST_LOG`. For testing:
`TUXDECK_VIRTUAL_DEVICE`, `TUXDECK_DRY_RUN`, `TUXDECK_NO_URL_HANDLER`,
`TUXDECK_NO_WINDOW_WATCH`.
</details>

<details>
<summary><b>Smart Profiles in detail</b></summary>

Link a profile to an application in the profile menu under "Smart Profile (per
application)…". While a window of that application has focus, the device shows the
profile, afterwards the previously selected one again. Matching uses the window class (Hyprland:
`hyprctl clients`, X11: `WM_CLASS`); separate several applications with `;`. Imported Mac
profiles with `/Applications/Firefox.app` also match `firefox`. If you pick another profile
yourself, that choice sticks. GNOME and KDE on Wayland offer no general way to find the
active window; there the feature stays off.
</details>

<details>
<summary><b>Plugins in detail</b></summary>

- Node.js plugins (SDK v2) run with the system `node` and the same arguments as with the
  official app. By default they are told the platform is `mac` so they use POSIX paths
  (configurable in the settings).
- HTML plugins (SDK v1) run in a background Chromium.
- A plugin can ship its own Linux program via the additional manifest field `CodePathLin`.
- The catalogue is the open-source plugin registry of the
  [OpenAction](https://github.com/OpenActionAPI/plugins) project, which OpenDeck uses as
  well. Packages are downloaded from the newest release of each plugin's GitHub
  repository; TuxDeck does not host any plugins.
- Plugins use `streamdeck://` links to finish their own sign-ins (OAuth). The handler is
  registered at startup. If `mimeapps.list` is read-only (home-manager), make it permanent
  with
  `xdg.mimeApps.defaultApplications."x-scheme-handler/streamdeck" = "tuxdeck-url-handler.desktop";`
- Imported Mac profiles: ⌘ in hotkeys is sent as **Ctrl** by default (setting
  "Treat ⌘ (Mac profiles) as Ctrl").
</details>

<details>
<summary><b>Security</b></summary>

The UI, API and plugin interface listen on `127.0.0.1` only. Requests with a foreign `Host`
or `Origin` header (DNS rebinding, other websites in your browser) are rejected, because
the API can install plugins and thus run code. The API additionally requires a secret
token that only your user can read (`~/.config/tuxdeck/api-token`), so other users and
programs on the same machine can't use it; the app menu entry and `tuxdeck ui` open the
UI with it. Plugins run with your user's rights, like with the official app: install only
plugins you trust.
</details>

## Contributing

Bug reports, ideas and especially feedback on untested devices are welcome as
[issues](https://github.com/tobayashi-san/tuxdeck/issues). Building, testing and the code
layout are described in [CONTRIBUTING.md](CONTRIBUTING.md), the HTTP API in
[docs/API.md](docs/API.md). Please do not report security issues publicly; see
[SECURITY.md](SECURITY.md).

## Acknowledgements

- The plugin catalogue comes from the [OpenAction plugin registry](https://github.com/OpenActionAPI/plugins).
- The protocol of the older devices follows the documentation of
  [python-elgato-streamdeck](https://github.com/abcminiuser/python-elgato-streamdeck).
- Plugins talk to TuxDeck through the open
  [Stream Deck SDK](https://github.com/elgatosf/streamdeck) protocol (MIT).

## License

[MIT](LICENSE). The bundled DejaVu Sans font has its own license
(`assets/fonts/LICENSE-DejaVu.txt`); the licenses of the Rust libraries compiled into the
program are listed in [THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md).
