# TuxDeck HTTP API (UI <-> daemon)

The daemon serves the web UI and this API on `http://127.0.0.1:47800` (default).
All bodies are JSON unless noted. Errors: non-2xx with `{"error": "..."}`.

Access: every `/api/*` request needs the secret from `~/.config/tuxdeck/api-token`
(created on first start, readable only by the user) as `X-Tuxdeck-Token: <token>`
header, `Authorization: Bearer <token>`, the `tuxdeck_token` cookie or (for WebSocket clients)
`?token=<token>`; otherwise `401`. Opening any UI page with `?token=<token>` (what
`tuxdeck ui` does) sets the cookie and redirects to the URL without it.

    curl -H "X-Tuxdeck-Token: $(cat ~/.config/tuxdeck/api-token)" http://127.0.0.1:47800/api/state

Terminology
- **profile**: Elgato `.sdProfile` (id = upper-case UUID string).
- **page**: one page/folder inside a profile (id = lower-case UUID). Top-level pages are
  listed in `profile.pages` (ordered). Folders (created by the action
  `com.elgato.streamdeck.profile.openchild`) are pages *not* in that list.
- **controller**: `"Keypad"` (normal keys), `"Neo"` (Stream Deck Neo info bar +
  touch points), `"Encoder"` (Stream Deck + dials).
- **coord**: `"col,row"` string, e.g. `"0,0"`. For controller `Neo`: `"0,0"` = left
  touch point, `"1,0"` = info bar, `"2,0"` = right touch point.
- **action instance**: Elgato JSON object stored in the page manifest:
  ```json
  {"ActionID":"uuid","LinkedTitle":true,"Name":"Website","Settings":{...},"State":0,
   "States":[{"Image":"Images/X.png","Title":"Hi","ShowTitle":true,"TitleAlignment":"bottom",
              "TitleColor":"#ffffff","FontSize":12,"FontFamily":"","FontStyle":"",
              "FontUnderline":false,"OutlineThickness":2}],
   "UUID":"com.elgato.streamdeck.system.website"}
  ```
  Any field of a state may be absent (defaults: ShowTitle true, TitleAlignment
  "bottom", TitleColor "#ffffff", FontSize 12, FontStyle "" (=Regular; other
  values "Bold", "Italic", "Bold Italic")).

## Read

### `GET /api/state`
```json
{
  "devices": [{
    "id": "A1B2C3D4E5F6G7", "name": "Stream Deck Neo", "model": "20GBJ9901", "kind": "neo",
    "connected": true, "cols": 4, "rows": 2, "key_px": 96, "touch_points": 2,
    "screen": [248, 58], "encoders": 0, "firmware": "1.00.013",
    "profile": "47A10A36-...", "page": "9a3bd784-...", "folder_depth": 0, "brightness": 70,
    "sleeping": false, "smart": false
  }],
  "profiles": [{
    "id": "47A10A36-...", "name": "Default Profile", "model": "20GBJ9901",
    "pages": ["9a3bd784-...", "4c1be6c6-..."], "default_page": "10decedc-...",
    "folders": ["5211d3c8-..."], "app": null
  }],
  "active_app": "firefox",
  "settings": {"cmd_as_ctrl": true, "platform": "mac", "language": "de", "touch_color": "#ffffff",
               "injector": "auto", "injector_active": "hyprctl",
               "injector_choices": ["auto", "uinput", "hyprctl", "wtype", "xdotool"]}
}
```
- `sleeping`: the device was put to sleep by the Sleep action (next input wakes it).
- `smart`: the device currently shows a smart profile (see `app`) and returns to the
  previous profile when that application loses focus.
- `app` (profile): application linked to the profile (smart profile, Elgato manifest field
  `AppIdentifier`), matched against the window class of the focused window; `null` = none.
- `active_app`: window class of the focused window ("" if unknown).
- `injector`: preferred key injection backend (setting), `injector_active`: backend in use.
- Models: Neo `20GBJ9901`, + `20GBD9901`, MK.2 `20GBA9901`, Original `20GAA9902` (2. Gen.)
  / `20GAA9901` (1. Gen.), XL `20GAT9901`/`20GAT9902`, Mini `20GAI9901`, Mini MK.2
  `20GAI9902`, Pedal `20GBF9901`. Profiles of models with the same layout are interchangeable.
`screen` is `null` when the device has no screen. A device may be listed with
`connected:false` if it was seen before.

