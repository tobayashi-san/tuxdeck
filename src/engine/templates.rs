//! The profiles TuxDeck brings: the starter profile ("Standard") and the samples Work,
//! Play and Streaming, made for Linux (built-in actions, catalogue plugins, programs
//! installed here). One look for all of them: dark keys with a light line icon, the
//! title below it and a small dot in the colour of the key's group; keys whose state
//! matters (live, recording, muted) turn red while it is on. The icons are Elgato's open
//! icon set (the bundled icon pack).

use super::*;
use crate::compat::{self, To};

#[derive(rust_embed::RustEmbed)]
#[folder = "assets/templates/"]
struct TemplateIcons;

/// What a key does.
enum Do {
    /// An installed program, found by name when the profile is created (empty command
    /// if there is none).
    App(&'static str),
    /// A folder, file, URL or command line.
    Open(&'static str),
    Website(&'static str),
    Plugin(&'static str),
    Media(u8),
    /// Keyboard shortcut: evdev code, Qt key code, Ctrl, Shift.
    Hotkey(u32, u32, bool, bool),
    Timer(u64),
    Sleep,
    Brightness,
}

/// Groups of keys and their colour (the dot), the same in every profile.
#[derive(Clone, Copy)]
enum Group {
    Media,
    Voice,
    Apps,
    Tools,
    System,
    Live,
    Scene,
    Games,
}

impl Group {
    fn color(self) -> &'static str {
        match self {
            Group::Media => "#2dd4bf",
            Group::Voice => "#fb7185",
            Group::Apps => "#60a5fa",
            Group::Tools => "#fbbf24",
            Group::System => "#94a3b8",
            Group::Live => "#f87171",
            Group::Scene => "#a78bfa",
            Group::Games => "#4ade80",
        }
    }
}

struct Key {
    title: &'static str,
    /// Icon of Elgato's set (outline).
    icon: &'static str,
    group: Group,
    /// Red while the action's second state is on (live, recording, muted).
    active: bool,
    action: Do,
}

const fn k(title: &'static str, icon: &'static str, group: Group, action: Do) -> Key {
    Key { title, icon, group, active: false, action }
}

/// A key whose second state is "on" (drawn red).
const fn on(title: &'static str, icon: &'static str, group: Group, action: Do) -> Key {
    Key { title, icon, group, active: true, action }
}

pub struct Template {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Catalogue plugins the keys use.
    pub plugins: &'static [&'static str],
    /// Pages of 4×2 keys: top row the profile's main actions, bottom row the controls.
    pages: &'static [&'static [Key]],
    /// Titles of the three keys the Pedal gets.
    pedal: [&'static str; 3],
}

const OBS: &str = "de.felitendo.obs-tools";
const DISCORD: &str = "me.amankhanna.oadiscord";
const LOCK: &str = "loginctl lock-session";
const MIC: &str = "wpctl set-mute @DEFAULT_AUDIO_SOURCE@ toggle || pactl set-source-mute @DEFAULT_SOURCE@ toggle";
const PRINT: Do = Do::Hotkey(99, 0x0100_0009, false, false);

use Group::*;

pub const STANDARD: &str = "standard";

pub const TEMPLATES: &[Template] = &[
    Template {
        id: STANDARD,
        name: "Standard",
        description: "Music and volume, programs, screenshot, timer, lock and sleep.",
        plugins: &[],
        pages: &[
            &[
                k("Previous", "skip-back", Media, Do::Media(0)),
                k("Play/Pause", "play-pause", Media, Do::Media(1)),
                k("Next", "skip-forward", Media, Do::Media(2)),
                k("Lock", "lock", System, Do::Open(LOCK)),
                k("Quieter", "volume-down", Media, Do::Media(6)),
                k("Louder", "volume", Media, Do::Media(5)),
                k("Mute sound", "volume-muted", Voice, Do::Media(4)),
                k("Sleep", "night", System, Do::Sleep),
            ],
            &[
                k("Files", "folder-open", Apps, Do::Open("~")),
                k("Browser", "globe", Apps, Do::App("Browser")),
                k("Terminal", "terminal", Apps, Do::App("Terminal")),
                k("Screenshot", "screenshot", Tools, PRINT),
                k("Editor", "edit", Apps, Do::App("Editor")),
                k("Mail", "mail", Apps, Do::Open("xdg-open mailto:")),
                k("Timer", "timer", Tools, Do::Timer(5 * 60)),
                k("Brightness", "brightness", System, Do::Brightness),
            ],
        ],
        pedal: ["Previous", "Play/Pause", "Next"],
    },
    Template {
        id: "work",
        name: "Work",
        description: "Mail, calendar, browser, a focus timer, microphone; copy, paste and undo.",
        plugins: &[],
        pages: &[
            &[
                k("Mail", "mail", Apps, Do::Open("xdg-open mailto:")),
                k("Calendar", "calendar", Apps, Do::App("Calendar")),
                k("Browser", "globe", Apps, Do::App("Browser")),
                k("Focus 25", "timer", Tools, Do::Timer(25 * 60)),
                k("Microphone", "microphone-off", Voice, Do::Open(MIC)),
                k("Calculator", "grid", Apps, Do::App("Calculator")),
                k("Downloads", "download", Apps, Do::Open("~/Downloads")),
                k("Lock", "lock", System, Do::Open(LOCK)),
            ],
            &[
                k("Copy", "copy", Tools, Do::Hotkey(46, 67, true, false)),
                k("Paste", "paste", Tools, Do::Hotkey(47, 86, true, false)),
                k("Cut", "cut", Tools, Do::Hotkey(45, 88, true, false)),
                k("Undo", "undo", Tools, Do::Hotkey(44, 90, true, false)),
                k("Files", "folder-open", Apps, Do::Open("~")),
                k("Terminal", "terminal", Apps, Do::App("Terminal")),
                k("Editor", "edit", Apps, Do::App("Editor")),
                k("Screenshot", "screenshot", Tools, PRINT),
            ],
        ],
        pedal: ["Copy", "Paste", "Screenshot"],
    },
    Template {
        id: "play",
        name: "Play",
        description: "Discord voice, music, game launchers.",
        plugins: &[DISCORD],
        pages: &[
            &[
                on("Mute", "microphone-off", Voice, Do::Plugin("me.amankhanna.oadiscord.togglemute")),
                on("Deafen", "headphones-off", Voice, Do::Plugin("me.amankhanna.oadiscord.toggledeafen")),
                on("Voice channel", "headphones", Voice, Do::Plugin("me.amankhanna.oadiscord.voicechannel")),
                k("Steam", "game", Games, Do::App("Steam")),
                k("Previous", "skip-back", Media, Do::Media(0)),
                k("Play/Pause", "play-pause", Media, Do::Media(1)),
                k("Next", "skip-forward", Media, Do::Media(2)),
                k("Mute sound", "volume-muted", Voice, Do::Media(4)),
            ],
            &[
                k("Heroic", "rocket", Games, Do::App("Heroic")),
                k("Lutris", "library", Games, Do::App("Lutris")),
                k("System monitor", "cpu", Games, Do::App("System Monitor")),
                k("Screenshot", "screenshot", Tools, PRINT),
                k("Quieter", "volume-down", Media, Do::Media(6)),
                k("Louder", "volume", Media, Do::Media(5)),
                on("Push to talk", "wave-mic", Voice, Do::Plugin("me.amankhanna.oadiscord.pushtotalk")),
                k("Discord", "message", Voice, Do::App("Discord")),
            ],
        ],
        pedal: ["Previous", "Play/Pause", "Next"],
    },
    Template {
        id: "stream",
        name: "Streaming",
        description: "OBS: go live, record, scenes, microphone, markers, virtual camera.",
        plugins: &[OBS],
        pages: &[
            &[
                on("Live", "stream", Live, Do::Plugin("de.felitendo.obs-tools.streamtoggle")),
                on("Record", "record", Live, Do::Plugin("de.felitendo.obs-tools.recordtoggle")),
                k("Previous scene", "chevrons-left", Scene, Do::Plugin("de.felitendo.obs-tools.previousscene")),
                k("Next scene", "chevrons-right", Scene, Do::Plugin("de.felitendo.obs-tools.nextscene")),
                on("Mic", "microphone-off", Voice, Do::Plugin("de.felitendo.obs-tools.mutetoggle")),
                k("Marker", "chapter-marker", Live, Do::Plugin("de.felitendo.obs-tools.recordtools")),
                k("Scene", "scene-collection", Scene, Do::Plugin("de.felitendo.obs-tools.smartscene")),
                k("OBS Studio", "video", Apps, Do::App("OBS Studio")),
            ],
            &[
                on("Virtual camera", "webcam", Scene, Do::Plugin("de.felitendo.obs-tools.virtualcam")),
                on("Studio mode", "presentation", Scene, Do::Plugin("de.felitendo.obs-tools.studiomode")),
                k("Screenshot", "screenshot", Tools, Do::Plugin("de.felitendo.obs-tools.screenshot")),
                k("Countdown", "timer", Tools, Do::Timer(5 * 60)),
                k("Quieter", "volume-down", Media, Do::Media(6)),
                k("Louder", "volume", Media, Do::Media(5)),
                k("Play/Pause", "play-pause", Media, Do::Media(1)),
                k("Chat", "message", Tools, Do::Website("https://dashboard.twitch.tv/")),
            ],
        ],
        pedal: ["Record", "Mic", "Next scene"],
    },
];

/// What a position of a profile's page holds.
enum Place<'a> {
    Key(&'a Key),
    Prev,
    Next,
}

/// The pages of a profile for a model, each a list of (key index, what). The 4×2 pages
/// keep their shape: side by side where they fit (XL: all on one page), else one per
/// deck page (Neo and + change pages by touch or swipe; bigger decks get Previous/Next
/// Page keys in the bottom corners of a free row). Decks narrower than four keys get the
/// keys in order with page keys in the bottom corners; the Pedal three keys of its own.
fn layout<'a>(t: &'a Template, model: &Model) -> Vec<Vec<(usize, Place<'a>)>> {
    let all: Vec<&Key> = t.pages.iter().flat_map(|p| p.iter()).collect();
    let cap = model.key_count();
    let (cols, rows) = (model.cols as usize, model.rows as usize);
    if model.key_px == 0 {
        let keys = t.pedal.iter().filter_map(|title| all.iter().find(|k| k.title == *title).copied());
        return vec![keys.take(cap).map(Place::Key).enumerate().collect()];
    }
    if cols >= 4 && rows >= 2 {
        let (bx, by) = (cols / 4, rows / 2);
        let block = |page: &'a [Key], bi: usize| {
            let (ox, oy) = ((bi % bx) * 4, (bi / bx) * 2);
            page.iter().enumerate().map(move |(i, k)| ((oy + i / 4) * cols + ox + i % 4, Place::Key(k)))
        };
        if bx * by >= t.pages.len() {
            return vec![t.pages.iter().enumerate().flat_map(|(i, p)| block(p, i)).collect()];
        }
        let n = t.pages.len();
        return t
            .pages
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut page: Vec<_> = block(p, 0).collect();
                if rows > 2 {
                    if i > 0 {
                        page.push(((rows - 1) * cols, Place::Prev));
                    }
                    if i + 1 < n {
                        page.push((cap - 1, Place::Next));
                    }
                }
                page
            })
            .collect();
    }
    let (prev_at, next_at) = (cap - cols, cap - 1);
    let mut pages = Vec::new();
    let mut rest = &all[..];
    while !rest.is_empty() {
        let prev = !pages.is_empty();
        let next = rest.len() > cap - prev as usize;
        let mut page = Vec::new();
        let mut keys = rest.iter();
        for i in 0..cap {
            if prev && i == prev_at {
                page.push((i, Place::Prev));
            } else if next && i == next_at {
                page.push((i, Place::Next));
            } else if let Some(k) = keys.next() {
                page.push((i, Place::Key(k)));
            }
        }
        rest = keys.as_slice();
        pages.push(page);
    }
    pages
}

