//! HTTP API + static web UI (see docs/API.md).

use crate::builtin::{self, FormCtx};
use crate::engine::{multi_lists, set_multi_lists, Shared, Slot};
use crate::layout;
use crate::plugins::{self, Runtime};
use crate::profile::{self, Profile};
use anyhow::anyhow;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

#[derive(RustEmbed)]
#[folder = "ui/"]
pub struct Assets;

struct ApiError(StatusCode, String);

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

fn not_found(what: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, crate::tr!("{what} not found", what = crate::i18n::tr(what)))
}

type ApiResult<T> = Result<T, ApiError>;

/// Only accept requests addressed to the loopback UI origin. Blocks DNS
/// rebinding (foreign Host header) and cross-site requests from other web pages
/// (foreign Origin header) – the API can install plugins, i.e. run code.
pub fn origin_allowed(headers: &HeaderMap, port: u16) -> bool {
    let ok_host = |h: &str| [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")].iter().any(|x| x == h);
    if let Some(h) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        if !ok_host(h) {
            return false;
        }
    }
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(o) => o.strip_prefix("http://").map(ok_host).unwrap_or(false),
    }
}

#[derive(Clone)]
struct Access {
    port: u16,
    token: std::sync::Arc<str>,
    session: std::sync::Arc<str>,
}

/// Name of the UI's session cookie. Cookies don't keep ports apart: with the port in the
/// name, a second instance (tests, another user's) doesn't replace this one's cookie.
fn session_cookie(port: u16) -> String {
    format!("tuxdeck_session_{port}")
}

/// How long a login code from `POST /api/login-code` can be used (once).
const LOGIN_CODE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

fn login_codes() -> &'static parking_lot::Mutex<Vec<(String, std::time::Instant)>> {
    static CODES: std::sync::OnceLock<parking_lot::Mutex<Vec<(String, std::time::Instant)>>> = std::sync::OnceLock::new();
    CODES.get_or_init(Default::default)
}

/// A one-time code that opens the UI (`/?code=…`): `tuxdeck ui` passes it to the browser
/// instead of the API token, so the browser's command line holds nothing lasting.
pub fn login_code() -> String {
    let code = uuid::Uuid::new_v4().simple().to_string();
    let mut codes = login_codes().lock();
    codes.retain(|(_, t)| t.elapsed() < LOGIN_CODE_TTL);
    codes.push((code.clone(), std::time::Instant::now()));
    code
}

fn take_login_code(code: &str) -> bool {
    let mut codes = login_codes().lock();
    codes.retain(|(_, t)| t.elapsed() < LOGIN_CODE_TTL);
    match codes.iter().position(|(c, _)| c == code) {
        Some(i) => {
            codes.remove(i);
            true
        }
        None => false,
    }
}

/// The API token of a request: `X-Tuxdeck-Token` or bearer header.
fn header_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-tuxdeck-token")
        .and_then(|v| v.to_str().ok())
        .or_else(|| headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")))
}

/// Whether a cookie `name` has this value (several cookies may share the name, e.g. with
/// different paths).
fn has_cookie(headers: &HeaderMap, name: &str, value: &str) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().strip_prefix(name)?.strip_prefix('='))
        .any(|v| v == value)
}

async fn guard(State(access): State<Access>, req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    if !origin_allowed(req.headers(), access.port) {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    let is_api = req.uri().path().starts_with("/api/");
    if let Some(denied) = check_access(&access, &req, is_api) {
        return denied;
    }
    next.run(req).await
}

/// The response that ends a request (login redirect, `401`), if any.
fn check_access(access: &Access, req: &axum::extract::Request, is_api: bool) -> Option<Response> {
    let query = |key: &str| req.uri().query().and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=')));
    // `?code=…` on a page (from `tuxdeck ui` / the app menu) becomes the session cookie,
    // then the URL is cleaned.
    if let (Some(q), Some(code), false) = (req.uri().query(), query("code"), is_api) {
        if take_login_code(code) {
            let rest: Vec<&str> = q.split('&').filter(|kv| !kv.starts_with("code=")).collect();
            // A path starting with "//" would leave the site.
            let path = format!("/{}", req.uri().path().trim_start_matches('/'));
            let target = if rest.is_empty() { path } else { format!("{path}?{}", rest.join("&")) };
            let cookie = format!("{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000", session_cookie(access.port), access.session);
            return Some((StatusCode::SEE_OTHER, [(header::LOCATION, target), (header::SET_COOKIE, cookie)]).into_response());
        }
    }
    // The API needs the token (header; WebSocket clients, which can't set headers, may
    // pass it as `?token=`) or the UI's session cookie. The cookie counts only for requests
    // from the UI page itself: browsers send it along with requests from other pages on
    // 127.0.0.1 as well (other ports are the same site).
    if is_api {
        let headers = req.headers();
        let from_ui = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()).is_none_or(|s| s == "same-origin" || s == "none");
        let ok =
            header_token(headers).or(query("token")) == Some(&*access.token) || (from_ui && has_cookie(headers, &session_cookie(access.port), &access.session));
        if !ok {
            return Some(
                (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": crate::tr!("Open the configuration from the application menu or with the command tuxdeck ui.")})),
                )
                    .into_response(),
            );
        }
    }
    None
}

/// Highest state index accepted from the UI (Elgato actions have one or two states).
const MAX_STATES: usize = 64;
/// Plugins and profiles can be well beyond axum's default 2 MB body limit.
const MAX_IMPORT_BYTES: usize = 512 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

pub fn router(sh: Shared) -> Router {
    let (port, token, session) = {
        let e = sh.lock();
        let _ = PLUGIN_DATA.set(e.paths.plugin_data());
        (e.settings.ui_port, e.api_token.clone(), e.ui_session.clone())
    };
    let access = Access { port, token: token.into(), session: session.into() };
    Router::new()
        .route("/api/state", get(state))
        .route("/api/login-code", post(new_login_code))
        .route("/api/actions", get(actions))
        .route("/api/pages/{profile}/{page}", get(get_page).delete(delete_page))
        // Custom key images (animated GIFs) are sent inline as data URLs.
        .route(
            "/api/pages/{profile}/{page}/slot",
            post(create_slot).delete(delete_slot).patch(patch_slot).layer(axum::extract::DefaultBodyLimit::max(MAX_IMAGE_BYTES)),
        )
        .route("/api/pages/{profile}/{page}/move", post(move_slot))
        .route("/api/pages/{profile}/{page}/paste", post(paste_slot).layer(axum::extract::DefaultBodyLimit::max(MAX_IMPORT_BYTES)))
        .route("/api/pages/{profile}/{page}/press", post(press_slot))
        .route("/api/render/{profile}/{page}/{controller}/{coord}", get(render))
        .route("/api/pi/{profile}/{page}", get(pi_info))
        .route("/api/profiles", post(create_profile))
        .route("/api/profiles/{profile}", axum::routing::patch(rename_profile).delete(delete_profile))
        .route("/api/profiles/{profile}/pages", post(add_page))
        .route("/api/profiles/{profile}/pages/order", post(reorder_pages))
        .route("/api/profiles/{profile}/export", get(export_profile))
        .route("/api/devices/{id}", post(update_device))
        .route("/api/import", post(import).layer(axum::extract::DefaultBodyLimit::max(MAX_IMPORT_BYTES)))
        .route("/api/settings", post(update_settings))
        .route("/api/backup", get(download_backup))
        .route("/api/backups", get(list_backups))
        .route("/api/backups/{name}", get(download_stored_backup))
        .route("/api/backups/{name}/restore", post(restore_stored_backup))
        .route("/api/restore", post(restore_uploaded_backup).layer(axum::extract::DefaultBodyLimit::max(MAX_IMPORT_BYTES)))
        .route("/api/events", get(events))
        // Plugin and icon pack files for the UI (icons): never run as a page here.
        .merge(plugin_file_routes().layer(axum::middleware::map_response(inert)))
        .route("/api/devices/{id}/input", post(device_input))
        .route("/api/devices/{id}/virtual", post(plug_virtual_device))
        .route("/api/windows", get(windows))
        .route("/api/iconpacks", get(list_iconpacks))
        .route("/api/iconpacks/{id}", axum::routing::delete(delete_iconpack))
        .route("/api/iconpacks/{id}/icons", get(iconpack_icons))
        .route("/api/active-app", post(set_active_app))
        .route("/api/open-url", post(open_external_url))
        .merge(crate::web_plugins::routes())
        .fallback(get(static_file))
        .with_state(sh)
        .layer(axum::middleware::from_fn_with_state(access, guard))
        .layer(axum::middleware::from_fn(json_errors))
}

