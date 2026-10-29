//! Icon packs (`.streamDeckIconPack`, Elgato Marketplace "Icons"). Layout of an
//! installed pack, as in the official app:
//! ```text
//! IconPacks/<id>.sdIconPack/
//!   manifest.json   {"Name", "Author", "Version", "Icon", "Description"?, "URL"?, "License"?}
//!   icons.json      [{"path": "x.svg", "name": "X", "tags": ["a"]}]   (paths relative to icons/)
//!   icons/…         SVG, PNG, JPG, GIF or WEBP
//! ```

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

const IMAGE_EXT: &[&str] = &["svg", "png", "jpg", "jpeg", "gif", "webp"];

#[derive(Debug, Clone, Deserialize)]
pub struct IconMeta {
    pub path: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct IconPack {
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Value,
    pub icons: Vec<IconMeta>,
}

impl IconPack {
    fn field(&self, k: &str) -> String {
        self.manifest.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
    }
    pub fn name(&self) -> String {
        let n = self.field("Name");
        if n.is_empty() {
            self.id.clone()
        } else {
            n
        }
    }

    pub fn summary(&self) -> Value {
        let icon = Some(self.field("Icon")).filter(|i| !i.is_empty() && safe_rel(i)).map(|i| format!("/iconpack-files/{}/{}", self.id, i));
        serde_json::json!({
            "id": self.id, "name": self.name(), "author": self.field("Author"), "version": self.field("Version"),
            "description": self.field("Description"), "url": self.field("URL"), "icon": icon, "count": self.icons.len(),
        })
    }

    /// Icons matching `query` (name, tag or file name; all words must match).
    pub fn search(&self, query: &str) -> Vec<&IconMeta> {
        let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
        self.icons
            .iter()
            .filter(|i| {
                words
                    .iter()
                    .all(|w| i.name.to_lowercase().contains(w) || i.path.to_lowercase().contains(w) || i.tags.iter().any(|t| t.to_lowercase().contains(w)))
            })
            .collect()
    }

    pub fn icon_url(&self, meta: &IconMeta) -> String {
        format!("/iconpack-files/{}/icons/{}", self.id, meta.path.split('/').map(crate::net::urlencode).collect::<Vec<_>>().join("/"))
    }
}

/// Relative path without `..` or root components.
pub fn safe_rel(p: &str) -> bool {
    !p.is_empty() && Path::new(p).components().all(|c| matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir))
}

pub fn load(dir: &Path) -> Result<IconPack> {
    let name = dir.file_name().and_then(|n| n.to_str()).context("bad icon pack dir")?;
    let id = name.strip_suffix(".sdIconPack").unwrap_or(name).to_string();
    let raw = fs::read(dir.join("manifest.json")).with_context(|| crate::tr!("{path}: no manifest.json", path = dir.display()))?;
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw);
    let manifest: Value = serde_json::from_slice(raw).with_context(|| crate::tr!("manifest.json is invalid"))?;
    let icons_dir = dir.join("icons");
    let mut icons: Vec<IconMeta> = match fs::read(dir.join("icons.json")) {
        Ok(b) => {
            let b = b.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&b).to_vec();
            serde_json::from_slice::<Vec<IconMeta>>(&b).with_context(|| crate::tr!("icons.json is invalid"))?
        }
        Err(_) => Vec::new(),
    };
    icons.retain(|i| safe_rel(&i.path) && icons_dir.join(&i.path).is_file());
    if icons.is_empty() {
        // No (usable) metadata: list the image files.
        let mut stack = vec![icons_dir.clone()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|x| x.to_str()).map(|x| IMAGE_EXT.contains(&x.to_lowercase().as_str())).unwrap_or(false) {
                    let rel = p.strip_prefix(&icons_dir).unwrap().to_string_lossy().to_string();
                    let name = p.file_stem().map(|s| s.to_string_lossy().replace(['_', '-'], " ")).unwrap_or_default();
                    icons.push(IconMeta { path: rel, name, tags: vec![] });
                }
            }
        }
        icons.sort_by(|a, b| a.path.cmp(&b.path));
    }
    for i in icons.iter_mut().filter(|i| i.name.is_empty()) {
        i.name = Path::new(&i.path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    }
    Ok(IconPack { id, dir: dir.to_path_buf(), manifest, icons })
}