### `GET /api/actions`
Action catalogue grouped by category (order matters; render in this order):
```json
[{"category": "Navigation", "icon": "/plugin-files/<plugin-uuid>/Images/x.svg" ,
  "actions": [{
    "uuid": "com.elgato.streamdeck.profile.openchild", "plugin": "com.elgato.streamdeck.profile.openchild",
    "name": "Create Folder", "tooltip": "...", "icon": "/plugin-files/.../Images/x.svg",
    "controllers": ["Keypad"], "builtin": true, "supported": true,
    "pi": null,
    "form": [ {"key":"path","type":"text","label":"URL","placeholder":"https://"} ]
  }]
}]
```
- `pi` is a URL of an HTML property inspector (third-party plugins) or `null`.
- `form` (built-in actions only, may be `[]`) describes the settings editor. Field types:
  - `text` / `textarea` / `number` (`min`,`max`,`step`) / `checkbox` / `color` (value `#rrggbb`)
  - `select` with `options: [{"value": <json value>, "label": "..."}]`
  - `file` (text field + note that it is a local path), `page` (select of the profile's
    top-level pages; value = page uuid), `profile` (select of all profiles; value = profile id)
  - `hotkey`: value is an array of up to 4 Elgato hotkey objects (see below), stored in
    `Settings.Hotkeys`; the UI must offer a "record" button capturing a key combination.
  - `actions`: a nested action list (multi action, Key Logic slot). Value: array of action instances.
    Optional `max` (Key Logic: 1) and `filter: "keylogic"` (only actions with `keylogic: true`).
    The UI may display it read-only with a remove button per entry and an
    "add action" select listing actions whose `multi` flag is true.
  Each field has `key` (settings key; dotted path allowed, e.g. `"a.b"`), `label`,
  optional `default`, optional `help`.
  `select` fields may carry `depends_on` (key of another field) and `options_by`
  (`{<value of that field>: [options]}`): offer only the options for the current value
  of the other field (e.g. the themes of the selected info bar layout); `options` is the
  union for clients that ignore this.
- `supported:false` means the plugin cannot run on Linux (show greyed out, still draggable).
- `multi: true` if the action can be used inside a multi action.
- `keylogic: true` if the action can be used in a Key Logic slot (`SupportedInKeyLogicActions`).

Hotkey object (Elgato format, keep all fields):
```json
{"KeyCmd":false,"KeyCtrl":true,"KeyOption":false,"KeyShift":false,"KeyModifiers":2,
 "QTKeyCode":90,"NativeCode":-1,"VKeyCode":-1,"LinuxCode":44}
```
`KeyModifiers` bitmask: Shift=1, Ctrl=2, Option/Alt=4, Cmd/Super=8.
`QTKeyCode`: Qt::Key value (letters = upper-case ASCII, digits = ASCII, space=0x20,
Escape=0x01000000, Tab=0x01000001, Backspace=0x01000003, Return=0x01000004,
Insert=0x01000006, Delete=0x01000007, Home=0x01000010, End=0x01000011,
Left=0x01000012, Up=0x01000013, Right=0x01000014, Down=0x01000015,
PageUp=0x01000016, PageDown=0x01000017, F1..F24 = 0x01000030 + n-1, punctuation = ASCII).
`LinuxCode`: Linux evdev key code derived from `KeyboardEvent.code` (layout independent),
e.g. KeyA=30, KeyZ=44, Digit1=2, F1=59, Enter=28, Space=57, Escape=1. Use -1 when unknown.
An unused slot is `{"QTKeyCode":33554431,"NativeCode":-1,"VKeyCode":-1,"KeyModifiers":0,...false}`.

### `GET /api/pages/{profile}/{page}`
```json
{"id": "9a3bd784-...", "name": "", "profile": "47A10A36-...", "parent": null,
 "controllers": [{"Type": "Keypad", "Actions": {"0,0": <action instance>}},
                 {"Type": "Neo", "Actions": {"1,0": <action instance>}}]}
```

### `GET /api/render/{profile}/{page}/{controller}/{coord}.png?size=144&v=<cache-buster>`
PNG preview exactly as shown on the device (title, image, state). Empty slot → black
key. For `Neo/1,0` the info bar image (248x58, `size` ignored). For `Neo/0,0` and
`Neo/2,0` a small circle in the touch point LED color. For `Encoder/<n>,0` (Stream
Deck +) the touch strip segment above that dial (200x100, `size` ignored).
`parent` in the page response is the page containing the folder key (`null` for
top-level pages).