/// Errors of the API are JSON (`{"error": "…"}`), also those axum or the guard produce
/// as plain text or with no body (403, 405, 413, 415, 422).
async fn json_errors(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let api = req.uri().path().starts_with("/api/");
    let r = next.run(req).await;
    let json = r.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("application/json"));
    if !api || json || !(r.status().is_client_error() || r.status().is_server_error()) {
        return r;
    }
    let status = r.status();
    let body = axum::body::to_bytes(r.into_body(), 64 * 1024).await.unwrap_or_default();
    let text = String::from_utf8_lossy(&body).trim().to_string();
    let msg = if text.is_empty() { status.canonical_reason().unwrap_or("error").to_lowercase() } else { text };
    (status, Json(json!({"error": msg}))).into_response()
}

async fn new_login_code() -> Json<Value> {
    Json(json!({"code": login_code()}))
}

fn plugin_file_routes() -> Router<Shared> {
    Router::new()
        .route("/plugin-files/{plugin}/{*path}", get(plugin_file))
        .route("/plugins/{dir}/{*path}", get(plugin_dir_file))
        .route("/iconpack-files/{id}/{*path}", get(iconpack_file))
}

/// Files of plugins and icon packs on the UI origin (key and action icons in the UI) must
/// not run scripts there: an HTML or SVG file opened as a page would get the UI's access.
async fn inert(mut r: Response) -> Response {
    let h = r.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, header::HeaderValue::from_static("sandbox"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, header::HeaderValue::from_static("nosniff"));
    r
}

/// The plugin file server: property inspectors, HTML plugins (`/plugin-host`), icon pack
/// and plugin data files on their own origin (port), which has no API. Plugin code there
/// can't use the UI's session; it talks to TuxDeck only through the SDK WebSocket, like
/// with the official app.
pub fn plugin_router(sh: Shared) -> Router {
    let (port, ui_port) = {
        let e = sh.lock();
        let _ = PLUGIN_DATA.set(e.paths.plugin_data());
        (e.plugin_port, e.settings.ui_port)
    };
    let origins = Value::from(["127.0.0.1", "localhost", "[::1]"].iter().map(|h| format!("http://{h}:{ui_port}")).collect::<Vec<_>>());
    // An external script, so inspectors whose CSP allows only 'self' scripts still get it.
    let script = PI_BRIDGE.replace("__UI_ORIGINS__", &origins.to_string());
    let bridge: std::sync::Arc<str> = format!("\n<script src=\"{BRIDGE_URL}\"></script>\n").into();
    plugin_file_routes()
        .route(BRIDGE_URL, get(move || async move { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], script) }))
        .route("/plugin-host", get(plugin_host))
        .fallback(get(plugin_static))
        .with_state(sh)
        .layer(axum::middleware::map_response_with_state(bridge, with_bridge))
        .layer(axum::middleware::from_fn_with_state((port, ui_port), plugin_guard))
}

const PI_BRIDGE: &str = include_str!("pi-bridge.js");
const BRIDGE_URL: &str = "/__tuxdeck/pi-bridge.js";

async fn plugin_guard(State((port, ui_port)): State<(u16, u16)>, req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    // DNS rebinding: only for pages on 127.0.0.1/localhost. No service workers: they would
    // outlive their plugin on this origin, which all plugins share.
    if !origin_allowed(req.headers(), port) || req.headers().contains_key("service-worker") {
        return (StatusCode::FORBIDDEN, "forbidden").into_response();
    }
    let mut r = next.run(req).await;
    // Pages here may only be framed by the UI (inspectors) or each other (plugin host).
    let ancestors = format!("frame-ancestors 'self' http://127.0.0.1:{ui_port} http://localhost:{ui_port} http://[::1]:{ui_port}");
    if let Ok(v) = header::HeaderValue::from_str(&ancestors) {
        r.headers_mut().insert(header::CONTENT_SECURITY_POLICY, v);
    }
    r
}

/// HTML pages get the bridge script (see `pi-bridge.js`).
async fn with_bridge(State(bridge): State<std::sync::Arc<str>>, r: Response) -> Response {
    let html = r.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("text/html"));
    if !html || !r.status().is_success() {
        return r;
    }
    let (mut parts, body) = r.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_IMPORT_BYTES).await else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    let mut out = bytes.to_vec();
    out.extend_from_slice(bridge.as_bytes());
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, axum::body::Body::from(out))
}

/// Files the plugins reference by absolute path (their data folder, clock faces).
async fn plugin_static(uri: axum::http::Uri) -> Response {
    plugin_data_file(uri.path()).or_else(|| uri.path().strip_prefix("/ClockFaces/").map(clock_faces)).unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

async fn static_file(uri: axum::http::Uri) -> Response {
    // Unknown API routes are errors, not the UI page.
    if uri.path().starts_with("/api/") {
        return not_found("Page").into_response();
    }
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path).or_else(|| if path.contains('.') { None } else { Assets::get("index.html") }) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.to_string()), (header::CACHE_CONTROL, "no-cache".to_string())], f.data.into_owned()).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The Clocks plugin's optional user clock faces, which it looks for next to the plugins
/// folder (`/ClockFaces/…` or `/plugins/ClockFaces/…` with our URLs). Without any, its
/// property inspector needs an empty list (a 404 makes it fail).
fn clock_faces(path: &str) -> Response {
    let path = path.trim_start_matches('/');
    plugin_data_file(&format!("{}/ClockFaces/{path}", elgato_data_dir().to_string_lossy())).unwrap_or_else(|| {
        if path == "fontstyles.json" {
            ([(header::CONTENT_TYPE, "application/json")], "[]").into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    })
}

/// Files the plugins wrote into their data folder (see `Paths::plugin_data`): property
/// inspectors reference them by absolute path, e.g. the weather plugin's key backgrounds.
fn plugin_data_file(url_path: &str) -> Option<Response> {
    let base = elgato_data_dir().canonicalize().ok()?;
    let file = std::path::Path::new(&crate::render::percent_decode(url_path)).canonicalize().ok()?;
    if !file.starts_with(&base) || !file.is_file() {
        return None;
    }
    let mime = mime_guess::from_path(&file).first_or_octet_stream();
    // Plugins keep their settings and tokens in there too: only images and fonts are
    // served (key backgrounds, clock faces), plus the clock faces' font list.
    let clock_faces = file.starts_with(base.join("ClockFaces"));
    let media = matches!(mime.type_(), mime_guess::mime::IMAGE | mime_guess::mime::FONT)
        || matches!(file.extension().and_then(|x| x.to_str()), Some("ttf" | "otf" | "woff" | "woff2"));
    let allowed = media || (clock_faces && mime.subtype() == mime_guess::mime::JSON);
    if !allowed {
        return None;
    }
    let data = std::fs::read(&file).ok()?;
    Some(([(header::CONTENT_TYPE, mime.to_string()), (header::CACHE_CONTROL, "no-cache".to_string())], data).into_response())
}

/// URL prefix of a plugin's files that keeps the `<name>.sdPlugin` directory name in the
/// path: Elgato's property inspector libraries locate the plugin root (translations,
/// shared resources) by looking for a path segment ending in `sdPlugin`.
pub fn plugin_base_url(p: &plugins::Plugin) -> String {
    match p.dir.file_name().map(|n| n.to_string_lossy().to_string()).filter(|n| n.ends_with(".sdPlugin")) {
        Some(n) => format!("/plugins/{}", url_path_encode(&n)),
        None => format!("/plugin-files/{}", url_path_encode(&p.uuid)),
    }
}

/// Percent-encode a (relative) path for use in a URL; `/` is kept.
pub fn url_path_encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

async fn plugin_dir_file(State(sh): State<Shared>, Path((dir, path)): Path<(String, String)>) -> Response {
    let uuid = {
        let e = sh.lock();
        e.registry.plugins.iter().find(|p| p.dir.file_name().map(|n| n.to_string_lossy() == dir).unwrap_or(false)).map(|p| p.uuid.clone())
    };
    match uuid {
        Some(u) => serve_plugin_file(&sh, &u, &path),
        None if dir == "ClockFaces" => clock_faces(&path),
        // Other folders next to the plugins in the official app's data directory.
        None => plugin_data_file(&format!("{}/{dir}/{path}", elgato_data_dir().to_string_lossy())).unwrap_or_else(|| StatusCode::NOT_FOUND.into_response()),
    }
}

static PLUGIN_DATA: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Where the plugins' SDK data lives (`Paths::plugin_data`, set when the router is built).
fn elgato_data_dir() -> std::path::PathBuf {
    PLUGIN_DATA.get().cloned().unwrap_or_default()
}

async fn plugin_file(State(sh): State<Shared>, Path((plugin, path)): Path<(String, String)>) -> Response {
    serve_plugin_file(&sh, &plugin, &path)
}

fn serve_plugin_file(sh: &Shared, plugin: &str, path: &str) -> Response {
    let dir = {
        let e = sh.lock();
        e.registry.plugin(plugin).map(|p| p.dir.clone())
    };
    let Some(dir) = dir else { return StatusCode::NOT_FOUND.into_response() };
    let rel = std::path::Path::new(path);
    if rel.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir)) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let full = dir.join(rel);
    let full = if full.is_file() { Some(full) } else { plugins::resolve_image(&dir, path) };
    // Links a plugin made may not lead out of its folder (to the user's files).
    let inside = |f: &std::path::Path| match (f.canonicalize(), dir.canonicalize()) {
        (Ok(f), Ok(d)) => f.starts_with(d),
        _ => false,
    };
    match full.filter(|f| inside(f)).and_then(|f| std::fs::read(&f).ok().map(|b| (f, b))) {
        Some((f, b)) => {
            let mime = mime_guess::from_path(&f).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.to_string())], b).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Page loaded by the headless browser: one iframe per HTML plugin.
