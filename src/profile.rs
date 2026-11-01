//! Elgato profile format (`ProfilesV2`, manifest version 2.0), compatible with the
//! official app so profiles can be imported/exported as `.streamDeckProfile` files.
//!
//! Layout on disk:
//! ```text
//! <UUID>.sdProfile/
//!   manifest.json                 ProfileManifest
//!   Images/
//!   Profiles/<encoded page id>/   one directory per page *and* per folder
//!     manifest.json               PageManifest
//!     Images/<id>.png             custom key images, referenced as "Images/<id>.png"
//! ```

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Alphabet the official app uses to name page directories (base32 without `U`).
const PAGE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHIJKLMNOPQRSTVW";

/// A relative path from profile data (image names, …) that stays inside its base folder.
pub fn inside(base: &Path, rel: &str) -> Option<PathBuf> {
    let r = Path::new(rel);
    r.components().all(|c| matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir)).then(|| base.join(r))
}

/// Encode a page UUID to its directory name (e.g. `J8TTF15EM553B1GO54W7BO0COCZ`).
pub fn encode_page_dir(page_id: &str) -> Result<String> {
    let u = uuid::Uuid::parse_str(page_id).with_context(|| format!("invalid page id {page_id}"))?;
    let n = u.as_u128();
    let mut s = String::with_capacity(27);
    // 128 bits padded to 130 bits = 26 base32 digits.
    for i in 0..26i32 {
        let shift = 123i32 - 5 * i;
        let digit = if shift >= 0 { (n >> shift) & 31 } else { (n << (-shift)) & 31 };
        s.push(PAGE_ALPHABET[digit as usize] as char);
    }
    s.push('Z');
    Ok(s)
}

