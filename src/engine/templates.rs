//! Sample profiles (Stream, Work, Play) made for Linux: built-in actions, catalogue
//! plugins and the programs installed here. Icons are our own (`assets/templates/`,
//! made from Elgato's open icon set by `tools/elgato-icon-art.py`).

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
    Open(&'static str),
    Website(&'static str),
    Plugin(&'static str),
    Media(u8),
    /// Keyboard shortcut: evdev code, Qt key code, Ctrl, Shift.
    Hotkey(u32, u32, bool, bool),
    Timer(u64),
}

struct Key {
    title: &'static str,
    icon: &'static str,
    action: Do,
}

const fn k(title: &'static str, icon: &'static str, action: Do) -> Key {
    Key { title, icon, action }
}

pub struct Template {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Catalogue plugins the keys use.
    pub plugins: &'static [&'static str],
    pages: &'static [&'static [Key]],
    /// Titles of the three keys the Pedal gets.
    pedal: [&'static str; 3],
}

const OBS: &str = "de.felitendo.obs-tools";
const DISCORD: &str = "me.amankhanna.oadiscord";

pub const TEMPLATES: &[Template] = &[
    Template {
        id: "stream",
        name: "Stream",
        description: "OBS: record, go live, switch scenes, mute the mic.",
        plugins: &[OBS],
        pages: &[&[
            k("OBS Studio", "stream-obs", Do::App("OBS Studio")),
            k("Record", "stream-record", Do::Plugin("de.felitendo.obs-tools.recordtoggle")),
            k("Go live", "stream-live", Do::Plugin("de.felitendo.obs-tools.streamtoggle")),
            k("Scene", "stream-scene", Do::Plugin("de.felitendo.obs-tools.smartscene")),
            k("Mic", "stream-mic", Do::Plugin("de.felitendo.obs-tools.mutetoggle")),
            k("Screenshot", "stream-screenshot", Do::Plugin("de.felitendo.obs-tools.screenshot")),
            k("Virtual camera", "stream-webcam", Do::Plugin("de.felitendo.obs-tools.virtualcam")),
            k("Chat", "stream-chat", Do::Website("https://dashboard.twitch.tv/")),
        ]],
        pedal: ["Record", "Mic", "Scene"],
    },
    Template {
        id: "work",
        name: "Work",
        description: "Files, browser, mail, editor, terminal, copy & paste, a focus timer.",
        plugins: &[],
        pages: &[
            &[
                k("Files", "work-files", Do::Open("~")),
                k("Browser", "work-web", Do::App("Browser")),
                k("Mail", "work-mail", Do::Open("xdg-open mailto:")),
                k("Calculator", "work-calc", Do::App("Calculator")),
                k("Editor", "work-editor", Do::App("Editor")),
                k("Terminal", "work-terminal", Do::App("Terminal")),
                k("Screenshot", "work-screenshot", Do::Hotkey(99, 0x0100_0009, false, false)),
                k("Lock", "work-lock", Do::Open("loginctl lock-session")),
            ],
            &[
                k("Copy", "work-copy", Do::Hotkey(46, 67, true, false)),
                k("Paste", "work-paste", Do::Hotkey(47, 86, true, false)),
                k("Downloads", "work-downloads", Do::Open("~/Downloads")),
                k("Focus", "work-timer", Do::Timer(25 * 60)),
            ],
        ],
        pedal: ["Copy", "Paste", "Screenshot"],
    },
    Template {
        id: "play",
        name: "Play",
        description: "Game launchers, Discord voice, music controls.",
        plugins: &[DISCORD],
        pages: &[&[
            k("Steam", "play-games", Do::App("Steam")),
            k("Heroic", "play-launcher", Do::App("Heroic")),
            k("Mute", "play-mic", Do::Plugin("me.amankhanna.oadiscord.togglemute")),
            k("Deafen", "play-deafen", Do::Plugin("me.amankhanna.oadiscord.toggledeafen")),
            k("Previous", "play-prev", Do::Media(0)),
            k("Play/Pause", "play-play", Do::Media(1)),
            k("Next", "play-next", Do::Media(2)),
            k("Mute sound", "play-mute", Do::Media(4)),
        ]],
        pedal: ["Previous", "Play/Pause", "Next"],
    },
];

/// What a position of a sample profile's page holds.
enum Place<'a> {
    Key(&'a Key),
    Prev,
    Next,
}

/// The pages of a sample profile for a model, each a list of (key index, what): all keys
/// on one page if they fit; the sample's own pages on the Neo and the + (they change
/// pages by touch or swipe); otherwise pages with Previous/Next Page keys in the bottom
/// corners. The Pedal gets three keys of its own.
fn layout<'a>(t: &'a Template, model: &Model) -> Vec<Vec<(usize, Place<'a>)>> {
    let all: Vec<&Key> = t.pages.iter().flat_map(|p| p.iter()).collect();
    let cap = model.key_count();
    let numbered = |keys: Vec<&'a Key>| keys.into_iter().map(Place::Key).enumerate().collect::<Vec<_>>();
    if model.key_px == 0 {
        return vec![numbered(t.pedal.iter().filter_map(|title| all.iter().find(|k| k.title == *title).copied()).take(cap).collect())];
    }
    if all.len() <= cap {
        return vec![numbered(all)];
    }
    if (model.touch_points > 0 || model.encoders > 0) && t.pages.iter().all(|p| p.len() <= cap) {
        return t.pages.iter().map(|p| numbered(p.iter().collect())).collect();
    }
    let (prev_at, next_at) = (cap - model.cols as usize, cap - 1);
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

pub(crate) fn icon_url(name: &str) -> Option<String> {
    use base64::Engine as _;
    let f = TemplateIcons::get(&format!("icons/{name}.svg"))?;
    Some(format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(f.data.as_ref())))
}