async fn plugin_host(State(sh): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let mut e = sh.lock();
    // Only the host browser TuxDeck starts knows this: the page hands out the plugins'
    // registration secrets.
    if e.html_host_key.is_empty() || q.get("k") != Some(&e.html_host_key) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let html: Vec<plugins::Plugin> = e.registry.plugins.iter().filter(|p| matches!(p.runtime, Runtime::Html(_))).cloned().collect();
    let plugins: Vec<Value> = html
        .iter()
        .filter_map(|p| match &p.runtime {
            // Registration secret, like the `-pluginUUID` of other plugins.
            Runtime::Html(rel) => Some(json!({"uuid": e.new_plugin_key(&p.uuid), "url": format!("{}/{}", plugin_base_url(p), rel), "info": e.app_info(p)})),
            _ => None,
        })
        .collect();
    let html = format!(
        r#"<!doctype html><meta charset="utf-8"><title>TuxDeck plugin host</title><body><script>
const PORT = {port}; const PLUGINS = {plugins};
for (const p of PLUGINS) {{
  const f = document.createElement('iframe');
  f.src = p.url;
  f.onload = () => {{
    try {{ f.contentWindow.connectElgatoStreamDeckSocket(PORT, p.uuid, 'registerPlugin', JSON.stringify(p.info)); }}
    catch (e) {{ console.error('plugin', p.uuid, e); }}
  }};
  document.body.appendChild(f);
}}
</script></body>"#,
        port = e.settings.sdk_port,
        plugins = Value::Array(plugins)
    );
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

async fn state(State(sh): State<Shared>) -> Json<Value> {
    Json(sh.lock().state_json())
}

fn icon_url(plugin: &plugins::Plugin, rel: Option<&String>) -> Option<String> {
    let rel = rel?;
    let p = plugins::resolve_image(&plugin.dir, rel)?;
    let r = p.strip_prefix(&plugin.dir).ok()?.to_string_lossy().to_string();
    Some(format!("/plugin-files/{}/{}", plugin.uuid, r))
}

/// Icon of a built-in category: a white symbol of the bundled Elgato set on a coloured
/// tile, like the icons plugins bring for theirs.
fn builtin_category_icon(category: &str) -> Option<String> {
    use base64::Engine as _;
    let (icon, color) = match category {
        builtin::CAT_NAV => ("stream-deck-mk2", "#3b82f6"),
        builtin::CAT_SYSTEM => ("settings", "#64748b"),
        builtin::CAT_MULTI => ("multi-action", "#8b5cf6"),
        builtin::CAT_SOUND => ("sound-effect", "#e84393"),
        builtin::CAT_NEO => ("infobar", "#0ea5e9"),
        _ => return None,
    };
    let glyph = crate::resources::pack_glyph(icon)?;
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 48 48"><rect width="48" height="48" rx="10" fill="{color}"/><g transform="translate(9,9) scale(1.25)" fill="#ffffff">{glyph}</g></svg>"##
    );
    Some(format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg)))
}

async fn actions(State(sh): State<Shared>) -> Json<Value> {
    let e = sh.lock();
    let layouts = e.paths.layouts();
    let ctx = FormCtx {
        digital_time_layouts: layout::list_layouts(layouts.as_deref(), "DigitalTime"),
        pagination_layouts: layout::list_layouts(layouts.as_deref(), "Pagination"),
    };
    let mut cats: Vec<(String, Option<String>, Vec<Value>)> = Vec::new();
    let order = [builtin::CAT_NAV, builtin::CAT_SYSTEM, builtin::CAT_MULTI, builtin::CAT_SOUND, builtin::CAT_NEO];
    for c in order {
        cats.push((c.to_string(), builtin_category_icon(c), vec![]));
    }
    for p in &e.registry.plugins {
        for a in &p.manifest.actions {
            if a.visible_in_actions_list == Some(false) {
                continue;
            }
            let def = builtin::def(&a.uuid);
            let native_missing = p.runtime == Runtime::Native && def.is_none();
            if native_missing {
                continue; // native action of the official app we do not implement
            }
            let category = match def {
                Some(d) => d.category.to_string(),
                None => p.manifest.category.clone().unwrap_or_else(|| p.name()),
            };
            let pi = if def.is_none() { p.pi_path(a).map(|rel| format!("/plugin-files/{}/{}", url_path_encode(&p.uuid), url_path_encode(&rel))) } else { None };
            let entry = json!({
                "uuid": a.uuid, "plugin": p.uuid,
                "name": def.map(|d| crate::i18n::tr(d.name)).unwrap_or_else(|| p.action_name(a)),
                "tooltip": def.map(|d| crate::i18n::tr(d.tooltip)).unwrap_or_else(|| p.action_tooltip(a)),
                "icon": icon_url(p, a.icon.as_ref()).or_else(|| icon_url(p, a.states.first().and_then(|s| s.image.as_ref()))).or_else(|| def.map(|d| {
                    use base64::Engine as _;
                    format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(crate::render::fallback_icon_svg(d.glyph, d.color)))
                })),
                "controllers": a.controllers.clone().unwrap_or_else(|| def.map(|d| d.controllers.iter().map(|s| s.to_string()).collect()).unwrap_or_else(|| vec!["Keypad".into()])),
                "builtin": def.is_some(),
                "supported": def.map(|d| d.supported).unwrap_or_else(|| p.supported()),
                "unsupported_reason": match &p.runtime { Runtime::Unsupported(r) => Some(r.clone()), _ => None },
                "multi": def.map(|d| d.multi).unwrap_or(a.supported_in_multi_actions.unwrap_or(true)),
                "keylogic": if def.is_some() { builtin::in_key_logic(&a.uuid) } else { a.supported_in_key_logic_actions.unwrap_or(true) },
                "states": a.states.len().max(1),
                "pi": pi,
                "form": if def.is_some() { builtin::form(&a.uuid, &ctx) } else { json!([]) },
            });
            match cats.iter_mut().find(|c| c.0 == category) {
                Some(c) => c.2.push(entry),
                None => cats.push((category, icon_url(p, p.manifest.category_icon.as_ref().or(p.manifest.icon.as_ref())), vec![entry])),
            }
        }
    }
    // Keep catalogue order of the built-ins inside their categories.
    for c in cats.iter_mut() {
        c.2.sort_by_key(|a| builtin::CATALOG.iter().position(|d| d.uuid == a["uuid"]).unwrap_or(usize::MAX));
    }
    Json(Value::Array(
        cats.into_iter()
            .filter(|c| !c.2.is_empty())
            .map(|(category, icon, actions)| json!({"category": crate::i18n::tr(&category), "icon": icon, "actions": actions}))
            .collect(),
    ))
}

async fn get_page(State(sh): State<Shared>, Path((profile, page)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    sh.lock().page_json(&profile, &page).map(Json).ok_or_else(|| not_found("Page"))
}

#[derive(Deserialize)]
struct SlotReq {
    controller: String,
    coord: String,
    #[serde(default)]
    uuid: Option<String>,
    /// `/press`: how long the key is held (default 80 ms).
    #[serde(default)]
    hold_ms: Option<u64>,
    /// The deck the UI has selected (several decks may show the page).
    #[serde(default)]
    device: Option<String>,
}

/// Controllers and coordinates as stored in Elgato page manifests ("Keypad" + "col,row").
fn check_slot(controller: &str, coord: &str) -> ApiResult<()> {
    // Canonical numbers only ("1", not "+1" or "01"), as used as keys in the manifest.
    let num = |n: &str| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && (n == "0" || !n.starts_with('0')) && n.len() <= 2;
    let coord_ok =
        coord.split_once(',').is_some_and(|(c, r)| num(c) && num(r) && c.parse::<u8>().is_ok_and(|n| n < 32) && r.parse::<u8>().is_ok_and(|n| n < 32));
    if ["Keypad", "Encoder", "Neo"].contains(&controller) && coord_ok {
        Ok(())
    } else {
        Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("invalid key position")))
    }
}

