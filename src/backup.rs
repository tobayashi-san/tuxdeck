//! Backups of the whole configuration and restoring them (Settings → Backups), like the
//! official app's profile backups. Restoring also accepts Elgato's own
//! `.streamDeckProfilesBackup` files: profiles, plugins and icon packs are recognized by
//! their folder names (`*.sdProfile`, `*.sdPlugin`, `*.sdIconPack`) wherever they are.

use crate::paths::{Paths, Settings};
use crate::plugins::{read_zip_entry, write_zip_entry};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

const MANIFEST: &str = "tuxdeck-backup.json";
/// Manifest name of backups made before the rename to TuxDeck.
const OLD_MANIFEST: &str = "streamdeck-linux-backup.json";
/// Automatic backups kept (daily ones and the ones made before a restore).
const KEEP_AUTOMATIC: usize = 10;
/// Where HTML plugins (in the plugin host browser) keep their settings.
const LOCAL_STORAGE: &str = "html-host/Default/Local Storage";

#[derive(Clone, Copy)]
pub struct Options {
    pub plugins: bool,
    pub icon_packs: bool,
}

pub fn dir(paths: &Paths) -> PathBuf {
    paths.data.join("Backups")
}

/// Write a backup to `out` (via a temporary file, so a failure leaves nothing behind).
pub fn create(paths: &Paths, out: &Path, opts: Options) -> Result<()> {
    if let Some(p) = out.parent() {
        fs::create_dir_all(p)?;
    }
    let tmp = out.with_extension("partial");
    let res = write_zip(paths, &tmp, opts).and_then(|_| Ok(fs::rename(&tmp, out)?));
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

fn write_zip(paths: &Paths, out: &Path, opts: Options) -> Result<()> {
    let file = fs::File::create(out)?;
    let mut zw = zip::ZipWriter::new(std::io::BufWriter::new(file));
    let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).large_file(true);
    let mut contents = vec!["profiles", "settings", "plugin-data"];
    add_dir(&mut zw, o, &paths.profiles(), "ProfilesV2")?;
    if let Ok(b) = fs::read(paths.settings_file()) {
        zw.start_file("config/settings.json", o)?;
        zw.write_all(&b)?;
    }
    add_dir(&mut zw, o, &paths.plugin_data(), "plugin-data")?;
    add_dir(&mut zw, o, &paths.data.join(LOCAL_STORAGE), LOCAL_STORAGE)?;
    if opts.plugins {
        add_dir(&mut zw, o, &paths.user_plugins(), "Plugins")?;
        contents.push("plugins");
    }
    if opts.icon_packs {
        add_dir(&mut zw, o, &paths.icon_packs(), "IconPacks")?;
        contents.push("icon packs");
    }
    let manifest = json!({
        "format": 1,
        "app": "tuxdeck",
        "version": env!("CARGO_PKG_VERSION"),
        "created": chrono::Local::now().to_rfc3339(),
        "contents": contents,
    });
    zw.start_file(MANIFEST, o)?;
    zw.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    zw.finish()?.flush()?;
    Ok(())
}

fn add_dir<W: Write + std::io::Seek>(zw: &mut zip::ZipWriter<W>, o: zip::write::SimpleFileOptions, root: &Path, prefix: &str) -> Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
            let ft = e.file_type()?;
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() && !rel.ends_with(".tmp") && !rel.ends_with("LOCK") {
                zw.start_file(format!("{prefix}/{rel}"), o)?;
                std::io::copy(&mut fs::File::open(&p)?, zw)?;
            }
        }
    }
    Ok(())
}

/// Name for a backup file: `<kind>-2026-10-28-0930.zip`.
pub fn file_name(kind: &str) -> String {
    format!("{kind}-{}.zip", chrono::Local::now().format("%Y-%m-%d-%H%M%S"))
}

