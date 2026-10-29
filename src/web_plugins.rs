//! Plugin endpoints of the web API: installed plugins, installing package files, the
//! open-source plugin catalogue, Linux checks and deep links.

use crate::catalogue;
use crate::engine::Shared;
use crate::packages::{self, Package};
use crate::plugins::Runtime;
use anyhow::{anyhow, Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

pub(crate) struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::BAD_REQUEST, format!("{e:#}"))
    }
}

pub(crate) type ApiResult<T> = Result<T, ApiError>;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/plugins", get(installed_plugins))
        .route("/api/plugins/{uuid}", delete(uninstall))
        .route("/api/plugins/{uuid}/diagnostics", get(plugin_diagnostics))
        .route("/api/plugins/{uuid}/linux-check", post(check_installed))
        .route("/api/catalogue", get(catalogue_list))
        .route("/api/catalogue/install", post(catalogue_install))
        .route("/api/catalogue/linux-check", post(catalogue_check))
        .route("/api/catalogue/linux-check-all", post(check_all).delete(cancel_check_all))
        .route("/api/catalogue/{id}", get(catalogue_item))
        .route("/api/linux-checks", get(linux_checks))
        .route("/api/profiles/{profile}/compat", get(compat_report).post(compat_fix))
        .route("/api/deeplink", post(deeplink))
}

/// Install state and Linux check of a plugin (or icon pack) for the catalogue views.
pub(crate) fn install_state(sh: &Shared, id: &str) -> Value {
    let e = sh.lock();
    let pack = e.paths.icon_packs().join(format!("{id}.sdIconPack"));
    if crate::iconpacks::safe_rel(&format!("{id}.sdIconPack")) && pack.join("manifest.json").is_file() {
        let v = crate::iconpacks::load(&pack).map(|p| p.summary()["version"].clone()).unwrap_or(Value::Null);
        return json!({"installed": true, "installed_version": v, "runs_on_linux": true, "kind": "iconpack"});
    }
    let check = crate::linuxcheck::summary(&e.paths, id);
    let checking = crate::linuxcheck::is_running(id);
    match e.registry.plugin(id) {
        Some(p) => json!({
            "installed": true,
            "installed_version": p.manifest.version,
            "runs_on_linux": p.supported(),
            "builtin": p.builtin,
            "reason": match &p.runtime { Runtime::Unsupported(r) => Some(r.clone()), _ => None },
            "linux_check": check,
            "checking": checking,
        }),
        None => json!({"installed": false, "linux_check": check, "checking": checking}),
    }
}

/// Install a downloaded package; with `check` (extra fields of the result, e.g. where the
/// package came from), a plugin gets a Linux check right away (in the background).
pub(crate) async fn install_package(sh: &Shared, bytes: Vec<u8>, filename: Option<String>, check: Option<Value>) -> Result<Value> {
    let sh2 = sh.clone();
    let result = tokio::task::spawn_blocking(move || packages::install(&sh2, &bytes, filename.as_deref())).await??;
    if let Some(extra) = check.filter(|_| result["kind"] == "plugin" && result["runs_on_linux"] == true) {
        let id = result["id"].as_str().unwrap_or_default().to_string();
        let dir = sh.lock().registry.plugin(&id).map(|p| p.dir.clone());
        if let Some(dir) = dir {
            let _ = start_check_with(sh, id, CheckSource::Dir(dir), extra, None);
        }
    }
    Ok(result)
}

fn log_failure<T>(sh: &Shared, r: Result<T>) -> ApiResult<T> {
    r.map_err(|e| {
        sh.lock().log("error", crate::tr!("Installation failed: {error}", error = format!("{e:#}")));
        e.into()
    })
}

async fn installed_plugins(State(sh): State<Shared>) -> Json<Value> {
    let e = sh.lock();
    let list: Vec<Value> = e
        .registry
        .plugins
        .iter()
        .filter(|p| !p.builtin)
        .map(|p| {
            let icon = p
                .manifest
                .icon
                .as_ref()
                .and_then(|rel| crate::plugins::resolve_image(&p.dir, rel))
                .and_then(|f| f.strip_prefix(&p.dir).ok().map(|r| format!("/plugin-files/{}/{}", p.uuid, r.to_string_lossy())));
            json!({"uuid": p.uuid, "name": p.name(), "icon": icon, "version": p.manifest.version, "author": p.manifest.author,
                   "runs_on_linux": p.supported(), "reason": match &p.runtime { Runtime::Unsupported(r) => Some(r.clone()), _ => None },
                   "actions": p.manifest.actions.len()})
        })
        .collect();
    Json(Value::Array(list))
}