async fn create_slot(State(sh): State<Shared>, Path((profile, page)): Path<(String, String)>, Json(req): Json<SlotReq>) -> ApiResult<Json<Value>> {
    check_slot(&req.controller, &req.coord)?;
    let mut e = sh.lock();
    let uuid = req.uuid.clone().ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, crate::tr!("uuid is missing")))?;
    e.page_mut(&profile, &page).map_err(|_| not_found("Page"))?;
    let inst = e.new_instance(&profile, &uuid)?;
    let old = e.page_mut(&profile, &page)?.controller_mut(&req.controller).actions.insert(req.coord.clone(), inst.clone());
    if let Some(old) = old {
        e.remove_instance(&profile, &old);
    }
    e.save_page(&profile, &page);
    e.emit(json!({"event": "state"}));
    Ok(Json(serde_json::to_value(inst).unwrap()))
}

async fn delete_slot(
    State(sh): State<Shared>,
    Path((profile, page)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let mut e = sh.lock();
    let (c, k) = (q.get("controller").cloned().unwrap_or_default(), q.get("coord").cloned().unwrap_or_default());
    check_slot(&c, &k)?;
    let removed = e.page_mut(&profile, &page).map_err(|_| not_found("Page"))?.controller_mut(&c).actions.remove(&k);
    if let Some(old) = &removed {
        e.remove_instance(&profile, old);
    }
    e.save_page(&profile, &page);
    e.emit(json!({"event": "state"}));
    // The removed action as stored, for undo (the UI's copy may be out of date).
    Ok(Json(json!({"ok": true, "removed": removed})))
}

#[derive(Deserialize)]
struct PatchReq {
    controller: String,
    coord: String,
    /// The deck the UI has selected (whose plugin context the change is reported to first).
    #[serde(default)]
    device: Option<String>,
    #[serde(default)]
    state: Option<usize>,
    #[serde(default)]
    style: Option<Map<String, Value>>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    settings: Option<Value>,
    /// Only these settings changed (merged into the stored ones), so a stale editor
    /// doesn't overwrite changes made elsewhere.
    #[serde(default)]
    settings_merge: Option<Map<String, Value>>,
    #[serde(default)]
    settings_remove: Option<Vec<String>>,
}

async fn patch_slot(State(sh): State<Shared>, Path((profile, page)): Path<(String, String)>, Json(mut req): Json<PatchReq>) -> ApiResult<Json<Value>> {
    // Decoding (and scaling a big picture) happens before taking the engine lock.
    let upload = match req.image.clone().filter(|i| !i.is_empty()) {
        Some(img) => Some(
            tokio::task::spawn_blocking(move || crate::render::ImageData::from_data_url(&img).map(crate::render::ImageData::storable))
                .await
                .map_err(|e| anyhow!("{e}"))?
                .ok_or_else(|| anyhow!(crate::tr!("invalid image")))?,
        ),
        None => None,
    };
    let mut guard = sh.lock();
    if req.settings_merge.is_some() || req.settings_remove.is_some() {
        let current =
            guard.profiles.get(&profile).and_then(|p| p.pages.get(&page)).and_then(|p| p.action(&req.controller, &req.coord)).map(|a| a.settings.clone());
        let mut merged = current.filter(Value::is_object).unwrap_or_else(|| json!({}));
        let o = merged.as_object_mut().unwrap();
        for k in req.settings_remove.take().unwrap_or_default() {
            o.remove(&k);
        }
        o.extend(req.settings_merge.take().unwrap_or_default());
        req.settings = Some(merged);
    }
    let e = &mut *guard;
    let page_dir = e.profiles.get(&profile).ok_or_else(|| not_found("Profile"))?.page_dir(&page).ok_or_else(|| not_found("Page"))?;
    let registry = &e.registry;
    let pm = e.profiles.get_mut(&profile).and_then(|p| p.pages.get_mut(&page)).ok_or_else(|| not_found("Page"))?;
    let inst = pm.action_mut(&req.controller, &req.coord).ok_or_else(|| not_found("Action"))?;
    let si = match req.state {
        Some(s) if s >= MAX_STATES => return Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("invalid state"))),
        Some(s) => s,
        // Stored state of an imported action may be out of range.
        None => inst.state.min(inst.states.len().max(1) - 1),
    };
    if req.settings.as_ref().is_some_and(|s| !s.is_object()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("Settings must be an object")));
    }
    while inst.states.len() <= si {
        inst.states.push(Default::default());
    }
    if let Some(style) = &req.style {
        let mut v = serde_json::to_value(&inst.states[si]).unwrap();
        for (k, val) in style {
            if val.is_null() {
                v.as_object_mut().unwrap().remove(k);
            } else {
                v[k] = val.clone();
            }
        }
        inst.states[si] = serde_json::from_value(v).map_err(|e| anyhow!(crate::tr!("invalid style: {e}", e = e)))?;
    }
    let replaced_image = req.image.as_ref().and(inst.states[si].image.clone());
    if let Some(img) = &req.image {
        if img.is_empty() {
            inst.states[si].image = None;
        } else {
            let data = upload.ok_or_else(|| anyhow!(crate::tr!("invalid image")))?;
            let ext = match data.mime.as_str() {
                "image/svg+xml" => "svg",
                "image/jpeg" => "jpg",
                "image/gif" => "gif",
                _ => "png",
            };
            let name = format!("Images/{}.{}", uuid::Uuid::new_v4().simple().to_string().to_uppercase(), ext);
            std::fs::create_dir_all(page_dir.join("Images")).map_err(anyhow::Error::from)?;
            std::fs::write(page_dir.join(&name), &data.bytes).map_err(anyhow::Error::from)?;
            inst.states[si].image = Some(name);
        }
    }
    if let Some(mut settings) = req.settings.clone() {
        // Multi action lists travel as `_actionsN` pseudo settings.
        let mut lists: Vec<(usize, Vec<Value>)> = Vec::new();
        if let Some(obj) = settings.as_object_mut() {
            let keys: Vec<String> = obj.keys().filter(|k| k.starts_with("_actions")).cloned().collect();
            for k in keys {
                let idx: usize = k.trim_start_matches("_actions").parse().unwrap_or(0);
                // At most three lists (Key Logic); entries must be action objects.
                if let Some(Value::Array(a)) = obj.remove(&k).filter(|_| idx < 3) {
                    lists.push((idx, a.into_iter().filter(Value::is_object).collect()));
                }
            }
        }
        if !lists.is_empty() {
            let mut cur: Vec<Vec<Value>> = multi_lists(inst).into_iter().map(|l| l.into_iter().map(|a| serde_json::to_value(a).unwrap()).collect()).collect();
            for (i, l) in lists {
                while cur.len() <= i {
                    cur.push(vec![]);
                }
                // Entries added by the UI may only carry a UUID: complete them.
                cur[i] = l
                    .into_iter()
                    .map(|mut a| {
                        if a.get("ActionID").is_none() {
                            a["ActionID"] = json!(uuid::Uuid::new_v4().to_string());
                        }
                        if a.get("Settings").is_none() {
                            let u = a.get("UUID").and_then(|u| u.as_str()).unwrap_or("").to_string();
                            a["Settings"] = builtin::default_settings(&u);
                        }
                        let u = a.get("UUID").and_then(|u| u.as_str()).unwrap_or("").to_string();
                        let want = builtin::def(&u)
                            .map(|d| d.states)
                            .or_else(|| registry.plugin_for_action(&u).and_then(|p| p.action(&u)).map(|x| x.states.len()))
                            .unwrap_or(1)
                            .max(1);
                        let have = a.get("States").and_then(|s| s.as_array()).map(|s| s.len()).unwrap_or(0);
                        if have < want {
                            let mut st = a.get("States").and_then(|s| s.as_array()).cloned().unwrap_or_default();
                            st.resize(want, json!({}));
                            a["States"] = Value::Array(st);
                        }
                        a
                    })
                    .collect();
            }
            set_multi_lists(inst, cur);
        }
        inst.settings = settings;
    }
    let inst = inst.clone();
    let ctx = e.ensure_context(&profile, &page, &Slot::new(&req.controller, &req.coord), req.device.as_deref()).map(|c| c.0);
    if let (Some(ctx), Some(_)) = (&ctx, &req.settings) {
        // Let a running plugin know (same as a PI setSettings).
        let s = inst.settings.clone();
        e.set_settings_from(ctx, s, true);
    }
    if let (Some(ctx), Some(_)) = (&ctx, &req.style) {
        e.send_title_params(ctx);
    }
    e.save_page(&profile, &page);
    // Remove the replaced custom image unless something else on the page still uses it.
    if let Some(old) = replaced_image.filter(|o| o.starts_with("Images/") && !o.contains("..")) {
        let used = e.profiles.get(&profile).and_then(|p| p.pages.get(&page)).and_then(|pm| serde_json::to_string(pm).ok()).is_some_and(|j| j.contains(&old));
        if !used {
            let _ = std::fs::remove_file(page_dir.join(&old));
        }
    }
    Ok(Json(serde_json::to_value(inst).unwrap()))
}

