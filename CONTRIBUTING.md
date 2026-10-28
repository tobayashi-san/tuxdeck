# Contributing

Thanks for your interest! Bug reports, device tests and pull requests are welcome. The most
helpful thing right now is someone trying the support for a Stream Deck +, Mini, XL, MK.2,
Pedal or an original first-generation device (issue template "Test a device").

## Building

Prerequisites: see the README ("Build from source"). Then:

```sh
make            # cargo build --release
make check      # unit tests, Clippy (-D warnings) and end-to-end tests (Node.js ≥ 22)
```

The minimum Rust version is 1.85 (`rust-version` in `Cargo.toml`). `.cargo/config.toml`
makes `cargo update` pick only dependency versions that support it. NixOS: `nix-shell`.

The UI (`ui/`) is embedded into the binary with `rust-embed` in release builds. After
changing `ui/`, rebuild, or serve the UI straight from the checkout:

```sh
node ui-mock/proxy.mjs 8080 http://127.0.0.1:47800    # UI from ui/, everything else from the daemon
```

## Testing without hardware and without side effects

| Variable | Effect |
|---|---|
| `TUXDECK_VIRTUAL_DEVICE=neo\|mk2\|xl\|plus\|pedal\|original\|mini` (or a model number) | simulated device; real devices are left alone (unless `TUXDECK_ALLOW_HARDWARE=1`) |
| `TUXDECK_DRY_RUN=1` | actions (processes, hotkeys, clipboard, media, audio) are only logged as `DRYRUN …` |
| `TUXDECK_DATA_DIR`, `TUXDECK_CONFIG_DIR` | separate data and config directories |
| `TUXDECK_NO_URL_HANDLER=1` | do not register the `streamdeck://` handler |
| `TUXDECK_NO_WINDOW_WATCH=1` | Smart Profiles do not follow the real window focus |
| `TUXDECK_NO_PLATFORM_SHIM=1` | do not override `process.platform` for Node plugins |

Set the ports of a test instance in `settings.json` (`ui_port`, `sdk_port`) so it can run
next to a regular installation (47800/47801). Press keys with
`POST /api/pages/{profile}/{page}/press`, dials and the touch strip with
`POST /api/devices/{id}/input`, and change focus with `POST /api/active-app` (see
[docs/API.md](docs/API.md)).

`node tests/e2e.mjs [--bin PATH] [--only neo,plus,mini,original,neo:plugins,...] [-v] [--keep]`
does exactly that automatically: test groups with a fresh daemon each and a simulated device
in dry-run mode, checked through the `DRYRUN` log lines and the API. Please cover new
built-in actions there.

`node tests/ui-smoke.mjs [--bin PATH] [--browser PATH]` drives the web UI in a headless
Chromium-based browser (Chrome, Chromium or Brave) over the DevTools protocol: editing,
keyboard use, drag & drop, pages, settings, offline edits. It fails on JS errors of the UI;
failing tests save a screenshot.

Tip: if a test instance is running, build into a separate directory
(`cargo build --release --target-dir target/dev`), otherwise its binary gets replaced.

## Code layout