/// Daily automatic backup (profiles, settings, plugin data; not plugins and icon packs,
/// which can be installed again), once per day.
pub fn daily(paths: &Paths) -> Result<Option<PathBuf>> {
    let today = chrono::Local::now().format("daily-%Y-%m-%d").to_string();
    let exists = fs::read_dir(dir(paths)).map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().starts_with(&today))).unwrap_or(false);
    if exists {
        return Ok(None);
    }
    let out = dir(paths).join(file_name("daily"));
    create(paths, &out, Options { plugins: false, icon_packs: false })?;
    prune(paths);
    Ok(Some(out))
}

/// Keep the newest automatic backups of each kind.
fn prune(paths: &Paths) {
    for kind in ["daily-", "before-restore-"] {
        let mut files: Vec<PathBuf> =
            fs::read_dir(dir(paths)).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| file_str(p).starts_with(kind)).collect();
        files.sort();
        let n = files.len().saturating_sub(KEEP_AUTOMATIC);
        for f in files.into_iter().take(n) {
            let _ = fs::remove_file(f);
        }
    }
}

fn file_str(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Stored backups, newest first.
pub fn list(paths: &Paths) -> Vec<Value> {
    let mut v: Vec<(String, Value)> = fs::read_dir(dir(paths))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "zip"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            let modified: chrono::DateTime<chrono::Local> = m.modified().ok()?.into();
            let kind = name.split('-').next().unwrap_or("").to_string();
            Some((name.clone(), json!({"name": name, "kind": kind, "size": m.len(), "created": modified.to_rfc3339()})))
        })
        .collect();
    v.sort_by(|a, b| b.1["created"].as_str().cmp(&a.1["created"].as_str()));
    v.into_iter().map(|(_, j)| j).collect()
}

/// A stored backup by name (no path tricks).
pub fn stored(paths: &Paths, name: &str) -> Option<PathBuf> {
    let ok = name.ends_with(".zip") && !name.contains(['/', '\\']) && !name.starts_with('.');
    let p = dir(paths).join(name);
    (ok && p.is_file()).then_some(p)
}

/// A backup unpacked next to the data, ready to be applied.
pub struct Staged {
    dir: PathBuf,
    pub profiles: Vec<String>,
    pub plugins: Vec<String>,
    pub icon_packs: Vec<String>,
    pub settings: Option<Value>,
    plugin_data: bool,
    local_storage: bool,
    /// Made by the official app (no manifest of ours).
    pub elgato: bool,
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Unpack a backup (ours or Elgato's) into a staging folder and check it.
pub fn unpack(paths: &Paths, data: &[u8]) -> Result<Staged> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).context(crate::tr!("not a valid archive"))?;
    let dir = paths.data.join(".restore-staging");
    let _ = fs::remove_dir_all(&dir);
    let mut st = Staged {
        dir: dir.clone(),
        profiles: vec![],
        plugins: vec![],
        icon_packs: vec![],
        settings: None,
        plugin_data: false,
        local_storage: false,
        elgato: true,
    };
    let mut unpacked = 0;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let Some(name) = crate::plugins::entry_path(f.name()) else { continue };
        let comps: Vec<String> =
            name.components().filter(|c| matches!(c, Component::Normal(_))).map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        if comps.is_empty() || crate::plugins::entry_is_dir(f.name()) || comps.iter().any(|c| c == "__MACOSX" || c == ".DS_Store") {
            continue;
        }
        let joined = comps.join("/");
        let target: Option<PathBuf> = if joined == MANIFEST || joined == OLD_MANIFEST {
            st.elgato = false;
            continue;
        } else if joined == "config/settings.json" {
            st.settings = serde_json::from_slice(&read_zip_entry(&mut f, &mut unpacked)?).ok();
            continue;
        } else if let Some(rest) = joined.strip_prefix("plugin-data/") {
            st.plugin_data = true;
            Some(dir.join("plugin-data").join(rest))
        } else if let Some(rest) = joined.strip_prefix(&format!("{LOCAL_STORAGE}/")) {
            st.local_storage = true;
            Some(dir.join("local-storage").join(rest))
        } else {
            // The outermost folder named like a profile, plugin or icon pack.
            comps.iter().enumerate().find_map(|(i, c)| {
                let (kind, list) = if c.ends_with(".sdProfile") {
                    ("profiles", &mut st.profiles)
                } else if c.ends_with(".sdPlugin") {
                    ("plugins", &mut st.plugins)
                } else if c.ends_with(".sdIconPack") {
                    ("iconpacks", &mut st.icon_packs)
                } else {
                    return None;
                };
                (i + 1 < comps.len()).then(|| {
                    if !list.contains(c) {
                        list.push(c.clone());
                    }
                    dir.join(kind).join(comps[i..].join("/"))
                })
            })
        };
        let Some(target) = target else { continue };
        if let Some(p) = target.parent() {
            fs::create_dir_all(p)?;
        }
        write_zip_entry(&mut f, &target, &mut unpacked)?;
    }
    // Only profiles that load are restored.
    st.profiles.retain(|p| crate::profile::Profile::load(&dir.join("profiles").join(p)).is_ok());
    if st.profiles.is_empty() && st.plugins.is_empty() && st.icon_packs.is_empty() {
        bail!(crate::tr!("The file contains no profiles, plugins or icon packs."));
    }
    Ok(st)
}