#[derive(Deserialize)]
struct MoveReq {
    from: SlotReq,
    to: SlotReq,
    #[serde(default)]
    copy: bool,
}

async fn move_slot(State(sh): State<Shared>, Path((profile, page)): Path<(String, String)>, Json(req): Json<MoveReq>) -> ApiResult<Json<Value>> {
    check_slot(&req.to.controller, &req.to.coord)?;
    let mut e = sh.lock();
    let pm = e.page_mut(&profile, &page).map_err(|_| not_found("Page"))?;
    let src = pm.action(&req.from.controller, &req.from.coord).cloned().ok_or_else(|| not_found("Source"))?;
    if req.copy {
        if src.uuid == builtin::OPEN_CHILD {
            return Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("Folders cannot be copied")));
        }
        let c = crate::engine::with_new_ids(&src);
        let old = pm.controller_mut(&req.to.controller).actions.insert(req.to.coord.clone(), c);
        if let Some(old) = old {
            e.remove_instance(&profile, &old);
        }
    } else {
        let dst = pm.controller_mut(&req.to.controller).actions.remove(&req.to.coord);
        pm.controller_mut(&req.from.controller).actions.remove(&req.from.coord);
        pm.controller_mut(&req.to.controller).actions.insert(req.to.coord.clone(), src);
        if let Some(d) = dst {
            pm.controller_mut(&req.from.controller).actions.insert(req.from.coord.clone(), d);
        }
    }
    e.save_page(&profile, &page);
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct PasteReq {
    controller: String,
    coord: String,
    /// The action as it was when copied (page view format).
    instance: Value,
    /// Page the action was copied from (for its custom images).
    from_profile: String,
    from_page: String,
}

/// Paste a copied action (from any page): new IDs, custom images are copied along.
async fn paste_slot(State(sh): State<Shared>, Path((profile, page)): Path<(String, String)>, Json(req): Json<PasteReq>) -> ApiResult<Json<Value>> {
    check_slot(&req.controller, &req.coord)?;
    let mut inst: crate::profile::ActionInstance =
        serde_json::from_value(req.instance).map_err(|e| ApiError(StatusCode::BAD_REQUEST, crate::tr!("invalid action: {e}", e = e)))?;
    if inst.uuid == builtin::OPEN_CHILD {
        return Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("Folders cannot be copied")));
    }
    // The page view exposes multi action lists as `_actionsN` settings.
    if let Some(o) = inst.settings.as_object_mut() {
        o.retain(|k, _| !k.starts_with("_actions"));
    }
    let mut inst = crate::engine::with_new_ids(&inst);
    let mut e = sh.lock();
    let dst_dir = e.profiles.get(&profile).ok_or_else(|| not_found("Profile"))?.page_dir(&page).ok_or_else(|| not_found("Page"))?;
    e.page_mut(&profile, &page).map_err(|_| not_found("Page"))?;
    // Custom images live next to the source page (or in its profile, like the renderer
    // resolves them); data URLs are self-contained.
    let src_dirs: Vec<std::path::PathBuf> =
        e.profiles.get(&req.from_profile).map(|p| p.page_dir(&req.from_page).into_iter().chain([p.dir.clone()]).collect()).unwrap_or_default();
    let mut lost = 0;
    for st in &mut inst.states {
        let Some(img) = st.image.take().filter(|i| !i.is_empty()) else { continue };
        if img.starts_with("data:") {
            st.image = Some(img);
            continue;
        }
        let rel = std::path::Path::new(&img);
        let safe = rel.is_relative() && !rel.components().any(|c| matches!(c, std::path::Component::ParentDir));
        let ext = rel.extension().and_then(|x| x.to_str()).unwrap_or("png").to_string();
        let name = format!("Images/{}.{ext}", uuid::Uuid::new_v4().simple().to_string().to_uppercase());
        let copied =
            safe && std::fs::create_dir_all(dst_dir.join("Images")).is_ok() && src_dirs.iter().any(|d| std::fs::copy(d.join(rel), dst_dir.join(&name)).is_ok());
        if copied {
            st.image = Some(name);
        } else {
            lost += 1;
        }
    }
    if lost > 0 {
        e.log("warn", crate::tr!("Custom images of the copied action could not be pasted (the source no longer exists)."));
    }
    let old = e.page_mut(&profile, &page)?.controller_mut(&req.controller).actions.insert(req.coord.clone(), inst.clone());
    if let Some(old) = old {
        e.remove_instance(&profile, &old);
    }
    e.save_page(&profile, &page);
    e.emit(json!({"event": "state"}));
    Ok(Json(serde_json::to_value(inst).unwrap()))
}

async fn press_slot(State(sh): State<Shared>, Path((profile, page)): Path<(String, String)>, Json(req): Json<SlotReq>) -> ApiResult<Json<Value>> {
    check_slot(&req.controller, &req.coord)?;
    let slot = Slot::new(&req.controller, &req.coord);
    let sh2 = sh.clone();
    tokio::task::spawn_blocking(move || {
        let mut e = sh2.lock();
        // The deck selected in the UI, if it shows the page; else the first that does.
        let wanted = req.device.as_deref();
        let showing = |d: &&crate::engine::DevState| d.profile == profile && d.page == page;
        let dev = e.devices.values().find(|d| showing(d) && wanted == Some(d.id.as_str())).or_else(|| e.devices.values().find(showing)).map(|d| d.id.clone());
        let hold = std::time::Duration::from_millis(req.hold_ms.unwrap_or(80).min(10_000));
        match dev {
            Some(d) if slot.controller == "Encoder" => {
                let idx = slot.col_row().0 as usize;
                e.dial_press(&d, idx, true);
                drop(e);
                std::thread::sleep(hold);
                sh2.lock().dial_press(&d, idx, false);
                Ok(())
            }
            Some(d) => {
                e.on_key(&d, &slot, true);
                drop(e);
                std::thread::sleep(hold);
                sh2.lock().on_key(&d, &slot, false);
                Ok(())
            }
            None => Err(ApiError(StatusCode::CONFLICT, crate::tr!("The page is not shown on any device"))),
        }
    })
    .await
    .map_err(|e| anyhow!("{e}"))??;
    Ok(Json(json!({"ok": true})))
}

/// Simulated device input (tests, virtual devices):
/// `{"type":"dial","index":0,"ticks":-2}`, `{"type":"dial_press","index":0,"down":true}`,
/// `{"type":"touch","x":250,"y":50,"kind":1|2|3,"x2":..,"y2":..}` (1 tap, 2 long, 3 swipe).
/// Unplug a simulated deck or plug it in again (tests).
async fn plug_virtual_device(State(sh): State<Shared>, Path(id): Path<String>, Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    let connected = req.get("connected").and_then(|v| v.as_bool()).unwrap_or(true);
    tokio::task::spawn_blocking(move || {
        let mut e = sh.lock();
        let (model, tx) = e.virtual_decks.get(&id).cloned().ok_or_else(|| not_found("Device"))?;
        match (connected, e.devices.get(&id).is_some_and(|d| d.connected())) {
            (false, true) => e.device_disconnected(&id),
            (true, false) => e.device_connected(id, model, "virtual".into(), tx),
            _ => {}
        }
        Ok::<_, ApiError>(())
    })
    .await
    .map_err(|e| anyhow!("{e}"))??;
    Ok(Json(json!({"ok": true})))
}

