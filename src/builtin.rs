//! Native implementations of the actions that the official app implements in C++
//! (the `PrivateAPI` plugins without `CodePath`). Settings keys follow the
//! official app (recovered from its profiles and binary) so imported profiles work.

use serde_json::{json, Value};

pub const OPEN_CHILD: &str = "com.elgato.streamdeck.profile.openchild";
pub const BACK_TO_PARENT: &str = "com.elgato.streamdeck.profile.backtoparent";
pub const SWITCH_PROFILE: &str = "com.elgato.streamdeck.profile.rotate";
pub const PAGE_NEXT: &str = "com.elgato.streamdeck.page.next";
pub const PAGE_PREV: &str = "com.elgato.streamdeck.page.previous";
pub const PAGE_GOTO: &str = "com.elgato.streamdeck.page.goto";
pub const PAGE_INDICATOR: &str = "com.elgato.streamdeck.page.indicator";
pub const BRIGHTNESS: &str = "com.elgato.streamdeck.system.keybrightness";
pub const SLEEP: &str = "com.elgato.streamdeck.system.sleep";
pub const HOTKEY: &str = "com.elgato.streamdeck.system.hotkey";
pub const HOTKEY_SWITCH: &str = "com.elgato.streamdeck.system.hotkeyswitch";
pub const OPEN: &str = "com.elgato.streamdeck.system.open";
pub const OPEN_APP: &str = "com.elgato.streamdeck.system.openapp";
pub const CLOSE: &str = "com.elgato.streamdeck.system.close";
pub const WEBSITE: &str = "com.elgato.streamdeck.system.website";
pub const MULTIMEDIA: &str = "com.elgato.streamdeck.system.multimedia";
pub const TEXT: &str = "com.elgato.streamdeck.system.text";
pub const TIMER: &str = "com.elgato.streamdeck.system.timer";
pub const MULTI: &str = "com.elgato.streamdeck.multiactions.routine";
pub const MULTI_SWITCH: &str = "com.elgato.streamdeck.multiactions.routine2";
pub const MULTI_RANDOM: &str = "com.elgato.streamdeck.multiactions.random";
pub const MULTI_DELAY: &str = "com.elgato.streamdeck.multiactions.delay";
pub const KEY_LOGIC: &str = "com.elgato.streamdeck.keys.logic";
pub const PLAY_AUDIO: &str = "com.elgato.streamdeck.soundboard.playaudio";
pub const STOP_AUDIO: &str = "com.elgato.streamdeck.soundboard.stopaudioplay";
pub const DIGITAL_TIME: &str = "com.elgato.streamdeck.system.digitaltime";
pub const PAGINATION: &str = "com.elgato.streamdeck.system.pagination";

pub struct BuiltinDef {
    pub uuid: &'static str,
    /// Directory name of the corresponding plugin in the official app.
    pub plugin: &'static str,
    pub name: &'static str,
    pub tooltip: &'static str,
    pub category: &'static str,
    pub glyph: &'static str,
    pub color: &'static str,
    pub states: usize,
    pub controllers: &'static [&'static str],
    pub multi: bool,
    pub supported: bool,
}

pub const CAT_NAV: &str = "Deck";
pub const CAT_SYSTEM: &str = "System";
pub const CAT_MULTI: &str = "Multi Action";
pub const CAT_SOUND: &str = "Soundboard";
pub const CAT_NEO: &str = "Neo Info Bar";

const K: &[&str] = &["Keypad"];
const KE: &[&str] = &["Keypad", "Encoder"];
const NEO: &[&str] = &["Neo"];

macro_rules! def {
    ($uuid:expr, $plugin:expr, $name:expr, $tip:expr, $cat:expr, $glyph:expr, $color:expr, $states:expr, $ctrl:expr, $multi:expr, $sup:expr) => {
        BuiltinDef {
            uuid: $uuid,
            plugin: $plugin,
            name: $name,
            tooltip: $tip,
            category: $cat,
            glyph: $glyph,
            color: $color,
            states: $states,
            controllers: $ctrl,
            multi: $multi,
            supported: $sup,
        }
    };
}

