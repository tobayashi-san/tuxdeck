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
mod devices;
mod display;
mod editing;
mod keys;
mod protocol;
mod system;

pub use actions::{multi_lists, set_multi_lists, with_new_ids};
use actions::{nested_entry_mut, KeyLogicState};
pub use display::PreviewJob;
use system::{expand_home, is_executable, play_audio};
pub use system::{kill_processes, open_url, set_runtime, spawn_detached, spawn_task};

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
    pub events: broadcast::Sender<String>,
    pub runtime: HashMap<String, Runtime>,
    pub contexts: HashMap<String, Loc>,
    pub plugin_conns: HashMap<String, PluginConn>,
    /// What each plugin did over the SDK (Linux check, diagnostics).
    pub plugin_stats: HashMap<String, PluginStats>,
    pub pis: HashMap<String, UnboundedSender<String>>,
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
        let mut e = Engine {
            paths,
            settings,
            registry,
            profiles: BTreeMap::new(),
            devices: BTreeMap::new(),
            input,
            api_token,
            events,
            runtime: HashMap::new(),
            contexts: HashMap::new(),
            plugin_conns: HashMap::new(),
            plugin_stats: HashMap::new(),
            pis: HashMap::new(),
            self_ref: None,
            active_app: String::new(),
            key_logic: HashMap::new(),
            clocks: (Instant::now(), std::time::SystemTime::now()),
        };
        e.reload_profiles();
        e.install_plugin_profiles();
        e
    }

    /// Import the profiles plugins ship (manifest `Profiles`) once, like the official app.
    pub(super) fn install_plugin_profiles(&mut self) {
        let mut todo = Vec::new();
        for p in &self.registry.plugins {
            for prof in p.manifest.profiles.iter().filter(|x| x.auto_install != Some(false)) {
                let key = format!("{}/{}", p.uuid, prof.name);
                let installed = self.settings.plugin_profiles.get(&key).is_some_and(|ids| ids.iter().any(|id| self.profiles.contains_key(id)));
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

    pub(super) fn scan_registry(paths: &Paths, settings: &Settings) -> Registry {
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