impl Engine {
    /// The sample profiles for the picker: name, description and the first page's icons.
    pub fn templates_json(&self) -> Value {
        json!(TEMPLATES
            .iter()
            .map(|t| json!({"id": t.id, "name": crate::i18n::tr(t.name), "description": crate::i18n::tr(t.description),
                            "icons": t.pages[0].iter().map(|k| icon_url(k.icon)).collect::<Vec<_>>()}))
            .collect::<Vec<_>>())
    }

    /// Create a sample profile for a device model (its plugins are installed already).
    pub fn create_from_template(&mut self, id: &str, model_id: &str) -> Result<String> {
        let t = TEMPLATES.iter().find(|t| t.id == id).with_context(|| crate::tr!("unknown sample profile"))?;
        let model = crate::device::MODELS.iter().find(|m| m.model_id == model_id).with_context(|| crate::tr!("unknown device model"))?;
        let mut p = Profile::new(&self.paths.profiles(), &crate::i18n::tr(t.name), model.model_id);
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
                let key = match place {
                    Place::Key(key) => key,
                    Place::Prev | Place::Next => {
                        let uuid = if matches!(place, Place::Prev) { PAGE_PREV } else { PAGE_NEXT };
                        let d = builtin::def(uuid).unwrap();
                        page.controller_mut("Keypad").actions.insert(coord, ActionInstance::new(uuid, &crate::i18n::tr(d.name), d.states));
                        continue;
                    }
                };
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
                let icon = icon_url(key.icon);
                for st in a.states.iter_mut() {
                    st.image = icon.clone();
                    st.title = Some(crate::i18n::tr(key.title));
                    st.show_title = Some(false);
                }
                page.controller_mut("Keypad").actions.insert(coord, a);
            }
            if model.kind == Kind::Neo {
                let d = builtin::def(DIGITAL_TIME).unwrap();
                let mut a = ActionInstance::new(DIGITAL_TIME, d.name, 1);
                a.settings = json!({"layout": "digital_time_01", "theme": "", "24HourFormat": true});
                page.controller_mut("Neo").actions.insert(NEO_INFOBAR.into(), a);
            }
        }
        p.save_all()?;
        let id = p.id.clone();
        self.profiles.insert(id.clone(), p);
        self.emit(json!({"event": "state"}));
        Ok(id)
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
    fn sample_layouts_fit_the_deck() {
        // Big decks: one page with everything.
        let mk2 = shape("work", Kind::Mk2);
        assert_eq!(mk2.len(), 1);
        assert_eq!(mk2[0].iter().filter(|t| !t.is_empty()).count(), 12);
        assert_eq!(shape("work", Kind::Xl).len(), 1);
        // Neo and +: the sample's pages (touch points / swipe change pages).
        assert_eq!(shape("work", Kind::Neo).iter().map(|p| p.iter().filter(|t| !t.is_empty()).count()).collect::<Vec<_>>(), [8, 4]);
        assert_eq!(shape("work", Kind::Plus).len(), 2);
        // Mini (3×2): page keys in the bottom corners, nothing dropped.
        let mini = shape("work", Kind::Mini);
        assert_eq!(mini[0], ["Files", "Browser", "Mail", "Calculator", "Editor", ">"]);
        assert_eq!(mini[1], ["Terminal", "Screenshot", "Lock", "<", "Copy", ">"]);
        assert_eq!(mini[2], ["Paste", "Downloads", "Focus", "<", "", ""]);
        assert_eq!(shape("play", Kind::Mini).concat().iter().filter(|t| t.len() > 1).count(), 8);
        // Pedal: three keys of its own.
        assert_eq!(shape("work", Kind::Pedal), [["Copy", "Paste", "Screenshot"]]);
        assert_eq!(shape("play", Kind::Pedal), [["Previous", "Play/Pause", "Next"]]);
    }
}
