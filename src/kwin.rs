//! Active window on KDE Plasma, for Smart Profiles.
//!
//! KWin tells no other program which window is active. TuxDeck loads a small KWin script
//! that reports focus changes and the open windows back over the session bus
//! (`callDBus`), to a name TuxDeck owns. Calls that don't come from KWin are ignored.
//! Works with Plasma 5 and 6; KWin restarts are noticed and the script is loaded again.

use crate::engine::Shared;
use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use zbus::fdo::{DBusProxy, RequestNameFlags, RequestNameReply};
use zbus::message::Header;
use zbus::names::BusName;
use zbus::Connection;

const NAME: &str = "io.github.tobayashi_san.TuxDeck";
const PATH: &str = "/io/github/tobayashi_san/TuxDeck";
const IFACE: &str = "io.github.tobayashi_san.TuxDeck.Focus";
const KWIN: &str = "org.kde.KWin";
/// KWin's name for the loaded script.
const PLUGIN: &str = "tuxdeck-focus";

/// The script runs and reports to us.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// (class, title) of the open windows, as last reported.
static WINDOWS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
static CONN: std::sync::OnceLock<Connection> = std::sync::OnceLock::new();

/// A KDE Plasma session. Hyprland and X11 with xprop are checked before (see `smart`), so
/// this is Plasma on Wayland, or on X11 without xprop.
pub fn wanted() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_uppercase().contains("KDE"))
}

/// Whether KWin reports the active window.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

pub fn open_windows() -> Vec<(String, String)> {
    WINDOWS.lock().clone()
}

/// The KWin script (Plasma 6 names its API `window…`, Plasma 5 `client…`).
fn script() -> String {
    format!(
        r#"// Loaded by TuxDeck: reports the active window for Smart Profiles.
const send = (method, arg) => callDBus("{NAME}", "{PATH}", "{IFACE}", method, arg);
const ws = workspace;
const cls = (w) => (w ? String(w.resourceClass) : "");
const all = () => (ws.windowList ? ws.windowList() : ws.clientList());
function windows() {{
    const v = [];
    for (const w of all()) {{
        if (w.normalWindow && !w.skipTaskbar) v.push([cls(w), String(w.caption)]);
    }}
    send("Windows", JSON.stringify(v));
}}
function activated(w) {{ send("ActiveWindow", cls(w)); }}
(ws.windowActivated || ws.clientActivated).connect(activated);
(ws.windowAdded || ws.clientAdded).connect(windows);
(ws.windowRemoved || ws.clientRemoved).connect(windows);
activated(ws.activeWindow !== undefined ? ws.activeWindow : ws.activeClient);
windows();
"#
    )
}

/// `[["class","title"],…]` from the script: one entry per class, sorted.
fn parse_windows(json: &str) -> Vec<(String, String)> {
    let list: Vec<(String, String)> = serde_json::from_str(json).unwrap_or_default();
    let mut v: Vec<_> = list.into_iter().filter(|(c, _)| !c.is_empty()).collect();
    v.sort();
    v.dedup_by(|a, b| a.0 == b.0);
    v
}

struct Focus {
    sh: Shared,
    /// Unique bus name of KWin, the only accepted caller.
    kwin: Mutex<Option<String>>,
}

impl Focus {
    async fn sent_by_kwin(&self, hdr: &Header<'_>, conn: &Connection) -> bool {
        let Some(sender) = hdr.sender().map(|s| s.to_string()) else { return false };
        if self.kwin.lock().as_deref() == Some(sender.as_str()) {
            return true;
        }
        // KWin restarted, or the first call.
        let owner = kwin_owner(conn).await;
        let ok = owner.as_deref() == Some(sender.as_str());
        *self.kwin.lock() = owner;
        ok
    }
}

#[zbus::interface(name = "io.github.tobayashi_san.TuxDeck.Focus")]
impl Focus {
    async fn active_window(&self, #[zbus(header)] hdr: Header<'_>, #[zbus(connection)] conn: &Connection, class: String) {
        if self.sent_by_kwin(&hdr, conn).await {
            let sh = self.sh.clone();
            let _ = tokio::task::spawn_blocking(move || sh.lock().active_app_changed(&class)).await;
        }
    }

    async fn windows(&self, #[zbus(header)] hdr: Header<'_>, #[zbus(connection)] conn: &Connection, json: String) {
        if self.sent_by_kwin(&hdr, conn).await {
            *WINDOWS.lock() = parse_windows(&json);
        }
    }
}

