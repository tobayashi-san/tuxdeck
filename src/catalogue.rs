//! Catalogue of open-source plugins: the OpenAction plugin registry
//! (<https://github.com/OpenActionAPI/plugins>, also used by OpenDeck). It lists plugins
//! with their GitHub repository; packages are downloaded straight from the authors'
//! releases.

use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

const CATALOGUE_URL: &str = "https://openactionapi.github.io/plugins/catalogue.json";
const ICON_URL: &str = "https://openactionapi.github.io/plugins/icons";
const GITHUB_API: &str = "https://api.github.com";
const CACHE_FOR: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    #[serde(skip)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub author: String,
    pub repository: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Direct package URL (instead of the latest GitHub release).
    #[serde(default)]
    pub download_url: Option<String>,
    #[serde(default)]
    pub flags: Value,
}

impl Entry {
    pub fn summary(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "author": self.author,
            "summary": self.description,
            "repository": self.repository,
            "icon": format!("{}/{}.png", icon_base(), crate::net::urlencode(&self.id)),
            "ai_assisted": self.flags.get("ai_assisted").and_then(|v| v.as_bool()).unwrap_or(false),
        })
    }

    /// Does the entry match a search (name, author, ID or description)?
    pub fn matches(&self, q: &str) -> bool {
        let q = q.to_lowercase();
        [Some(&self.name), Some(&self.author), Some(&self.id), self.description.as_ref()].into_iter().flatten().any(|s| s.to_lowercase().contains(&q))
    }
}

/// Catalogue location; `TUXDECK_CATALOGUE_URL` overrides it (tests).
fn catalogue_url() -> String {
    std::env::var("TUXDECK_CATALOGUE_URL").unwrap_or_else(|_| CATALOGUE_URL.into())
}

fn icon_base() -> String {
    std::env::var("TUXDECK_CATALOGUE_ICONS").unwrap_or_else(|_| ICON_URL.into())
}

fn github_api() -> String {
    std::env::var("TUXDECK_GITHUB_API").unwrap_or_else(|_| GITHUB_API.into())
}

/// When the catalogue was loaded, and its entries.
type Cached = Option<(Instant, Arc<Vec<Entry>>)>;

/// The catalogue in its published order (cached for an hour).
pub async fn load() -> Result<Arc<Vec<Entry>>> {
    static CACHE: OnceLock<Mutex<Cached>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some((at, list)) = cache.lock().as_ref() {
        if at.elapsed() < CACHE_FOR {
            return Ok(list.clone());
        }
    }
    let fetched = async {
        let r = crate::net::http().get(catalogue_url()).timeout(crate::net::SHORT).send().await?.error_for_status()?;
        r.json::<serde_json::Map<String, Value>>().await.context("invalid catalogue")
    }
    .await;
    let raw = match fetched {
        Ok(raw) => raw,
        // Offline for a while: the last list is better than none.
        Err(e) => match cache.lock().as_ref() {
            Some((_, list)) => {
                tracing::warn!("plugin catalogue: {e:#} (using the last list)");
                return Ok(list.clone());
            }
            None => return Err(e),
        },
    };
    let list: Vec<Entry> = raw
        .into_iter()
        .filter_map(|(id, v)| {
            let mut e: Entry = serde_json::from_value(v).ok()?;
            e.id = id;
            Some(e)
        })
        .collect();
    let list = Arc::new(list);
    *cache.lock() = Some((Instant::now(), list.clone()));
    Ok(list)
}

pub async fn entry(id: &str) -> Result<Entry> {
    load().await?.iter().find(|e| e.id == id).cloned().with_context(|| crate::tr!("plugin not in the catalogue"))
}

/// A package to download.
#[derive(Debug, Clone)]
pub struct Release {
    /// Release tag (empty for direct download URLs).
    pub tag: String,
    pub file: String,
    pub url: String,
}

/// `owner/repo` of a GitHub repository URL.
fn github_repo(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.trim_end_matches('/').trim_end_matches(".git").split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    let ok = |s: &str| !s.is_empty() && s != "." && s != ".." && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    (ok(owner) && ok(repo)).then(|| format!("{owner}/{repo}"))
}

/// Token for the GitHub API (60 requests per hour without one): `GITHUB_TOKEN`/`GH_TOKEN`,
/// else the one of a signed-in `gh` CLI. Only sent to the GitHub API.
fn github_token() -> Option<String> {
    static TOKEN: OnceLock<Option<String>> = OnceLock::new();
    TOKEN
        .get_or_init(|| {
            ["GITHUB_TOKEN", "GH_TOKEN"].iter().find_map(|k| std::env::var(k).ok().filter(|t| !t.trim().is_empty())).or_else(|| {
                let out = std::process::Command::new("gh").args(["auth", "token"]).stderr(std::process::Stdio::null()).output().ok()?;
                let t = String::from_utf8(out.stdout).ok()?.trim().to_string();
                (out.status.success() && !t.is_empty()).then_some(t)
            })
        })
        .clone()
}

