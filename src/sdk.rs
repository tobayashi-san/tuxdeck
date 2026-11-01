//! Elgato Stream Deck SDK host: WebSocket server for plugins and property
//! inspectors, and plugin process management (Node.js plugins run unchanged).

use crate::engine::Shared;
use crate::plugins::Runtime;
use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub async fn serve(shared: Shared, port: u16) -> Result<()> {
    let app = Router::new().route("/", get(ws_handler)).fallback(get(ws_handler)).with_state(shared);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.with_context(|| format!("SDK port {port} is in use"))?;
    tracing::info!("plugin SDK listening on ws://127.0.0.1:{port}");
    let local = listener.local_addr()?;
    axum::serve(listener, crate::peer::guard(app, local)).await?;
    Ok(())
}

async fn ws_handler(ws: WebSocketUpgrade, headers: axum::http::HeaderMap, State(sh): State<Shared>) -> Response {
    // Plugins (Node) send no Origin; property inspectors and HTML plugins come from the
    // plugin file origin (`web::plugin_router`). Anything else is a foreign web page.
    let port = sh.lock().plugin_port;
    if let Some(o) = headers.get(axum::http::header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let ok = [format!("http://127.0.0.1:{port}"), format!("http://localhost:{port}")].iter().any(|x| x == o);
        if !ok {
            tracing::warn!("rejected SDK connection from origin {o}");
            return axum::http::StatusCode::FORBIDDEN.into_response();
        }
    }
    ws.max_message_size(64 << 20).on_upgrade(move |s| connection(s, sh))
}

enum Peer {
    /// (plugin UUID, registration secret)
    Plugin(String, String),
    /// (action context, alias)
    Pi(String, String),
}

async fn connection(socket: WebSocket, sh: Shared) {
    let (mut sink, mut stream) = socket.split();
    // The first message must be the registration.
    // Plugins register with the secret they were started with, property inspectors with
    // the alias the UI got from `/api/pi`. Anything else is refused: the port is open to
    // every local program.
    let peer = loop {
        match stream.next().await {
            Some(Ok(Message::Text(t))) => {
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                let uuid = v.get("uuid").and_then(|u| u.as_str()).unwrap_or("").to_string();
                let e = sh.lock();
                let peer = match v.get("event").and_then(|e| e.as_str()) {
                    Some("registerPlugin") => e.plugin_by_key(&uuid).map(|p| Peer::Plugin(p, uuid)),
                    Some("registerPropertyInspector") => e.pi_context(&uuid).map(|c| Peer::Pi(c, uuid)),
                    _ => continue,
                };
                match peer {
                    Some(p) => break p,
                    None => {
                        tracing::warn!("SDK registration with an unknown id refused");
                        return;
                    }
                }
            }
            Some(Ok(_)) => continue,
            _ => return,
        }
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    {
        let mut e = sh.lock();
        match &peer {
            Peer::Plugin(uuid, _) => {
                tracing::info!("plugin registered: {uuid}");
                e.plugin_registered(uuid, tx.clone());
            }
            Peer::Pi(ctx, alias) => {
                e.pis.insert(ctx.clone(), tx.clone());
                e.pi_alias.insert(ctx.clone(), alias.clone());
                e.pi_opened(ctx);
                if let Some((loc, inst)) = e.lookup(ctx) {
                    if let Some(pl) = e.registry.plugin_for_action(&inst.uuid).map(|p| p.uuid.clone()) {
                        *e.plugin_stats.entry(pl.clone()).or_default().inspectors.entry(ctx.clone()).or_default() += 1;
                        e.send_plugin(&pl, json!({"event": "propertyInspectorDidAppear", "action": inst.uuid, "context": ctx, "device": loc.device}));
                    }
                }
            }
        }
    }
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(Message::Text(m.into())).await.is_err() {
                break;
            }
        }
    });
    while let Some(Ok(msg)) = stream.next().await {
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(mut v) = serde_json::from_str::<Value>(&text) else { continue };
        tracing::trace!(target: "sdk", "<- {}", text.chars().take(2000).collect::<String>());
        let sh2 = sh.clone();
        let (owner, alias, is_pi) = match &peer {
            Peer::Plugin(u, key) => (u.clone(), key, false),
            Peer::Pi(c, alias) => (c.clone(), alias, true),
        };
        // The registration id as context (plugins: global settings; inspectors: their
        // action) stands for the real one.
        if v.get("context").and_then(|c| c.as_str()) == Some(alias.as_str()) {
            v["context"] = Value::String(owner.clone());
        }
        // Handlers may do blocking work (key injection, rendering). A picture is decoded
        // (and scaled) before taking the engine lock; the handler then finds it ready.
        let _ = tokio::task::spawn_blocking(move || {
            if v.get("event").and_then(Value::as_str) == Some("setImage") {
                if let Some(url) = v.pointer("/payload/image").and_then(Value::as_str).filter(|u| u.starts_with("data:")) {
                    let _ = crate::render::ImageData::from_data_url(url);
                }
            }
            sh2.lock().handle_sdk(&owner, is_pi, v)
        })
        .await;
    }
    writer.abort();
    let mut e = sh.lock();
    match &peer {
        Peer::Plugin(uuid, _) => {
            if let Some(c) = e.plugin_conns.get_mut(uuid) {
                if c.tx.as_ref().map(|t| t.same_channel(&tx)).unwrap_or(false) {
                    c.tx = None;
                }
            }
            tracing::info!("plugin disconnected: {uuid}");
        }
        Peer::Pi(ctx, _) => {
            if e.pis.get(ctx).map(|t| t.same_channel(&tx)).unwrap_or(false) {
                e.pis.remove(ctx);
                e.pi_alias.remove(ctx);
            }
            if let Some((loc, inst)) = e.lookup(ctx) {
                if let Some(pl) = e.registry.plugin_for_action(&inst.uuid).map(|p| p.uuid.clone()) {
                    e.send_plugin(&pl, json!({"event": "propertyInspectorDidDisappear", "action": inst.uuid, "context": ctx, "device": loc.device}));
                }
            }
            if !e.pis.contains_key(ctx) {
                e.pi_closed(ctx);
            }
        }
    }
}