async fn device_input(State(sh): State<Shared>, Path(id): Path<String>, Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    tokio::task::spawn_blocking(move || {
        let mut e = sh.lock();
        let enc = e.devices.get(&id).map(|d| d.model.encoders as usize).ok_or_else(|| not_found("Device"))?;
        let n = |k: &str| req.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        let idx = n("index") as usize;
        match req.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "dial" | "dial_press" if idx >= enc => return Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("no such dial"))),
            "dial" => {
                let mut t = vec![0i8; enc];
                t[idx] = n("ticks").clamp(-127, 127) as i8;
                e.on_input(&id, crate::device::Input::EncoderTurn(t));
            }
            "dial_press" => e.dial_press(&id, idx, req.get("down").and_then(|v| v.as_bool()).unwrap_or(true)),
            "key" => {
                if !e.key_press(&id, idx, req.get("down").and_then(|v| v.as_bool()).unwrap_or(true)) {
                    return Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("no such key")));
                }
            }
            "touch" if enc > 0 => {
                let (x, y) = (n("x").clamp(0, 65535) as u16, n("y").clamp(0, 65535) as u16);
                let input = crate::device::Input::Touch {
                    kind: n("kind").clamp(1, 3) as u8,
                    x,
                    y,
                    x2: req.get("x2").and_then(|v| v.as_u64()).unwrap_or(x as u64) as u16,
                    y2: n("y2") as u16,
                };
                e.on_input(&id, input);
            }
            _ => return Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("unknown input"))),
        }
        Ok(())
    })
    .await
    .map_err(|e| anyhow!("{e}"))??;
    Ok(Json(json!({"ok": true})))
}

async fn render(
    State(sh): State<Shared>,
    Path((profile, page, controller, coord)): Path<(String, String, String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let coord = coord.trim_end_matches(".png").to_string();
    let size: u32 = q.get("size").and_then(|s| s.parse().ok()).unwrap_or(144).clamp(16, 512);
    let state: Option<usize> = q.get("state").and_then(|s| s.parse().ok());
    let png = tokio::task::spawn_blocking(move || {
        let job = sh.lock().preview_job(&profile, &page, &Slot::new(&controller, &coord), size, state, q.get("device").map(String::as_str));
        job.render()
    })
    .await
    .unwrap_or_default();
    ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "no-cache")], png).into_response()
}

async fn pi_info(State(sh): State<Shared>, Path((profile, page)): Path<(String, String)>, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let mut e = sh.lock();
    let slot = Slot::new(q.get("controller").map(String::as_str).unwrap_or("Keypad"), q.get("coord").map(String::as_str).unwrap_or("0,0"));
    let (ctx, loc, inst) = e.ensure_context(&profile, &page, &slot, q.get("device").map(String::as_str)).ok_or_else(|| not_found("Action"))?;
    let pl = e.registry.plugin_for_action(&inst.uuid).cloned().ok_or_else(|| not_found("Plugin"))?;
    let a = pl.action(&inst.uuid).cloned().ok_or_else(|| not_found("Action"))?;
    let rel = pl.pi_path(&a).ok_or_else(|| not_found("Property Inspector"))?;
    if builtin::def(&inst.uuid).is_some() {
        return Err(not_found("Property Inspector"));
    }
    let info = e.app_info(&pl);
    let payload = e.action_payload(&loc.slot, &inst, false);
    // The inspector registers with an alias and knows its action only by that.
    let alias = e.new_pi_alias(&ctx);
    Ok(Json(json!({
        "url": format!("http://127.0.0.1:{}{}/{}", e.plugin_port, plugin_base_url(&pl), url_path_encode(&rel)),
        "port": e.settings.sdk_port,
        "uuid": alias,
        "registerEvent": "registerPropertyInspector",
        "info": info,
        "actionInfo": {"action": inst.uuid, "context": alias, "device": loc.device, "payload": payload},
    })))
}

#[derive(Deserialize)]
struct NewProfile {
    name: String,
    model: String,
}

async fn create_profile(State(sh): State<Shared>, Json(req): Json<NewProfile>) -> ApiResult<Json<Value>> {
    if req.name.trim().is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("The name must not be empty")));
    }
    if !crate::device::MODELS.iter().any(|m| m.accepts_profile_model(&req.model)) {
        return Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("unknown device model")));
    }
    let mut e = sh.lock();
    // Like renaming: no line breaks or other control characters in names.
    let p = Profile::new(&e.paths.profiles(), &req.name.replace(char::is_control, " "), &req.model);
    p.save_all()?;
    let v = json!({"id": p.id, "name": p.manifest.name, "model": p.manifest.device.model, "pages": p.manifest.pages.pages});
    e.profiles.insert(p.id.clone(), p);
    e.emit(json!({"event": "state"}));
    Ok(Json(v))
}

#[derive(Deserialize)]
struct ProfilePatch {
    #[serde(default)]
    name: Option<String>,
    /// Smart profile: application (window class) that activates this profile; "" = none.
    #[serde(default)]
    app: Option<String>,
    /// The deck a smart profile is meant for; "" = every deck of the model.
    #[serde(default)]
    device: Option<String>,
}

async fn rename_profile(State(sh): State<Shared>, Path(profile): Path<String>, Json(req): Json<ProfilePatch>) -> ApiResult<Json<Value>> {
    let mut e = sh.lock();
    let p = e.profiles.get_mut(&profile).ok_or_else(|| not_found("Profile"))?;
    if let Some(n) = req.name {
        if n.trim().is_empty() {
            return Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("The name must not be empty")));
        }
        p.manifest.name = n.replace(char::is_control, " ");
    }
    if let Some(a) = &req.app {
        p.set_app_identifier(a);
    }
    if let Some(d) = &req.device {
        p.manifest.device.uuid = d.trim().to_string();
    }
    p.save_manifest()?;
    if req.app.is_some() || req.device.is_some() {
        // Re-evaluate with the currently focused application.
        let app = std::mem::take(&mut e.active_app);
        e.active_app_changed(&app);
    }
    e.emit(json!({"event": "state"}));
    Ok(Json(json!({"ok": true})))
}

/// Open application windows (window class + title), for linking smart profiles.
async fn windows(State(sh): State<Shared>) -> Json<Value> {
    let active = sh.lock().active_app.clone();
    let (list, available) = tokio::task::spawn_blocking(|| (crate::smart::open_windows(), crate::smart::available())).await.unwrap_or_default();
    Json(json!({"active": active, "available": available,
                "windows": list.into_iter().map(|(c, t)| json!({"class": c, "title": t})).collect::<Vec<_>>()}))
}

/// Simulate a focus change (tests, or desktops without a supported focus source).
async fn set_active_app(State(sh): State<Shared>, Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    let app = req.get("app").and_then(|v| v.as_str()).unwrap_or("").to_string();
    tokio::task::spawn_blocking(move || sh.lock().active_app_changed(&app)).await.map_err(|e| anyhow!("{e}"))?;
    Ok(Json(json!({"ok": true})))
}

/// Open a web link of the UI (marketplace, plugin source) in the user's default browser
/// instead of the UI window's own browser profile.
async fn open_external_url(Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    let url = req.get("url").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if !external_web_link(&url) {
        return Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("only web links can be opened")));
    }
    tokio::task::spawn_blocking(move || crate::engine::open_url(&url)).await.map_err(|e| anyhow!("{e}"))??;
    Ok(Json(json!({"ok": true})))
}

/// An http(s) link to another computer.
fn external_web_link(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else { return false };
    let host = u.host_str().unwrap_or("").trim_matches(['[', ']']).trim_end_matches('.').to_ascii_lowercase();
    let local = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => ip.is_loopback() || ip.is_unspecified() || matches!(ip, std::net::IpAddr::V6(v6) if v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())),
        Err(_) => host.is_empty() || host == "localhost" || host.ends_with(".localhost"),
    };
    matches!(u.scheme(), "http" | "https") && !local
}

async fn delete_profile(State(sh): State<Shared>, Path(profile): Path<String>) -> ApiResult<Json<Value>> {
    let mut e = sh.lock();
    if !e.profiles.contains_key(&profile) {
        return Err(not_found("Profile"));
    }
    // A deck showing it needs another profile that fits it (MK.2 and Original share theirs).
    let users: Vec<String> = e.devices.values().filter(|d| d.profile == profile).map(|d| d.id.clone()).collect();
    // Preferably a regular profile (not a smart one) that isn't meant for another deck.
    let next_for = |e: &crate::engine::Engine, d: &str| {
        let fits: Vec<&crate::profile::Profile> = e.profiles.values().filter(|o| o.id != profile && e.profile_fits(d, &o.id)).collect();
        let own_deck = |o: &&crate::profile::Profile| e.deck_named(&o.manifest.device.uuid).is_none_or(|x| x == d);
        fits.iter().find(|o| o.app_identifier().is_none() && own_deck(o)).or(fits.iter().find(|o| own_deck(o))).or(fits.first()).map(|o| o.id.clone())
    };
    if users.iter().any(|d| next_for(&e, d).is_none()) {
        return Err(ApiError(StatusCode::CONFLICT, crate::tr!("The last profile of a connected device cannot be deleted")));
    }
    let next: Vec<(String, String)> = users.iter().filter_map(|d| next_for(&e, d).map(|n| (d.clone(), n))).collect();
    let p = e.profiles.remove(&profile).unwrap();
    std::fs::remove_dir_all(&p.dir).map_err(anyhow::Error::from)?;
    for (d, n) in next {
        e.set_profile(&d, &n);
    }
    e.emit(json!({"event": "state"}));
    Ok(Json(json!({"ok": true})))
}