pub fn decode_page_dir(name: &str) -> Option<String> {
    // Format 3.0 profiles use the plain (upper-case) UUID as directory name.
    if let Ok(u) = uuid::Uuid::parse_str(name) {
        return Some(u.to_string());
    }
    let body = name.strip_suffix('Z')?;
    if body.len() != 26 {
        return None;
    }
    let mut n: u128 = 0;
    for (i, c) in body.bytes().enumerate() {
        let d = PAGE_ALPHABET.iter().position(|&a| a == c)? as u128;
        if i < 25 {
            n = (n << 5) | d;
        } else {
            n = (n << 3) | (d >> 2);
        }
    }
    Some(uuid::Uuid::from_u128(n).to_string())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct DeviceRef {
    #[serde(default)]
    pub model: String,
    #[serde(rename = "UUID", default)]
    pub uuid: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Pages {
    #[serde(default)]
    pub current: String,
    #[serde(default)]
    pub default: String,
    #[serde(default)]
    pub pages: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ProfileManifest {
    #[serde(default)]
    pub device: DeviceRef,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub pages: Pages,
    #[serde(default = "version2")]
    pub version: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn version2() -> String {
    "2.0".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StateDef {
    #[serde(rename = "FontFamily", skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(rename = "FontSize", skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    #[serde(rename = "FontStyle", skip_serializing_if = "Option::is_none")]
    pub font_style: Option<String>,
    #[serde(rename = "FontUnderline", skip_serializing_if = "Option::is_none")]
    pub font_underline: Option<bool>,
    #[serde(rename = "Image", skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Key background colour (`#rrggbb`), shown behind the image. Our own extension.
    #[serde(rename = "Background", skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(rename = "OutlineThickness", skip_serializing_if = "Option::is_none")]
    pub outline_thickness: Option<f32>,
    #[serde(rename = "ShowTitle", skip_serializing_if = "Option::is_none")]
    pub show_title: Option<bool>,
    #[serde(rename = "Title", skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "TitleAlignment", skip_serializing_if = "Option::is_none")]
    pub title_alignment: Option<String>,
    #[serde(rename = "TitleColor", skip_serializing_if = "Option::is_none")]
    pub title_color: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionInstance {
    #[serde(rename = "ActionID", default = "new_uuid")]
    pub action_id: String,
    #[serde(rename = "LinkedTitle", default = "yes")]
    pub linked_title: bool,
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(rename = "Plugin", default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<Value>,
    #[serde(rename = "Resources", default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Value>,
    #[serde(rename = "Settings", default = "empty_obj")]
    pub settings: Value,
    #[serde(rename = "State", default)]
    pub state: usize,
    #[serde(rename = "States", default)]
    pub states: Vec<StateDef>,
    #[serde(rename = "UUID")]
    pub uuid: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn yes() -> bool {
    true
}
pub fn empty_obj() -> Value {
    Value::Object(Map::new())
}

impl ActionInstance {
    pub fn new(uuid: &str, name: &str, states: usize) -> Self {
        ActionInstance {
            action_id: new_uuid(),
            linked_title: true,
            name: name.to_string(),
            plugin: None,
            resources: None,
            settings: empty_obj(),
            state: 0,
            states: vec![StateDef::default(); states.max(1)],
            uuid: uuid.to_string(),
            extra: Map::new(),
        }
    }

    pub fn current_state(&self) -> Option<&StateDef> {
        self.states.get(self.state).or(self.states.first())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Controller {
    #[serde(rename = "Actions", default, deserialize_with = "actions_or_null")]
    pub actions: BTreeMap<String, ActionInstance>,
    #[serde(rename = "Type")]
    pub kind: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn actions_or_null<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, ActionInstance>, D::Error> {
    Ok(Option::<BTreeMap<String, ActionInstance>>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PageManifest {
    #[serde(rename = "Controllers", default)]
    pub controllers: Vec<Controller>,
    #[serde(rename = "Icon", default)]
    pub icon: String,
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The official app sometimes writes `{Invalid}` as controller type for the Neo
/// info bar controller of older profiles.
pub fn normalize_controller(kind: &str) -> &str {
    match kind {
        "{Invalid}" => "Neo",
        k => k,
    }
}

impl PageManifest {
    pub fn controller(&self, kind: &str) -> Option<&Controller> {
        // Prefer an exact match, then the legacy alias with actions.
        self.controllers
            .iter()
            .find(|c| c.kind == kind && !c.actions.is_empty())
            .or_else(|| self.controllers.iter().find(|c| normalize_controller(&c.kind) == kind && !c.actions.is_empty()))
            .or_else(|| self.controllers.iter().find(|c| c.kind == kind))
    }

    pub fn controller_mut(&mut self, kind: &str) -> &mut Controller {
        let idx = self
            .controllers
            .iter()
            .position(|c| c.kind == kind && !c.actions.is_empty())
            .or_else(|| self.controllers.iter().position(|c| normalize_controller(&c.kind) == kind && !c.actions.is_empty()))
            .or_else(|| self.controllers.iter().position(|c| c.kind == kind));
        let idx = match idx {
            Some(i) => i,
            None => {
                self.controllers.push(Controller { kind: kind.to_string(), ..Default::default() });
                self.controllers.len() - 1
            }
        };
        &mut self.controllers[idx]
    }

    pub fn action(&self, kind: &str, coord: &str) -> Option<&ActionInstance> {
        self.controller(kind)?.actions.get(coord)
    }

    pub fn action_mut(&mut self, kind: &str, coord: &str) -> Option<&mut ActionInstance> {
        self.controller_mut(kind).actions.get_mut(coord)
    }

    /// All (controller, coord, action) triples.
    pub fn iter_actions(&self) -> impl Iterator<Item = (&str, &str, &ActionInstance)> {
        self.controllers.iter().flat_map(|c| c.actions.iter().map(move |(k, a)| (normalize_controller(&c.kind), k.as_str(), a)))
    }
}

#[derive(Debug, Clone)]
pub struct Profile {
    /// Upper-case UUID, also the directory stem.
    pub id: String,
    pub dir: PathBuf,
    pub manifest: ProfileManifest,
    pub pages: HashMap<String, PageManifest>,
}

impl Profile {
    /// Directory of a page. Page ids are UUIDs; anything else (e.g. a crafted folder target
    /// in an imported profile) has no directory, so it can never point outside the profile.
    pub fn page_dir(&self, page_id: &str) -> Option<PathBuf> {
        let id = uuid::Uuid::parse_str(page_id).ok()?.hyphenated().to_string();
        let name = if self.manifest.version.starts_with('3') { id.to_uppercase() } else { encode_page_dir(&id).ok()? };
        Some(self.dir.join("Profiles").join(name))
    }

    pub fn load(dir: &Path) -> Result<Profile> {
        let stem = dir.file_stem().and_then(|s| s.to_str()).context("bad profile dir")?;
        let manifest: ProfileManifest =
            serde_json::from_slice(&fs::read(dir.join("manifest.json"))?).with_context(|| format!("parsing {}", dir.join("manifest.json").display()))?;
        let mut pages = HashMap::new();
        if let Ok(rd) = fs::read_dir(dir.join("Profiles")) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let Some(id) = decode_page_dir(&name) else { continue };
                let path = e.path().join("manifest.json");
                match fs::read(&path).map_err(anyhow::Error::from).and_then(|b| Ok(serde_json::from_slice::<PageManifest>(&b)?)) {
                    Ok(p) => {
                        pages.insert(id, p);
                    }
                    Err(err) => tracing::warn!("skipping page {}: {err:#}", path.display()),
                }
            }
        }
        let mut p = Profile { id: stem.to_uppercase(), dir: dir.to_path_buf(), manifest, pages };
        p.repair();
        Ok(p)
    }

    /// Make sure referenced pages exist and at least one page is present.
    fn repair(&mut self) {
        let pages = &mut self.manifest.pages;
        let existing = &self.pages;
        pages.pages.retain(|id| existing.contains_key(id));
        if pages.pages.is_empty() {
            let id = new_uuid();
            self.pages.insert(id.clone(), PageManifest::default());
            self.manifest.pages.pages.push(id);
        }
        if !self.manifest.pages.pages.contains(&self.manifest.pages.current) {
            self.manifest.pages.current = self.manifest.pages.pages[0].clone();
        }
    }

    pub fn new(root: &Path, name: &str, model: &str) -> Profile {
        let id = uuid::Uuid::new_v4().to_string().to_uppercase();
        let page = new_uuid();
        let mut pages = HashMap::new();
        pages.insert(page.clone(), PageManifest::default());
        Profile {
            dir: root.join(format!("{id}.sdProfile")),
            id,
            manifest: ProfileManifest {
                device: DeviceRef { model: model.to_string(), uuid: String::new() },
                name: name.to_string(),
                pages: Pages { current: page.clone(), default: String::new(), pages: vec![page] },
                version: version2(),
                extra: Map::new(),
            },
            pages,
        }
    }

    pub fn save_manifest(&self) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        write_atomic(&self.dir.join("manifest.json"), &serde_json::to_vec(&self.manifest)?)
    }

    pub fn save_page(&self, page_id: &str) -> Result<()> {
        let page = self.pages.get(page_id).context("unknown page")?;
        let dir = self.page_dir(page_id).context("invalid page id")?;
        fs::create_dir_all(dir.join("Images"))?;
        write_atomic(&dir.join("manifest.json"), &serde_json::to_vec(page)?)
    }

    pub fn save_all(&self) -> Result<()> {
        self.save_manifest()?;
        for id in self.pages.keys() {
            self.save_page(id)?;
        }
        Ok(())
    }

    pub fn delete_page_dir(&self, page_id: &str) {
        match self.page_dir(page_id) {
            Some(dir) => drop(fs::remove_dir_all(dir)),
            None => tracing::warn!("not deleting page {page_id:?}: not a page id"),
        }
    }

    /// Application linked to this profile (smart profile), `AppIdentifier` in the manifest.
    pub fn app_identifier(&self) -> Option<&str> {
        self.manifest.extra.get("AppIdentifier").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty())
    }

    pub fn set_app_identifier(&mut self, app: &str) {
        if app.trim().is_empty() {
            self.manifest.extra.remove("AppIdentifier");
        } else {
            self.manifest.extra.insert("AppIdentifier".into(), Value::String(app.trim().to_string()));
        }
    }

    /// Pages that are folders (not top-level and not the template page).
    pub fn folders(&self) -> Vec<String> {
        let mut v: Vec<String> =
            self.pages.keys().filter(|id| !self.manifest.pages.pages.contains(id) && **id != self.manifest.pages.default).cloned().collect();
        v.sort();
        v
    }

    /// Find the folder page that contains an `openchild` pointing at `child`.
    pub fn parent_of(&self, child: &str) -> Option<String> {
        self.pages.iter().find_map(|(id, p)| {
            p.iter_actions()
                .any(|(_, _, a)| a.uuid == crate::builtin::OPEN_CHILD && a.settings.get("ProfileUUID").and_then(|v| v.as_str()) == Some(child))
                .then(|| id.clone())
        })
    }
}

pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Load every `*.sdProfile` below `root`.
pub fn load_all(root: &Path) -> Vec<Profile> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("sdProfile") {
                match Profile::load(&p) {
                    Ok(pr) => out.push(pr),
                    Err(err) => tracing::warn!("cannot load profile {}: {err:#}", p.display()),
                }
            }
        }
    }
    out.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    out
}

/// Extract a `.streamDeckProfile` (zip) into `root`. Returns the new profile directory.
/// A fresh profile UUID is assigned so the same file can be imported several times.
pub fn import_archive(data: &[u8], root: &Path) -> Result<PathBuf> {
    let new_id = uuid::Uuid::new_v4().to_string().to_uppercase();
    let target = root.join(format!("{new_id}.sdProfile"));
    match unpack_profile(data, &target) {
        Ok(()) => Ok(target),
        Err(e) => {
            let _ = fs::remove_dir_all(&target);
            Err(e)
        }
    }
}

fn unpack_profile(data: &[u8], target: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).context("not a zip archive")?;
    let mut found = false;
    let mut unpacked = 0;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let Some(name) = crate::plugins::entry_path(f.name()) else { continue };
        let comps: Vec<_> = name.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
        // Format 2: "<UUID>.sdProfile/...", format 3: "Profiles/<UUID>.sdProfile/..."
        if comps.iter().any(|c| c == "__MACOSX" || c == ".DS_Store") {
            continue;
        }
        let Some(pos) = comps.iter().position(|c| c.ends_with(".sdProfile")) else { continue };
        found = true;
        let rel: PathBuf = comps[pos + 1..].iter().collect();
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = target.join(rel);
        if crate::plugins::entry_is_dir(f.name()) {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        crate::plugins::write_zip_entry(&mut f, &out, &mut unpacked)?;
    }
    if !found {
        bail!("archive contains no .sdProfile");
    }
    Ok(())
}

/// Zip a profile directory (`<id>.sdProfile/...`); reads from disk only, so callers can
/// run it without holding the engine lock.
pub fn export_archive(root: &Path, id: &str) -> Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let base = format!("{id}.sdProfile");
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(&dir)?.flatten() {
                let p = e.path();
                let rel = p.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
                if p.is_dir() {
                    zw.add_directory(format!("{base}/{rel}"), opts)?;
                    stack.push(p);
                } else if !rel.ends_with(".tmp") {
                    zw.start_file(format!("{base}/{rel}"), opts)?;
                    zw.write_all(&fs::read(&p)?)?;
                }
            }
        }
        zw.finish()?;
    }
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_dir_roundtrip() {
        // Values observed in the official StreamDeckNeo default profile.
        assert_eq!(encode_page_dir("9a3bd784-aeb1-4a35-8618-293e75e00cc3").unwrap(), "J8TTF15EM553B1GO54W7BO0COCZ");
        assert_eq!(encode_page_dir("5211d3c8-731e-42c4-b6e7-dc1d5135ac96").unwrap(), "A88T7I3J3P1C9DN7RGEL2DDCIOZ");
        assert_eq!(decode_page_dir("9GDVDHMPPD77F4S4WSH81KIWN0Z").unwrap(), "4c1be6c6-d9cb-4e77-9384-ff2280d25fb8");
    }

    #[test]
    fn only_page_ids_have_a_directory() {
        let mut p = Profile::new(Path::new("/x"), "P", "20GBJ9901");
        let id = "9a3bd784-aeb1-4a35-8618-293e75e00cc3";
        assert_eq!(p.page_dir(id).unwrap(), Path::new("/x").join(format!("{}.sdProfile/Profiles/J8TTF15EM553B1GO54W7BO0COCZ", p.id)));
        p.manifest.version = "3.0".into();
        assert_eq!(p.page_dir(&id.to_uppercase()).unwrap().file_name().unwrap(), "9A3BD784-AEB1-4A35-8618-293E75E00CC3");
        for bad in ["", ".", "..", "/tmp/x", "../../x", "a/b", "9a3bd784/../../x"] {
            assert_eq!(p.page_dir(bad), None, "{bad}");
        }
        assert_eq!(inside(Path::new("/b"), "Images/a.png").unwrap(), Path::new("/b/Images/a.png"));
        for bad in ["/etc/passwd", "../a.png", "Images/../../a.png"] {
            assert_eq!(inside(Path::new("/b"), bad), None, "{bad}");
        }
    }
}