async fn kwin_owner(conn: &Connection) -> Option<String> {
    let dbus = DBusProxy::new(conn).await.ok()?;
    dbus.get_name_owner(BusName::try_from(KWIN).ok()?).await.ok().map(|n| n.to_string())
}

async fn call<R>(conn: &Connection, path: &str, iface: &str, method: &str, args: &(impl serde::Serialize + zbus::zvariant::DynamicType)) -> Result<R>
where
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    let reply = conn.call_method(Some(KWIN), path, Some(iface), method, args).await?;
    Ok(reply.body().deserialize::<R>()?)
}

async fn script_loaded(conn: &Connection) -> Result<bool> {
    call(conn, "/Scripting", "org.kde.kwin.Scripting", "isScriptLoaded", &(PLUGIN,)).await
}

async fn unload(conn: &Connection) {
    let _ = call::<bool>(conn, "/Scripting", "org.kde.kwin.Scripting", "unloadScript", &(PLUGIN,)).await;
}

/// Write the script and load it into KWin (replacing one left by an earlier run).
async fn load(conn: &Connection, file: &PathBuf) -> Result<()> {
    std::fs::write(file, script()).with_context(|| format!("writing {}", file.display()))?;
    unload(conn).await;
    let id: i32 = call(conn, "/Scripting", "org.kde.kwin.Scripting", "loadScript", &(file.display().to_string(), PLUGIN)).await?;
    if id < 0 {
        bail!("KWin did not load the script");
    }
    // Plasma 6 and Plasma 5 publish the script under different paths.
    for path in [format!("/Scripting/Script{id}"), format!("/{id}")] {
        if call::<()>(conn, &path, "org.kde.kwin.Script", "run", &()).await.is_ok() {
            return Ok(());
        }
    }
    unload(conn).await;
    bail!("KWin did not run the script")
}

async fn serve(sh: Shared) -> Result<()> {
    let conn = zbus::connection::Builder::session()?.serve_at(PATH, Focus { sh, kwin: Mutex::new(None) })?.build().await?;
    // Not with a second instance (a test instance next to the installed one).
    let reply = DBusProxy::new(&conn).await?.request_name(NAME.try_into()?, RequestNameFlags::DoNotQueue.into()).await?;
    if reply != RequestNameReply::PrimaryOwner {
        bail!("another TuxDeck owns {NAME}");
    }
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let file = dir.join("tuxdeck-kwin-focus.js");
    load(&conn, &file).await?;
    let _ = CONN.set(conn.clone());
    ACTIVE.store(true, Ordering::Relaxed);
    tracing::info!("Smart Profiles: active window via KWin");
    // A restarted KWin has forgotten the script.
    loop {
        tokio::time::sleep(Duration::from_secs(10)).await;
        if !script_loaded(&conn).await.unwrap_or(true) {
            match load(&conn, &file).await {
                Ok(()) => tracing::info!("KWin restarted: focus script loaded again"),
                Err(e) => tracing::debug!("KWin focus script: {e:#}"),
            }
        }
    }
}

/// Report the active window from KWin; returns when that is not possible.
pub fn run(sh: Shared) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => return tracing::warn!("Smart Profiles (KWin): {e:#}"),
    };
    if let Err(e) = rt.block_on(serve(sh)) {
        ACTIVE.store(false, Ordering::Relaxed);
        tracing::info!("Smart Profiles: off (KWin: {e:#})");
    }
}

/// Remove the script from KWin when TuxDeck stops.
pub async fn stop() {
    if let Some(conn) = CONN.get() {
        let _ = tokio::time::timeout(Duration::from_secs(1), unload(conn)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_from_script() {
        let v = parse_windows(r#"[["org.kde.dolphin","Home"],["firefox","B"],["firefox","A"],["","ghost"]]"#);
        assert_eq!(v, [("firefox".into(), "A".into()), ("org.kde.dolphin".into(), "Home".into())] as [(String, String); 2]);
        assert!(parse_windows("[]").is_empty());
        assert!(parse_windows("nonsense").is_empty());
        assert!(parse_windows(r#"[["only one"]]"#).is_empty());
    }

    #[test]
    fn script_names_our_service() {
        let s = script();
        assert!(s.contains(NAME) && s.contains(PATH) && s.contains(IFACE));
        assert!(s.contains("\"ActiveWindow\"") && s.contains("\"Windows\""));
    }
}
