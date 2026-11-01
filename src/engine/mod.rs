//! Core application state: devices, profiles/pages, key handling, rendering and the
//! built-in actions. Shared behind a mutex by the device threads, the plugin SDK
//! server and the web UI.
//!
//! The `Engine` methods are split by topic: [`devices`] (connect, navigation),
//! [`keys`] (input), [`actions`] (running actions), [`protocol`] (plugin SDK),
//! [`display`] (what the decks show), [`editing`] (web UI edits) and [`system`]
//! (processes).

use crate::builtin::{self, *};
use crate::device::{Input, Kind, Model};
use crate::input::{self, Combo, Injector, InputWorker};
use crate::layout::{self, InfoContent, PageInfo};
use crate::paths::{DeviceSettings, Paths, Settings};
use crate::plugins::{self, Registry, Runtime as PluginRuntime};
use crate::profile::{self, ActionInstance, PageManifest, Profile};
use crate::render::{self, ImageData, KeySpec, Overlay, Title};
use crate::strip;
use anyhow::{anyhow, bail, Context as _, Result};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc::UnboundedSender};

mod actions;
mod compat;
mod devices;
mod display;
mod editing;
mod keys;
mod protocol;
mod system;
pub mod templates;

pub use actions::{multi_lists, set_multi_lists, with_new_ids};
use actions::{nested_entry_mut, KeyLogicState};
pub use compat::looks_imported;
pub use display::PreviewJob;
use system::{exit_problem, launch, quit_launched, url_opener, Launched, OnExit};
use system::{expand_home, has_cmd, is_executable, play_audio};
pub use system::{kill_processes, open_url, scopes_available, set_runtime, spawn_detached, spawn_task};

pub type Shared = Arc<Mutex<Engine>>;

pub enum DevCmd {
    /// Key image description; rendered and encoded by the device thread.
    Key(u8, Box<KeySpec>),
    /// Neo info bar content and the layouts folder; rendered by the device thread.
    Screen(Box<InfoContent>, Option<std::path::PathBuf>),
    /// Stream Deck + touch strip segment (SVG, `w` x screen height at `x`); rasterized by
    /// the device thread.
    Strip {
        x: u16,
        w: u16,
        svg: String,
    },
    Touch(u8, [u8; 3]),
    Brightness(u8),
    #[allow(dead_code)]
    Reset,
}