// ---------------------------------------------------------------------- catalogue

async fn catalogue_list(State(sh): State<Shared>, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let limit: usize = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(24).min(100);
    let offset: usize = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    let search = q.get("search").map(|s| s.trim()).unwrap_or("");
    let list = catalogue::load().await?;
    let mut found: Vec<&catalogue::Entry> = list.iter().filter(|e| search.is_empty() || e.matches(search)).collect();
    if q.get("ordering").map(String::as_str) == Some("name") {
        found.sort_by_key(|e| e.name.to_lowercase());
    }
    let items: Vec<Value> = found
        .iter()
        .skip(offset)
        .take(limit)
        .map(|e| {
            let mut it = e.summary();
            it["state"] = install_state(&sh, &e.id);
            it
        })
        .collect();
    Ok(Json(json!({"count": found.len(), "offset": offset, "limit": limit, "items": items})))
}

async fn catalogue_item(State(sh): State<Shared>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let mut it = catalogue::entry(&id).await?.summary();
    it["state"] = install_state(&sh, &id);
    Ok(Json(it))
}

#[derive(Deserialize)]
struct CatalogueReq {
    id: String,
}

/// Download the newest release of a catalogue plugin.
async fn fetch_release(id: &str) -> Result<(catalogue::Release, Vec<u8>)> {
    let entry = catalogue::entry(id).await?;
    let release = catalogue::resolve(&entry).await?;
    let bytes = catalogue::download(&release).await?;
    Ok((release, bytes))
}

async fn catalogue_install(State(sh): State<Shared>, Json(req): Json<CatalogueReq>) -> ApiResult<Json<Value>> {
    let r = async {
        let (release, bytes) = fetch_release(&req.id).await?;
        install_package(&sh, bytes, Some(release.file), Some(json!({"source": "openaction", "release": release.tag}))).await
    }
    .await;
    Ok(Json(log_failure(&sh, r)?))
}

/// What an imported profile needs to work on Linux (see `Engine::compat_report`).
async fn compat_report(State(sh): State<Shared>, Path(profile): Path<String>) -> ApiResult<Json<Value>> {
    // Offline the catalogue is unknown; the rest of the report still works.
    let list = catalogue::load().await.unwrap_or_default();
    let report = tokio::task::spawn_blocking(move || sh.lock().compat_report(&profile, &list)).await.map_err(|e| anyhow!(e))??;
    Ok(Json(report))
}

#[derive(Deserialize)]
struct CompatReq {
    group: String,
}

/// Fix one group of the report: install the plugin from the catalogue and, for
/// replacements, rewrite the profile's keys.
async fn compat_fix(State(sh): State<Shared>, Path(profile): Path<String>, Json(req): Json<CompatReq>) -> ApiResult<Json<Value>> {
    let r = async {
        let install = match req.group.strip_prefix("install:") {
            Some(id) => Some(id.to_string()),
            None => {
                let rule = crate::compat::RULES.iter().find(|r| r.group == req.group).with_context(|| crate::tr!("nothing to replace"))?;
                rule.plugin.map(|(id, _)| id.to_string()).filter(|id| sh.lock().registry.plugin(id).is_none())
            }
        };
        if let Some(id) = &install {
            let (release, bytes) = fetch_release(id).await?;
            install_package(&sh, bytes, Some(release.file), Some(json!({"source": "openaction", "release": release.tag}))).await?;
        }
        let changed = if req.group.starts_with("install:") {
            0
        } else {
            let sh2 = sh.clone();
            tokio::task::spawn_blocking(move || sh2.lock().apply_compat(&profile, &req.group)).await??
        };
        Ok::<_, anyhow::Error>(json!({"installed": install, "changed": changed}))
    }
    .await;
    Ok(Json(log_failure(&sh, r)?))
}

/// Write a package for a check of a plugin that is not installed.
fn check_file(sh: &Shared, key: &str, bytes: &[u8], filename: Option<&str>) -> Result<std::path::PathBuf> {
    if !matches!(packages::classify(bytes, filename)?, Package::Plugin) {
        anyhow::bail!(crate::tr!("Only plugins can be checked."));
    }
    let dir = sh.lock().paths.data.join("tools");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("check-{}.streamDeckPlugin", crate::net::urlencode(key)));
    std::fs::write(&file, bytes)?;
    Ok(file)
}