async fn add_page(State(sh): State<Shared>, Path(profile): Path<String>) -> ApiResult<Json<Value>> {
    let mut e = sh.lock();
    let p = e.profiles.get_mut(&profile).ok_or_else(|| not_found("Profile"))?;
    let id = uuid::Uuid::new_v4().to_string();
    // New pages start as a copy of the profile's template page (e.g. the Neo info bar).
    let template = p.pages.get(&p.manifest.pages.default).cloned().map(|mut t| {
        for c in t.controllers.iter_mut() {
            for a in c.actions.values_mut() {
                *a = crate::engine::with_new_ids(a);
            }
        }
        t
    });
    p.pages.insert(id.clone(), template.unwrap_or_default());
    p.manifest.pages.pages.push(id.clone());
    p.save_page(&id)?;
    p.save_manifest()?;
    let devs: Vec<String> = e.devices.values().filter(|d| d.profile == profile).map(|d| d.id.clone()).collect();
    for d in devs {
        e.refresh(&d);
    }
    e.emit(json!({"event": "state"}));
    Ok(Json(json!({"id": id})))
}

#[derive(Deserialize)]
struct PageOrder {
    pages: Vec<String>,
}

async fn reorder_pages(State(sh): State<Shared>, Path(profile): Path<String>, Json(req): Json<PageOrder>) -> ApiResult<Json<Value>> {
    let mut e = sh.lock();
    if !e.profiles.contains_key(&profile) {
        return Err(not_found("Profile"));
    }
    e.reorder_pages(&profile, &req.pages)?;
    let pages = e.profiles.get(&profile).map(|p| p.manifest.pages.pages.clone()).unwrap_or_default();
    Ok(Json(json!({"pages": pages})))
}

async fn delete_page(State(sh): State<Shared>, Path((profile, page)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    let mut e = sh.lock();
    let p = e.profiles.get_mut(&profile).ok_or_else(|| not_found("Profile"))?;
    if !p.manifest.pages.pages.contains(&page) {
        return Err(not_found("Page"));
    }
    if p.manifest.pages.pages.len() <= 1 {
        return Err(ApiError(StatusCode::CONFLICT, crate::tr!("The last page cannot be deleted")));
    }
    let actions: Vec<_> = p.pages.get(&page).map(|pg| pg.iter_actions().map(|(_, _, a)| a.clone()).collect()).unwrap_or_default();
    for a in actions {
        e.remove_instance(&profile, &a);
    }
    let p = e.profiles.get_mut(&profile).unwrap();
    p.manifest.pages.pages.retain(|x| *x != page);
    if p.manifest.pages.current == page {
        p.manifest.pages.current = p.manifest.pages.pages[0].clone();
    }
    p.delete_page_dir(&page);
    p.pages.remove(&page);
    p.save_manifest()?;
    let first = p.manifest.pages.pages[0].clone();
    let devs: Vec<String> = e.devices.values().filter(|d| d.profile == profile && (d.page == page || d.stack.contains(&page))).map(|d| d.id.clone()).collect();
    for d in devs {
        e.goto_page(&d, &first);
    }
    e.emit(json!({"event": "state"}));
    Ok(Json(json!({"ok": true})))
}

async fn export_profile(State(sh): State<Shared>, Path(profile): Path<String>) -> ApiResult<Response> {
    let (dir, id, name) = {
        let e = sh.lock();
        let p = e.profiles.get(&profile).ok_or_else(|| not_found("Profile"))?;
        (p.dir.clone(), p.id.clone(), p.manifest.name.clone())
    };
    let data = tokio::task::spawn_blocking(move || profile::export_archive(&dir, &id)).await.map_err(|e| anyhow!("{e}"))??;
    let name: String = name.chars().map(|c| if c.is_control() || matches!(c, '/' | '"' | '\\') { '_' } else { c }).collect();
    // ASCII fallback plus the UTF-8 name (RFC 6266).
    let ascii: String = name.chars().map(|c| if c.is_ascii() { c } else { '_' }).collect();
    let utf8: String = format!("{name}.streamDeckProfile")
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{ascii}.streamDeckProfile\"; filename*=UTF-8''{utf8}")),
        ],
        data,
    )
        .into_response())
}

#[derive(Deserialize)]
struct DeviceReq {
    profile: Option<String>,
    page: Option<String>,
    brightness: Option<u8>,
}

async fn update_device(State(sh): State<Shared>, Path(id): Path<String>, Json(req): Json<DeviceReq>) -> ApiResult<Json<Value>> {
    let sh2 = sh.clone();
    tokio::task::spawn_blocking(move || {
        let mut e = sh2.lock();
        if !e.devices.contains_key(&id) {
            return Err(not_found("Device"));
        }
        if let Some(p) = req.profile {
            if !e.profiles.contains_key(&p) {
                return Err(not_found("Profile"));
            }
            if !e.profile_fits(&id, &p) {
                return Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("The profile was made for a different device model")));
            }
            e.set_profile(&id, &p);
        }
        if let Some(p) = req.page {
            let profile = e.devices.get(&id).map(|d| d.profile.clone()).unwrap_or_default();
            if !e.profiles.get(&profile).is_some_and(|x| x.pages.contains_key(&p)) {
                return Err(not_found("Page"));
            }
            e.goto_page(&id, &p);
        }
        if let Some(b) = req.brightness {
            e.set_brightness(&id, b);
        }
        Ok(())
    })
    .await
    .map_err(|e| anyhow!("{e}"))??;
    Ok(Json(json!({"ok": true})))
}

/// Install a profile, plugin or icon pack file (`X-Filename` names it; `X-Linux-Check: 1`
/// tests a plugin on Linux right after installing it).
async fn import(State(sh): State<Shared>, headers: HeaderMap, body: Bytes) -> ApiResult<Json<Value>> {
    let name = headers.get("x-filename").and_then(|v| v.to_str().ok()).map(str::to_string);
    let check = headers.get("x-linux-check").is_some_and(|v| v == "1").then(|| json!({"source": "file"}));
    Ok(Json(crate::web_plugins::install_package(&sh, body.to_vec(), name, check).await?))
}

// ------------------------------------------------------------------------- backups

fn zip_download(name: &str, data: Vec<u8>) -> Response {
    ([(header::CONTENT_TYPE, "application/zip".to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""))], data).into_response()
}

/// A full backup to download (`?iconpacks=1` includes the icon packs, which can be big).
async fn download_backup(State(sh): State<Shared>, Query(q): Query<HashMap<String, String>>) -> ApiResult<Response> {
    let paths = sh.lock().paths.clone();
    let opts = crate::backup::Options { plugins: q.get("plugins").map(|v| v != "0").unwrap_or(true), icon_packs: q.get("iconpacks").is_some_and(|v| v == "1") };
    let name = crate::backup::file_name("tuxdeck-backup");
    let data = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let tmp = crate::backup::dir(&paths).join(format!(".{name}"));
        crate::backup::create(&paths, &tmp, opts)?;
        let data = std::fs::read(&tmp)?;
        let _ = std::fs::remove_file(&tmp);
        Ok(data)
    })
    .await
    .map_err(|e| anyhow!("{e}"))??;
    Ok(zip_download(&crate::backup::file_name("tuxdeck-backup"), data))
}

async fn list_backups(State(sh): State<Shared>) -> Json<Value> {
    let paths = sh.lock().paths.clone();
    Json(Value::Array(tokio::task::spawn_blocking(move || crate::backup::list(&paths)).await.unwrap_or_default()))
}

async fn download_stored_backup(State(sh): State<Shared>, Path(name): Path<String>) -> ApiResult<Response> {
    let paths = sh.lock().paths.clone();
    let file = crate::backup::stored(&paths, &name).ok_or_else(|| not_found("Backup"))?;
    let data = tokio::fs::read(file).await.map_err(anyhow::Error::from)?;
    Ok(zip_download(&name, data))
}

async fn restore_stored_backup(State(sh): State<Shared>, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    let paths = sh.lock().paths.clone();
    let file = crate::backup::stored(&paths, &name).ok_or_else(|| not_found("Backup"))?;
    let data = tokio::fs::read(file).await.map_err(anyhow::Error::from)?;
    restore_backup(sh, data).await
}

async fn restore_uploaded_backup(State(sh): State<Shared>, body: Bytes) -> ApiResult<Json<Value>> {
    restore_backup(sh, body.to_vec()).await
}