/// Apply a staged backup. Plugins must be stopped. Profiles are replaced as a whole
/// (like the official app); plugins, icon packs and plugin data from the backup replace
/// their installed counterparts, others stay. Settings are taken over except the ports.
pub fn apply(paths: &Paths, settings: &mut Settings, st: &Staged) -> Result<()> {
    let staged = |kind: &str| st.dir.join(kind);
    if !st.profiles.is_empty() {
        let profiles = paths.profiles();
        let previous = paths.data.join(".restore-previous");
        let _ = fs::remove_dir_all(&previous);
        fs::rename(&profiles, &previous)?;
        let moved = (|| -> Result<()> {
            fs::create_dir_all(&profiles)?;
            for p in &st.profiles {
                fs::rename(staged("profiles").join(p), profiles.join(p))?;
            }
            Ok(())
        })();
        if let Err(e) = moved {
            // Put the old profiles back.
            let _ = fs::remove_dir_all(&profiles);
            let _ = fs::rename(&previous, &profiles);
            return Err(e);
        }
        let _ = fs::remove_dir_all(&previous);
    }
    for (kind, list, target) in [("plugins", &st.plugins, paths.user_plugins()), ("iconpacks", &st.icon_packs, paths.icon_packs())] {
        fs::create_dir_all(&target)?;
        for name in list {
            let _ = fs::remove_dir_all(target.join(name));
            fs::rename(staged(kind).join(name), target.join(name))?;
        }
    }
    if st.plugin_data {
        replace_entries(&staged("plugin-data"), &paths.plugin_data(), 2)?;
    }
    if st.local_storage {
        let target = paths.data.join(LOCAL_STORAGE);
        let _ = fs::remove_dir_all(&target);
        if let Some(p) = target.parent() {
            fs::create_dir_all(p)?;
        }
        fs::rename(staged("local-storage"), &target)?;
    }
    if let Some(Ok(mut restored)) = st.settings.clone().map(serde_json::from_value::<Settings>) {
        // The running service keeps its ports.
        restored.ui_port = settings.ui_port;
        restored.sdk_port = settings.sdk_port;
        *settings = restored;
        settings.save(paths);
    }
    Ok(())
}

