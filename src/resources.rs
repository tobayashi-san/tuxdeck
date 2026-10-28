//! Resources: generated stand-in manifests for the native actions, and import of
//! the original resources (icons, layouts, default profiles) from the official
//! macOS installer that the user owns.

use crate::builtin::CATALOG;
use crate::paths::Paths;
use crate::render::fallback_icon_svg;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Write `builtin/<plugin>.sdPlugin` stand-ins so the native actions are always
/// available, even without the original resources.
pub fn write_builtin_plugins(paths: &Paths) {
    let root = paths.builtin_plugins();
    let mut groups: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for d in CATALOG {
        let dir = root.join(format!("{}.sdPlugin", d.plugin));
        let _ = fs::create_dir_all(dir.join("Images"));
        let img = format!("Images/{}", d.uuid);
        let _ = fs::write(dir.join(format!("{img}.svg")), fallback_icon_svg(d.glyph, d.color));
        groups.entry(d.plugin).or_default().push(json!({
            "UUID": d.uuid, "Name": d.name, "Tooltip": d.tooltip, "Icon": img,
            "States": vec![json!({"Image": img}); d.states],
            "Controllers": d.controllers, "SupportedInMultiActions": d.multi,
            "SupportedInKeyLogicActions": crate::builtin::in_key_logic(d.uuid),
        }));
    }
    for (plugin, actions) in groups {
        let name = CATALOG.iter().find(|d| d.plugin == plugin).map(|d| d.category).unwrap_or("System");
        let m = json!({"Name": name, "Author": "TuxDeck", "Version": "1.0", "Description": "", "PrivateAPI": true, "Actions": actions});
        let _ = fs::write(root.join(format!("{plugin}.sdPlugin")).join("manifest.json"), serde_json::to_vec_pretty(&m).unwrap());
    }
}

fn has_cmd(name: &str) -> bool {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file())).unwrap_or(false)
}

/// Locate `Contents/Resources` of the app inside an extracted tree.
fn find_resources(root: &Path) -> Option<PathBuf> {
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((d, depth)) = stack.pop() {
        if d.ends_with("Contents/Resources") && d.join("Plugins").is_dir() {
            return Some(d);
        }
        if depth > 6 {
            continue;
        }
        if let Ok(rd) = fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    stack.push((e.path(), depth + 1));
                }
            }
        }
    }
    None
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let t = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &t)?;
        } else if e.file_type()?.is_file() {
            fs::copy(e.path(), &t)?;
        }
    }
    Ok(())
}

/// Import resources from `Stream_Deck_*.pkg`, an extracted payload, or a
/// `Stream Deck.app` directory.
pub fn import_elgato(paths: &Paths, source: &Path) -> Result<PathBuf> {
    let tmp = std::env::temp_dir().join(format!("tuxdeck-import-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;
    let resources = if source.is_dir() {
        find_resources(source).context("no 'Contents/Resources' with plugins found in the folder")?
    } else {
        if !has_cmd("bsdtar") {
            bail!("bsdtar (libarchive) is needed to extract the .pkg");
        }
        // xar container -> Payload (gzip cpio) -> app bundle resources
        let st = Command::new("bsdtar").arg("-xf").arg(source).arg("-C").arg(&tmp).status()?;
        if !st.success() {
            bail!("could not extract the pkg");
        }
        let payload = walk_find(&tmp, "Payload").context("Payload not found in the pkg")?;
        let out = tmp.join("payload");
        fs::create_dir_all(&out)?;
        let st = Command::new("bsdtar").arg("-xf").arg(&payload).arg("-C").arg(&out).arg("--include").arg("*Contents/Resources/*").status()?;
        if !st.success() {
            bail!("could not extract the Payload");
        }
        find_resources(&out).context("app resources not found in the Payload")?
    };
    let dst = paths.elgato();
    let _ = fs::remove_dir_all(&dst);
    fs::create_dir_all(&dst)?;
    for sub in ["Plugins", "InfobarLayouts", "DefaultProfiles", "PageIcons", "TouchBackgrounds", "Distributables"] {
        let s = resources.join(sub);
        if s.is_dir() {
            copy_dir(&s, &dst.join(sub))?;
        }
    }
    let _ = fs::remove_dir_all(&tmp);
    Ok(dst)
}

fn walk_find(root: &Path, name: &str) -> Option<PathBuf> {
    for e in fs::read_dir(root).ok()?.flatten() {
        let p = e.path();
        if p.file_name().map(|f| f == name).unwrap_or(false) && p.is_file() {
            return Some(p);
        }
        if p.is_dir() {
            if let Some(f) = walk_find(&p, name) {
                return Some(f);
            }
        }
    }
    None
}

/// Default profile shipped with the official app for a model (`20GBJ9901` etc.).
pub fn default_profile_for(paths: &Paths, model_id: &str) -> Option<PathBuf> {
    let dir = paths.elgato().join("DefaultProfiles");
    let name = match model_id {
        "20GBJ9901" => "StreamDeckNeo_macDefault",
        "20GAA9902" | "20GBA9901" => "StreamDeck_macDefault",
        "20GAT9901" | "20GAT9902" => "StreamDeckXL_macDefault",
        "20GBD9901" => "StreamDeckPlus_macDefault",
        "20GAI9901" => "StreamDeckMini_macDefault",
        _ => return None,
    };
    let p = dir.join(format!("{name}.streamDeckProfile"));
    p.is_file().then_some(p)
}

/// Font directories bundled with the info bar layouts.
pub fn layout_font_dirs(paths: &Paths) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(root) = paths.layouts() else { return out };
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.file_name().map(|f| f == "fonts").unwrap_or(false) {
                        out.push(p.clone());
                    }
                    stack.push(p);
                }
            }
        }
    }
    out
}

/// The default icon pack shipped with TuxDeck (original icons; see `assets/iconpack/`).
#[derive(rust_embed::RustEmbed)]
#[folder = "assets/iconpack/"]
struct DefaultIconPack;

const DEFAULT_ICONPACK_ID: &str = "tuxdeck-essentials";
/// Bump to re-seed the pack after changing the icons (user deletions still stick for the
/// current version).
const DEFAULT_ICONPACK_VERSION: u32 = 1;

/// Write the bundled icon pack into `IconPacks/` on first run. A version marker next to it
/// records that it was seeded, so removing the pack in the UI is respected and only a
/// version bump re-seeds it.
pub fn write_default_iconpack(paths: &Paths) {
    let root = paths.icon_packs();
    let marker = root.join(format!(".{DEFAULT_ICONPACK_ID}.seeded"));
    let seeded = fs::read_to_string(&marker).ok().and_then(|s| s.trim().parse::<u32>().ok());
    if seeded == Some(DEFAULT_ICONPACK_VERSION) {
        return;
    }
    let dir = root.join(format!("{DEFAULT_ICONPACK_ID}.sdIconPack"));
    if let Err(e) = (|| -> std::io::Result<()> {
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        for path in DefaultIconPack::iter() {
            let Some(file) = DefaultIconPack::get(&path) else { continue };
            let out = dir.join(path.as_ref());
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(out, file.data)?;
        }
        fs::create_dir_all(&root)?;
        fs::write(&marker, DEFAULT_ICONPACK_VERSION.to_string())
    })() {
        tracing::warn!("could not write the default icon pack: {e}");
    }
}
