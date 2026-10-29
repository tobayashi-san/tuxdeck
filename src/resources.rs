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

/// Vector icons for the built-in actions (see `assets/action-icons/<uuid>.svg`).
#[derive(rust_embed::RustEmbed)]
#[folder = "assets/action-icons/"]
struct ActionIcons;

/// Write `builtin/<plugin>.sdPlugin` stand-ins so the native actions are always
/// available, even without the original resources.
pub fn write_builtin_plugins(paths: &Paths) {
    let root = paths.builtin_plugins();
    let mut groups: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for d in CATALOG {
        let dir = root.join(format!("{}.sdPlugin", d.plugin));
        let _ = fs::create_dir_all(dir.join("Images"));
        let img = format!("Images/{}", d.uuid);
        // Use the bundled vector icon for this action; fall back to a glyph-on-colour tile.
        let svg = ActionIcons::get(&format!("{}.svg", d.uuid)).map(|f| f.data.into_owned()).unwrap_or_else(|| fallback_icon_svg(d.glyph, d.color).into_bytes());
        let _ = fs::write(dir.join(format!("{img}.svg")), svg);
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
const DEFAULT_ICONPACK_VERSION: u32 = 4;

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

/// Colourful key icons for the starter profile (see `assets/starter/`).
#[derive(rust_embed::RustEmbed)]
#[folder = "assets/starter/"]
struct StarterIcons;

/// A starter-profile icon as a `data:` URL.
pub fn starter_icon_data_url(name: &str) -> Option<String> {
    use base64::Engine as _;
    let file = StarterIcons::get(&format!("{name}.svg"))?;
    Some(format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(file.data.as_ref())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sd-resources-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn paths(d: &Path) -> Paths {
        Paths { data: d.join("data"), config: d.join("config") }
    }

    #[test]
    fn starter_icons() {
        for name in ["play", "next", "previous", "mute", "volume-up", "volume-down", "brightness", "home", "lock", "sleep", "timer"] {
            let url = starter_icon_data_url(name).unwrap_or_else(|| panic!("{name}"));
            let b64 = url.strip_prefix("data:image/svg+xml;base64,").unwrap();
            let svg = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
            assert_eq!(svg, StarterIcons::get(&format!("{name}.svg")).unwrap().data.as_ref());
            assert!(String::from_utf8(svg).unwrap().contains("<svg"), "{name}");
        }
        assert_eq!(starter_icon_data_url("nope"), None);
        assert_eq!(starter_icon_data_url("play.svg"), None);
    }

    #[test]
    fn default_iconpack_seeding() {
        let d = tmp("iconpack");
        let p = paths(&d);
        let dir = p.icon_packs().join("tuxdeck-essentials.sdIconPack");
        let marker = p.icon_packs().join(".tuxdeck-essentials.seeded");
        write_default_iconpack(&p);
        assert_eq!(fs::read_to_string(&marker).unwrap(), DEFAULT_ICONPACK_VERSION.to_string());
        let manifest: Value = serde_json::from_slice(&fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert!(manifest.is_object());
        assert!(dir.join("icons.json").is_file());
        assert_eq!(DefaultIconPack::iter().filter(|f| !dir.join(f.as_ref()).is_file()).count(), 0);

        // Removed by the user: stays removed for this version.
        fs::remove_dir_all(&dir).unwrap();
        write_default_iconpack(&p);
        assert!(!dir.exists());

        // Older (or unreadable) marker: seeded again, stale files of the old version go.
        for old in ["3", "", "garbage"] {
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("stale.svg"), "").unwrap();
            fs::write(&marker, old).unwrap();
            write_default_iconpack(&p);
            assert!(dir.join("manifest.json").is_file() && !dir.join("stale.svg").exists(), "{old:?}");
            assert_eq!(fs::read_to_string(&marker).unwrap(), DEFAULT_ICONPACK_VERSION.to_string());
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn default_profiles() {
        let d = tmp("default-profiles");
        let p = paths(&d);
        let root = p.elgato().join("DefaultProfiles");
        assert_eq!(default_profile_for(&p, "20GBA9901"), None);
        fs::create_dir_all(&root).unwrap();
        let expected = [
            ("20GBJ9901", "StreamDeckNeo_macDefault"),
            ("20GAA9902", "StreamDeck_macDefault"),
            ("20GBA9901", "StreamDeck_macDefault"),
            ("20GAT9901", "StreamDeckXL_macDefault"),
            ("20GAT9902", "StreamDeckXL_macDefault"),
            ("20GBD9901", "StreamDeckPlus_macDefault"),
            ("20GAI9901", "StreamDeckMini_macDefault"),
        ];
        for (_, name) in expected {
            fs::write(root.join(format!("{name}.streamDeckProfile")), "").unwrap();
        }
        for (model, name) in expected {
            assert_eq!(default_profile_for(&p, model), Some(root.join(format!("{name}.streamDeckProfile"))), "{model}");
        }
        assert_eq!(default_profile_for(&p, "20GAV9901"), None);
        assert_eq!(default_profile_for(&p, ""), None);
        fs::remove_file(root.join("StreamDeckMini_macDefault.streamDeckProfile")).unwrap();
        assert_eq!(default_profile_for(&p, "20GAI9901"), None);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn layout_fonts() {
        let d = tmp("fonts");
        let p = paths(&d);
        assert!(layout_font_dirs(&p).is_empty());
        let layouts = p.elgato().join("InfobarLayouts");
        for sub in ["a/fonts", "b/c/fonts", "b/fonts/fonts", "d/notfonts"] {
            fs::create_dir_all(layouts.join(sub)).unwrap();
        }
        fs::write(layouts.join("e-fonts"), "").unwrap();
        fs::write(layouts.join("a/fonts/x.ttf"), "").unwrap();
        let mut found = layout_font_dirs(&p);
        found.sort();
        assert_eq!(found, ["a/fonts", "b/c/fonts", "b/fonts", "b/fonts/fonts"].map(|s| layouts.join(s)));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn builtin_manifests() {
        let d = tmp("builtin");
        let p = paths(&d);
        write_builtin_plugins(&p);
        let mut uuids = Vec::new();
        for e in fs::read_dir(p.builtin_plugins()).unwrap().flatten() {
            let m: Value = serde_json::from_slice(&fs::read(e.path().join("manifest.json")).unwrap()).unwrap();
            assert_eq!(m["Author"], "TuxDeck");
            for a in m["Actions"].as_array().unwrap() {
                let uuid = a["UUID"].as_str().unwrap();
                let def = CATALOG.iter().find(|d| d.uuid == uuid).unwrap();
                assert_eq!(e.file_name().to_string_lossy(), format!("{}.sdPlugin", def.plugin));
                assert_eq!(m["Name"], CATALOG.iter().find(|x| x.plugin == def.plugin).unwrap().category);
                assert_eq!(a["States"].as_array().unwrap().len(), def.states);
                assert_eq!(a["SupportedInKeyLogicActions"], crate::builtin::in_key_logic(uuid));
                let svg = fs::read_to_string(e.path().join(format!("{}.svg", a["Icon"].as_str().unwrap()))).unwrap();
                assert!(svg.contains("<svg"), "{uuid}");
                uuids.push(uuid.to_string());
            }
        }
        uuids.sort();
        let mut all: Vec<String> = CATALOG.iter().map(|d| d.uuid.to_string()).collect();
        all.sort();
        assert_eq!(uuids, all);
        let _ = fs::remove_dir_all(&d);
    }

    /// A fake `Stream Deck.app` with resources.
    fn fake_app(root: &Path) -> PathBuf {
        let res = root.join("Payload/Elgato Stream Deck.app/Contents/Resources");
        fs::create_dir_all(res.join("Plugins/com.elgato.x.sdPlugin")).unwrap();
        fs::write(res.join("Plugins/com.elgato.x.sdPlugin/manifest.json"), "{}").unwrap();
        fs::create_dir_all(res.join("DefaultProfiles")).unwrap();
        fs::write(res.join("DefaultProfiles/StreamDeck_macDefault.streamDeckProfile"), "zip").unwrap();
        fs::create_dir_all(res.join("InfobarLayouts/L/fonts")).unwrap();
        fs::create_dir_all(res.join("Unrelated")).unwrap();
        fs::write(res.join("Unrelated/x"), "").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", res.join("Plugins/link")).unwrap();
        res
    }

    #[test]
    fn find_app_resources() {
        let d = tmp("find");
        assert_eq!(find_resources(&d), None);
        // `Contents/Resources` without plugins is not it.
        fs::create_dir_all(d.join("Other.app/Contents/Resources")).unwrap();
        assert_eq!(find_resources(&d), None);
        let res = fake_app(&d);
        assert_eq!(find_resources(&d), Some(res.clone()));
        assert_eq!(find_resources(&res), Some(res));
        fs::create_dir_all(d.join("Payload/a/b")).unwrap();
        fs::write(d.join("Payload/a/b/Payload"), "").unwrap();
        assert_eq!(walk_find(&d, "Payload"), Some(d.join("Payload/a/b/Payload")));
        assert_eq!(walk_find(&d, "nothing"), None);
        assert_eq!(walk_find(&d.join("missing"), "Payload"), None);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn import_from_folder() {
        let d = tmp("import");
        let p = paths(&d);
        fake_app(&d.join("src"));
        // Old imports are replaced completely.
        fs::create_dir_all(p.elgato().join("Plugins/old")).unwrap();
        let dst = import_elgato(&p, &d.join("src")).unwrap();
        assert_eq!(dst, p.elgato());
        assert!(dst.join("Plugins/com.elgato.x.sdPlugin/manifest.json").is_file());
        assert!(!dst.join("Plugins/old").exists());
        // Links are not followed out of the bundle.
        assert!(!dst.join("Plugins/link").exists());
        assert_eq!(fs::read_to_string(dst.join("DefaultProfiles/StreamDeck_macDefault.streamDeckProfile")).unwrap(), "zip");
        assert!(!dst.join("Unrelated").exists());
        assert_eq!(default_profile_for(&p, "20GBA9901"), Some(dst.join("DefaultProfiles/StreamDeck_macDefault.streamDeckProfile")));
        assert_eq!(layout_font_dirs(&p), [dst.join("InfobarLayouts/L/fonts")]);
        assert!(import_elgato(&p, &d.join("data")).is_err());
        let _ = fs::remove_dir_all(&d);
    }
}