pub fn node_binary() -> String {
    std::env::var("TUXDECK_NODE").unwrap_or_else(|_| "node".into())
}

/// Major version of the Node.js the plugins run with (`None`: not found). Looked up again
/// after a minute, so installing Node.js needs no restart.
pub fn node_major() -> Option<u32> {
    static NODE: parking_lot::Mutex<Option<(Instant, Option<u32>)>> = parking_lot::Mutex::new(None);
    let mut n = NODE.lock();
    if let Some((at, v)) = *n {
        if at.elapsed() < Duration::from_secs(60) {
            return v;
        }
    }
    let v = Command::new(node_binary())
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_node_version(&String::from_utf8_lossy(&o.stdout)));
    *n = Some((Instant::now(), v));
    v
}

/// "v20.11.1" → 20.
fn parse_node_version(s: &str) -> Option<u32> {
    s.trim().trim_start_matches('v').split('.').next()?.parse().ok()
}

/// The Node.js major version a plugin needs: its manifest's `Nodejs.Version`, at least 20
/// (what TuxDeck's own start code needs).
fn needed_node(p: &crate::plugins::Plugin) -> u32 {
    let v = p.manifest.nodejs.as_ref().and_then(|n| n.get("Version"));
    let wanted = v.and_then(|v| v.as_u64().map(|n| n as u32).or_else(|| v.as_str().and_then(parse_node_version)));
    wanted.unwrap_or(0).max(20)
}

/// Node's `process.platform` value for the platform we report to plugins.
/// Plugins often dispatch on it (`{darwin, win32}[process.platform]`), which
/// would throw on Linux; we override it to stay consistent with the reported
/// platform. `None` leaves the real value (Linux) untouched.
fn node_platform(reported: &str) -> Option<&'static str> {
    match reported {
        "mac" | "macos" | "darwin" => Some("darwin"),
        "windows" | "win32" => Some("win32"),
        _ => None,
    }
}