/// Check a catalogue plugin without installing it.
async fn catalogue_check(State(sh): State<Shared>, Json(req): Json<CatalogueReq>) -> ApiResult<Json<Value>> {
    not_twice(&req.id)?;
    let (release, bytes) = fetch_release(&req.id).await?;
    let file = check_file(&sh, &req.id, &bytes, Some(&release.file))?;
    start_check_with(&sh, req.id.clone(), CheckSource::Archive(file), json!({"source": "openaction", "release": release.tag}), None)?;
    Ok(Json(json!({"started": true, "key": req.id})))
}

// ---------------------------------------------------------------------- Linux check

async fn linux_checks(State(sh): State<Shared>) -> Json<Value> {
    let paths = sh.lock().paths.clone();
    Json(crate::linuxcheck::results(&paths))
}

/// Run a Linux check in the background; the UI gets a `linux-check` event when done.
pub(crate) fn start_check(sh: &Shared, key: String, source: CheckSource) -> Result<()> {
    start_check_with(sh, key, source, Value::Null, None)
}

pub(crate) fn start_check_with(sh: &Shared, key: String, source: CheckSource, extra: Value, done: Option<tokio::sync::oneshot::Sender<()>>) -> Result<()> {
    if !crate::linuxcheck::begin(&key) {
        anyhow::bail!(crate::tr!("A Linux check of this plugin is already running."));
    }
    sh.lock().emit(json!({"event": "linux-check", "key": key, "running": true}));
    let sh = sh.clone();
    tokio::task::spawn_blocking(move || {
        let paths = sh.lock().paths.clone();
        let node = crate::sdk::node_binary();
        let r = match &source {
            CheckSource::Dir(d) => crate::linuxcheck::run(&paths, &node, crate::linuxcheck::Source::Dir(d), extra),
            CheckSource::Archive(f) => {
                let r = crate::linuxcheck::run(&paths, &node, crate::linuxcheck::Source::Archive(f), extra);
                let _ = std::fs::remove_file(f);
                r
            }
        };
        crate::linuxcheck::end(&key);
        if let Some(d) = done {
            let _ = d.send(());
        }
        let e = sh.lock();
        match r {
            Ok(report) => {
                e.log(
                    "info",
                    crate::tr!(
                        "Linux check of {name}: {verdict}",
                        name = report["name"].as_str().unwrap_or(&key),
                        verdict = report["verdict"].as_str().unwrap_or("")
                    ),
                );
                e.emit(json!({"event": "linux-check", "key": key, "running": false, "result": report}));
            }
            Err(err) => {
                e.log("warn", crate::tr!("Linux check failed: {error}", error = format!("{err:#}")));
                e.emit(json!({"event": "linux-check", "key": key, "running": false, "error": format!("{err:#}")}));
            }
        }
    });
    Ok(())
}

pub(crate) enum CheckSource {
    Dir(std::path::PathBuf),
    Archive(std::path::PathBuf),
}

pub(crate) fn not_twice(key: &str) -> ApiResult<()> {
    if crate::linuxcheck::is_running(key) {
        return Err(ApiError(StatusCode::CONFLICT, crate::tr!("A Linux check of this plugin is already running.")));
    }
    Ok(())
}

async fn check_installed(State(sh): State<Shared>, Path(uuid): Path<String>) -> ApiResult<Json<Value>> {
    not_twice(&uuid)?;
    let dir = sh.lock().registry.plugin(&uuid).filter(|p| !p.builtin).map(|p| p.dir.clone()).with_context(|| crate::tr!("plugin not found"))?;
    start_check(&sh, uuid.clone(), CheckSource::Dir(dir))?;
    Ok(Json(json!({"started": true, "key": uuid})))
}

/// Bulk run (one at a time, see `check_all`).
pub(crate) static BULK: AtomicBool = AtomicBool::new(false);
pub(crate) static BULK_CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Deserialize)]
pub(crate) struct BulkReq {
    #[serde(default = "three")]
    pub parallel: usize,
    #[serde(default)]
    pub limit: Option<usize>,
}

fn three() -> usize {
    3
}