pub static CATALOG: &[BuiltinDef] = &[
    def!(OPEN_CHILD, "com.elgato.streamdeck.profile.openchild", "Create Folder", "Create a folder", CAT_NAV, "▣", "#3a7bd5", 1, K, false, true),
    def!(BACK_TO_PARENT, "com.elgato.streamdeck.profile.backtoparent", "Parent Folder", "Open Parent Folder", CAT_NAV, "↩", "#3a7bd5", 1, K, false, true),
    def!(
        SWITCH_PROFILE,
        "com.elgato.streamdeck.profile.rotate",
        "Switch Profile",
        "Switch to another preconfigured key profile",
        CAT_NAV,
        "⇄",
        "#3a7bd5",
        1,
        K,
        true,
        true
    ),
    def!(PAGE_PREV, "com.elgato.streamdeck.page", "Previous Page", "Switch to previous Page", CAT_NAV, "‹", "#3a7bd5", 1, K, false, true),
    def!(PAGE_NEXT, "com.elgato.streamdeck.page", "Next Page", "Switch to next Page", CAT_NAV, "›", "#3a7bd5", 1, K, false, true),
    def!(PAGE_GOTO, "com.elgato.streamdeck.page", "Go to Page", "Switch to specified page number", CAT_NAV, "#", "#3a7bd5", 1, K, false, true),
    def!(
        PAGE_INDICATOR,
        "com.elgato.streamdeck.page",
        "Page Indicator",
        "Displays the page number of the current profile",
        CAT_NAV,
        "1",
        "#3a7bd5",
        1,
        K,
        false,
        true
    ),
    def!(
        BRIGHTNESS,
        "com.elgato.streamdeck.system.keybrightness",
        "Brightness",
        "Adjust the brightness of your Stream Deck keys",
        CAT_NAV,
        "☀",
        "#e0a800",
        1,
        KE,
        true,
        true
    ),
    def!(SLEEP, "com.elgato.streamdeck.system.sleep", "Sleep", "Put your Stream Deck to sleep", CAT_NAV, "☾", "#5a5a8a", 1, K, true, true),
    def!(HOTKEY, "com.elgato.streamdeck.system.hotkey", "Hotkey", "Activate a key command", CAT_SYSTEM, "⌨", "#a53de0", 1, KE, true, true),
    def!(
        HOTKEY_SWITCH,
        "com.elgato.streamdeck.system.hotkeyswitch",
        "Hotkey Switch",
        "Toggle between two key commands",
        CAT_SYSTEM,
        "⌨",
        "#a53de0",
        2,
        K,
        false,
        true
    ),
    def!(OPEN, "com.elgato.streamdeck.system.open", "Open", "Open an app, file or folder", CAT_SYSTEM, "⬈", "#2d9c5a", 1, K, true, true),
    def!(
        OPEN_APP,
        "com.elgato.streamdeck.system.openapp",
        "Open Application",
        "Open an app (long press: quit it)",
        CAT_SYSTEM,
        "▶",
        "#2d9c5a",
        1,
        K,
        true,
        true
    ),
    def!(CLOSE, "com.elgato.streamdeck.system.close", "Close", "Close all instances of an app", CAT_SYSTEM, "✕", "#c0392b", 1, K, true, true),
    def!(WEBSITE, "com.elgato.streamdeck.system.website", "Website", "Open URL", CAT_SYSTEM, "🌐", "#2980b9", 1, K, true, true),
    def!(MULTIMEDIA, "com.elgato.streamdeck.system.multimedia", "Multimedia", "Control your media playback", CAT_SYSTEM, "♫", "#4ae03d", 1, KE, true, true),
    def!(TEXT, "com.elgato.streamdeck.system.text", "Text", "Type or paste a precomposed text", CAT_SYSTEM, "T", "#7f8c8d", 1, K, true, true),
    def!(TIMER, "com.elgato.streamdeck.timer", "Timer", "Countdown timer", CAT_SYSTEM, "⏱", "#d35400", 1, K, false, true),
    def!(MULTI, "com.elgato.streamdeck.multiactions", "Multi Action", "Execute multiple actions", CAT_MULTI, "≡", "#16a085", 1, K, false, true),
    def!(
        MULTI_SWITCH,
        "com.elgato.streamdeck.multiactions",
        "Multi Action Switch",
        "Toggle between two Multi Actions",
        CAT_MULTI,
        "⇋",
        "#16a085",
        2,
        K,
        false,
        true
    ),
    def!(MULTI_RANDOM, "com.elgato.streamdeck.multiactions", "Random Action", "Execute a random action", CAT_MULTI, "?", "#16a085", 1, K, false, true),
    def!(MULTI_DELAY, "com.elgato.streamdeck.multiactions", "Delay", "Pause the actions sequence", CAT_MULTI, "…", "#16a085", 1, K, true, true),
    def!(
        KEY_LOGIC,
        "com.elgato.streamdeck.keys",
        "Key Logic",
        "One action each for press, double press and hold",
        CAT_MULTI,
        "⁘",
        "#16a085",
        1,
        K,
        false,
        true
    ),
    def!(
        PLAY_AUDIO,
        "com.elgato.streamdeck.soundboard",
        "Play Audio",
        "Play a sound bite, audio effect or music clip",
        CAT_SOUND,
        "♪",
        "#e84393",
        1,
        K,
        true,
        true
    ),
    def!(STOP_AUDIO, "com.elgato.streamdeck.soundboard", "Stop Audio", "Stop all Soundboard playback", CAT_SOUND, "■", "#e84393", 1, K, true, true),
    def!(
        DIGITAL_TIME,
        "com.elgato.streamdeck.system.digitaltime",
        "Digital Time",
        "Digital clock in the info bar",
        CAT_NEO,
        "◷",
        "#204cfe",
        1,
        NEO,
        false,
        true
    ),
    def!(PAGINATION, "com.elgato.streamdeck.system.pagination", "Pagination", "Current page in the info bar", CAT_NEO, "▤", "#204cfe", 1, NEO, false, true),
];