/// The package of the newest release (pre-releases only if there is nothing else).
pub async fn resolve(e: &Entry) -> Result<Release> {
    if let Some(url) = e.download_url.as_deref().filter(|u| u.starts_with("https://") || u.starts_with("http://127.0.0.1")) {
        let file = url.split('?').next().and_then(|u| u.rsplit('/').next()).map(crate::net::urldecode).unwrap_or_default();
        return Ok(Release { tag: String::new(), file, url: url.to_string() });
    }
    let repo = github_repo(&e.repository).with_context(|| crate::tr!("not a GitHub repository: {url}", url = e.repository))?;
    // A token that doesn't work (any more) is dropped: the releases are public.
    static TOKEN_BAD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let token = github_token().filter(|_| std::env::var_os("TUXDECK_GITHUB_API").is_none() && !TOKEN_BAD.load(std::sync::atomic::Ordering::Relaxed));
    let get = |token: Option<&String>| {
        let req = crate::net::http()
            .get(format!("{}/repos/{repo}/releases?per_page=10", github_api()))
            .header("Accept", "application/vnd.github+json")
            .timeout(crate::net::SHORT);
        match token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    };
    let mut r = get(token.as_ref()).send().await?;
    if token.is_some() && r.status() == reqwest::StatusCode::UNAUTHORIZED {
        tracing::warn!("GitHub refuses the token (GITHUB_TOKEN, GH_TOKEN or gh login): continuing without it");
        TOKEN_BAD.store(true, std::sync::atomic::Ordering::Relaxed);
        r = get(None).send().await?;
    }
    match r.status() {
        reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::TOO_MANY_REQUESTS => {
            bail!(crate::tr!("GitHub limits the number of requests – please try again in an hour"))
        }
        reqwest::StatusCode::NOT_FOUND => bail!(crate::tr!("The plugin's GitHub repository no longer exists")),
        s if !s.is_success() => bail!(crate::tr!("GitHub answered with an error ({status})", status = s)),
        _ => {}
    }
    let releases: Vec<Value> = r.json().await?;
    let usable: Vec<&Value> = releases.iter().filter(|r| r["draft"] != true).collect();
    let release = usable.iter().find(|r| r["prerelease"] != true).or(usable.first()).with_context(|| crate::tr!("the plugin has no release yet"))?;
    pick_asset(release).with_context(|| crate::tr!("the newest release contains no plugin package"))
}

/// The plugin package among a release's assets: a `.streamDeckPlugin` (one built for
/// Linux if there are several), else a `.zip`.
fn pick_asset(release: &Value) -> Option<Release> {
    let assets: Vec<(&str, &str)> = release["assets"]
        .as_array()?
        .iter()
        .filter_map(|a| Some((a["name"].as_str()?, a["browser_download_url"].as_str()?)))
        .filter(|(_, url)| url.starts_with("https://") || url.starts_with("http://127.0.0.1"))
        .collect();
    let has = |n: &str, ext: &str| n.to_lowercase().ends_with(ext);
    let linux = |n: &str| n.to_lowercase().contains("linux");
    let pick = assets
        .iter()
        .filter(|(n, _)| has(n, ".streamdeckplugin"))
        .max_by_key(|(n, _)| linux(n))
        .or_else(|| assets.iter().filter(|(n, _)| has(n, ".zip")).max_by_key(|(n, _)| linux(n)))?;
    Some(Release { tag: release["tag_name"].as_str().unwrap_or("").to_string(), file: pick.0.to_string(), url: pick.1.to_string() })
}

/// Download a release package (size-limited).
pub async fn download(r: &Release) -> Result<Vec<u8>> {
    let stalled = |e: reqwest::Error| if e.is_timeout() { anyhow::anyhow!(crate::tr!("The download stalled – please try again")) } else { e.into() };
    let mut resp = crate::net::http().get(&r.url).send().await.map_err(stalled)?;
    if !resp.status().is_success() {
        bail!(crate::tr!("Download failed ({status})", status = resp.status()));
    }
    let max = crate::packages::MAX_BYTES;
    if resp.content_length().is_some_and(|n| n as usize > max) {
        bail!(crate::tr!("the package is too large"));
    }
    // Checked while it arrives (a server may not say how big it is).
    let mut out = Vec::with_capacity(resp.content_length().unwrap_or(0) as usize);
    while let Some(chunk) = resp.chunk().await.map_err(stalled)? {
        if out.len() + chunk.len() > max {
            bail!(crate::tr!("the package is too large"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_urls() {
        assert_eq!(github_repo("https://github.com/OpenActionPlugins/counter").as_deref(), Some("OpenActionPlugins/counter"));
        assert_eq!(github_repo("https://github.com/a-b/c.d.git/").as_deref(), Some("a-b/c.d"));
        assert!(github_repo("https://gitlab.com/a/b").is_none());
        assert!(github_repo("https://github.com/a/../b").is_none());
        assert!(github_repo("https://github.com/a").is_none());
    }

    #[test]
    fn asset_choice() {
        let rel = json!({"tag_name": "v2", "assets": [
            {"name": "source.zip", "browser_download_url": "https://x/source.zip"},
            {"name": "plugin-windows.streamDeckPlugin", "browser_download_url": "https://x/w"},
            {"name": "plugin-linux.streamDeckPlugin", "browser_download_url": "https://x/l"},
        ]});
        let r = pick_asset(&rel).unwrap();
        assert_eq!((r.tag.as_str(), r.file.as_str(), r.url.as_str()), ("v2", "plugin-linux.streamDeckPlugin", "https://x/l"));
        let zip_only = json!({"tag_name": "1", "assets": [{"name": "p.zip", "browser_download_url": "https://x/p.zip"}]});
        assert_eq!(pick_asset(&zip_only).unwrap().file, "p.zip");
        assert!(pick_asset(&json!({"assets": [{"name": "notes.txt", "browser_download_url": "https://x/n"}]})).is_none());
    }
}