/// Write a preload module that sets `process.platform` and return its path, so
/// Node plugins built for macOS/Windows see the platform we report to them.
fn platform_shim(paths: &crate::paths::Paths, node_plat: &str) -> Option<PathBuf> {
    if std::env::var_os("TUXDECK_NO_PLATFORM_SHIM").is_some() {
        return None;
    }
    let path = paths.logs().join("node-platform-shim.mjs");
    let os_type = if node_plat == "darwin" { "Darwin" } else { "Windows_NT" };
    let home = dirs::home_dir().unwrap_or_default();
    let js = |p: &std::path::Path| serde_json::to_string(&p.to_string_lossy()).unwrap_or_default();
    // Elgato's SDK keeps plugin data in the macOS/Windows folders of the official app;
    // paths built below them are redirected to our data directory.
    let from = [home.join("Library/Application Support/com.elgato.StreamDeck"), home.join("AppData/Roaming/Elgato/StreamDeck")];
    let code = [
        "// Auto-generated: report the Stream Deck platform to plugins (see sdk.rs).".to_string(),
        format!("Object.defineProperty(process,'platform',{{value:'{node_plat}',configurable:true}});"),
        format!("try{{const os=(await import('node:os')).default;os.platform=()=>'{node_plat}';os.type=()=>'{os_type}';}}catch{{}}"),
        format!("process.env.APPDATA??={};", js(&home.join("AppData/Roaming"))),
        format!(
            "try{{const path=(await import('node:path')).default;const FROM=[{},{}],TO={};\
             const map=(p)=>{{if(typeof p==='string')for(const f of FROM)if(p===f||p.startsWith(f+'/'))return TO+p.slice(f.length);return p;}};\
             for(const fn of ['join','resolve','normalize']){{const orig=path[fn];path[fn]=(...a)=>map(orig.apply(path,a));}}\
             (await import('node:module')).syncBuiltinESMExports();}}catch{{}}",
            js(&from[0]),
            js(&from[1]),
            js(&paths.plugin_data())
        ),
        String::new(),
    ]
    .join("\n");
    match std::fs::write(&path, code) {
        Ok(()) => Some(path),
        Err(e) => {
            tracing::warn!("could not write node platform shim: {e}");
            None
        }
    }
}