### `GET /api/pi/{profile}/{page}?controller=Keypad&coord=0,0`
Everything needed to open a third-party property inspector:
```json
{"url": "/plugin-files/<plugin>/pi/index.html", "port": 47801,
 "uuid": "<context>", "registerEvent": "registerPropertyInspector",
 "info": {...}, "actionInfo": {...}}
```
`url` keeps the plugin directory name (`/plugins/<name>.sdPlugin/...`): Elgato's PI
libraries locate the plugin root by looking for a path segment ending in `sdPlugin`.
The UI loads `url` in an `<iframe>` (same origin) and after `load` calls
`iframe.contentWindow.connectElgatoStreamDeckSocket(port, uuid, registerEvent,
JSON.stringify(info), JSON.stringify(actionInfo))`. (This is the official Elgato
SDK contract; the PI then talks to the daemon over its own WebSocket.)
Returns 404 when the action has no PI.

### `GET /plugin-files/{plugin-uuid}/{path...}`, `GET /plugins/{dir}.sdPlugin/{path...}`
Static files of an installed plugin (icons, property inspector HTML/JS/CSS).

### `GET /api/profiles/{profile}/export`
Download `.streamDeckProfile` (zip).

## Write

| Method & path | Body | Effect |
|---|---|---|
| `POST /api/pages/{profile}/{page}/slot` | `{"controller","coord","uuid"}` | create instance of action `uuid` in an empty slot (replaces an existing one). Returns the instance. For `openchild` a folder page is created automatically. |
| `DELETE /api/pages/{profile}/{page}/slot?controller=..&coord=..` | – | remove instance (folders are deleted too) |
| `PATCH /api/pages/{profile}/{page}/slot` | `{"controller","coord", "state": 0, "style": {Title, ShowTitle, TitleAlignment, TitleColor, FontSize, FontStyle, FontUnderline}, "image": "data:image/png;base64,..." , "settings": {...}}` | every key except controller/coord is optional. `style` patches `States[state]` (fields set to `null` are removed). `image`: data URL sets custom image for that state, `""` resets to default. `settings` replaces `Settings`. |
| `POST /api/pages/{profile}/{page}/move` | `{"from":{"controller","coord"},"to":{"controller","coord"},"copy":false}` | move (swap if target occupied) or copy |
| `POST /api/pages/{profile}/{page}/press` | `{"controller","coord","hold_ms"?}` | simulate key press+release on the device showing that page (`hold_ms` default 80, max 10000; long press e.g. for "Open App → quit"). `controller:"Encoder"` presses that dial. |
| `POST /api/devices/{id}/input` | `{"type":"dial","index",ticks}` / `{"type":"dial_press","index","down"}` / `{"type":"touch","kind":1\|2\|3,"x","y","x2"?,"y2"?}` | simulated Stream Deck + input: dial rotation, dial press/release, touch strip tap (1), long touch (2) or swipe (3, from x/y to x2/y2; swipes change pages). Coordinates are strip pixels (0–799, 0–99). |
| `POST /api/profiles` | `{"name","model"}` | new empty profile, returns profile summary |
| `PATCH /api/profiles/{profile}` | `{"name"?, "app"?}` | rename (non-empty) and/or link to an application (smart profile; `""` removes the link) |
| `GET /api/windows` | – | `{"active": "firefox", "windows": [{"class","title"}]}` open windows (Hyprland) for picking a smart profile app |
| `POST /api/active-app` | `{"app": "firefox"}` | simulate a focus change (tests, desktops without focus tracking) |
| `DELETE /api/profiles/{profile}` | – | delete |
| `POST /api/profiles/{profile}/pages` | – | append a new empty page, returns `{"id"}` |
| `DELETE /api/pages/{profile}/{page}` | – | delete a top-level page (not the last one) |
| `POST /api/devices/{id}` | `{"profile"?, "page"?, "brightness"?}` | switch profile / navigate to page (top-level page or folder) / brightness 0-100 |
| `POST /api/import` | raw bytes, header `X-Filename: name.streamDeckProfile`, `.streamDeckPlugin` or `.streamDeckIconPack` | import profile, install plugin or icon pack; returns `{"kind":"profile"\|"plugin"\|"iconpack","id","name","runs_on_linux","reason"}` |
| `POST /api/settings` | partial settings object (`cmd_as_ctrl`, `platform`, `language`, `touch_color` `#rrggbb`, `injector` one of `injector_choices`) | update global settings; invalid values → 400 |

## Icon packs