/// Run a bulk check in the background (`run` returns `(tested, total)`).
pub(crate) fn spawn_bulk<F>(sh: Shared, run: F) -> ApiResult<()>
where
    F: std::future::Future<Output = Result<(usize, usize)>> + Send + 'static,
{
    if BULK.swap(true, SeqCst) {
        return Err(ApiError(StatusCode::CONFLICT, crate::tr!("The Linux check of all plugins is already running.")));
    }
    BULK_CANCEL.store(false, SeqCst);
    tokio::spawn(async move {
        let r = run.await;
        BULK.store(false, SeqCst);
        let e = sh.lock();
        match r {
            Ok((done, total)) => e.log("info", crate::tr!("Linux check of all plugins finished: {done} of {total} tested", done = done, total = total)),
            Err(err) => e.log("warn", crate::tr!("Linux check of all plugins stopped: {error}", error = format!("{err:#}"))),
        }
        e.emit(json!({"event": "linux-check-all", "running": false}));
    });
    Ok(())
}

/// Check every catalogue plugin whose newest release wasn't tested yet (resumable).
async fn check_all(State(sh): State<Shared>, Json(req): Json<BulkReq>) -> ApiResult<Json<Value>> {
    spawn_bulk(sh.clone(), catalogue_bulk(sh, req))?;
    Ok(Json(json!({"started": true})))
}

async fn cancel_check_all() -> Json<Value> {
    BULK_CANCEL.store(true, SeqCst);
    Json(json!({"ok": true}))
}

async fn catalogue_bulk(sh: Shared, req: BulkReq) -> Result<(usize, usize)> {
    let mut todo: Vec<catalogue::Entry> = catalogue::load().await?.iter().cloned().collect();
    if let Some(n) = req.limit {
        todo.truncate(n);
    }
    let total = todo.len();
    let paths = sh.lock().paths.clone();
    sh.lock().log("info", crate::tr!("Linux check of all plugins: {n} to test", n = total));
    let parallel = req.parallel.clamp(1, 6);
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(parallel));
    let mut done = 0;
    for (i, entry) in todo.into_iter().enumerate() {
        if BULK_CANCEL.load(SeqCst) {
            break;
        }
        let permit = slots.clone().acquire_owned().await?;
        sh.lock().emit(json!({"event": "linux-check-all", "running": true, "done": i, "total": total, "current": entry.name}));
        let prepared = async {
            let release = catalogue::resolve(&entry).await?;
            // Skip what was tested in this release already (resume after an interruption).
            let tested = crate::linuxcheck::local(&paths, &entry.id).is_some_and(|r| !release.tag.is_empty() && r["release"] == release.tag.as_str());
            if tested {
                return Ok(None);
            }
            let bytes = catalogue::download(&release).await?;
            Ok::<_, anyhow::Error>(Some((check_file(&sh, &entry.id, &bytes, Some(&release.file))?, release.tag)))
        }
        .await;
        let (file, tag) = match prepared {
            Ok(Some(x)) => x,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!("linux check of {}: {e:#}", entry.id);
                continue;
            }
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        if start_check_with(&sh, entry.id.clone(), CheckSource::Archive(file), json!({"source": "openaction", "release": tag}), Some(tx)).is_err() {
            continue;
        }
        done += 1;
        tokio::spawn(async move {
            let _ = rx.await;
            drop(permit);
        });
    }
    // Wait for the last checks.
    let _ = slots.acquire_many(parallel as u32).await;
    Ok((done, total))
}

/// Background work for the published results: fetch them, publish new local ones.
pub fn spawn_check_sync(sh: Shared) {
    // Results of earlier runs that may not be published yet (publishing only adds newer ones).
    crate::linuxcheck::mark_dirty();
    tokio::spawn(async move {
        let mut last_fetch: Option<std::time::Instant> = None;
        loop {
            let (paths, repo, publish) = {
                let e = sh.lock();
                (e.paths.clone(), e.settings.checks_repo.clone(), e.settings.checks_publish)
            };
            if !repo.is_empty() {
                if publish {
                    let (p, r) = (paths.clone(), repo.clone());
                    match tokio::task::spawn_blocking(move || crate::linuxcheck::publish(&p, &r)).await {
                        Ok(Ok(n)) if n > 0 => sh.lock().log("info", crate::tr!("Linux check results published: {n} plugins", n = n)),
                        Ok(Err(e)) => tracing::warn!("publishing Linux checks: {e:#}"),
                        _ => {}
                    }
                }
                if last_fetch.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(12 * 3600)) {
                    match crate::linuxcheck::fetch_remote(&paths, &repo).await {
                        Ok(n) => {
                            last_fetch = Some(std::time::Instant::now());
                            tracing::info!("Linux check results of {n} plugins loaded from {repo}");
                        }
                        Err(e) => tracing::debug!("Linux check results not loaded: {e:#}"),
                    }
                }
            }
            // While a bulk run is going, publish less often.
            let pause = if BULK.load(SeqCst) { 600 } else { 60 };
            tokio::time::sleep(std::time::Duration::from_secs(pause)).await;
        }
    });
}

