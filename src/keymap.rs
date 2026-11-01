//! The keyboard layout of the session, for typing text through the virtual keyboard
//! (uinput sends key positions; which character a key gives depends on the layout).
//! libxkbcommon is loaded at runtime: every desktop has it, and without it typing over
//! uinput is just not offered.

use std::collections::HashMap;
use std::ffi::CString;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use xkbcommon_dl::{xkb_context_flags, xkb_keymap_compile_flags, xkb_rule_names, xkbcommon_option};

/// How to produce a character: evdev key code, Shift, AltGr.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stroke {
    pub code: u16,
    pub shift: bool,
    pub altgr: bool,
}

/// Characters of a layout.
pub struct Keymap {
    /// `layout` or `layout(variant)`, for messages.
    pub name: String,
    chars: HashMap<char, Stroke>,
}

impl Keymap {
    pub fn stroke(&self, c: char) -> Option<Stroke> {
        self.chars.get(&c).copied()
    }
}

/// XKB layout and variant of the session (first of several): an explicit setting
/// (`TUXDECK_XKB_LAYOUT`/`_VARIANT`), Hyprland, GNOME, KDE, X11, else the system's.
pub fn session_layout() -> (String, String) {
    let env = |k: &str| std::env::var(k).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(l) = env("TUXDECK_XKB_LAYOUT") {
        return (l, env("TUXDECK_XKB_VARIANT").unwrap_or_default());
    }
    let out = |cmd: &mut Command| -> Option<String> {
        let o = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
        Some(String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_uppercase();
    let found = if crate::input::hypr_instance().is_some() {
        out(crate::input::hyprctl_command().args(["devices", "-j"])).and_then(|j| hyprland_layout(&j))
    } else if desktop.contains("GNOME") {
        out(Command::new("gsettings").args(["get", "org.gnome.desktop.input-sources", "mru-sources"]))
            .and_then(|s| gnome_layout(&s))
            .or_else(|| out(Command::new("gsettings").args(["get", "org.gnome.desktop.input-sources", "sources"])).and_then(|s| gnome_layout(&s)))
    } else if desktop.contains("KDE") {
        dirs::config_dir().and_then(|d| std::fs::read_to_string(d.join("kxkbrc")).ok()).and_then(|s| kde_layout(&s))
    } else {
        None
    };
    found
        .or_else(|| {
            (std::env::var_os("DISPLAY").is_some() && std::env::var_os("WAYLAND_DISPLAY").is_none())
                .then(|| out(Command::new("setxkbmap").arg("-query")).and_then(|s| fields(&s, "layout:", "variant:")))
                .flatten()
        })
        .or_else(|| out(Command::new("localectl").arg("status")).and_then(|s| fields(&s, "X11 Layout:", "X11 Variant:")))
        .unwrap_or_else(|| ("us".into(), String::new()))
}

/// First entry of comma-separated layout/variant lists (`de,us` → `de`).
fn first(layout: &str, variant: &str, index: usize) -> Option<(String, String)> {
    let l = layout.split(',').nth(index).or(layout.split(',').next())?.trim().to_string();
    let v = variant.split(',').nth(index).unwrap_or("").trim().to_string();
    (!l.is_empty()).then_some((l, v))
}

/// `hyprctl devices -j`: the layout Hyprland applies to TuxDeck's virtual keyboard. Each
/// keyboard has its own active layout; a new one starts with the first.
fn hyprland_layout(json: &str) -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let kbs = v.get("keyboards")?.as_array()?;
    let name = |k: &serde_json::Value| k.get("name").and_then(|n| n.as_str()).unwrap_or("").to_lowercase();
    let ours = kbs.iter().find(|k| name(k).contains("tuxdeck"));
    let kb = ours.or_else(|| kbs.iter().find(|k| k.get("main").and_then(|m| m.as_bool()) == Some(true))).or(kbs.first())?;
    let s = |k: &str| kb.get(k).and_then(|x| x.as_str()).unwrap_or("");
    let index = ours.and_then(|k| k.get("active_layout_index")).and_then(|i| i.as_u64()).unwrap_or(0) as usize;
    first(s("layout"), s("variant"), index)
}

/// gsettings `[('xkb', 'de+nodeadkeys'), ('xkb', 'us')]`: the first xkb source.
fn gnome_layout(s: &str) -> Option<(String, String)> {
    let start = s.find("('xkb', '")? + "('xkb', '".len();
    let id = &s[start..start + s[start..].find('\'')?];
    let (l, v) = id.split_once('+').unwrap_or((id, ""));
    first(l, v, 0)
}

/// `~/.config/kxkbrc`: `LayoutList=de,us`, `VariantList=nodeadkeys,`.
fn kde_layout(s: &str) -> Option<(String, String)> {
    let get = |k: &str| s.lines().find_map(|l| l.trim().strip_prefix(k).map(str::trim)).unwrap_or("");
    first(get("LayoutList="), get("VariantList="), 0)
}

/// `layout:     de` lines (setxkbmap -query, localectl status).
fn fields(s: &str, layout: &str, variant: &str) -> Option<(String, String)> {
    let get = |k: &str| s.lines().find_map(|l| l.trim().strip_prefix(k).map(str::trim)).unwrap_or("");
    first(get(layout), get(variant), 0)
}

/// The keymap of the session's layout (looked up again after a while: the user may
/// switch layouts). `None` without libxkbcommon or for an unknown layout.
pub fn current() -> Option<std::sync::Arc<Keymap>> {
    type Cached = Option<(Instant, (String, String), Option<std::sync::Arc<Keymap>>)>;
    static CACHE: parking_lot::Mutex<Cached> = parking_lot::Mutex::new(None);
    let mut c = CACHE.lock();
    if let Some((at, _, map)) = c.as_ref() {
        if at.elapsed() < Duration::from_secs(10) {
            return map.clone();
        }
    }
    let layout = session_layout();
    let map = match c.as_ref() {
        Some((_, l, map)) if *l == layout => map.clone(),
        _ => compile(&layout.0, &layout.1).map(std::sync::Arc::new),
    };
    *c = Some((Instant::now(), layout, map.clone()));
    map
}

/// Which keys give which characters in a layout, with Shift and AltGr (level three).
pub fn compile(layout: &str, variant: &str) -> Option<Keymap> {
    let xkb = xkbcommon_option()?;
    let name = if variant.is_empty() { layout.to_string() } else { format!("{layout}({variant})") };
    let (l, v, empty) = (CString::new(layout).ok()?, CString::new(variant).ok()?, CString::new("").ok()?);
    unsafe {
        let ctx = (xkb.xkb_context_new)(xkb_context_flags::XKB_CONTEXT_NO_ENVIRONMENT_NAMES);
        if ctx.is_null() {
            return None;
        }
        let names = xkb_rule_names { rules: empty.as_ptr(), model: empty.as_ptr(), layout: l.as_ptr(), variant: v.as_ptr(), options: empty.as_ptr() };
        let keymap = (xkb.xkb_keymap_new_from_names)(ctx, &names, xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS);
        (xkb.xkb_context_unref)(ctx);
        if keymap.is_null() {
            return None;
        }
        let state = (xkb.xkb_state_new)(keymap);
        let index = |n: &str| CString::new(n).map(|c| (xkb.xkb_keymap_mod_get_index)(keymap, c.as_ptr())).unwrap_or(u32::MAX);
        let bit = |i: u32| if i < 32 { 1u32 << i } else { 0 };
        let (shift, altgr) = (bit(index("Shift")), bit(index("Mod5")));
        let (min, max) = ((xkb.xkb_keymap_min_keycode)(keymap), (xkb.xkb_keymap_max_keycode)(keymap));
        let mut chars = HashMap::new();
        // Fewest modifiers first: a character keeps its simplest way to type it.
        for (mods, with_shift, with_altgr) in [(0, false, false), (shift, true, false), (altgr, false, true), (shift | altgr, true, true)] {
            if (with_altgr && altgr == 0) || (with_shift && shift == 0) {
                continue;
            }
            (xkb.xkb_state_update_mask)(state, mods, 0, 0, 0, 0, 0);
            for kc in min..=max.min(255 + 8) {
                let cp = (xkb.xkb_state_key_get_utf32)(state, kc);
                let Some(c) = char::from_u32(cp).filter(|c| *c != '\0' && kc >= 8) else { continue };
                chars.entry(c).or_insert(Stroke { code: (kc - 8) as u16, shift: with_shift, altgr: with_altgr });
            }
        }
        (xkb.xkb_state_unref)(state);
        (xkb.xkb_keymap_unref)(keymap);
        (!chars.is_empty()).then_some(Keymap { name, chars })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_of_desktops() {
        let hypr = r#"{"keyboards": [{"name": "x", "layout": "de,us", "variant": "nodeadkeys,", "active_layout_index": 1, "main": false},
                                     {"name": "y", "layout": "fr,de", "variant": ",", "active_layout_index": 1, "main": true}]}"#;
        // Not the main keyboard's switched layout: the virtual keyboard starts with the first.
        assert_eq!(hyprland_layout(hypr), Some(("fr".into(), "".into())));
        let ours = r#"{"keyboards": [{"name": "at-kbd", "layout": "us,de", "variant": ",", "active_layout_index": 1, "main": true},
                                     {"name": "tuxdeck-virtual-keyboard", "layout": "us,de", "variant": ",", "active_layout_index": 1, "main": false}]}"#;
        assert_eq!(hyprland_layout(ours), Some(("de".into(), "".into())));
        assert_eq!(gnome_layout("[('xkb', 'de+nodeadkeys'), ('xkb', 'us')]"), Some(("de".into(), "nodeadkeys".into())));
        assert_eq!(gnome_layout("[('ibus', 'anthy'), ('xkb', 'ch+fr')]"), Some(("ch".into(), "fr".into())));
        assert_eq!(gnome_layout("@a(ss) []"), None);
        assert_eq!(kde_layout("[Layout]\nDisplayNames=,\nLayoutList=fr,us\nVariantList=azerty,\n"), Some(("fr".into(), "azerty".into())));
        assert_eq!(fields("rules:      evdev\nmodel:      pc105\nlayout:     de\n", "layout:", "variant:"), Some(("de".into(), "".into())));
        assert_eq!(
            fields("   System Locale: LANG=de_CH.UTF-8\n       X11 Layout: ch\n      X11 Variant: de_nodeadkeys\n", "X11 Layout:", "X11 Variant:"),
            Some(("ch".into(), "de_nodeadkeys".into()))
        );
    }

    #[test]
    fn characters_of_a_layout() {
        // Only where libxkbcommon and the layout data are installed.
        let Some(de) = compile("de", "") else {
            eprintln!("libxkbcommon not found: skipped");
            return;
        };
        let s = |code, shift, altgr| Some(Stroke { code, shift, altgr });
        assert_eq!(de.stroke('z'), s(21, false, false), "QWERTZ: z is where the US y is");
        assert_eq!(de.stroke('Z'), s(21, true, false));
        assert_eq!(de.stroke('ü'), s(26, false, false));
        assert_eq!(de.stroke('@'), s(16, false, true), "AltGr+Q");
        assert_eq!(de.stroke('1'), s(2, false, false));
        assert_eq!(de.stroke('!'), s(2, true, false));
        assert_eq!(de.stroke('€'), s(18, false, true));
        let us = compile("us", "").unwrap();
        assert_eq!(us.stroke('z'), s(44, false, false));
        assert_eq!(us.stroke('ü'), None);
    }
}
