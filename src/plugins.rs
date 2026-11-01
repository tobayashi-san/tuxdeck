//! Plugin registry: Elgato `.sdPlugin` manifests (SDK v1/v2) plus the native
//! built-in actions of the official app.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[allow(dead_code)] // deserialized manifest fields kept for completeness
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ManifestState {
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub multi_action_image: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub show_title: Option<bool>,
    #[serde(default)]
    pub title_alignment: Option<String>,
    #[serde(default)]
    pub title_color: Option<String>,
    #[serde(default)]
    pub font_size: Option<Value>,
    #[serde(default)]
    pub font_style: Option<String>,
    #[serde(default)]
    pub font_underline: Option<bool>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ManifestAction {
    #[serde(rename = "UUID")]
    pub uuid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub tooltip: Option<String>,
    #[serde(default)]
    pub states: Vec<ManifestState>,
    #[serde(default)]
    pub controllers: Option<Vec<String>>,
    #[serde(default)]
    pub property_inspector_path: Option<String>,
    #[serde(default)]
    pub supported_in_multi_actions: Option<bool>,
    #[serde(default)]
    pub supported_in_key_logic_actions: Option<bool>,
    #[serde(default)]
    pub visible_in_actions_list: Option<bool>,
    #[serde(default)]
    pub disable_automatic_states: Option<bool>,
    #[serde(default)]
    pub encoder: Option<Value>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Manifest {
    #[serde(rename = "UUID", default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub category_icon: Option<String>,
    #[serde(default)]
    pub code_path: Option<String>,
    #[serde(default)]
    pub code_path_mac: Option<String>,
    #[serde(default)]
    pub code_path_win: Option<String>,
    /// Non-standard extension: explicit Linux entry point.
    #[serde(default)]
    pub code_path_lin: Option<String>,
    #[serde(default)]
    pub property_inspector_path: Option<String>,
    #[serde(default)]
    pub actions: Vec<ManifestAction>,
    #[serde(rename = "SDKVersion", default)]
    pub sdk_version: Option<u32>,
    #[serde(default)]
    pub nodejs: Option<Value>,
    /// Applications whose start and end the plugin wants to hear about
    /// (`{"mac": [bundle ids], "windows": [program names]}`).
    #[serde(rename = "ApplicationsToMonitor", default)]
    pub applications_to_monitor: Option<Value>,
    #[serde(rename = "PrivateAPI", default)]
    pub private_api: bool,
    /// Profiles shipped with the plugin (`<Name>.streamDeckProfile` in its folder).
    #[serde(default)]
    pub profiles: Vec<ManifestProfile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ManifestProfile {
    pub name: String,
    #[serde(default)]
    pub auto_install: Option<bool>,
}

/// How a plugin's backend can be executed on Linux.
#[derive(Debug, Clone, PartialEq)]
pub enum Runtime {
    /// Implemented natively by this application.
    Native,
    /// Node.js script.
    Node(PathBuf),
    /// Native Linux executable (via `CodePathLin`).
    Executable(PathBuf),
    /// HTML/JavaScript plugin (SDK v1 style), hosted in a headless Chromium.
    Html(String),
    /// Cannot be run on Linux (macOS/Windows binary, HTML plugin, ...).
    Unsupported(String),
    /// Property-inspector only plugin without backend.
    None,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Plugin {
    pub uuid: String,
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub runtime: Runtime,
    pub builtin: bool,
    /// Localized strings (`<lang>.json`), keyed by manifest string or action UUID.
    pub strings: Map<String, Value>,
}

impl Plugin {
    pub fn load(dir: &Path, builtin: bool, lang: &str) -> Result<Plugin> {
        let raw = fs::read(dir.join("manifest.json")).with_context(|| format!("{}: no manifest.json", dir.display()))?;
        if is_protected(&raw) {
            bail!(crate::tr!("protected plugin (encrypted manifest, only works with the official app)"));
        }
        // Some manifests contain a UTF-8 BOM.
        let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw);
        let manifest: Manifest = serde_json::from_slice(raw).with_context(|| format!("parsing {}/manifest.json", dir.display()))?;
        let uuid = manifest.uuid.clone().unwrap_or_else(|| dir.file_name().unwrap().to_string_lossy().trim_end_matches(".sdPlugin").to_string());
        let runtime = if manifest.private_api && manifest.code_path.is_none() { Runtime::Native } else { detect_runtime(dir, &manifest) };
        let mut strings = Map::new();
        for l in [lang, "en"] {
            if let Ok(b) = fs::read(dir.join(format!("{l}.json"))) {
                if let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(b.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&b)) {
                    // SDK v2 nests under "Localization"; built-ins use the top level.
                    strings = match m.get("Localization") {
                        Some(Value::Object(inner)) => {
                            let mut s = m.clone();
                            s.extend(inner.clone());
                            s
                        }
                        _ => m,
                    };
                    break;
                }
            }
        }
        Ok(Plugin { uuid, dir: dir.to_path_buf(), manifest, runtime, builtin, strings })
    }

    pub fn name(&self) -> String {
        let n = self.strings.get("Name").and_then(|v| v.as_str()).map(str::to_string).unwrap_or_else(|| self.manifest.name.clone());
        if n.trim().is_empty() {
            self.uuid.clone()
        } else {
            n
        }
    }

    pub fn action_name(&self, a: &ManifestAction) -> String {
        self.strings.get(&a.uuid).and_then(|v| v.get("Name")).and_then(|v| v.as_str()).map(str::to_string).unwrap_or_else(|| a.name.clone())
    }

    pub fn action_tooltip(&self, a: &ManifestAction) -> String {
        self.strings.get(&a.uuid).and_then(|v| v.get("Tooltip")).and_then(|v| v.as_str()).map(str::to_string).or_else(|| a.tooltip.clone()).unwrap_or_default()
    }

    pub fn action(&self, uuid: &str) -> Option<&ManifestAction> {
        self.manifest.actions.iter().find(|a| a.uuid == uuid)
    }

    pub fn supported(&self) -> bool {
        !matches!(self.runtime, Runtime::Unsupported(_))
    }

    /// Property inspector path (relative to plugin dir) for an action.
    pub fn pi_path(&self, a: &ManifestAction) -> Option<String> {
        a.property_inspector_path.clone().or_else(|| self.manifest.property_inspector_path.clone())
    }
}

fn detect_runtime(dir: &Path, m: &Manifest) -> Runtime {
    if let Some(p) = &m.code_path_lin {
        return Runtime::Executable(dir.join(p));
    }
    let candidates = [m.code_path.as_ref(), m.code_path_mac.as_ref(), m.code_path_win.as_ref()];
    // A JavaScript entry point (SDK v2 / Node.js) runs everywhere.
    for c in candidates.iter().flatten() {
        let lc = c.to_lowercase();
        if lc.ends_with(".js") || lc.ends_with(".mjs") || lc.ends_with(".cjs") {
            return Runtime::Node(dir.join(c));
        }
    }
    let Some(code) = candidates.iter().flatten().next() else {
        return Runtime::None;
    };
    let lc = code.to_lowercase();
    if lc.ends_with(".html") || lc.ends_with(".htm") {
        if html_browser().is_some() {
            return Runtime::Html(code.to_string());
        }
        return Runtime::Unsupported(crate::tr!("HTML plugin: Chromium/Brave/Chrome is required"));
    }
    if lc.ends_with(".exe") {
        return Runtime::Unsupported(crate::tr!("Windows program"));
    }
    let path = dir.join(code);
    // A real ELF binary shipped by the plugin would run on Linux.
    if let Ok(head) = fs::read(&path).map(|b| b.into_iter().take(4).collect::<Vec<u8>>()) {
        if head == b"\x7fELF" {
            return Runtime::Executable(path);
        }
    }
    Runtime::Unsupported(crate::tr!("macOS/Windows program"))
}

/// Manifests of DRM-protected plugins (encrypted, only usable by the official app) start
/// with this magic.
pub fn is_protected(data: &[u8]) -> bool {
    data.starts_with(b"ELGATO")
}

/// Chromium-based browser used to host HTML plugins headless.
pub fn html_browser() -> Option<PathBuf> {
    static B: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    B.get_or_init(|| {
        if let Some(b) = std::env::var_os("TUXDECK_BROWSER") {
            return Some(PathBuf::from(b));
        }
        let path = std::env::var_os("PATH")?;
        for name in ["chromium", "chromium-browser", "google-chrome-stable", "google-chrome", "brave", "brave-browser", "microsoft-edge", "vivaldi"] {
            for d in std::env::split_paths(&path) {
                let p = d.join(name);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        None
    })
    .clone()
}

#[derive(Default)]
pub struct Registry {
    pub plugins: Vec<Plugin>,
    /// action UUID -> plugin index
    by_action: HashMap<String, usize>,
}

impl Registry {
    pub fn scan(dirs: &[(PathBuf, bool)], lang: &str) -> Registry {
        let mut reg = Registry::default();
        for (root, builtin) in dirs {
            let Ok(rd) = fs::read_dir(root) else { continue };
            let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            entries.sort();
            for p in entries {
                if !p.is_dir() || p.extension().and_then(|s| s.to_str()) != Some("sdPlugin") {
                    continue;
                }
                match Plugin::load(&p, *builtin, lang) {
                    Ok(pl) => reg.add(pl),
                    Err(e) => tracing::warn!("plugin {}: {e:#}", p.display()),
                }
            }
        }
        reg
    }

    fn add(&mut self, p: Plugin) {
        // A user-installed plugin overrides a bundled one with the same UUID.
        if let Some(i) = self.plugins.iter().position(|x| x.uuid == p.uuid) {
            self.plugins.remove(i);
            self.reindex();
        }
        let idx = self.plugins.len();
        for a in &p.manifest.actions {
            self.by_action.insert(a.uuid.clone(), idx);
        }
        self.plugins.push(p);
    }

    fn reindex(&mut self) {
        self.by_action.clear();
        for (i, p) in self.plugins.iter().enumerate() {
            for a in &p.manifest.actions {
                self.by_action.insert(a.uuid.clone(), i);
            }
        }
    }

    pub fn plugin_for_action(&self, action_uuid: &str) -> Option<&Plugin> {
        self.by_action.get(action_uuid).map(|&i| &self.plugins[i])
    }

    pub fn plugin(&self, uuid: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.uuid == uuid)
    }
}

/// Resolve an image reference from a manifest ("Images/key" without extension).
pub fn resolve_image(dir: &Path, rel: &str) -> Option<PathBuf> {
    let base = dir.join(rel);
    if base.is_file() {
        return Some(base);
    }
    for ext in ["@2x.png", ".png", ".svg", "@2x.jpg", ".jpg", ".gif"] {
        let p = PathBuf::from(format!("{}{}", base.display(), ext));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Upper bound for the unpacked size of an imported archive (zip bombs).
const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Read one (small) zip entry, counting towards the archive's total unpacked size.
pub fn read_zip_entry(f: &mut impl std::io::Read, unpacked: &mut u64) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let mut buf = Vec::new();
    let left = MAX_UNPACKED_BYTES.saturating_sub(*unpacked);
    f.take(left + 1).read_to_end(&mut buf)?;
    *unpacked += buf.len() as u64;
    if *unpacked > MAX_UNPACKED_BYTES {
        bail!(crate::tr!("archive is too large when unpacked"));
    }
    Ok(buf)
}

/// Stream one zip entry to `out` (callers remove partial output on error).
/// Path of a zip entry, also in archives packed on Windows with `\\` as separator (the
/// official app accepts those). `None` for unsafe names: absolute, `..`, drive letters.
pub fn entry_path(raw: &str) -> Option<PathBuf> {
    let norm = raw.replace('\\', "/");
    if norm.starts_with('/') {
        return None;
    }
    let mut out = PathBuf::new();
    for c in norm.split('/') {
        match c {
            "" | "." => {}
            ".." => return None,
            c if c.contains(':') => return None,
            c => out.push(c),
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// A directory entry, with either separator.
pub fn entry_is_dir(raw: &str) -> bool {
    raw.ends_with('/') || raw.ends_with('\\')
}

pub fn write_zip_entry(f: &mut impl std::io::Read, out: &Path, unpacked: &mut u64) -> Result<()> {
    use std::io::Read as _;
    let left = MAX_UNPACKED_BYTES.saturating_sub(*unpacked);
    let mut file = fs::File::create(out)?;
    *unpacked += std::io::copy(&mut f.take(left + 1), &mut file)?;
    if *unpacked > MAX_UNPACKED_BYTES {
        bail!(crate::tr!("archive is too large when unpacked"));
    }
    Ok(())
}

/// Install a `.streamDeckPlugin` archive (zip containing `<uuid>.sdPlugin/`).
pub fn install_archive(data: &[u8], plugins_dir: &Path) -> Result<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).context("not a zip archive")?;
    let mut root: Option<String> = None;
    for i in 0..zip.len() {
        let f = zip.by_index(i)?;
        if let Some(n) = entry_path(f.name()) {
            if let Some(first) = n.components().next() {
                let s = first.as_os_str().to_string_lossy().to_string();
                // Any case, like on Windows and macOS (`….sdplugin`).
                if s.to_ascii_lowercase().ends_with(".sdplugin") {
                    root = Some(s);
                    break;
                }
            }
        }
    }
    let Some(root) = root else { bail!("archive contains no .sdPlugin directory") };
    // Installed under the usual spelling, which the plugin scan looks for.
    let name = format!("{}.sdPlugin", &root[..root.len() - ".sdPlugin".len()]);
    // Each install unpacks into a folder of its own next to the plugins (not scanned: no
    // .sdPlugin extension) and replaces an installed version only once the new one is
    // complete and loadable.
    let staging = plugins_dir.join(format!(".installing-{}", uuid::Uuid::new_v4().simple()));
    let tmp = staging.join(&name);
    let result = unpack_plugin(&mut zip, &root, &tmp).and_then(|_| Plugin::load(&tmp, false, "en"));
    let plugin = match result {
        Ok(p) => p,
        Err(e) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    // Replacing: one install at a time, and the old version is only removed once the new
    // one is in place.
    static REPLACE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = REPLACE.lock().unwrap_or_else(|e| e.into_inner());
    let target = plugins_dir.join(&name);
    let old = plugins_dir.join(format!(".replaced-{}", uuid::Uuid::new_v4().simple()));
    let had_old = target.exists() && fs::rename(&target, &old).is_ok();
    if let Err(e) = fs::rename(&tmp, &target) {
        if had_old {
            let _ = fs::rename(&old, &target);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(e.into());
    }
    if had_old {
        let _ = fs::remove_dir_all(&old);
    }
    let _ = fs::remove_dir_all(&staging);
    Ok(plugin.uuid)
}

fn unpack_plugin(zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, root: &str, dest: &Path) -> Result<()> {
    let mut unpacked = 0;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let Some(name) = entry_path(f.name()) else { continue };
        let Ok(rel) = name.strip_prefix(root) else { continue };
        let out = dest.join(rel);
        if entry_is_dir(f.name()) {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        write_zip_entry(&mut f, &out, &mut unpacked)?;
        #[cfg(unix)]
        if let Some(mode) = f.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            // Only the executable bit is taken over: never writable for group or others.
            let _ = fs::set_permissions(&out, fs::Permissions::from_mode(if mode & 0o111 != 0 { 0o755 } else { 0o644 }));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plugin package: `files` below the root folder `root`.
    fn package(root: &str, version: &str, files: usize) -> Vec<u8> {
        use std::io::Write;
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        for i in 0..files {
            zw.start_file(format!("{root}/lib/f{i}.js"), o).unwrap();
            zw.write_all(b"//").unwrap();
        }
        zw.start_file(format!("{root}/manifest.json"), o).unwrap();
        let m = format!(r#"{{"Name":"T","UUID":"com.test.inst","Version":"{version}","Author":"t","SDKVersion":2,"CodePath":"p.js","Actions":[]}}"#);
        zw.write_all(m.as_bytes()).unwrap();
        zw.finish().unwrap().into_inner()
    }

    #[test]
    fn installs_run_side_by_side_and_replace_whole() {
        let dir = std::env::temp_dir().join(format!("tuxdeck-install-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // A lower-case folder name is installed under the usual spelling.
        assert_eq!(install_archive(&package("com.test.inst.sdplugin", "1", 3), &dir).unwrap(), "com.test.inst");
        let target = dir.join("com.test.inst.sdPlugin");
        assert!(target.join("manifest.json").is_file());
        // Several installs at once: each is complete, the last one wins, nothing is left over.
        let big = package("com.test.inst.sdPlugin", "2", 400);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| install_archive(&big, &dir).unwrap());
            }
        });
        assert_eq!(fs::read_dir(target.join("lib")).unwrap().count(), 400);
        assert!(fs::read_to_string(target.join("manifest.json")).unwrap().contains(r#""Version":"2""#));
        let names: Vec<String> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["com.test.inst.sdPlugin"]);
        // A broken package leaves the installed one alone.
        assert!(install_archive(&package("com.test.inst.sdPlugin", "3", 0)[..40], &dir).is_err());
        assert!(fs::read_to_string(target.join("manifest.json")).unwrap().contains(r#""Version":"2""#));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn zip_entry_paths() {
        let p = |s: &str| entry_path(s).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(p("com.jk.weather.sdPlugin\\manifest.json").as_deref(), Some("com.jk.weather.sdPlugin/manifest.json"));
        assert_eq!(p("a.sdPlugin/./b/c.js").as_deref(), Some("a.sdPlugin/b/c.js"));
        for bad in ["../x", "/etc/passwd", "C:\\Windows\\x", "a\\..\\..\\b", "", "\\\\server\\share"] {
            assert_eq!(p(bad), None, "{bad}");
        }
        assert!(entry_is_dir("com.jk.weather.sdPlugin\\"));
    }
}