/// Key background of the profiles' look (a CSS gradient, changeable in the editor): dark,
/// dark red while the key's state is on.
fn key_background(active: bool) -> &'static str {
    if active {
        "linear-gradient(180deg, #3a1214 0%, #1c0a0b 100%)"
    } else {
        "linear-gradient(180deg, #1b1c20 0%, #0c0c0e 100%)"
    }
}

/// A key image in the profiles' look: light line icon (red while on) and the group's dot
/// on a transparent key, so the key's background shows; `preview` paints the background
/// in too (sample picker). The title is drawn below the icon by the key itself.
fn key_image(icon: &str, color: &str, active: bool, preview: bool) -> Option<String> {
    use base64::Engine as _;
    let glyph = crate::resources::pack_glyph(icon)?;
    let fg = if active { "#f87171" } else { "#e5e7eb" };
    let bg = if preview {
        let (top, bottom) = if active { ("#3a1214", "#1c0a0b") } else { ("#1b1c20", "#0c0c0e") };
        format!(
            r##"<defs><linearGradient id="bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/></linearGradient></defs><rect width="144" height="144" rx="18" fill="url(#bg)"/>"##
        )
    } else {
        String::new()
    };
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">{bg}<circle cx="123" cy="21" r="7" fill="{color}"/><g transform="translate(42,26) scale(2.5)" fill="{fg}">{glyph}</g></svg>"##
    );
    Some(format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg)))
}