/// Start every runnable plugin that is not running yet.
pub fn start_plugins(sh: &Shared) {
    let mut e = sh.lock();
    let port = e.settings.sdk_port;
    let plugins: Vec<_> = e.registry.plugins.iter().filter(|p| matches!(p.runtime, Runtime::Node(_) | Runtime::Executable(_))).cloned().collect();
    let _ = std::fs::create_dir_all(e.paths.logs());
    for p in plugins {
        let running = e.plugin_conns.get_mut(&p.uuid).and_then(|c| c.child.as_mut()).map(|c| matches!(c.try_wait(), Ok(None))).unwrap_or(false);
        let gave_up = e.plugin_conns.get(&p.uuid).map(|c| c.restarts > 5).unwrap_or(false);
        if running || gave_up {
            continue;
        }
        let info = e.app_info(&p).to_string();
        let shim = node_platform(&e.settings.platform).and_then(|np| platform_shim(&e.paths, np));
        let mut cmd = match &p.runtime {
            Runtime::Node(script) => {
                // Without (a new enough) Node.js the plugin would only crash and restart.
                let need = needed_node(&p);
                let problem = match node_major() {
                    None => Some(crate::tr!(
                        "{name} can't start: Node.js {need} or newer is not installed, or not on the service's PATH (see \"Node.js\" in the handbook).",
                        name = p.name(),
                        need = need
                    )),
                    Some(v) if v < need => Some(crate::tr!(
                        "{name} can't start: it needs Node.js {need} or newer, but {found} is installed (see \"Node.js\" in the handbook).",
                        name = p.name(),
                        need = need,
                        found = v
                    )),
                    _ => None,
                };
                if let Some(msg) = problem {
                    let c = e.plugin_conns.entry(p.uuid.clone()).or_default();
                    if c.blocked.as_ref() != Some(&msg) {
                        c.blocked = Some(msg.clone());
                        e.log("error", msg);
                    }
                    continue;
                }
                if let Some(c) = e.plugin_conns.get_mut(&p.uuid) {
                    c.blocked = None;
                }
                let mut c = Command::new(node_binary());
                if let Some(shim) = &shim {
                    c.arg("--import").arg(shim);
                }
                c.arg(script);
                c
            }
            Runtime::Executable(bin) => Command::new(bin),
            _ => continue,
        };
        let key = e.new_plugin_key(&p.uuid);
        cmd.args(["-port", &port.to_string(), "-pluginUUID", &key, "-registerEvent", "registerPlugin", "-info", &info])
            .current_dir(&p.dir)
            .stdin(Stdio::null())
            .process_group(0);
        let log_path = e.paths.logs().join(format!("{}.log", p.uuid));
        // Keep plugin logs bounded: one previous generation.
        if std::fs::metadata(&log_path).is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
            let _ = std::fs::rename(&log_path, log_path.with_extension("log.1"));
        }
        let log = std::fs::OpenOptions::new().create(true).append(true).open(&log_path);
        match log {
            Ok(f) => {
                let f2 = f.try_clone().ok();
                cmd.stdout(f);
                if let Some(f2) = f2 {
                    cmd.stderr(f2);
                }
            }
            Err(_) => {
                cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        match cmd.spawn() {
            Ok(child) => {
                tracing::info!("started plugin {} (pid {})", p.uuid, child.id());
                let c = e.plugin_conns.entry(p.uuid.clone()).or_default();
                c.child = Some(child);
                c.started = Some(Instant::now());
            }
            Err(err) => e.log("error", crate::tr!("Could not start plugin {name}: {error}", name = p.name(), error = err)),
        }
    }
}

/// Names a monitored application's processes may have on Linux: the program name of a
/// Windows entry and its Linux counterparts (`obs64.exe` → `obs64`, `obs`), and for a
/// macOS bundle id its parts that name a known application or a program
/// (`com.spotify.client` → `spotify`).
fn process_names(app: &str) -> Vec<String> {
    let stem = crate::smart::normalize(app);
    let mut out = vec![];
    let known = |n: &str| crate::apps::linux_names(n);
    if app.contains('.') && !app.to_ascii_lowercase().ends_with(".exe") {
        const GENERIC: &[&str] = &["com", "org", "net", "io", "app", "apps", "client", "desktop", "mac", "macos", "osx", "studio", "helper"];
        for part in app.split('.').map(str::to_ascii_lowercase).filter(|p| p.len() >= 3 && !GENERIC.contains(&p.as_str())) {
            out.extend(known(&part));
            out.push(part);
        }
    } else {
        out.extend(known(&stem));
        out.push(stem);
    }
    // Desktop ids (`com.obsproject.studio`) are no process names.
    let mut names: Vec<String> = out.into_iter().filter(|n| !n.contains('.')).collect();
    names.sort();
    names.dedup();
    names
}

/// Names of the running processes (process name and the file name of argv[0]), lower case.
fn running_programs() -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(rd) = std::fs::read_dir("/proc") else { return out };
    for e in rd.flatten() {
        if !e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Ok(c) = std::fs::read_to_string(e.path().join("comm")) {
            out.insert(c.trim_end().to_lowercase());
        }
        if let Ok(cmd) = std::fs::read(e.path().join("cmdline")) {
            let argv0 = cmd.split(|b| *b == 0).next().map(|a| String::from_utf8_lossy(a).into_owned()).unwrap_or_default();
            if let Some(f) = std::path::Path::new(&argv0).file_name() {
                out.insert(f.to_string_lossy().to_lowercase());
            }
        }
    }
    out
}

/// applicationDidLaunch / applicationDidTerminate for the applications plugins monitor.
fn watch_applications(e: &mut crate::engine::Engine, running: &std::collections::HashSet<String>) {
    let mut now = std::collections::HashSet::new();
    for p in &e.registry.plugins {
        let Some(m) = p.manifest.applications_to_monitor.as_ref() else { continue };
        let apps = ["mac", "windows"].iter().filter_map(|k| m.get(k).and_then(Value::as_array)).flatten().filter_map(Value::as_str);
        for app in apps {
            if process_names(app).iter().any(|n| running.contains(n)) {
                now.insert((p.uuid.clone(), app.to_string()));
            }
        }
    }
    let started: Vec<_> = now.difference(&e.apps_running).cloned().collect();
    let ended: Vec<_> = e.apps_running.difference(&now).cloned().collect();
    for (plugin, app) in started {
        e.send_plugin(&plugin, json!({"event": "applicationDidLaunch", "payload": {"application": app}}));
    }
    for (plugin, app) in ended {
        e.send_plugin(&plugin, json!({"event": "applicationDidTerminate", "payload": {"application": app}}));
    }
    e.apps_running = now;
}