| Method & path | Result |
|---|---|
| `GET /api/iconpacks` | `[{"id","name","author","version","description","url","icon","count"}]` (`icon`: URL or null) |
| `GET /api/iconpacks/{id}/icons?search=&offset=&limit=` | `{"pack", "total", "offset", "items": [{"path","name","tags","url"}]}`; search matches name, tags and file name (all words), `limit` ≤ 500 (default 200) |
| `GET /iconpack-files/{id}/{path...}` | file of an installed pack |
| `DELETE /api/iconpacks/{id}` | uninstall |

To put an icon on a key the UI downloads it and sends it as `image` data URL with `PATCH …/slot`
(the profile then contains the image, as with the official app).

## Live events: `GET /api/events` (WebSocket, server → client JSON)
- `{"event":"state"}`: something in `/api/state` changed → refetch.
- `{"event":"page","profile","page"}`: content of that page changed → refetch page + previews.
- `{"event":"input","device","controller","coord","down":true|false}`: physical press (highlight).
  Stream Deck +: dial press/release with `controller:"Encoder"`; touch strip taps carry `"touch":true`.
- `{"event":"iconpacks"}`: icon pack installed or removed.
- `{"event":"actions"}`: action catalogue changed (plugin installed).
- `{"event":"log","level":"info|warn|error","message":"..."}`

## Plugins and the plugin catalogue

| Method & path | Body / query | Result |
|---|---|---|
| `GET /api/plugins` | – | user-installed plugins `[{"uuid","name","icon","version","author","runs_on_linux","reason","actions"}]` |
| `DELETE /api/plugins/{uuid}` | – | uninstall |
| `GET /api/plugins/{uuid}/diagnostics` | – | what the plugin did over the SDK (registrations, events, errors) |
| `GET /api/catalogue` | `search`, `ordering` (catalogue order default, `name`), `offset`, `limit` (≤100) | `{"count", "offset", "limit", "items": [Item]}` |
| `GET /api/catalogue/{id}` | – | `Item` |
| `POST /api/catalogue/install` | `{"id"}` | downloads the newest release of the plugin's repository and installs it: `{"kind":"plugin","id","name","runs_on_linux":bool,"reason"}`; errors: 400 `{"error"}` (no release, no package in it, GitHub rate limit, protected package, …). A Linux check starts right after. |

`Item` (an entry of the [OpenAction plugin registry](https://github.com/OpenActionAPI/plugins)):
```json
{"id": "com.example.plugin", "name": "Example", "author": "someone",
 "summary": "short text" | null, "repository": "https://github.com/someone/example",
 "icon": "https://openactionapi.github.io/plugins/icons/com.example.plugin.png", "ai_assisted": false,
 "state": {"installed": false, "linux_check": null | {"verdict","version","tested_at","source"}, "checking": false}
        | {"installed": true, "installed_version": "1.0.0", "runs_on_linux": true, "reason": null, …}}
```

`POST /api/import` with the header `X-Linux-Check: 1` also starts a Linux check of an
installed plugin (the UI does that for "Install from file…").

## Linux checks

| Method & path | Body | Result |
|---|---|---|
| `GET /api/linux-checks` | – | `{"results": {uuid: Report}, "community": {uuid: Report}, "running": [key]}` (`community`: the published results) |
| `POST /api/plugins/{uuid}/linux-check` | – | test an installed plugin: `{"started": true, "key"}`; 409 while one runs |
| `POST /api/catalogue/linux-check` | `{"id"}` | test a catalogue plugin without installing it |
| `POST /api/catalogue/linux-check-all` | `{"parallel"?: 3, "limit"?}` | test every catalogue plugin whose newest release was not tested yet (resumable); `DELETE` stops it |

`Report`: `{"uuid","name","version","verdict":"works"|"issues"|"fails"|"unsupported","tested_at","app_version","sandboxed","checks":[{"id","ok":true|false|null,"detail"}], "source"?, "release"?}`.
Results with `"local_only": true` are never published.

Events: `{"event":"linux-check","key","running", "result"?|"error"?}`,
`{"event":"linux-check-all","running","done","total","current"}`,
`{"event":"market","type":"installed"|"open"}` (`open`: a `streamdeck://open/store…`
link asked to show the plugins view).

## Deep links

`POST /api/deeplink {"url"}` (what `tuxdeck open-url` forwards):
`streamdeck://plugins/message/<uuid>/<path>` reaches the plugin as `didReceiveDeepLink`
(plugins finish their OAuth sign-ins that way); install links of the Elgato Marketplace
website are refused with a hint to install the downloaded file instead.
