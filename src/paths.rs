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
        if let Some(old) = Self::legacy_plugin_data() {
            move_plugin_data(&old, &self.plugin_data());
        }
    }

    /// Secret for the local HTTP API (created on first use, readable only by the user):
    /// other users and programs on the machine can reach 127.0.0.1 as well.
    pub fn api_token(&self) -> std::io::Result<String> {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let file = self.config.join("api-token");
        // `mode` only applies to new files: also restrict one created otherwise (copied, restored).
        let private = |f: &std::fs::File| -> std::io::Result<()> {
            if f.metadata()?.permissions().mode() & 0o077 != 0 {
                f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        };
        if let Ok(t) = std::fs::read_to_string(&file) {
            if t.trim().len() >= 32 {
                if let Ok(f) = std::fs::File::open(&file) {
                    let _ = private(&f);
                }
                return Ok(t.trim().to_string());
            }
        }
        std::fs::create_dir_all(&self.config)?;
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&file)?;
        private(&f)?;
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

/// Move the plugin data folder `old` to `new` (unless `new` exists) and remove the two
/// parent folders of `old` if that left them empty.
fn move_plugin_data(old: &std::path::Path, new: &std::path::Path) {
    if !old.is_dir() || old.is_symlink() || new.exists() {
        return;
    }
    let moved = std::fs::rename(old, new).is_ok() || (copy_dir(old, new).is_ok() && std::fs::remove_dir_all(old).is_ok());
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
        let file = p.settings_file();
        let parsed = std::fs::read(&file).ok().map(|b| serde_json::from_slice::<Settings>(&b));
        let mut s = match parsed {
            Some(Ok(s)) => s,
            Some(Err(e)) => {
                // Keep the broken file: the next save would otherwise replace the user's
                // device, plugin and profile settings with the defaults.
                let keep = file.with_file_name(format!("settings.json.broken-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
                let _ = std::fs::rename(&file, &keep);
                tracing::warn!("{} is not valid ({e}); kept as {} and started with default settings", file.display(), keep.display());
                serde_json::from_str("{}").unwrap()
            }
            None => serde_json::from_str("{}").unwrap(),
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sd-paths-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn paths(d: &Path) -> Paths {
        Paths { data: d.join("data"), config: d.join("config") }
    }

    #[test]
    fn layout() {
        let d = tmp("layout");
        let p = paths(&d);
        assert_eq!(p.profiles(), d.join("data/ProfilesV2"));
        assert_eq!(p.icon_packs(), d.join("data/IconPacks"));
        assert_eq!(p.user_plugins(), d.join("data/Plugins"));
        assert_eq!(p.builtin_plugins(), d.join("data/builtin"));
        assert_eq!(p.elgato_plugins(), d.join("data/elgato/Plugins"));
        assert_eq!(p.settings_file(), d.join("config/settings.json"));
        assert_eq!(p.logs(), d.join("data/logs"));
        assert_eq!(p.plugin_data(), d.join("data/plugin-data"));
        assert_eq!(p.layouts(), None);
        std::fs::create_dir_all(d.join("data/elgato/InfobarLayouts")).unwrap();
        assert_eq!(p.layouts(), Some(d.join("data/elgato/InfobarLayouts")));
        assert!(Paths::legacy_plugin_data().unwrap().ends_with("Library/Application Support/com.elgato.StreamDeck"));
        // Never touches data outside the regular installation.
        p.migrate_plugin_data();
        assert!(!p.plugin_data().exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn settings_defaults_and_roundtrip() {
        let d = tmp("settings");
        let p = paths(&d);
        let s = Settings::load(&p);
        assert!(s.cmd_as_ctrl);
        assert_eq!((s.platform.as_str(), s.language.as_str(), s.injector.as_str()), ("mac", "auto", "auto"));
        assert_eq!((s.ui_port, s.sdk_port), (47800, 47801));
        assert_eq!(s.touch_color, "#ffffff");
        assert_eq!(s.checks_repo, "tobayashi-san/tuxdeck-plugin-checks");
        assert!(!s.checks_publish && s.devices.is_empty() && s.global_settings.is_empty() && s.plugin_profiles.is_empty());

        // Saving into a missing config folder fails quietly.
        s.save(&p);
        assert!(!p.settings_file().exists());
        std::fs::create_dir_all(&p.config).unwrap();
        let mut s = s;
        s.cmd_as_ctrl = false;
        s.ui_port = 1234;
        s.devices.insert("SERIAL".into(), DeviceSettings { profile: "abc".into(), brightness: 30 });
        s.global_settings.insert("com.x.plugin".into(), serde_json::json!({"k": 1}));
        s.plugin_profiles.insert("com.x.plugin/Main".into(), vec!["id1".into()]);
        s.save(&p);
        assert!(!p.config.join("settings.tmp").exists());
        let back = Settings::load(&p);
        assert!(!back.cmd_as_ctrl);
        assert_eq!(back.ui_port, 1234);
        assert_eq!(back.devices["SERIAL"].profile, "abc");
        assert_eq!(back.devices["SERIAL"].brightness, 30);
        assert_eq!(back.global_settings["com.x.plugin"]["k"], 1);
        assert_eq!(back.plugin_profiles["com.x.plugin/Main"], ["id1"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn settings_partial_old_and_broken() {
        let d = tmp("settings-old");
        let p = paths(&d);
        std::fs::create_dir_all(&p.config).unwrap();
        let write = |v: &str| std::fs::write(p.settings_file(), v).unwrap();
        // Fields missing in older files get their defaults.
        write(r#"{"platform": "linux", "devices": {"A": {}}, "unknown_key": 1}"#);
        let s = Settings::load(&p);
        assert_eq!(s.platform, "linux");
        assert!(s.cmd_as_ctrl);
        assert_eq!(s.devices["A"].brightness, 70);
        assert_eq!(s.devices["A"].profile, "");
        // Repository of the results before the rename.
        write(r#"{"checks_repo": "tobayashi-san/streamdeck-linux-plugin-checks"}"#);
        assert_eq!(Settings::load(&p).checks_repo, "tobayashi-san/tuxdeck-plugin-checks");
        write(r#"{"checks_repo": ""}"#);
        assert_eq!(Settings::load(&p).checks_repo, "");
        write(r#"{"checks_repo": "me/my-checks"}"#);
        assert_eq!(Settings::load(&p).checks_repo, "me/my-checks");
        // Unreadable files fall back to the defaults.
        write("{not json");
        assert_eq!(Settings::load(&p).ui_port, 47800);
        write(r#"{"ui_port": "high"}"#);
        assert_eq!(Settings::load(&p).platform, "mac");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn api_token_file() {
        let d = tmp("token");
        let p = paths(&d);
        let t = p.api_token().unwrap();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        let file = p.config.join("api-token");
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(p.api_token().unwrap(), t);
        // Surrounding whitespace is ignored; a too short secret is replaced.
        std::fs::write(&file, format!("  {t}\n")).unwrap();
        assert_eq!(p.api_token().unwrap(), t);
        std::fs::write(&file, "short").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let new = p.api_token().unwrap();
        assert_ne!(new, t);
        assert_eq!(new.len(), 64);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), new);
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        // A readable token file (e.g. restored from elsewhere) is made private.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(p.api_token().unwrap(), new);
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rename_migration() {
        let d = tmp("adopt");
        let (old, new) = (d.join(OLD_APP), d.join(APP));
        // Nothing to migrate.
        assert_eq!(adopt_old_dir(Some(d.clone())), new);
        assert!(!new.exists() && !old.exists());
        // The old folder moves and a link keeps the old paths working.
        std::fs::create_dir_all(old.join("ProfilesV2")).unwrap();
        std::fs::write(old.join("ProfilesV2/x.json"), "1").unwrap();
        assert_eq!(adopt_old_dir(Some(d.clone())), new);
        assert!(new.join("ProfilesV2/x.json").is_file());
        assert!(old.is_symlink());
        assert_eq!(std::fs::read_link(&old).unwrap(), Path::new(APP));
        assert_eq!(std::fs::read_to_string(old.join("ProfilesV2/x.json")).unwrap(), "1");
        // Second start: the link is left alone.
        assert_eq!(adopt_old_dir(Some(d.clone())), new);
        assert!(old.is_symlink() && new.join("ProfilesV2/x.json").is_file());
        // Both exist (e.g. a fresh install ran first): nothing moves.
        std::fs::remove_file(&old).unwrap();
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("y"), "").unwrap();
        adopt_old_dir(Some(d.clone()));
        assert!(old.join("y").is_file() && !new.join("y").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn plugin_data_migration() {
        let d = tmp("plugin-data");
        let library = d.join("home/Library");
        let old = library.join("Application Support/com.elgato.StreamDeck");
        let new = d.join("data/plugin-data");
        std::fs::create_dir_all(old.join("com.x.sdPlugin")).unwrap();
        std::fs::write(old.join("com.x.sdPlugin/settings.json"), "{}").unwrap();
        std::fs::create_dir_all(d.join("data")).unwrap();
        move_plugin_data(&old, &new);
        assert!(new.join("com.x.sdPlugin/settings.json").is_file());
        assert!(!old.exists());
        // The emptied ~/Library folders are removed, but not the home folder.
        assert!(!library.exists());
        assert!(d.join("home").is_dir());

        // Never over existing data; other content in ~/Library stays.
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("again"), "").unwrap();
        std::fs::write(library.join("other"), "").unwrap();
        move_plugin_data(&old, &new);
        assert!(old.join("again").is_file() && !new.join("again").exists());
        std::fs::remove_dir_all(&new).unwrap();
        move_plugin_data(&old, &new);
        assert!(new.join("again").is_file() && library.join("other").is_file());
        assert!(!library.join("Application Support").exists());
        // A missing source does nothing.
        move_plugin_data(&d.join("missing"), &d.join("target"));
        assert!(!d.join("target").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn recursive_copy() {
        let d = tmp("copy");
        std::fs::create_dir_all(d.join("a/b/c")).unwrap();
        std::fs::write(d.join("a/top"), "1").unwrap();
        std::fs::write(d.join("a/b/c/deep"), "2").unwrap();
        copy_dir(&d.join("a"), &d.join("z")).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("z/top")).unwrap(), "1");
        assert_eq!(std::fs::read_to_string(d.join("z/b/c/deep")).unwrap(), "2");
        assert!(copy_dir(&d.join("missing"), &d.join("y")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn broken_settings_are_kept() {
        let d = tmp("broken");
        let paths = Paths { data: d.join("data"), config: d.join("config") };
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.settings_file(), br#"{"ui_port": "high", "devices": {"X": {"profile": "P", "brightness": 40}}}"#).unwrap();
        let s = Settings::load(&paths);
        assert_eq!(s.ui_port, 47800);
        assert!(!paths.settings_file().exists(), "broken file moved away before anything overwrites it");
        let kept: Vec<_> =
            std::fs::read_dir(&paths.config).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.broken-")).collect();
        assert_eq!(kept.len(), 1);
        assert!(std::fs::read_to_string(kept[0].path()).unwrap().contains("\"brightness\": 40"));
    }
}