pub fn load_all(root: &Path) -> Vec<IconPack> {
    let mut out: Vec<IconPack> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.extension().and_then(|x| x.to_str()) == Some("sdIconPack"))
        .filter_map(|p| load(&p).map_err(|e| tracing::warn!("icon pack {}: {e:#}", p.display())).ok())
        .collect();
    out.sort_by_key(|a| a.name().to_lowercase());
    out
}

/// Install (or update) a `.streamDeckIconPack` archive. Returns the pack id.
pub fn install_archive(data: &[u8], root: &Path) -> Result<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).with_context(|| crate::tr!("not a valid archive"))?;
    let mut folder: Option<String> = None;
    for i in 0..zip.len() {
        let f = zip.by_index(i)?;
        let Some(name) = crate::plugins::entry_path(f.name()) else { continue };
        if let Some(c) = name.components().map(|c| c.as_os_str().to_string_lossy().to_string()).find(|c| c.ends_with(".sdIconPack")) {
            folder = Some(c);
            break;
        }
    }
    let folder = folder.with_context(|| crate::tr!("the archive contains no .sdIconPack folder"))?;
    let id = folder.trim_end_matches(".sdIconPack");
    if id.is_empty() || !safe_rel(&folder) {
        bail!(crate::tr!("invalid icon pack name"));
    }
    fs::create_dir_all(root)?;
    let tmp = root.join(format!(".{folder}.installing"));
    let _ = fs::remove_dir_all(&tmp);
    let mut unpacked = 0;
    let mut unpack = || -> Result<()> {
        for i in 0..zip.len() {
            let mut f = zip.by_index(i)?;
            let Some(name) = crate::plugins::entry_path(f.name()) else { continue };
            let comps: Vec<String> = name.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
            let Some(pos) = comps.iter().position(|c| *c == folder) else { continue };
            if comps.iter().any(|c| c == "__MACOSX" || c == ".DS_Store") {
                continue;
            }
            let rel: PathBuf = comps[pos + 1..].iter().collect();
            if rel.as_os_str().is_empty() {
                continue;
            }
            let out = tmp.join(rel);
            if crate::plugins::entry_is_dir(f.name()) {
                fs::create_dir_all(&out)?;
                continue;
            }
            if let Some(p) = out.parent() {
                fs::create_dir_all(p)?;
            }
            crate::plugins::write_zip_entry(&mut f, &out, &mut unpacked)?;
        }
        Ok(())
    };
    if let Err(e) = unpack() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if let Err(e) = load(&tmp) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e.context(crate::tr!("invalid icon pack")));
    }
    let target = root.join(&folder);
    let _ = fs::remove_dir_all(&target);
    fs::rename(&tmp, &target)?;
    Ok(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn pack_zip(with_meta: bool) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("com.test.icons.sdIconPack/manifest.json", o).unwrap();
            z.write_all(br#"{"Name":"Test Icons","Author":"me","Version":"1.0","Icon":"icon.svg"}"#).unwrap();
            if with_meta {
                z.start_file("com.test.icons.sdIconPack/icons.json", o).unwrap();
                z.write_all(br#"[{"path":"train.svg","name":"Train","tags":["travel"]},{"path":"../evil.svg","name":"x","tags":[]}]"#).unwrap();
            }
            z.start_file("com.test.icons.sdIconPack/icons/train.svg", o).unwrap();
            z.write_all(br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#).unwrap();
            z.start_file("com.test.icons.sdIconPack/icons/bike_red.png", o).unwrap();
            z.write_all(b"\x89PNG").unwrap();
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn install_and_search() {
        let root = std::env::temp_dir().join(format!("sd-iconpack-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        assert_eq!(install_archive(&pack_zip(true), &root).unwrap(), "com.test.icons");
        let packs = load_all(&root);
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].icons.len(), 1, "unsafe path must be dropped");
        assert_eq!(packs[0].search("trav").len(), 1);
        assert!(packs[0].search("car").is_empty());
        // Update without icons.json: files are listed instead.
        install_archive(&pack_zip(false), &root).unwrap();
        let p = &load_all(&root)[0];
        assert_eq!(p.icons.len(), 2);
        assert_eq!(p.search("bike red").len(), 1);
        let _ = fs::remove_dir_all(&root);
    }
}