/// Keep plugin logs bounded while the plugins run: the full file becomes the previous
/// generation, the open one starts empty (plugins append, so they carry on writing).
fn rotate_logs(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "log") && e.metadata().is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
            let _ = std::fs::copy(&p, p.with_extension("log.1"));
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&p) {
                let _ = f.set_len(0);
            }
        }
    }
}

/// Restart crashed plugins (with a limit) every few seconds; tell plugins about the
/// applications they monitor; keep their logs bounded.
pub async fn watchdog(sh: Shared) {
    let mut last_restart = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let (logs, monitors) = {
            let e = sh.lock();
            (e.paths.logs(), e.registry.plugins.iter().any(|p| p.manifest.applications_to_monitor.is_some()) || !e.apps_running.is_empty())
        };
        let running = tokio::task::spawn_blocking(move || {
            rotate_logs(&logs);
            if monitors {
                running_programs()
            } else {
                Default::default()
            }
        })
        .await
        .unwrap_or_default();
        watch_applications(&mut sh.lock(), &running);
        let need = {
            let mut e = sh.lock();
            let mut need = false;
            let names: Vec<String> = e.plugin_conns.keys().cloned().collect();
            for n in names {
                let c = e.plugin_conns.get_mut(&n).unwrap();
                if let Some(child) = c.child.as_mut() {
                    if let Ok(Some(status)) = child.try_wait() {
                        // Clean up anything the plugin left running in its group.
                        unsafe {
                            libc::kill(-(child.id() as i32), libc::SIGKILL);
                        }
                        c.child = None;
                        // Crashes far apart don't add up to giving up on the plugin.
                        if c.started.is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
                            c.restarts = 0;
                        }
                        c.restarts += 1;
                        let r = c.restarts;
                        let n = if n == HTML_HOST { crate::tr!("HTML plugin host") } else { e.registry.plugin(&n).map(|p| p.name()).unwrap_or(n) };
                        if r <= 5 {
                            e.log("warn", crate::tr!("Plugin {name} exited ({status}), restart {r}/5", name = n, status = status, r = r));
                            need = true;
                        } else {
                            e.log("error", crate::tr!("Plugin {name} exited ({status}) and is no longer restarted", name = n, status = status));
                        }
                    }
                }
            }
            // Plugins waiting for Node.js: try again (the version is looked up once a minute).
            need || e.plugin_conns.values().any(|c| c.blocked.is_some())
        };
        if need && last_restart.elapsed() > Duration::from_secs(3) {
            last_restart = Instant::now();
            let sh2 = sh.clone();
            let _ = tokio::task::spawn_blocking(move || {
                start_plugins(&sh2);
                let host_dead = sh2.lock().plugin_conns.get(HTML_HOST).map(|c| c.child.is_none() && c.restarts <= 5).unwrap_or(false);
                if host_dead {
                    start_html_host(&sh2);
                }
            })
            .await;
        }
    }
}

const HTML_HOST: &str = "__html_plugin_host__";

/// Start one headless Chromium hosting all HTML plugins (like the official app's
/// QtWebEngine). The host page is served by the web UI server.
pub fn start_html_host(sh: &Shared) {
    let Some(browser) = crate::plugins::html_browser() else { return };
    let has_html = |e: &crate::engine::Engine| e.registry.plugins.iter().any(|p| matches!(p.runtime, Runtime::Html(_)));
    let running = {
        let mut e = sh.lock();
        let any = has_html(&e);
        let Some(c) = e.plugin_conns.get_mut(HTML_HOST) else { return start_html_host_locked(e, browser) };
        let alive = c.child.as_mut().is_some_and(|ch| matches!(ch.try_wait(), Ok(None)));
        match (alive, any) {
            // Reload so newly installed plugins are picked up.
            (true, true) => c.child.take(),
            (true, false) => return,
            (false, _) => None,
        }
    };
    // Killing waits for the browser to exit: not while holding the engine lock.
    if let Some(mut c) = running {
        kill_group(&mut c);
    }
    start_html_host_locked(sh.lock(), browser);
}

