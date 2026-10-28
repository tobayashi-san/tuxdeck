use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Folder name under the XDG data and config directories.
const APP: &str = "tuxdeck";
/// Folder name of versions before the rename to TuxDeck.
const OLD_APP: &str = "streamdeck-linux";

#[derive(Clone, Debug)]
pub struct Paths {
    pub data: PathBuf,
    pub config: PathBuf,
}

impl Paths {
    pub fn new() -> Paths {
        let data = std::env::var_os("TUXDECK_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| adopt_old_dir(dirs::data_dir()));
        let config = std::env::var_os("TUXDECK_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| adopt_old_dir(dirs::config_dir()));
        let p = Paths { data, config };
        for d in [p.profiles(), p.user_plugins(), p.builtin_plugins(), p.config.clone()] {
            let _ = std::fs::create_dir_all(d);
        }
        p
    }
    pub fn profiles(&self) -> PathBuf {
        self.data.join("ProfilesV2")
    }
    /// Installed icon packs (`*.sdIconPack`).
    pub fn icon_packs(&self) -> PathBuf {
        self.data.join("IconPacks")
    }
    pub fn user_plugins(&self) -> PathBuf {
        self.data.join("Plugins")
    }
    /// Generated stand-ins for the native plugins of the official app.
    pub fn builtin_plugins(&self) -> PathBuf {
        self.data.join("builtin")
    }
    /// Resources imported from the official installer (`tuxdeck import-elgato`).
    pub fn elgato(&self) -> PathBuf {
        self.data.join("elgato")
    }
    pub fn elgato_plugins(&self) -> PathBuf {
        self.elgato().join("Plugins")
    }
    pub fn layouts(&self) -> Option<PathBuf> {
        let p = self.elgato().join("InfobarLayouts");
        p.is_dir().then_some(p)
    }
    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }
    pub fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }

    /// Data the plugins store through Elgato's SDK. The SDK wants it under
    /// `~/Library/Application Support/com.elgato.StreamDeck` (macOS) or
    /// `%APPDATA%/Elgato/StreamDeck` (Windows); the node shim redirects those paths here.
    pub fn plugin_data(&self) -> PathBuf {
        self.data.join("plugin-data")
    }

    /// The macOS-style folder of older versions (see `plugin_data`).
    pub fn legacy_plugin_data() -> Option<PathBuf> {
        Some(dirs::home_dir()?.join("Library/Application Support/com.elgato.StreamDeck"))
    }

    /// Move plugin data of older versions from `~/Library/...` to `plugin_data` (once) and
    /// remove the then empty `~/Library` folders.
    pub fn migrate_plugin_data(&self) {
        // Only for the regular installation: an instance with its own data directory
        // (tests, TUXDECK_DATA_DIR) must never take the user's data with it.
        if std::env::var_os("TUXDECK_DATA_DIR").is_some() || dirs::data_dir().map(|d| d.join(APP)) != Some(self.data.clone()) {
            return;
        }
        let (Some(old), new) = (Self::legacy_plugin_data(), self.plugin_data()) else { return };
        if !old.is_dir() || old.is_symlink() || new.exists() {
            return;
        }
        let moved = std::fs::rename(&old, &new).is_ok() || (copy_dir(&old, &new).is_ok() && std::fs::remove_dir_all(&old).is_ok());
        if moved {
            tracing::info!("plugin data moved from {} to {}", old.display(), new.display());
            // Only if nothing else lives there.
            for dir in old.ancestors().skip(1).take(2) {
                let _ = std::fs::remove_dir(dir);
            }
        } else {
            tracing::warn!("could not move plugin data from {} to {}", old.display(), new.display());
        }
    }

    /// Secret for the local HTTP API (created on first use, readable only by the user):
    /// other users and programs on the machine can reach 127.0.0.1 as well.
    pub fn api_token(&self) -> std::io::Result<String> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = self.config.join("api-token");
        if let Ok(t) = std::fs::read_to_string(&file) {
            if t.trim().len() >= 32 {
                return Ok(t.trim().to_string());
            }
        }
        std::fs::create_dir_all(&self.config)?;
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&file)?;
        f.write_all(token.as_bytes())?;
        Ok(token)
    }
}

/// `<base>/tuxdeck`; the folder of versions before the rename is moved there once and
/// replaced by a link, so paths that plugins or scripts stored keep working.
fn adopt_old_dir(base: Option<PathBuf>) -> PathBuf {
    let base = base.unwrap_or_else(|| PathBuf::from("."));
    let (old, new) = (base.join(OLD_APP), base.join(APP));
    if old.is_dir() && !old.is_symlink() && !new.exists() {
        match std::fs::rename(&old, &new) {
            Ok(()) => {
                let _ = std::os::unix::fs::symlink(APP, &old);
                tracing::info!("{} moved to {}", old.display(), new.display());
            }
            Err(e) => tracing::warn!("could not move {} to {}: {e}", old.display(), new.display()),
        }
    }
    new
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceSettings {
    #[serde(default)]
    pub profile: String,
    #[serde(default = "default_brightness")]
    pub brightness: u8,
}

fn default_brightness() -> u8 {
    70
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    /// Treat the macOS ⌘ modifier of imported hotkeys as Ctrl.
    #[serde(default = "yes")]
    pub cmd_as_ctrl: bool,
    /// Platform reported to plugins ("mac" | "windows" | "linux").
    #[serde(default = "mac")]
    pub platform: String,
    /// UI and plugin language: "auto" (system language) or a code such as "en", "de".
    #[serde(default = "auto")]
    pub language: String,
    #[serde(default = "ui_port")]
    pub ui_port: u16,
    #[serde(default = "sdk_port")]
    pub sdk_port: u16,
    /// LED color of the Neo touch points when navigation is possible.
    #[serde(default = "white")]
    pub touch_color: String,
    /// Preferred key injection backend (`auto`, `uinput`, `hyprctl`, `wtype`, `xdotool`).
    #[serde(default = "auto")]
    pub injector: String,
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceSettings>,
    #[serde(default)]
    pub global_settings: Map<String, Value>,
    /// Installed profiles shipped with plugins: "<plugin uuid>/<profile name>" -> profile ids.
    #[serde(default)]
    pub plugin_profiles: BTreeMap<String, Vec<String>>,
    /// GitHub repository with the published Linux check results ("" = none).
    #[serde(default = "checks_repo")]
    pub checks_repo: String,
    /// Publish this computer's Linux check results there (needs `gh` with write access).
    #[serde(default)]
    pub checks_publish: bool,
}

fn checks_repo() -> String {
    "tobayashi-san/tuxdeck-plugin-checks".into()
}

/// Repository of the published results before the rename (see `Settings::load`).
const OLD_CHECKS_REPO: &str = "tobayashi-san/streamdeck-linux-plugin-checks";

fn yes() -> bool {
    true
}
fn mac() -> String {
    "mac".into()
}
fn auto() -> String {
    "auto".into()
}
fn white() -> String {
    "#ffffff".into()
}
fn ui_port() -> u16 {
    47800
}
fn sdk_port() -> u16 {
    47801
}

impl Settings {
    pub fn load(p: &Paths) -> Settings {
        let mut s: Settings =
            std::fs::read(p.settings_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| serde_json::from_str("{}").unwrap());
        if s.checks_repo == OLD_CHECKS_REPO {
            s.checks_repo = checks_repo();
        }
        s
    }
    pub fn save(&self, p: &Paths) {
        if let Ok(b) = serde_json::to_vec_pretty(self) {
            let _ = crate::profile::write_atomic(&p.settings_file(), &b);
        }
    }
}