/// An icon of the earlier sample profiles (`assets/templates/icons/`): keys that still
/// show its old version are updated once (see `update_bundled_icons`).
pub(crate) fn icon_url(name: &str) -> Option<String> {
    use base64::Engine as _;
    let f = TemplateIcons::get(&format!("icons/{name}.svg"))?;
    Some(format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(f.data.as_ref())))
}

impl Engine {
    /// The sample profiles for the picker: name, description and the first page's keys.
    pub fn templates_json(&self) -> Value {
        json!(TEMPLATES
            .iter()
            .map(|t| json!({"id": t.id, "name": crate::i18n::tr(t.name), "description": crate::i18n::tr(t.description),
                            "icons": t.pages[0].iter().map(|k| key_image(k.icon, k.group.color(), false, true)).collect::<Vec<_>>()}))
            .collect::<Vec<_>>())
    }

    /// Create a sample profile for a device model (its plugins are installed already).
    pub fn create_from_template(&mut self, id: &str, model_id: &str) -> Result<String> {
        let t = TEMPLATES.iter().find(|t| t.id == id).with_context(|| crate::tr!("unknown sample profile"))?;
        let model = crate::device::MODELS.iter().find(|m| m.model_id == model_id).with_context(|| crate::tr!("unknown device model"))?;
        let p = self.profile_from_template(t, model, &crate::i18n::tr(t.name));
        p.save_all()?;
        let id = p.id.clone();
        self.profiles.insert(id.clone(), p);
        self.emit(json!({"event": "state"}));
        Ok(id)
    }