pub fn def(uuid: &str) -> Option<&'static BuiltinDef> {
    CATALOG.iter().find(|d| d.uuid == uuid)
}

/// `SupportedInKeyLogicActions` of the official built-ins (default: supported).
pub fn in_key_logic(uuid: &str) -> bool {
    ![KEY_LOGIC, MULTI, MULTI_SWITCH, MULTI_RANDOM, MULTI_DELAY, OPEN_CHILD].contains(&uuid)
}

/// Settings for freshly created instances.
pub fn default_settings(uuid: &str) -> Value {
    match uuid {
        HOTKEY => json!({"Coalesce": true, "Hotkeys": [empty_hotkey(), empty_hotkey(), empty_hotkey(), empty_hotkey()]}),
        HOTKEY_SWITCH => json!({"Coalesce": true, "Hotkeys": [empty_hotkey(), empty_hotkey()]}),
        WEBSITE => json!({"openInBrowser": true, "path": ""}),
        TEXT => json!({"isSendingEnter": false, "isTypingMode": false, "pastedText": ""}),
        MULTIMEDIA => json!({"actionIdx": 1}),
        BRIGHTNESS => json!({"actionIdx": 0}),
        DIGITAL_TIME => json!({"layout": "digital_time_01", "theme": "", "24HourFormat": true}),
        PAGINATION => json!({"layout": "pagination_01", "theme": ""}),
        PLAY_AUDIO => json!({"actionType": 0, "path": "", "volume": 100, "fadeLen": 0, "fadeType": 0, "outputType": ""}),
        MULTI_DELAY => json!({"delay": 500}),
        TIMER => json!({"seconds": 300}),
        PAGE_GOTO => json!({"PageIndex": 1}),
        _ => json!({}),
    }
}

pub fn empty_hotkey() -> Value {
    json!({"KeyCmd": false, "KeyCtrl": false, "KeyModifiers": 0, "KeyOption": false, "KeyShift": false,
           "NativeCode": -1, "QTKeyCode": 33554431, "VKeyCode": -1})
}

/// Multimedia action list in the order used by `actionIdx` (from the official binary).
pub const MEDIA_ACTIONS: &[&str] = &["Previous track", "Play/Pause", "Next track", "Stop", "Mute", "Volume up", "Volume down"];

/// Brightness action list in `actionIdx` order (icon names in the official binary:
/// increase, decrease, max, high, medium, low, min).
pub const BRIGHTNESS_ACTIONS: &[(&str, i32)] = &[
    ("Increase (+10 %)", 10),
    ("Decrease (−10 %)", -10),
    ("Maximum (100 %)", 100),
    ("High (75 %)", 75),
    ("Medium (50 %)", 50),
    ("Low (25 %)", 25),
    ("Minimum (5 %)", 5),
    ("Cycle (10 → 40 → 70 → 100 %)", 0),
];

/// TuxDeck addition to the official list: a single key steps through `BRIGHTNESS_STEPS`
/// and wraps around, so one key is enough to make the deck brighter and darker again.
pub const BRIGHTNESS_CYCLE: usize = 7;
pub const BRIGHTNESS_STEPS: [i32; 4] = [10, 40, 70, 100];

/// Image names in the official plugin for the media/brightness variants.
pub fn variant_image(uuid: &str, settings: &Value) -> Option<&'static str> {
    let idx = settings.get("actionIdx").and_then(|v| v.as_i64()).unwrap_or(0);
    match uuid {
        MULTIMEDIA => [
            "Images/btn_media_prev_track",
            "Images/btn_media_play_pause",
            "Images/btn_media_next_track",
            "Images/btn_media_stop",
            "Images/btn_media_mute",
            "Images/btn_media_volume_up",
            "Images/btn_media_volume_down",
        ]
        .get(idx as usize)
        .copied(),
        BRIGHTNESS => [
            "Images/btn_keybrightness_increase",
            "Images/btn_keybrightness_decrease",
            "Images/btn_keybrightness_max",
            "Images/btn_keybrightness_high",
            "Images/btn_keybrightness_medium",
            "Images/btn_keybrightness_low",
            "Images/btn_keybrightness_min",
            "Images/btn_keybrightness_increase",
        ]
        .get(idx as usize)
        .copied(),
        _ => None,
    }
}

