//! Smart profiles: switch a device to the profile linked to the focused application
//! and back to the previous profile when that application loses focus.
//!
//! Profiles store the linked application like the official app in the manifest field
//! `AppIdentifier` (macOS: a bundle path such as `/Applications/Firefox.app`). On Linux
//! it is matched against the window class of the focused window, so both `firefox`
//! and an imported `/Applications/Firefox.app` match a Firefox window.
//!
//! Focus sources: Hyprland's event socket (`activewindow>>class,title`), otherwise
//! polling `xprop` on X11. Disabled with `TUXDECK_NO_WINDOW_WATCH=1` (tests).

use crate::engine::Shared;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Normalized application name: lower case file name without `.app`/`.exe`/`.desktop`.
pub fn normalize(id: &str) -> String {
    let id = id.trim().trim_end_matches('/');
    let name = id.rsplit(['/', '\\']).next().unwrap_or(id).to_lowercase();
    let stem = name.strip_suffix(".app").or_else(|| name.strip_suffix(".exe")).or_else(|| name.strip_suffix(".desktop")).unwrap_or(&name);
    stem.to_string()
}

/// Does the window class `class` belong to the application `identifier`?
/// Several identifiers can be separated by `;` or `,`.
pub fn app_matches(identifier: &str, class: &str) -> bool {
    let class = class.trim().to_lowercase();
    if class.is_empty() {
        return false;
    }
    let last = class.rsplit('.').next().unwrap_or(&class).to_string();
    identifier.split([';', ',']).map(normalize).filter(|n| !n.is_empty()).any(|n| n == class || n == last || n.replace(' ', "-") == class)
}

fn hypr_socket() -> Option<PathBuf> {
    let sig = crate::input::hypr_instance()?;
    let mut dirs = vec![];
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
        dirs.push(PathBuf::from(rt).join("hypr"));
    }
    dirs.push(PathBuf::from("/tmp/hypr"));
    dirs.into_iter().map(|d| d.join(&sig).join(".socket2.sock")).find(|p| p.exists())
}

fn hypr_active_class() -> Option<String> {
    let out = crate::input::hyprctl_command().args(["activewindow", "-j"]).output().ok()?;
    active_class_from_json(&out.stdout)
}

/// Window class in `hyprctl activewindow -j` output.
fn active_class_from_json(json: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    v.get("class").and_then(|c| c.as_str()).map(str::to_string)
}

/// (class, title) of every window in `hyprctl clients -j` output, one per class.
fn clients_from_json(json: &[u8]) -> Option<Vec<(String, String)>> {
    let serde_json::Value::Array(a) = serde_json::from_slice::<serde_json::Value>(json).ok()? else { return None };
    let mut v: Vec<(String, String)> = a
        .iter()
        .filter_map(|c| Some((c.get("class")?.as_str()?.to_string(), c.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string())))
        .filter(|(c, _)| !c.is_empty())
        .collect();
    v.sort();
    v.dedup_by(|a, b| a.0 == b.0);
    Some(v)
}

/// Window class of a Hyprland socket2 event line (`activewindow>>class,title`).
fn focus_event_class(line: &str) -> Option<&str> {
    line.strip_prefix("activewindow>>").map(|rest| rest.split(',').next().unwrap_or(""))
}

/// Window classes of all open windows (for the profile editor).
pub fn open_windows() -> Vec<(String, String)> {
    if crate::input::hypr_instance().is_some() {
        if let Ok(out) = crate::input::hyprctl_command().args(["clients", "-j"]).output() {
            if let Some(v) = clients_from_json(&out.stdout) {
                return v;
            }
        }
    }
    x11_active_class().map(|c| vec![(c, String::new())]).unwrap_or_default()
}

fn x11_active_class() -> Option<String> {
    let out = Command::new("xprop").args(["-root", "_NET_ACTIVE_WINDOW"]).output().ok()?;
    let id = xprop_window_id(&String::from_utf8_lossy(&out.stdout))?;
    let out = Command::new("xprop").args(["-id", &id, "WM_CLASS"]).output().ok()?;
    xprop_class(&String::from_utf8_lossy(&out.stdout))
}

/// `_NET_ACTIVE_WINDOW(WINDOW): window id # 0x3a00007` -> `0x3a00007` (none when unfocused).
fn xprop_window_id(s: &str) -> Option<String> {
    let id = s.split_whitespace().last()?.trim().to_string();
    (id.starts_with("0x") && id != "0x0").then_some(id)
}

/// `WM_CLASS(STRING) = "instance", "Class"` -> `Class`.
fn xprop_class(s: &str) -> Option<String> {
    s.split('"').nth(3).or_else(|| s.split('"').nth(1)).map(str::to_string)
}

fn notify(sh: &Shared, class: String) {
    sh.lock().active_app_changed(&class);
}