    /// A profile with the keys of `t`, laid out for `model` (not saved yet).
    pub(super) fn profile_from_template(&self, t: &Template, model: &Model, name: &str) -> Profile {
        let mut p = Profile::new(&self.paths.profiles(), name, model.model_id);
        let cols = model.cols as usize;
        for (i, places) in layout(t, model).into_iter().enumerate() {
            let page_id = if i == 0 {
                p.manifest.pages.pages[0].clone()
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                p.pages.insert(id.clone(), PageManifest::default());
                p.manifest.pages.pages.push(id.clone());
                id
            };
            let page = p.pages.get_mut(&page_id).unwrap();
            for (n, place) in places {
                let coord = format!("{},{}", n % cols, n / cols);
                let a = match place {
                    Place::Key(key) => self.template_key(key),
                    Place::Prev | Place::Next => {
                        let uuid = if matches!(place, Place::Prev) { PAGE_PREV } else { PAGE_NEXT };
                        let d = builtin::def(uuid).unwrap();
                        ActionInstance::new(uuid, &crate::i18n::tr(d.name), d.states)
                    }
                };
                page.controller_mut("Keypad").actions.insert(coord, a);
            }
            if model.kind == Kind::Neo {
                let d = builtin::def(DIGITAL_TIME).unwrap();
                let mut a = ActionInstance::new(DIGITAL_TIME, d.name, 1);
                a.settings = json!({"layout": "digital_time_01", "theme": "", "24HourFormat": true});
                page.controller_mut("Neo").actions.insert(NEO_INFOBAR.into(), a);
            }
        }
        p
    }

