# TuxDeck handbook

Everything that didn't fit on the front page: building from source, the details behind
each feature, troubleshooting, and notes for packagers and contributors. New here? Start
with the [README](../README.md).

- [Build from source](#build-from-source)
- [Plugins from the Elgato Marketplace](#plugins-from-the-elgato-marketplace)
- [What works, what doesn't](#what-works-what-doesnt)
- [Troubleshooting](#troubleshooting)
- [Advanced](#advanced)
- [For contributors and maintainers](#for-contributors-and-maintainers)

## Build from source

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

## Plugins from the Elgato Marketplace

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
| ✅ Neo info bar | clock, page indicator and your own text (placeholders, colours, gradients) |
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
| `.deb` / `.rpm` | `sudo cp /usr/share/tuxdeck/udev/71-tuxdeck-uinput.rules /etc/udev/rules.d/`<br>`echo uinput \| sudo tee /etc/modules-load.d/tuxdeck-uinput.conf && sudo modprobe uinput`<br>`sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=misc --sysname-match=uinput` |
| NixOS | `hardware.uinput.enable = true;` and the uinput line from `packaging/71-tuxdeck-uinput.rules` in `services.udev.extraRules` |

Then restart the app (`systemctl --user restart tuxdeck`); if `/dev/uinput` still can't
be used, reboot once. To disable it again, delete `71-tuxdeck-uinput.rules` from
`/etc/udev/rules.d/` (or `sudo make uninstall-uinput`) and reboot.

Without uinput the program falls back to Hyprland, `wtype` (Sway and other wlroots
desktops) or `xdotool` (X11 sessions). GNOME and KDE on Wayland need uinput. On Hyprland
without uinput the keys only reach the focused window; a combination that is one of
Hyprland's own key bindings (workspaces, launchers, a screenshot bind) runs that binding
directly. Global shortcuts of other programs (e.g. push-to-talk) need uinput. Pasting text additionally needs
`wl-copy` (`wl-clipboard`) or `xclip`. The settings (gear icon) show which method is
active.

**Recording a combination with Super:** the desktop keeps many Super combinations to
itself, so the recorder only sees "Super". Switch on **Super** (or Ctrl, Alt, Shift)
below the combination and record just the other key; the switches also change a
combination recorded before.

"Type instead of paste" types over uinput with your keyboard layout (read from Hyprland,
GNOME, KDE or X11; `libxkbcommon` must be installed). Characters the layout has no key
for are reported instead of typed; set the layout yourself with
`TUXDECK_XKB_LAYOUT=de` (and `TUXDECK_XKB_VARIANT=nodeadkeys`) if it is not detected.
</details>

<details>
<summary><b>A key shows a yellow warning sign</b></summary>

The action failed, usually because a helper program is missing. The message appears in the
UI and in the log (`journalctl --user -u tuxdeck -f`). Required programs are listed
under [Helper programs](#advanced). Profiles from a Mac often point to paths
like `/Applications/…`; you need to adjust those.
</details>

<details>
<summary><b>A plugin does not start</b></summary>

Node.js plugins need `node` (≥ 20), HTML plugins a Chromium-based browser (Chromium,
Chrome, Brave, Edge or Vivaldi). Plugins with Mac or Windows programs do not run on Linux.
Plugin logs are in `~/.local/share/tuxdeck/logs/`.

**Node.js:** TuxDeck checks the version before starting a plugin and reports it if Node.js
is missing or too old (a plugin may ask for a newer one than 20 in its manifest). Ubuntu
22.04 and Debian 12 ship an older Node.js: install version 20 or newer from
[nodejs.org](https://nodejs.org/en/download) or your distribution's newer streams.
The service only sees programs on its own `PATH`. A Node.js from nvm, fnm, volta or a
home directory has to be named with `TUXDECK_NODE`:

```sh
systemctl --user edit tuxdeck
#   [Service]
#   Environment=TUXDECK_NODE=/home/you/.nvm/versions/node/v22.11.0/bin/node
systemctl --user restart tuxdeck
```
</details>

<details>
<summary><b>The taskbar shows the browser's name/icon instead of TuxDeck</b></summary>

The UI opens in a Chromium-based browser in app mode. On Wayland, Chromium derives the
window's `app_id` from the URL and ignores the app name, so a Wayland taskbar would show
the browser. TuxDeck therefore opens the UI window through XWayland with its own window
class (`tuxdeck`), which the installed desktop entry (`StartupWMClass=tuxdeck`) and icon
match. If you prefer the browser's native Wayland window, set `TUXDECK_UI_OZONE=wayland`
(or empty to use the browser default). Make sure the app was installed (so the `tuxdeck`
icon is in the icon theme); after installing you may need to log out and back in once.
</details>

<details>
<summary><b>It does not start with the session</b></summary>

`systemctl --user enable tuxdeck` starts TuxDeck with `graphical-session.target`. GNOME, KDE
Plasma and sessions started through uwsm or a systemd session target reach it; plain
Hyprland, Sway, i3 and most other window managers don't, so there the service runs only
after a manual start. Start it from your compositor's config instead (after the line that
passes the environment to systemd, if you have one):

| Desktop | Line |
|---|---|
| Hyprland | `exec-once = dbus-update-activation-environment --systemd --all && systemctl --user start tuxdeck` |
| Sway, i3 | `exec dbus-update-activation-environment --systemd --all && systemctl --user start tuxdeck` |
| Other (XDG autostart) | an autostart entry running `systemctl --user start tuxdeck` |

Without systemd, run `tuxdeck run --no-browser` as the autostart command.
`systemctl --user status tuxdeck` shows whether it is running.
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

**NixOS:** `default.nix` builds a package with the program, udev rule, desktop entry,
user service and documentation. Make it part of your system in `configuration.nix`:

```nix
let tuxdeck = pkgs.callPackage /path/to/tuxdeck/default.nix { }; in {
  environment.systemPackages = [ tuxdeck ];  # program, menu entry, icons
  services.udev.packages = [ tuxdeck ];      # device access
  systemd.packages = [ tuxdeck ];            # the user service
}
```

After `nixos-rebuild switch`: `systemctl --user enable --now tuxdeck`. The optional
`/dev/uinput` access is described above. For development: `nix-shell`.
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
| Open Application with a `.desktop` file | `gtk-launch` (`libgtk-3-bin`) or `gio` (`libglib2.0-bin`); without them TuxDeck runs the entry's command itself |
| Node.js plugins | `node` (Node.js ≥ 20) |
| HTML plugins | Chromium, Chrome, Brave, Edge or Vivaldi |
| Linux check of plugins | `node`. Optional `bwrap` (`bubblewrap`): the plugin under test then runs without network and sees only system folders, not your files, and plugins can be tested before installing; without it installed plugins are tested in the isolated test instances and for others only the files are checked |
| Smart Profiles on X11 | `xprop` (`x11-utils`) |
| Notifications | `notify-send` (`libnotify-bin`) |
</details>

<details>
<summary><b>Commands</b></summary>

```text
tuxdeck run [--no-browser]      start the daemon and UI (default)
tuxdeck ui                      open the UI (starts the daemon if needed)
tuxdeck list                    list connected devices
tuxdeck probe [sec]             hardware test: fill keys, print input (stop the service first)
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
| `~/.config/tuxdeck/ui-session` | session cookie value of the browser UI (mode 0600) |
| `~/.config/tuxdeck/settings.json` | settings, brightness, profile assignment, global plugin settings |

Versions before 0.5 used the folders `streamdeck-linux`; they are moved on the first start.

Environment variables: `TUXDECK_DATA_DIR`, `TUXDECK_CONFIG_DIR`, `TUXDECK_NODE`
(Node.js for plugins), `TUXDECK_BROWSER` (Chromium for HTML plugins),
`TUXDECK_XKB_LAYOUT`/`TUXDECK_XKB_VARIANT` (keyboard layout for typing), `GITHUB_TOKEN`
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
- Plugins that monitor applications (`ApplicationsToMonitor`) hear when such a program
  starts or ends: TuxDeck looks for its process every few seconds. Windows program names
  and macOS app ids are matched to their Linux programs as far as known (OBS, Spotify,
  Discord, …). Marketplace secrets (`getSecrets`) are always empty.
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
<summary><b>Network use</b></summary>

TuxDeck itself only goes online for the plugin catalogue: the list and icons from
`openactionapi.github.io`, plugin downloads and published Linux check results from GitHub
(`api.github.com`, release downloads). For GitHub it uses `GITHUB_TOKEN`/`GH_TOKEN` or,
if the GitHub CLI is logged in, its token (`gh auth token`), only to raise the download
limit; a token GitHub refuses is dropped. Publishing your Linux check results to a
repository (`checks_publish`, off by default, meant for maintainers) uploads them with
`gh`. Nothing else is sent anywhere. Plugins are separate programs and may use the
network as they like.

Behind a proxy, give the service `HTTPS_PROXY` (desktop proxy settings don't reach it),
for example in `~/.config/environment.d/proxy.conf` with the line
`HTTPS_PROXY=http://proxy.example:3128`, then log in again. Certificates installed on the
system (company or school networks) are trusted.
</details>

<details>
<summary><b>Security</b></summary>

The UI, API and plugin interface listen on `127.0.0.1` only. Requests with a foreign `Host`
or `Origin` header (DNS rebinding, other websites in your browser) are rejected, because
the API can install plugins and thus run code. The API additionally requires a secret
token that only your user can read (`~/.config/tuxdeck/api-token`), so other users and
programs on the same machine can't use it. The app menu entry and `tuxdeck ui` don't pass
that token to the browser (its command line is visible to other users): they get a login
code that works once and only for 30 seconds, and the browser keeps a separate session
cookie. Plugins register with a secret they get when started (as the official app does,
it is on their command line); property inspectors run on a separate port without access to
the API. Plugins run with your user's rights, like with the official app: install only
plugins you trust.
</details>

## For contributors and maintainers

| | |
|---|---|
| [CONTRIBUTING.md](../CONTRIBUTING.md) | building, tests without hardware, code layout, translations |
| [API.md](API.md) | the local HTTP API the UI uses |
| [SECURITY.md](../SECURITY.md) | reporting vulnerabilities (privately, please) |
| [CHANGELOG.md](../CHANGELOG.md) | what changed in each release |

**Releasing.** Set the version in `Cargo.toml` and `default.nix`, run `cargo update -p
tuxdeck`, add a section to `CHANGELOG.md` and push a tag `v<version>`. The release
workflow builds the `.tar.gz`, `.deb` and `.rpm`, runs the tests and publishes the
release; tags with a suffix (`v0.6.0-beta.1`) become GitHub pre-releases. nfpm names
pre-release packages `0.6.0~beta.1`, so they sort before the final `0.6.0`.

**Packaging.** `make install PREFIX=/usr DESTDIR=… UDEVRULESDIR=/usr/lib/udev/rules.d`
installs the binary, desktop entry, icons, user service and the device rule. Ship
`packaging/71-tuxdeck-uinput.rules` as an optional file, not in the active rules
directory (it lets every program of the logged-in user inject keystrokes).