pub enum DevEvent {
    Connected { id: String, model: &'static Model, firmware: String, tx: std::sync::mpsc::Sender<DevCmd> },
    Disconnected { id: String },
    Input { id: String, input: Input },
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Slot {
    pub controller: String,
    pub coord: String,
}

impl Slot {
    pub fn new(controller: &str, coord: &str) -> Slot {
        Slot { controller: controller.to_string(), coord: coord.to_string() }
    }
    fn key(&self) -> String {
        format!("{}/{}", self.controller, self.coord)
    }
    pub fn col_row(&self) -> (u32, u32) {
        let mut it = self.coord.split(',').map(|s| s.trim().parse::<u32>().unwrap_or(0));
        (it.next().unwrap_or(0), it.next().unwrap_or(0))
    }
}

pub const NEO_LEFT: &str = "0,0";

pub const NEO_INFOBAR: &str = "1,0";

pub const NEO_RIGHT: &str = "2,0";

/// Where an action instance lives.
#[derive(Clone, Debug)]
pub struct Loc {
    pub device: String,
    pub profile: String,
    pub page: String,
    pub slot: Slot,
    /// Index path inside multi actions: (list index, entry index).
    pub nested: Option<(usize, usize)>,
}

struct Press {
    at: Instant,
    profile: String,
    page: String,
    /// The press only woke the deck up: its release does nothing either.
    swallowed: bool,
}

impl DevState {
    pub fn connected(&self) -> bool {
        self.tx.is_some()
    }
}

pub struct DevState {
    pub id: String,
    pub model: &'static Model,
    pub firmware: String,
    pub tx: Option<std::sync::mpsc::Sender<DevCmd>>,
    pub profile: String,
    pub page: String,
    /// Folder navigation stack (parent pages).
    pub stack: Vec<String>,
    pub brightness: u8,
    pub sleeping: bool,
    /// Profile to return to when the application of the current smart profile loses focus.
    pub smart_base: Option<String>,
    sent: HashMap<String, u64>,
    buttons: Vec<bool>,
    encoders: Vec<bool>,
    /// A key shows an animated GIF: refresh often.
    animated: bool,
    /// Profile to return to after a plugin's `switchToProfile`.
    plugin_return: Option<String>,
    /// Keys/dials currently held down and where they were pressed (the release belongs
    /// to the same action even if the page changed in between).
    pressed: HashMap<Slot, Press>,
    /// context -> (plugin uuid, action uuid, payload) for plugin actions currently shown.
    visible: BTreeMap<String, (String, String, Value)>,
}

#[derive(Default)]
pub struct Runtime {
    /// Set by the plugin (`setTitle` / `setImage`), per state or (`None`) for all states.
    pub titles: HashMap<Option<usize>, String>,
    pub images: HashMap<Option<usize>, ImageData>,
    pub overlay: Option<(Overlay, Instant)>,
    pub timer: Option<TimerState>,
    pub audio: Vec<Child>,
    /// The program this key started last (Open Application quits exactly that).
    pub launched: Option<Launched>,
    /// Touch strip values sent by the plugin (`setFeedback`).
    pub feedback: Map<String, Value>,
    /// Touch strip layout set by the plugin (`setFeedbackLayout`).
    pub layout: Option<Value>,
}

impl Runtime {
    pub fn title(&self, state: usize) -> Option<String> {
        self.titles.get(&Some(state)).or(self.titles.get(&None)).cloned()
    }
}

#[derive(Clone)]
pub struct TimerState {
    pub total: Duration,
    pub elapsed: Duration,
    pub started: Option<Instant>,
}

impl TimerState {
    fn remaining(&self) -> Duration {
        let el = self.elapsed + self.started.map(|s| s.elapsed()).unwrap_or_default();
        self.total.saturating_sub(el)
    }
}

/// SDK activity of one plugin since the start (see `/api/plugins/{uuid}/diagnostics`).
#[derive(Default, serde::Serialize)]
pub struct PluginStats {
    pub registrations: u32,
    /// Messages from the plugin by event.
    pub events: BTreeMap<String, u32>,
    /// Messages from the plugin per action context.
    pub contexts: BTreeMap<String, u32>,
    /// Property inspector connections per action context.
    pub inspectors: BTreeMap<String, u32>,
}

#[derive(Default)]
pub struct PluginConn {
    pub tx: Option<UnboundedSender<String>>,
    pub pending: Vec<String>,
    pub child: Option<Child>,
    pub restarts: u32,
    /// When the current process was started.
    pub started: Option<Instant>,
    /// Secret the plugin registers with (its `-pluginUUID`, new for every start): other
    /// local programs can reach the SDK port too, but can't pose as the plugin.
    pub key: Option<String>,
    /// Why the plugin can't start (no or too old Node.js), reported once.
    pub blocked: Option<String>,
}

pub struct Engine {
    /// Monotonic and wall clock at the last tick (detects system suspend/resume).
    clocks: (Instant, std::time::SystemTime),
    pub paths: Paths,
    pub settings: Settings,
    pub registry: Registry,
    pub profiles: BTreeMap<String, Profile>,
    pub devices: BTreeMap<String, DevState>,
    pub input: InputWorker,
    /// Secret of the local HTTP API (see `Paths::api_token`).
    pub api_token: String,
    /// Session cookie value of the browser UI (see `Paths::ui_session`).
    pub ui_session: String,
    /// Port of the plugin file server (`web::plugin_router`), 0 until it listens.
    pub plugin_port: u16,
    /// When the fonts of info bar layouts were last looked for (layouts imported while
    /// the service runs bring theirs).
    fonts_checked: Instant,
    /// Simulated decks of a test instance (they can be unplugged and plugged in again).
    pub virtual_decks: HashMap<String, (&'static Model, std::sync::mpsc::Sender<DevCmd>)>,
    /// Secret of the current HTML plugin host page (`/plugin-host?k=…`).
    pub html_host_key: String,
    pub events: broadcast::Sender<String>,
    pub runtime: HashMap<String, Runtime>,
    pub contexts: HashMap<String, Loc>,
    pub plugin_conns: HashMap<String, PluginConn>,
    /// What each plugin did over the SDK (Linux check, diagnostics).
    pub plugin_stats: HashMap<String, PluginStats>,
    pub pis: HashMap<String, UnboundedSender<String>>,
    /// Keys are being released because their deck disconnected (see `trigger`).
    releasing: bool,
    /// (plugin, application) pairs of monitored applications that run now.
    pub apps_running: HashSet<(String, String)>,
    /// Contexts whose plugin got willAppear only because their PI is open (see `pi_opened`).
    pub pi_shown: HashSet<String>,
    /// Property inspectors register with an alias instead of their action's context
    /// (handed out by `/api/pi`, which needs the API token): (alias, context), newest last.
    pub pi_aliases: Vec<(String, String)>,
    /// Alias of each connected property inspector (context -> alias).
    pub pi_alias: HashMap<String, String>,
    pub self_ref: Option<std::sync::Weak<Mutex<Engine>>>,
    /// Window class of the focused application (smart profiles).
    pub active_app: String,
    /// Press tracking of Key Logic keys, per (device, slot).
    key_logic: HashMap<(String, Slot), KeyLogicState>,
}

impl PreviewJob {
    pub fn render(self) -> Vec<u8> {
        match self {
            PreviewJob::Info(c, layouts) => render::encode_png(&layout::render_info_bar(layouts.as_deref(), &c, 248, 58)),
            PreviewJob::Svg(svg, w, h) => render::encode_png(&render::rasterize(&svg, w, h)),
            PreviewJob::Key(spec, size) => render::cached(&spec, size, "png", render::encode_png),
        }
    }
}

pub fn context_id(device: &str, action_id: &str) -> String {
    let mut a = std::collections::hash_map::DefaultHasher::new();
    (device, action_id, 1u8).hash(&mut a);
    let mut b = std::collections::hash_map::DefaultHasher::new();
    (action_id, device, 2u8).hash(&mut b);
    format!("{:016X}{:016X}", a.finish(), b.finish())
}

fn spec_hash<T: Hash>(v: &T) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

fn parse_color(c: &str) -> [u8; 3] {
    let c = c.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(c.get(i..i + 2).unwrap_or("ff"), 16).unwrap_or(255);
    [p(0), p(2), p(4)]
}

impl Engine {
    pub fn new(paths: Paths, events: broadcast::Sender<String>) -> Engine {
        let settings = Settings::load(&paths);
        paths.migrate_plugin_data();
        crate::i18n::set_language(&crate::i18n::resolve(&settings.language));
        crate::resources::write_builtin_plugins(&paths);
        crate::resources::write_default_iconpack(&paths);
        let registry = Self::scan_registry(&paths, &settings);
        let input = InputWorker::start(&settings.injector);
        let api_token = paths.api_token().unwrap_or_else(|e| {
            tracing::error!("cannot store the API token: {e}");
            format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
        });
        let ui_session = paths.ui_session().unwrap_or_else(|e| {
            tracing::error!("cannot store the UI session: {e}");
            format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
        });
        let mut e = Engine {
            paths,
            settings,
            registry,
            profiles: BTreeMap::new(),
            devices: BTreeMap::new(),
            input,
            api_token,
            ui_session,
            plugin_port: 0,
            virtual_decks: HashMap::new(),
            fonts_checked: Instant::now(),
            html_host_key: String::new(),
            events,
            runtime: HashMap::new(),
            contexts: HashMap::new(),
            plugin_conns: HashMap::new(),
            plugin_stats: HashMap::new(),
            pis: HashMap::new(),
            pi_shown: HashSet::new(),
            apps_running: HashSet::new(),
            releasing: false,
            pi_aliases: Vec::new(),
            pi_alias: HashMap::new(),
            self_ref: None,
            active_app: String::new(),
            key_logic: HashMap::new(),
            clocks: (Instant::now(), std::time::SystemTime::now()),
        };
        e.reload_profiles();
        e.mark_old_imports();
        e.update_bundled_icons();
        e.install_plugin_profiles();
        e
    }

    /// Keys that still show an icon of the starter or sample profiles from before TuxDeck
    /// used Elgato's icons get the new icon of the same key, once. Only images that are
    /// exactly one of those old icons change; a backup is made first.
    fn update_bundled_icons(&mut self) {
        if self.settings.elgato_icons {
            return;
        }
        let replacement = |img: &str| -> Option<String> {
            use base64::Engine as _;
            let b64 = img.strip_prefix("data:image/svg+xml;base64,")?;
            let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
            let digest = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
            let (kind, name, _) = OLD_BUNDLED_ICONS.iter().find(|(_, _, d)| *d == digest)?;
            match *kind {
                "starter" => crate::resources::starter_icon_data_url(name),
                _ => templates::icon_url(name),
            }
        };
        let todo: Vec<(String, String)> = self
            .profiles
            .values()
            .flat_map(|p| p.pages.iter().map(move |(id, pg)| (p, id, pg)))
            .filter(|(_, _, pg)| pg.iter_actions().any(|(_, _, a)| a.states.iter().any(|s| s.image.as_deref().and_then(replacement).is_some())))
            .map(|(p, id, _)| (p.id.clone(), id.clone()))
            .collect();
        if !todo.is_empty() {
            if let Err(e) = crate::backup::before_change(&self.paths, "before-icons") {
                tracing::warn!("no backup, icons not updated: {e:#}");
                return;
            }
            for (profile, page) in &todo {
                if let Some(pg) = self.profiles.get_mut(profile).and_then(|p| p.pages.get_mut(page)) {
                    for c in pg.controllers.iter_mut() {
                        for a in c.actions.values_mut() {
                            for st in a.states.iter_mut() {
                                if let Some(new) = st.image.as_deref().and_then(replacement) {
                                    st.image = Some(new);
                                }
                            }
                        }
                    }
                }
                self.save_page(profile, page);
            }
            tracing::info!("updated the icons of {} pages to Elgato's icons", todo.len());
        }
        self.settings.elgato_icons = true;
        self.settings.save(&self.paths);
    }

    /// Profiles imported before imports were marked: mark those that look like they come
    /// from the official app, once, so they are still offered the conversion.
    fn mark_old_imports(&mut self) {
        if self.settings.import_marks {
            return;
        }
        let registry = &self.registry;
        for p in self.profiles.values_mut() {
            if !p.imported() && looks_imported(p, registry) {
                p.set_imported(true);
                let _ = p.save_manifest();
            }
        }
        self.settings.import_marks = true;
        self.settings.save(&self.paths);
    }

    /// Import the profiles plugins ship (manifest `Profiles`) once, like the official app:
    /// a profile the user deleted stays deleted.
    pub(super) fn install_plugin_profiles(&mut self) {
        let mut todo = Vec::new();
        for p in &self.registry.plugins {
            for prof in p.manifest.profiles.iter().filter(|x| x.auto_install != Some(false)) {
                let key = format!("{}/{}", p.uuid, prof.name);
                let installed = self.settings.plugin_profiles.contains_key(&key);
                let file = p.dir.join(format!("{}.streamDeckProfile", prof.name));
                if !installed && file.is_file() {
                    todo.push((key, prof.name.clone(), file, p.name()));
                }
            }
        }
        for (key, name, file, plugin) in todo {
            let res = std::fs::read(&file).map_err(anyhow::Error::from).and_then(|data| {
                let dir = profile::import_archive(&data, &self.paths.profiles())?;
                let load = || -> Result<Profile> {
                    let mut p = Profile::load(&dir)?;
                    p.manifest.name = name.clone();
                    for page in p.pages.values_mut() {
                        for c in page.controllers.iter_mut() {
                            for a in c.actions.values_mut() {
                                *a = with_new_ids(a);
                            }
                        }
                    }
                    p.save_all()?;
                    Ok(p)
                };
                load().inspect_err(|_| {
                    let _ = std::fs::remove_dir_all(&dir);
                })
            });
            match res {
                Ok(p) => {
                    self.settings.plugin_profiles.entry(key).or_default().push(p.id.clone());
                    self.profiles.insert(p.id.clone(), p);
                    self.log("info", crate::tr!("Profile \"{name}\" of {plugin} installed", name = name, plugin = plugin));
                }
                Err(e) => tracing::warn!("profile {name} of {plugin}: {e:#}"),
            }
            self.settings.save(&self.paths);
        }
    }

    pub fn scan_registry(paths: &Paths, settings: &Settings) -> Registry {
        Registry::scan(
            &[(paths.builtin_plugins(), true), (paths.elgato_plugins(), true), (paths.user_plugins(), false)],
            crate::i18n::plugin_language(&crate::i18n::resolve(&settings.language)),
        )
    }

    pub fn rescan_plugins(&mut self) {
        self.registry = Self::scan_registry(&self.paths, &self.settings);
        self.install_plugin_profiles();
        self.emit(json!({"event": "actions"}));
        self.emit(json!({"event": "state"}));
    }

    pub fn reload_profiles(&mut self) {
        self.profiles = profile::load_all(&self.paths.profiles()).into_iter().map(|p| (p.id.clone(), p)).collect();
    }

    pub fn emit(&self, v: Value) {
        let _ = self.events.send(v.to_string());
    }

    pub fn log(&self, level: &str, msg: impl Into<String>) {
        let msg = msg.into();
        match level {
            "error" => tracing::error!("{msg}"),
            "warn" => tracing::warn!("{msg}"),
            _ => tracing::info!("{msg}"),
        }
        self.emit(json!({"event": "log", "level": level, "message": msg}));
    }
}

/// The icons of the starter and sample profiles before TuxDeck used Elgato's icons:
/// (kind, name, FNV-1a 64 of the SVG file) – see `Engine::update_bundled_icons`.
const OLD_BUNDLED_ICONS: &[(&str, &str, u64)] = &[
    ("starter", "brightness", 0x2fae30036200921b),
    ("starter", "home", 0xcffdb11320051287),
    ("starter", "lock", 0x4047975d5ce40ab5),
    ("starter", "mute", 0x389f9d12bf317825),
    ("starter", "next", 0x023532eaef442257),
    ("starter", "play", 0xdeb0ba39673a0366),
    ("starter", "previous", 0xe85cba9d1d2605ab),
    ("starter", "sleep", 0xf47df090d32702d8),
    ("starter", "timer", 0x79a695781d09d2c9),
    ("starter", "volume-down", 0x3f993686ad87d104),
    ("starter", "volume-up", 0x5c48b3cef52fcf66),
    ("templates", "play-chat", 0x0f64a4920bbac71d),
    ("templates", "play-deafen", 0xf78f6bab8acfb503),
    ("templates", "play-games", 0x295aa44bb05d92e5),
    ("templates", "play-launcher", 0xbeca9ecdcf357578),
    ("templates", "play-mic", 0x1516bca4685af982),
    ("templates", "play-mute", 0xd37a43edc4aa2a71),
    ("templates", "play-next", 0xcdb354fafcebf0e3),
    ("templates", "play-play", 0xa6045287ce49445f),
    ("templates", "play-prev", 0x93dd16d82c19f207),
    ("templates", "play-shuffle", 0xcbf5e73723130adf),
    ("templates", "play-voice", 0x71bc87cd819b824b),
    ("templates", "play-volume", 0xd338cb45c1ce33f1),
    ("templates", "stream-chat", 0x2f534e85f7db1878),
    ("templates", "stream-live", 0xf1c6bbe4aeb951bd),
    ("templates", "stream-mic", 0x0998988b92d56a79),
    ("templates", "stream-obs", 0x14dbc001cb3fc09f),
    ("templates", "stream-record", 0x1a99c4139df4cf89),
    ("templates", "stream-scene", 0x054d1b5e4d96a400),
    ("templates", "stream-screenshot", 0x29bb2ab8f0f2b45a),
    ("templates", "stream-webcam", 0xbadea676bc2ff700),
    ("templates", "work-calc", 0xfdeb054f617b5363),
    ("templates", "work-copy", 0x3395bad51b30bec0),
    ("templates", "work-downloads", 0xd783c76e49d39f07),
    ("templates", "work-editor", 0xe6e7db6eb57c009c),
    ("templates", "work-files", 0x2d84612490c6b962),
    ("templates", "work-lock", 0x2764cac528424c38),
    ("templates", "work-mail", 0x9752e398e46f80a3),
    ("templates", "work-paste", 0xac66ebda82aaead0),
    ("templates", "work-screenshot", 0x4c5108cd35210e92),
    ("templates", "work-terminal", 0xd44558fcdf9330f1),
    ("templates", "work-timer", 0x8fbb630bdb3788de),
    ("templates", "work-web", 0x782c6ebc5cae0163),
];