    /// The action of a profile key with its image and title.
    fn template_key(&self, key: &Key) -> ActionInstance {
        let (uuid, settings) = match key.action {
            // Not installed: no command, the key waits for one of the user's own.
            Do::App(name) => (
                OPEN,
                json!({"path": match crate::apps::resolve_exact("", name) {
                    Some(crate::apps::Target::App { exec, .. }) => crate::apps::launch_command(&exec),
                    _ => String::new(),
                }}),
            ),
            Do::Open(path) => (OPEN, json!({"path": path})),
            Do::Website(url) => (WEBSITE, json!({"openInBrowser": true, "path": url})),
            Do::Plugin(uuid) => (uuid, json!({})),
            Do::Media(i) => (MULTIMEDIA, json!({"actionIdx": i})),
            Do::Hotkey(l, q, c, s) => (HOTKEY, compat::replacement_settings(To::Hotkey(l, q, c, s))),
            Do::Timer(secs) => (TIMER, json!({"seconds": secs})),
            Do::Sleep => (SLEEP, json!({})),
            Do::Brightness => (BRIGHTNESS, json!({"actionIdx": BRIGHTNESS_CYCLE})),
        };
        let (name, states) = match builtin::def(uuid) {
            Some(d) => (crate::i18n::tr(d.name), d.states),
            None => match self.registry.plugin_for_action(uuid).and_then(|p| p.action(uuid).map(|a| (p.action_name(a), a.states.len().max(1)))) {
                Some(x) => x,
                // The plugin could not be installed: the key keeps its look and waits for a command.
                None => (crate::i18n::tr("Open"), 1),
            },
        };
        let uuid = if builtin::def(uuid).is_none() && self.registry.plugin_for_action(uuid).is_none() { OPEN } else { uuid };
        let mut a = ActionInstance::new(uuid, &name, states);
        a.settings = if uuid == OPEN && !matches!(key.action, Do::App(_) | Do::Open(_)) { json!({"path": ""}) } else { settings };
        let title = crate::i18n::tr(key.title);
        for (i, st) in a.states.iter_mut().enumerate() {
            let lit = key.active && i == 1;
            st.image = key_image(key.icon, key.group.color(), lit, false);
            st.background = Some(key_background(lit).into());
            st.title = Some(title.clone());
            st.show_title = Some(true);
            st.title_alignment = Some("bottom".into());
            st.title_color = Some(if lit { "#fca5a5" } else { "#a1a1aa" }.into());
            st.font_size = Some(9.0);
        }
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Titles per page, "<" and ">" for the page keys, "" for an empty position.
    fn shape(id: &str, kind: Kind) -> Vec<Vec<String>> {
        let t = TEMPLATES.iter().find(|t| t.id == id).unwrap();
        let model = crate::device::MODELS.iter().find(|m| m.kind == kind).unwrap();
        layout(t, model)
            .into_iter()
            .map(|page| {
                let mut out = vec![String::new(); model.key_count()];
                for (i, place) in page {
                    out[i] = match place {
                        Place::Key(k) => k.title.to_string(),
                        Place::Prev => "<".into(),
                        Place::Next => ">".into(),
                    };
                }
                out
            })
            .collect()
    }

    #[test]
    fn profile_layouts_keep_their_rows() {
        let row = |p: &[String], r: usize, cols: usize| p[r * cols..(r + 1) * cols].to_vec();
        // Neo and +: one 4×2 page per page (touch points / swipe change pages).
        let neo = shape("work", Kind::Neo);
        assert_eq!(neo.len(), 2);
        assert_eq!(row(&neo[0], 0, 4), ["Mail", "Calendar", "Browser", "Focus 25"]);
        assert_eq!(row(&neo[1], 1, 4), ["Files", "Terminal", "Editor", "Screenshot"]);
        assert_eq!(shape("work", Kind::Plus).len(), 2);
        // XL (8×4): the pages side by side and below each other, all on one page.
        let xl = shape("stream", Kind::Xl);
        assert_eq!(xl.len(), 1);
        assert_eq!(row(&xl[0], 0, 8)[..5], ["Live", "Record", "Previous scene", "Next scene", "Virtual camera"]);
        assert_eq!(row(&xl[0], 1, 8)[4], "Quieter");
        // MK.2 (5×3): one 4×2 page at a time, page keys in the bottom corners.
        let mk2 = shape("standard", Kind::Mk2);
        assert_eq!(mk2.len(), 2);
        assert_eq!(row(&mk2[0], 0, 5), ["Previous", "Play/Pause", "Next", "Lock", ""]);
        assert_eq!(row(&mk2[0], 2, 5), ["", "", "", "", ">"]);
        assert_eq!(row(&mk2[1], 2, 5), ["<", "", "", "", ""]);
        // Mini (3×2): keys in order, page keys in the bottom corners, nothing dropped.
        let mini = shape("work", Kind::Mini);
        assert_eq!(mini[0], ["Mail", "Calendar", "Browser", "Focus 25", "Microphone", ">"]);
        assert_eq!(mini.concat().iter().filter(|t| t.len() > 1).count(), 16);
        // Pedal: three keys of its own.
        assert_eq!(shape("work", Kind::Pedal), [["Copy", "Paste", "Screenshot"]]);
        assert_eq!(shape("stream", Kind::Pedal), [["Record", "Mic", "Next scene"]]);
    }

    #[test]
    fn every_key_has_its_icon() {
        for t in TEMPLATES {
            assert_eq!(t.pages.len(), 2, "{}", t.id);
            for k in t.pages.iter().flat_map(|p| p.iter()) {
                assert!(key_image(k.icon, k.group.color(), false, false).is_some(), "{}: icon {}", t.id, k.icon);
            }
            for p in t.pages {
                assert_eq!(p.len(), 8, "{}: 4×2 pages", t.id);
            }
            assert!(t.pedal.iter().all(|title| t.pages.iter().flat_map(|p| p.iter()).any(|k| k.title == *title)), "{}: pedal keys", t.id);
        }
        // The group's dot and a light icon on a transparent key (its background shows);
        // red while on; the picker's preview has the dark key painted in.
        use base64::Engine as _;
        let svg = |url: String| String::from_utf8(base64::engine::general_purpose::STANDARD.decode(url.split(',').nth(1).unwrap()).unwrap()).unwrap();
        let off = svg(key_image("record", Group::Live.color(), false, false).unwrap());
        assert!(off.contains(r##"fill="#f87171"/>"##) && off.contains(r##"fill="#e5e7eb""##) && !off.contains("<rect"), "{off}");
        assert!(svg(key_image("record", Group::Live.color(), true, false).unwrap()).contains(r##"<g transform="translate(42,26) scale(2.5)" fill="#f87171">"##));
        assert!(svg(key_image("record", Group::Live.color(), false, true).unwrap()).contains("<rect"));
        assert!(crate::render::css_paint(key_background(false), &mut String::new(), "x").starts_with("url(#"), "a gradient TuxDeck draws");
        let img = crate::render::rasterize(&off, 144, 144);
        let px = |x, y| img.get_pixel(x, y).0;
        assert!(px(123, 21)[0] > 200 && px(4, 140)[3] == 0, "{:?} {:?}", px(123, 21), px(4, 140));
        assert!((40..104).any(|x| px(x, 56)[0] > 180), "icon drawn");
    }
}