async fn plugin_diagnostics(State(sh): State<Shared>, Path(uuid): Path<String>) -> Json<Value> {
    Json(sh.lock().plugin_diagnostics(&uuid))
}

async fn uninstall(State(sh): State<Shared>, Path(uuid): Path<String>) -> ApiResult<Json<Value>> {
    let sh2 = sh.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let (p, old) = {
            let mut e = sh2.lock();
            let p = e.registry.plugin(&uuid).cloned().with_context(|| crate::tr!("plugin not found"))?;
            if p.builtin || !p.dir.starts_with(e.paths.user_plugins()) {
                anyhow::bail!(crate::tr!("built-in plugins cannot be removed"));
            }
            let old = e.plugin_conns.get_mut(&uuid).and_then(|c| {
                c.restarts = 99;
                c.child.take()
            });
            (p, old)
        };
        if let Some(mut ch) = old {
            crate::sdk::kill_group(&mut ch);
        }
        std::fs::remove_dir_all(&p.dir)?;
        let mut e = sh2.lock();
        e.rescan_plugins();
        e.log("info", crate::tr!("Plugin \"{name}\" removed", name = p.name()));
        Ok(())
    })
    .await
    .map_err(|e| anyhow!("{e}"))??;
    crate::sdk::start_html_host(&sh);
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct DeepLinkReq {
    url: String,
}

/// `streamdeck://…` links forwarded by `open-url`.
pub async fn handle_deeplink(sh: &Shared, url: &str) -> Result<Value> {
    let rest = url.strip_prefix("streamdeck://").unwrap_or(url);
    let path = rest.split(['?', '#']).next().unwrap_or("");
    tracing::info!("deep link: {path}");
    // streamdeck://plugins/message/<UUID>/hello?x=1 -> didReceiveDeepLink {url: "/hello?x=1"}
    // (used e.g. to finish OAuth logins of plugins).
    if let Some(msg) = rest.strip_prefix("plugins/message/") {
        let (uuid, tail) = msg.split_at(msg.find(['/', '?', '#']).unwrap_or(msg.len()));
        let mut e = sh.lock();
        if e.registry.plugin(uuid).is_none() {
            anyhow::bail!(crate::tr!("plugin not found"));
        }
        let url = if tail.starts_with('/') { tail.to_string() } else { format!("/{tail}") };
        e.send_plugin(uuid, json!({"event": "didReceiveDeepLink", "payload": {"url": url}}));
        return Ok(json!({"action": "plugin_message", "plugin": uuid}));
    }
    if path.starts_with("open/store") {
        sh.lock().emit(json!({"event": "market", "type": "open"}));
        return Ok(json!({"action": "open_store"}));
    }
    // Install buttons of the Elgato Marketplace website need the official app: its
    // packages are downloaded on the website and installed from the file instead.
    if path.starts_with("plugins/install")
        || path.starts_with("plugin/installprofiles")
        || path.starts_with("icons/install")
        || path.starts_with("marketplace/")
    {
        sh.lock().emit(json!({"event": "market", "type": "open", "hint": "elgato"}));
        anyhow::bail!(crate::tr!(
            "Elgato Marketplace links only work with the official app. Download the file on the Marketplace website and use \"Install from file\"."
        ));
    }
    Ok(json!({"action": "ignored"}))
}

async fn deeplink(State(sh): State<Shared>, Json(req): Json<DeepLinkReq>) -> ApiResult<Json<Value>> {
    match handle_deeplink(&sh, &req.url).await {
        Ok(v) => Ok(Json(v)),
        Err(e) => {
            sh.lock().log("error", format!("{e:#}"));
            Err(e.into())
        }
    }
}
