//! Package files (`.streamDeckPlugin`, `.streamDeckProfile`, `.streamDeckIconPack`):
//! detect the kind and install them.

use crate::engine::Shared;
use crate::plugins::Runtime;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

/// Largest package accepted for installation.
pub const MAX_BYTES: usize = 512 * 1024 * 1024;

/// What a package contains.
pub enum Package {
    Plugin,
    Profile,
    IconPack,
}

pub fn classify(bytes: &[u8], filename: Option<&str>) -> Result<Package> {
    if !bytes.starts_with(b"PK") {
        bail!(crate::tr!("not a package file (expected a .streamDeckPlugin, .streamDeckProfile or .streamDeckIconPack archive)"));
    }
    let name = filename.unwrap_or("").to_lowercase();
    let zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).with_context(|| crate::tr!("invalid archive"))?;
    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();
    if name.ends_with(".streamdeckplugin") || names.iter().any(|n| n.split('/').next().map(|f| f.ends_with(".sdPlugin")).unwrap_or(false)) {
        Ok(Package::Plugin)
    } else if name.ends_with(".streamdeckprofile") || names.iter().any(|n| n.contains(".sdProfile/")) {
        Ok(Package::Profile)
    } else if name.ends_with(".streamdeckiconpack") || names.iter().any(|n| n.contains(".sdIconPack")) {
        Ok(Package::IconPack)
    } else {
        bail!(crate::tr!("unknown package type"))
    }
}

/// Unpack a `.streamDeckProfile` into the profiles folder and load it (also used by the
/// `import-profile` command). Nothing is left behind if it can't be loaded.
pub fn import_profile(bytes: &[u8], profiles: &std::path::Path) -> Result<crate::profile::Profile> {
    let dir = crate::profile::import_archive(bytes, profiles)?;
    let load = || -> Result<crate::profile::Profile> {
        let mut p = crate::profile::Profile::load(&dir)?;
        // A re-imported profile must not share plugin contexts with the original.
        for page in p.pages.values_mut() {
            for c in page.controllers.iter_mut() {
                for a in c.actions.values_mut() {
                    *a = crate::engine::with_new_ids(a);
                }
            }
        }
        p.save_all()?;
        Ok(p)
    };
    load().inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&dir);
    })
}

/// Install a package (blocking). Returns `{kind, id, name, runs_on_linux, reason}`.
pub fn install(sh: &Shared, bytes: &[u8], filename: Option<&str>) -> Result<Value> {
    match classify(bytes, filename)? {
        Package::Plugin => {
            let dir = sh.lock().paths.user_plugins();
            let id = crate::plugins::install_archive(bytes, &dir)?;
            // Restart it if an older version is running (or was uninstalled before).
            let old = {
                let mut e = sh.lock();
                let old = crate::sdk::stop_for_update(&mut e, &id);
                e.rescan_plugins();
                old
            };
            if let Some(mut ch) = old {
                crate::sdk::kill_group(&mut ch);
            }
            crate::sdk::start_plugins(sh);
            crate::sdk::start_html_host(sh);
            let e = sh.lock();
            let p = e.registry.plugin(&id).ok_or_else(|| anyhow!(crate::tr!("plugin not found after installation")))?;
            let (ok, reason) = match &p.runtime {
                Runtime::Unsupported(r) => (false, Some(r.clone())),
                _ => (true, None),
            };
            let name = p.name();
            if ok {
                e.log("info", crate::tr!("Plugin \"{name}\" installed", name = name));
            } else {
                e.log(
                    "warn",
                    crate::tr!("Plugin \"{name}\" installed, but it does not run on Linux: {reason}", name = name, reason = reason.clone().unwrap_or_default()),
                );
            }
            e.emit(json!({"event": "market", "type": "installed", "id": id}));
            Ok(json!({"kind": "plugin", "id": id, "name": name, "runs_on_linux": ok, "reason": reason}))
        }
        Package::Profile => {
            // Unpacking can take a while: don't hold the engine lock meanwhile.
            let profiles = sh.lock().paths.profiles();
            let p = import_profile(bytes, &profiles)?;
            let mut e = sh.lock();
            let (id, name) = (p.id.clone(), p.manifest.name.clone());
            e.profiles.insert(id.clone(), p);
            e.emit(json!({"event": "state"}));
            e.log("info", crate::tr!("Profile \"{name}\" imported", name = name));
            Ok(json!({"kind": "profile", "id": id, "name": name, "runs_on_linux": true}))
        }
        Package::IconPack => {
            let packs = sh.lock().paths.icon_packs();
            let id = crate::iconpacks::install_archive(bytes, &packs)?;
            let e = sh.lock();
            let name = crate::iconpacks::load(&e.paths.icon_packs().join(format!("{id}.sdIconPack"))).map(|p| p.name()).unwrap_or(id.clone());
            e.log("info", crate::tr!("Icon pack \"{name}\" installed", name = name));
            e.emit(json!({"event": "iconpacks"}));
            e.emit(json!({"event": "market", "type": "installed", "id": id}));
            Ok(json!({"kind": "iconpack", "id": id, "name": name, "runs_on_linux": true}))
        }
    }
}