/// Restore: first a backup of the current state, then plugins stop, the backup replaces
/// the data and everything is loaded again.
async fn restore_backup(sh: Shared, data: Vec<u8>) -> ApiResult<Json<Value>> {
    let r = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let paths = sh.lock().paths.clone();
        let staged = crate::backup::unpack(&paths, &data)?;
        let safety = crate::backup::new_file(&paths, "before-restore");
        crate::backup::create(&paths, &safety, crate::backup::Options { plugins: true, icon_packs: false })
            .map_err(|e| e.context(crate::tr!("Could not back up the current state; nothing was restored.")))?;
        crate::sdk::stop_all_plugins(&sh);
        let applied = {
            let mut e = sh.lock();
            let mut settings = e.settings.clone();
            let r = crate::backup::apply(&paths, &mut settings, &staged);
            e.settings = settings;
            e.after_restore();
            r
        };
        crate::sdk::start_plugins(&sh);
        crate::sdk::start_html_host(&sh);
        applied?;
        let summary = crate::tr!(
            "Restored: {profiles} profiles, {plugins} plugins, {packs} icon packs.",
            profiles = staged.profiles.len(),
            plugins = staged.plugins.len(),
            packs = staged.icon_packs.len()
        );
        sh.lock().log("info", summary.clone());
        Ok(json!({"ok": true, "message": summary, "profiles": staged.profiles.len(), "plugins": staged.plugins.len(),
                  "icon_packs": staged.icon_packs.len(), "elgato": staged.elgato,
                  "safety_backup": safety.file_name().map(|n| n.to_string_lossy().into_owned())}))
    })
    .await
    .map_err(|e| anyhow!("{e}"))??;
    Ok(Json(r))
}

async fn update_settings(State(sh): State<Shared>, Json(req): Json<Map<String, Value>>) -> ApiResult<Json<Value>> {
    // Check the whole request first: a rejected request changes nothing.
    let bad = |msg: String| Err(ApiError(StatusCode::BAD_REQUEST, msg));
    let text = |k: &str| -> Result<Option<&str>, ApiError> {
        match req.get(k) {
            None => Ok(None),
            Some(Value::String(v)) => Ok(Some(v.as_str())),
            Some(_) => Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("{name} must be a string", name = k))),
        }
    };
    let flag = |k: &str| -> Result<Option<bool>, ApiError> {
        match req.get(k) {
            None => Ok(None),
            Some(Value::Bool(v)) => Ok(Some(*v)),
            Some(_) => Err(ApiError(StatusCode::BAD_REQUEST, crate::tr!("{name} must be true or false", name = k))),
        }
    };
    let (cmd_as_ctrl, checks_publish) = (flag("cmd_as_ctrl")?, flag("checks_publish")?);
    let (checks_repo, platform, language, touch_color, injector) =
        (text("checks_repo")?, text("platform")?, text("language")?, text("touch_color")?, text("injector")?);
    if let Some(v) = checks_repo {
        let valid = v.is_empty() || (v.split('/').count() == 2 && v.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c)));
        if !valid {
            return bad(crate::tr!("invalid repository {name}", name = format!("{v:?}")));
        }
    }
    if let Some(v) = platform {
        if !["mac", "windows", "linux"].contains(&v) {
            return bad(crate::tr!("unknown platform {name}", name = format!("{v:?}")));
        }
    }
    if let Some(v) = language {
        let valid = v == "auto" || crate::i18n::PLUGIN_LANGUAGES.contains(&v) || crate::i18n::ui_languages().iter().any(|l| l == v);
        if !valid {
            return bad(crate::tr!("unknown language {name}", name = format!("{v:?}")));
        }
    }
    if let Some(v) = touch_color {
        let valid = v.len() == 7 && v.starts_with('#') && v[1..].chars().all(|c| c.is_ascii_hexdigit());
        if !valid {
            return bad(crate::tr!("touch_color must be #rrggbb"));
        }
    }
    if let Some(v) = injector {
        if !crate::input::Injector::CHOICES.contains(&v) {
            return bad(crate::tr!("unknown input backend {name}", name = format!("{v:?}")));
        }
    }

    let mut e = sh.lock();
    // Plugins learn the language and platform only at start.
    let mut restart_plugins = false;
    if let Some(v) = cmd_as_ctrl {
        e.settings.cmd_as_ctrl = v;
    }
    if let Some(v) = checks_publish {
        e.settings.checks_publish = v;
    }
    if let Some(v) = checks_repo {
        e.settings.checks_repo = v.to_string();
    }
    if let Some(v) = platform {
        restart_plugins |= e.settings.platform != v;
        e.settings.platform = v.to_string();
    }
    if let Some(v) = language {
        restart_plugins |= e.settings.language != v;
        e.settings.language = v.to_string();
        crate::i18n::set_language(&crate::i18n::resolve(v));
        e.rescan_plugins();
        e.emit(json!({"event": "state"}));
    }
    if let Some(v) = touch_color {
        e.settings.touch_color = v.to_lowercase();
        e.refresh_all();
    }
    if let Some(v) = injector {
        e.settings.injector = v.to_string();
        let active = e.input.set_backend(v);
        e.log("info", crate::tr!("Input backend: {name}", name = active));
    }
    e.settings.save(&e.paths);
    e.emit(json!({"event": "state"}));
    drop(e);
    if restart_plugins {
        tokio::task::spawn_blocking(move || crate::sdk::restart_plugins(&sh));
    }
    Ok(Json(json!({"ok": true})))
}

fn icon_pack(sh: &Shared, id: &str) -> ApiResult<crate::iconpacks::IconPack> {
    let root = sh.lock().paths.icon_packs();
    let folder = format!("{id}.sdIconPack");
    if !crate::iconpacks::safe_rel(&folder) || id.contains('/') {
        return Err(not_found("Icon pack"));
    }
    crate::iconpacks::load(&root.join(folder)).map_err(|_| not_found("Icon pack"))
}

async fn list_iconpacks(State(sh): State<Shared>) -> Json<Value> {
    let root = sh.lock().paths.icon_packs();
    let packs = tokio::task::spawn_blocking(move || crate::iconpacks::load_all(&root)).await.unwrap_or_default();
    Json(Value::Array(packs.iter().map(|p| p.summary()).collect()))
}

/// `?search=&offset=&limit=` (limit ≤ 500, default 200).
async fn iconpack_icons(State(sh): State<Shared>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let pack = icon_pack(&sh, &id)?;
    let found = pack.search(q.get("search").map(String::as_str).unwrap_or(""));
    let offset: usize = q.get("offset").and_then(|s| s.parse().ok()).unwrap_or(0);
    let limit: usize = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(200).min(500);
    let items: Vec<Value> =
        found.iter().skip(offset).take(limit).map(|i| json!({"path": i.path, "name": i.name, "tags": i.tags, "url": pack.icon_url(i)})).collect();
    Ok(Json(json!({"pack": pack.summary(), "total": found.len(), "offset": offset, "items": items})))
}

async fn iconpack_file(State(sh): State<Shared>, Path((id, path)): Path<(String, String)>) -> Response {
    let Ok(pack) = icon_pack(&sh, &id) else { return StatusCode::NOT_FOUND.into_response() };
    if !crate::iconpacks::safe_rel(&path) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let f = pack.dir.join(&path);
    match std::fs::read(&f) {
        Ok(b) => {
            let mime = mime_guess::from_path(&f).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.to_string()), (header::CACHE_CONTROL, "max-age=3600".to_string())], b).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn delete_iconpack(State(sh): State<Shared>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let pack = icon_pack(&sh, &id)?;
    std::fs::remove_dir_all(&pack.dir).map_err(anyhow::Error::from)?;
    let e = sh.lock();
    e.emit(json!({"event": "iconpacks"}));
    e.log("info", crate::tr!("Icon pack \"{name}\" removed", name = pack.name()));
    Ok(Json(json!({"ok": true})))
}

async fn events(ws: WebSocketUpgrade, State(sh): State<Shared>) -> Response {
    let rx = sh.lock().events.subscribe();
    ws.on_upgrade(move |s| events_conn(s, rx))
}

async fn events_conn(mut socket: WebSocket, mut rx: tokio::sync::broadcast::Receiver<String>) {
    loop {
        tokio::select! {
            m = rx.recv() => match m {
                Ok(m) => if socket.send(Message::Text(m.into())).await.is_err() { break },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if socket.send(Message::Text(json!({"event": "state"}).to_string().into())).await.is_err() { break }
                }
                Err(_) => break,
            },
            r = socket.recv() => match r {
                Some(Ok(_)) => {}
                _ => break,
            }
        }
    }
}