pub struct FormCtx {
    pub digital_time_layouts: Vec<(String, Vec<String>)>,
    pub pagination_layouts: Vec<(String, Vec<String>)>,
}

fn opts(items: impl IntoIterator<Item = (Value, String)>) -> Value {
    Value::Array(items.into_iter().map(|(v, l)| json!({"value": v, "label": l})).collect())
}

/// "digital_time_04" -> "Layout 4".
fn layout_label(id: &str) -> String {
    if let Some(imported) = id.strip_prefix(crate::layout::IMPORTED_PREFIX) {
        return crate::tr!("{layout} (Elgato)", layout = layout_label(imported));
    }
    match id.rsplit('_').next().and_then(|n| n.parse::<u32>().ok()) {
        Some(n) => crate::tr!("Layout {n}", n = n),
        None => id.replace('_', " "),
    }
}

/// Info bar color scheme ids ("purple_orange", "night") as readable, translated names.
fn theme_label(id: &str) -> String {
    let word = |w: &str| -> String {
        match w {
            "red" => crate::tr!("Red"),
            "black" => crate::tr!("Black"),
            "white" => crate::tr!("White"),
            "brown" => crate::tr!("Brown"),
            "blue" => crate::tr!("Blue"),
            "green" => crate::tr!("Green"),
            "yellow" => crate::tr!("Yellow"),
            "orange" => crate::tr!("Orange"),
            "purple" => crate::tr!("Purple"),
            "pink" => crate::tr!("Pink"),
            "mint" => crate::tr!("Mint"),
            "night" => crate::tr!("Night"),
            "beach" => crate::tr!("Beach"),
            "autumn" => crate::tr!("Autumn"),
            "city" => crate::tr!("City"),
            "default" => crate::tr!("Default"),
            other => {
                let mut c = other.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            }
        }
    };
    id.split(['_', '-']).filter(|w| !w.is_empty()).map(word).collect::<Vec<_>>().join(" ")
}

fn layout_fields(layouts: &[(String, Vec<String>)], default_layout: &str) -> Vec<Value> {
    let mut themes: Vec<String> = layouts.iter().flat_map(|(_, t)| t.clone()).collect();
    themes.sort();
    themes.dedup();
    let mut v = vec![json!({"key": "layout", "type": "select", "label": "Layout", "default": default_layout,
        "options": if layouts.is_empty() { opts([(json!(default_layout), "Default".to_string())]) } else { opts(layouts.iter().map(|(id, _)| (json!(id), layout_label(id)))) }})];
    let theme_opts = |list: &[String]| {
        opts(std::iter::once((json!(""), "Default".to_string())).chain(list.iter().filter(|t| t.as_str() != "default").map(|t| (json!(t), theme_label(t)))))
    };
    // Themes differ per layout: `options_by` lets the editor offer only the themes of
    // the selected layout (`options` stays as the union for older clients).
    let by: serde_json::Map<String, Value> = layouts.iter().map(|(id, t)| (id.clone(), theme_opts(t))).collect();
    v.push(json!({"key": "theme", "type": "select", "label": "Color scheme", "options": theme_opts(&themes),
        "depends_on": "layout", "options_by": by}));
    v
}

