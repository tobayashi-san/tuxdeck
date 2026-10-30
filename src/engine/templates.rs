//! Sample profiles (Stream, Work, Play) made for Linux: built-in actions, catalogue
//! plugins and the programs installed here. Icons are our own (`assets/templates/`,
//! drawn by `tools/template-art.py`).

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
    },
];

fn icon_url(name: &str) -> Option<String> {
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
        let (cols, rows) = (model.cols as usize, model.rows as usize);
        for (i, keys) in t.pages.iter().enumerate() {
            let page_id = if i == 0 {
                p.manifest.pages.pages[0].clone()
            } else {
                let id = uuid::Uuid::new_v4().to_string();
                p.pages.insert(id.clone(), PageManifest::default());
                p.manifest.pages.pages.push(id.clone());
                id
            };
            let page = p.pages.get_mut(&page_id).unwrap();
            for (n, key) in keys.iter().enumerate().take(cols * rows) {
                let (uuid, settings) = match key.action {
                    // Not installed: no command, the key waits for one of the user's own.
                    Do::App(name) => (
                        OPEN,
                        json!({"path": match crate::apps::resolve("", name) {
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
                page.controller_mut("Keypad").actions.insert(format!("{},{}", n % cols, n / cols), a);
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