| File | Purpose |
|---|---|
| `main.rs` | commands, hotplug, device threads (encode images in the model's format), virtual device |
| `device.rs` | HID protocol gen 2 (Neo, MK.2, XL, +, Pedal) and gen 1 (Mini, Original); report building as pure functions with unit tests |
| `engine/` | central state behind `Arc<Mutex<Engine>>`, one `impl Engine` per topic: `devices.rs` (connect, profiles, pages), `keys.rs` (input), `actions.rs` (built-in actions, Key Logic, multi actions), `protocol.rs` (SDK contexts and events), `display.rs` (key images, info bar, touch strip, refresh), `editing.rs` (web UI edits), `system.rs` (processes) |
| `builtin.rs` | catalogue and forms of the built-in actions (settings keys as in the original) |
| `input.rs` | keyboard, text, media: uinput → Hyprland → wtype → xdotool; dry run |
| `render.rs`, `layout.rs` | key images via SVG and resvg; Neo info bar from `.sdLayoutEx` layouts |
| `strip.rs` | Stream Deck +: touch strip layouts (`$X1`, `$A0`, `$A1`, `$B1`, `$B2`, `$C1`, custom) and `setFeedback` |
| `profile.rs` | Elgato profile format 2.0/3.0, import/export |
| `plugins.rs`, `sdk.rs` | plugin manifests, runtime detection; SDK WebSocket, plugin processes, HTML plugin host |
| `web.rs`, `web_plugins.rs` | HTTP API with Host/Origin check, embedded UI; plugins, catalogue, Linux checks, deep links |
| `packages.rs`, `catalogue.rs` | installing package files; the open-source plugin catalogue (OpenAction registry, GitHub releases) |
| `linuxcheck.rs`, `linux-check.mjs` | "Works on Linux" checks of plugins in isolated test instances, published results |
| `backup.rs` | backups and restore (also of the official app's backups) |
| `smart.rs`, `iconpacks.rs`, `resources.rs` | Smart Profiles, icon packs, built-in plugin manifests and `import-elgato` |
| `net.rs`, `urlhandler.rs`, `paths.rs` | HTTP client, `streamdeck://` handler for plugin deep links, folders and settings |

The UI in `ui/` deliberately has no build step (ES modules, no framework).

Long-running or blocking work (killing processes, unpacking archives, decoding images,
rasterizing) must not happen while the engine lock is held: take what you need under
the lock, release it, then do the work.

## Formats and protocols

To keep imported profiles and plugins working, TuxDeck follows the formats of the
official app. The essentials:

**HID, generation 2** (e.g. Neo, VID `0x0fd9`): key images are JPEGs in 1024-byte reports
`02 07 <key> <last> <len LE16> <page LE16>`, 96×96 and rotated by 180° on the Neo; the info
bar (248×58) uses `02 0b`. Brightness `03 08 <0–100>`, reset `03 02`, touch LEDs
`03 06 <8+i> R G B`. Input: `01 00 <count LE16>` followed by one byte per key (Neo: 8 keys +
2 touch points). **Generation 1** (Mini, Original 2017) uses BMP images; details in
`device.rs`.

**Profiles:** `ProfilesV2/<UUID>.sdProfile/manifest.json` with `Device.Model`,
`Pages.Pages`, `Pages.Default`. Page folders use a base32 encoding of the UUID in format 2.0
(see `profile.rs`) and the UUID itself in format 3.0. Controllers: `Keypad`
(`"column,row"`), `Encoder` (Stream Deck +), `Neo` (info bar). Folders are separate pages,
linked via `profile.openchild` with `Settings.ProfileUUID`.

**Nested actions:** multi actions store `"Actions": [{"Actions": [...]}, ...]` (one list
per state). Key Logic stores exactly three entries (press, double press, hold), each either
an action or `{"Actions": []}`; double-press window 300 ms, hold from 500 ms.

**Plugins:** SDK v2 as documented at docs.elgato.com; Node.js plugins are started with
`-port -pluginUUID -registerEvent -info`. Packages protected with DRM (a `manifest.json`
that starts with `ELGATO`) are detected and rejected. TuxDeck does not circumvent DRM or
other protections: contributions that do will not be accepted.

## Style

- `cargo clippy -- -D warnings` must pass; format like the existing code.
- New features come with a test: a unit test for pure logic, otherwise a case in
  `tests/e2e.mjs`.
- Code, comments and source strings are in English; user-visible text goes through `t()`
  (UI) or `tr!()` (daemon), see below.

## Translations

The UI and the daemon's messages use English source strings, translated by
`ui/i18n/<lang>.json` (English text → translation, `{placeholders}` are kept). The same file
serves the web UI and is embedded into the daemon. Like the official app, the language
follows the system locale unless one is chosen in the settings.

- **Add a language:** copy `ui/i18n/de.json` to e.g. `ui/i18n/fr.json` and translate the
  values. Nothing else is needed: the language shows up in the settings automatically.
- **Add text:** wrap it in `t('…')` / `tn(n, '… one', '… many')` (UI), `data-i18n` (static
  HTML) or `tr!("…", name = value)` (daemon), and add the translation to every dictionary.
- `node tests/i18n-check.mjs` (part of `make check` and CI) lists missing and unused
  entries and placeholder mismatches.

Plugins ship their own translations (`de.json`, `en.json`, … in the plugin folder); they
follow the same language setting.

## Releases

Bump the version in `Cargo.toml`, run `cargo build` (updates `Cargo.lock`), commit, then

```sh
git tag v0.2.0 && git push origin main v0.2.0
```

`.github/workflows/release.yml` builds on Ubuntu 22.04 (glibc 2.35), runs the tests and
attaches the archive, `.deb`, `.rpm` and `SHA256SUMS` to the GitHub release. The tag must
match the version in `Cargo.toml`.