/// Settings editor schema for the web UI (see docs/API.md), translated into the
/// current language.
pub fn form(uuid: &str, ctx: &FormCtx) -> Value {
    let f = match uuid {
        HOTKEY => vec![json!({"key": "Hotkeys", "type": "hotkey", "label": "Key combination(s)", "help": "Up to 4 combinations are sent one after another."})],
        HOTKEY_SWITCH => {
            vec![json!({"key": "Hotkeys", "type": "hotkey", "label": "Combination state 1 / state 2", "help": "First entry for state 1, second for state 2."})]
        }
        OPEN => vec![json!({"key": "path", "type": "file", "label": "File, folder, program or command", "placeholder": "/usr/bin/firefox or ~/Documents"})],
        OPEN_APP => vec![
            json!({"key": "exec", "type": "file", "label": "Application", "placeholder": "firefox"}),
            json!({"key": "args", "type": "text", "label": "Arguments"}),
            json!({"key": "long_press", "type": "select", "label": "On long press", "default": "quit",
                   "options": opts([(json!("none"), "Nothing".into()), (json!("quit"), "Quit the application".into()), (json!("kill"), "Kill the application".into())])}),
        ],
        CLOSE => vec![json!({"key": "app_name", "type": "text", "label": "Process name", "placeholder": "firefox"})],
        WEBSITE => vec![
            json!({"key": "path", "type": "text", "label": "URL", "placeholder": "https://"}),
            json!({"key": "openInBrowser", "type": "checkbox", "label": "Open in the default browser", "default": true}),
        ],
        MULTIMEDIA => vec![json!({"key": "actionIdx", "type": "select", "label": "Action",
            "options": opts(MEDIA_ACTIONS.iter().enumerate().map(|(i, l)| (json!(i), l.to_string())))})],
        BRIGHTNESS => vec![json!({"key": "actionIdx", "type": "select", "label": "Brightness",
            "options": opts(BRIGHTNESS_ACTIONS.iter().enumerate().map(|(i, (l, _))| (json!(i), l.to_string())))})],
        TEXT => vec![
            json!({"key": "pastedText", "type": "textarea", "label": "Text"}),
            json!({"key": "isTypingMode", "type": "checkbox", "label": "Type instead of paste"}),
            json!({"key": "isSendingEnter", "type": "checkbox", "label": "Press Enter afterwards"}),
        ],
        SWITCH_PROFILE => {
            vec![json!({"key": "DeviceUUID", "type": "device", "label": "Deck"}), json!({"key": "ProfileUUID", "type": "profile", "label": "Profile"})]
        }
        PAGE_GOTO => vec![json!({"key": "PageIndex", "type": "number", "label": "Page number", "min": 1, "max": 10, "step": 1, "default": 1})],
        PLAY_AUDIO => vec![
            json!({"key": "path", "type": "file", "label": "Audio file", "placeholder": "/path/to/sound.mp3"}),
            json!({"key": "actionType", "type": "select", "label": "Behavior", "default": 0,
                   "options": opts([(json!(0), "Play / Stop".into()), (json!(1), "Play / Overlap".into()), (json!(2), "Play while pressed".into())])}),
            json!({"key": "volume", "type": "number", "label": "Volume (%)", "min": 0, "max": 100, "step": 5, "default": 100}),
        ],
        MULTI_DELAY => vec![json!({"key": "delay", "type": "number", "label": "Delay (ms)", "min": 1, "max": 600000, "step": 50, "default": 500})],
        TIMER => vec![
            json!({"key": "seconds", "type": "number", "label": "Duration (seconds)", "min": 1, "max": 86400, "step": 1, "default": 300}),
            json!({"key": "alarm", "type": "file", "label": "Alarm sound (optional)"}),
        ],
        MULTI | MULTI_RANDOM => vec![json!({"key": "_actions0", "type": "actions", "label": "Actions"})],
        MULTI_SWITCH => vec![
            json!({"key": "_actions0", "type": "actions", "label": "Actions state 1"}),
            json!({"key": "_actions1", "type": "actions", "label": "Actions state 2"}),
        ],
        // Slot order and timing as in the official app: click, double click (within
        // 300 ms), long press (held for 500 ms).
        KEY_LOGIC => vec![
            json!({"key": "_actions0", "type": "actions", "label": "Press", "max": 1, "filter": "keylogic"}),
            json!({"key": "_actions1", "type": "actions", "label": "Double press", "max": 1, "filter": "keylogic"}),
            json!({"key": "_actions2", "type": "actions", "label": "Hold", "max": 1, "filter": "keylogic",
                   "help": "Double press: second press within 0.3 s. Hold: at least 0.5 s."}),
        ],
        DIGITAL_TIME => {
            let mut v = layout_fields(&ctx.digital_time_layouts, "digital_time_01");
            v.push(json!({"key": "24HourFormat", "type": "checkbox", "label": "24-hour format", "default": true}));
            v
        }
        PAGINATION => layout_fields(&ctx.pagination_layouts, "pagination_01"),
        _ => vec![],
    };
    let mut v = Value::Array(f);
    translate_form(&mut v);
    v
}

/// Translate `label`, `help` and `placeholder` texts (option labels included).
fn translate_form(v: &mut Value) {
    match v {
        Value::Array(a) => a.iter_mut().for_each(translate_form),
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                match x {
                    Value::String(s) if matches!(k.as_str(), "label" | "help" | "placeholder") => *s = crate::i18n::tr(s),
                    _ => translate_form(x),
                }
            }
        }
        _ => {}
    }
}