/// Move the entries `depth` levels below `from` into `to`, replacing existing ones
/// (plugin data: `Data/<plugin>`, `Plugins/<plugin>`).
fn replace_entries(from: &Path, to: &Path, depth: usize) -> Result<()> {
    for e in fs::read_dir(from)?.flatten() {
        let target = to.join(e.file_name());
        if depth > 1 && e.file_type()?.is_dir() {
            fs::create_dir_all(&target)?;
            replace_entries(&e.path(), &target, depth - 1)?;
        } else {
            if target.is_dir() {
                fs::remove_dir_all(&target)?;
            } else {
                let _ = fs::remove_file(&target);
            }
            fs::rename(e.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths(name: &str) -> Paths {
        let base = std::env::temp_dir().join(format!("sd-backup-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let paths = Paths { data: base.join("data"), config: base.join("config") };
        fs::create_dir_all(&paths.config).unwrap();
        paths
    }

    #[test]
    fn roundtrip_and_elgato_layout() {
        let paths = temp_paths("roundtrip");
        let profile = crate::profile::Profile::new(&paths.profiles(), "Backed up", "20GBJ9901");
        profile.save_all().unwrap();
        fs::create_dir_all(paths.plugin_data().join("Data/com.x")).unwrap();
        fs::write(paths.plugin_data().join("Data/com.x/token"), "secret").unwrap();
        let mut settings = Settings::load(&paths);
        settings.language = "de".into();
        settings.save(&paths);

        let out = dir(&paths).join("full.zip");
        create(&paths, &out, Options { plugins: true, icon_packs: false }).unwrap();
        // Everything changes afterwards ...
        fs::remove_dir_all(paths.profiles()).unwrap();
        fs::create_dir_all(paths.profiles()).unwrap();
        crate::profile::Profile::new(&paths.profiles(), "Other", "20GBJ9901").save_all().unwrap();
        fs::write(paths.plugin_data().join("Data/com.x/token"), "changed").unwrap();
        let mut current = Settings::load(&paths);
        current.language = "en".into();
        current.ui_port = 1234;

        // ... and the backup brings the old state back (but not the ports).
        let st = unpack(&paths, &fs::read(&out).unwrap()).unwrap();
        assert!(!st.elgato);
        apply(&paths, &mut current, &st).unwrap();
        drop(st);
        let names: Vec<String> = crate::profile::load_all(&paths.profiles()).into_iter().map(|p| p.manifest.name).collect();
        assert_eq!(names, vec!["Backed up".to_string()]);
        assert_eq!(fs::read_to_string(paths.plugin_data().join("Data/com.x/token")).unwrap(), "secret");
        assert_eq!((current.language.as_str(), current.ui_port), ("de", 1234));
        assert!(!paths.data.join(".restore-staging").exists());

        // An Elgato-style backup: profiles below some folder of their own.
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            let src = crate::profile::load_all(&paths.profiles()).remove(0);
            for e in walk(&src.dir) {
                let rel = e.strip_prefix(&src.dir).unwrap().to_string_lossy().into_owned();
                zw.start_file(format!("ProfilesV3/{}.sdProfile/{rel}", src.id), o).unwrap();
                zw.write_all(&fs::read(&e).unwrap()).unwrap();
            }
            zw.finish().unwrap();
        }
        let st = unpack(&paths, buf.get_ref()).unwrap();
        assert!(st.elgato);
        assert_eq!(st.profiles.len(), 1);
        let _ = fs::remove_dir_all(paths.data.parent().unwrap());
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = vec![];
        for e in fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_dir() {
                out.extend(walk(&e.path()));
            } else {
                out.push(e.path());
            }
        }
        out
    }

    #[test]
    fn rejects_other_archives() {
        let paths = temp_paths("reject");
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut buf);
            zw.start_file("readme.txt", zip::write::SimpleFileOptions::default()).unwrap();
            zw.write_all(b"hello").unwrap();
            zw.finish().unwrap();
        }
        assert!(unpack(&paths, buf.get_ref()).is_err());
        assert!(stored(&paths, "../settings.json").is_none());
        let _ = fs::remove_dir_all(paths.data.parent().unwrap());
    }
}