/// Start the focus watcher thread (no-op when no supported desktop is found).
pub fn spawn_watcher(sh: Shared) {
    if std::env::var_os("TUXDECK_NO_WINDOW_WATCH").is_some() {
        return;
    }
    let _ = std::thread::Builder::new().name("smart-profiles".into()).spawn(move || {
        if crate::input::hypr_instance().is_some() {
            tracing::info!("Smart Profiles: active window via Hyprland");
            loop {
                if let Some(c) = hypr_active_class() {
                    notify(&sh, c);
                }
                if let Some(Ok(stream)) = hypr_socket().map(std::os::unix::net::UnixStream::connect) {
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { break };
                        if let Some(class) = focus_event_class(&line) {
                            notify(&sh, class.to_string());
                        }
                    }
                }
                std::thread::sleep(Duration::from_secs(3));
            }
        } else if std::env::var_os("DISPLAY").is_some() && Command::new("xprop").arg("-version").output().is_ok() {
            tracing::info!("Smart Profiles: active window via xprop (X11)");
            let mut last = String::new();
            loop {
                let c = x11_active_class().unwrap_or_default();
                if c != last {
                    last = c.clone();
                    notify(&sh, c);
                }
                std::thread::sleep(Duration::from_millis(800));
            }
        } else {
            tracing::info!("Smart Profiles: no supported desktop (Hyprland or X11) found");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching() {
        assert!(app_matches("firefox", "firefox"));
        assert!(app_matches("/Applications/Firefox.app", "firefox"));
        assert!(app_matches("nautilus", "org.gnome.Nautilus"));
        assert!(app_matches("code; codium", "codium"));
        assert!(!app_matches("firefox", ""));
        assert!(!app_matches("fire", "firefox"));
        assert!(app_matches("Google Chrome", "google-chrome"));
    }

    #[test]
    fn identifiers() {
        assert_eq!(normalize("/Applications/Firefox.app"), "firefox");
        assert_eq!(normalize("/Applications/Visual Studio Code.app/"), "visual studio code");
        assert_eq!(normalize(r"C:\Program Files\Mozilla Firefox\firefox.exe"), "firefox");
        assert_eq!(normalize(r"C:\Program Files\Spotify\Spotify.EXE"), "spotify");
        assert_eq!(normalize("org.gnome.Nautilus.desktop"), "org.gnome.nautilus");
        assert_eq!(normalize("  kitty  "), "kitty");
        assert_eq!(normalize("my.app.exe"), "my.app");
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("/"), "");
    }

    #[test]
    fn matching_more() {
        assert!(app_matches("org.gnome.Nautilus.desktop", "org.gnome.Nautilus"));
        assert!(app_matches("C:\\Apps\\Discord.exe", "discord"));
        assert!(app_matches("steam, firefox", "Firefox"));
        assert!(app_matches(";;kitty;", " kitty "));
        assert!(app_matches("Visual Studio Code", "visual-studio-code"));
        assert!(!app_matches("", "firefox"));
        assert!(!app_matches(" ; , ", "firefox"));
        assert!(!app_matches("firefox", "   "));
        assert!(!app_matches("gnome", "org.gnome.Nautilus"));
        assert!(!app_matches("firefox-esr", "firefox"));
    }

    #[test]
    fn hyprland_events() {
        assert_eq!(focus_event_class("activewindow>>firefox,Mozilla Firefox, a, b"), Some("firefox"));
        assert_eq!(focus_event_class("activewindow>>kitty,"), Some("kitty"));
        // Focus on an empty workspace.
        assert_eq!(focus_event_class("activewindow>>,"), Some(""));
        assert_eq!(focus_event_class("activewindow>>"), Some(""));
        assert_eq!(focus_event_class("activewindowv2>>5612e0a9c7f0"), None);
        assert_eq!(focus_event_class("workspace>>2"), None);
        assert_eq!(focus_event_class(""), None);
    }

    #[test]
    fn hyprland_json() {
        assert_eq!(active_class_from_json(br#"{"address": "0x1", "class": "firefox", "title": "x"}"#).as_deref(), Some("firefox"));
        assert_eq!(active_class_from_json(b"{}"), None);
        assert_eq!(active_class_from_json(b"Invalid"), None);
        let clients = br#"[
            {"class": "kitty", "title": "zsh"}, {"class": "firefox", "title": "B"}, {"class": "firefox", "title": "A"},
            {"class": "", "title": "ghost"}, {"title": "no class"}, {"class": "steam"}
        ]"#;
        assert_eq!(
            clients_from_json(clients).unwrap(),
            [("firefox".into(), "A".into()), ("kitty".into(), "zsh".into()), ("steam".into(), String::new())] as [(String, String); 3]
        );
        assert_eq!(clients_from_json(b"[]").unwrap(), vec![]);
        assert_eq!(clients_from_json(b"{}"), None);
        assert_eq!(clients_from_json(b""), None);
    }

    #[test]
    fn xprop_output() {
        assert_eq!(xprop_window_id("_NET_ACTIVE_WINDOW(WINDOW): window id # 0x3a00007\n").as_deref(), Some("0x3a00007"));
        assert_eq!(xprop_window_id("_NET_ACTIVE_WINDOW(WINDOW): window id # 0x0"), None);
        assert_eq!(xprop_window_id("_NET_ACTIVE_WINDOW:  not found."), None);
        assert_eq!(xprop_window_id(""), None);
        assert_eq!(xprop_class("WM_CLASS(STRING) = \"Navigator\", \"firefox\"\n").as_deref(), Some("firefox"));
        assert_eq!(xprop_class("WM_CLASS(STRING) = \"xterm\"").as_deref(), Some("xterm"));
        assert_eq!(xprop_class("WM_CLASS:  not found."), None);
    }
}