fn start_html_host_locked(mut e: parking_lot::MutexGuard<'_, crate::engine::Engine>, browser: PathBuf) {
    if !e.registry.plugins.iter().any(|p| matches!(p.runtime, Runtime::Html(_))) {
        return;
    }
    let profile_dir = e.paths.data.join("html-host");
    // Never restore tabs of a previous (killed) host: every restored tab would
    // register the plugins again. Local storage of the plugins is kept.
    let _ = std::fs::remove_dir_all(profile_dir.join("Default").join("Sessions"));
    for f in ["Current Session", "Current Tabs", "Last Session", "Last Tabs"] {
        let _ = std::fs::remove_file(profile_dir.join("Default").join(f));
    }
    e.html_host_key = uuid::Uuid::new_v4().simple().to_string();
    let url = format!("http://127.0.0.1:{}/plugin-host?k={}", e.plugin_port, e.html_host_key);
    let mut cmd = Command::new(browser);
    cmd.args([
        "--headless=new",
        "--disable-gpu",
        "--no-first-run",
        "--no-default-browser-check",
        "--mute-audio",
        "--autoplay-policy=no-user-gesture-required",
        "--hide-crash-restore-bubble",
        "--disable-session-crashed-bubble",
    ])
    .arg(format!("--user-data-dir={}", profile_dir.display()))
    .arg(url)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .process_group(0);
    // A browser left over from an unclean shutdown would keep the profile locked.
    let _ = Command::new("pkill").arg("-f").arg(format!("user-data-dir={}", profile_dir.display())).status();
    match cmd.spawn() {
        Ok(child) => {
            tracing::info!("HTML plugin host started (pid {})", child.id());
            let c = e.plugin_conns.entry(HTML_HOST.into()).or_default();
            c.child = Some(child);
            c.started = Some(Instant::now());
        }
        Err(err) => e.log("error", crate::tr!("Could not start the HTML plugin host: {error}", error = err)),
    }
}

/// Detach a plugin that is about to be replaced by a new version, so `start_plugins`
/// starts the new one (also after an uninstall gave it up). The returned process must
/// be stopped with [`kill_group`] after releasing the engine lock.
#[must_use]
pub fn stop_for_update(e: &mut crate::engine::Engine, uuid: &str) -> Option<Child> {
    let c = e.plugin_conns.get_mut(uuid)?;
    c.restarts = 0;
    c.child.take()
}

/// Terminate a plugin together with every process it spawned.
pub fn kill_group(child: &mut Child) {
    let pgid = child.id() as i32;
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    for _ in 0..20 {
        if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let _ = child.wait();
}

/// Stop all plugin processes (and the HTML plugin host); `start_plugins` and
/// `start_html_host` bring them back.
pub fn stop_all_plugins(sh: &Shared) {
    let children: Vec<Child> = {
        let mut e = sh.lock();
        e.plugin_conns
            .values_mut()
            .filter_map(|c| {
                c.restarts = 0;
                c.child.take()
            })
            .collect()
    };
    // Killing waits for the processes: not under the engine lock.
    for mut c in children {
        kill_group(&mut c);
    }
}

/// Restart all plugins (e.g. after a language change), like a fresh start.
pub fn restart_plugins(sh: &Shared) {
    stop_all_plugins(sh);
    start_plugins(sh);
    start_html_host(sh);
}

pub fn stop_plugins(sh: &Shared) {
    let mut e = sh.lock();
    for c in e.plugin_conns.values_mut() {
        if let Some(mut ch) = c.child.take() {
            kill_group(&mut ch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_versions() {
        assert_eq!(parse_node_version("v20.11.1\n"), Some(20));
        assert_eq!(parse_node_version("v12.22.9"), Some(12));
        assert_eq!(parse_node_version("nope"), None);
    }

    #[test]
    fn monitored_application_names() {
        assert_eq!(process_names("obs64.exe"), ["obs", "obs64"]);
        assert_eq!(process_names("com.obsproject.obs-studio"), ["obs", "obs-studio", "obsproject"]);
        assert_eq!(process_names("com.spotify.client"), ["spotify"]);
        assert_eq!(process_names("Discord.exe"), ["discord", "vesktop"]);
    }
}
