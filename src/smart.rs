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
    let name = id.rsplit(['/', '\\']).next().unwrap_or(id);
    let name = name.strip_suffix(".app").or_else(|| name.strip_suffix(".exe")).or_else(|| name.strip_suffix(".desktop")).unwrap_or(name);
    name.to_lowercase()
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
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v.get("class").and_then(|c| c.as_str()).map(str::to_string)
}

/// Window classes of all open windows (for the profile editor).
pub fn open_windows() -> Vec<(String, String)> {
    if crate::input::hypr_instance().is_some() {
        if let Ok(out) = crate::input::hyprctl_command().args(["clients", "-j"]).output() {
            if let Ok(serde_json::Value::Array(a)) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                let mut v: Vec<(String, String)> = a
                    .iter()
                    .filter_map(|c| Some((c.get("class")?.as_str()?.to_string(), c.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string())))
                    .filter(|(c, _)| !c.is_empty())
                    .collect();
                v.sort();
                v.dedup_by(|a, b| a.0 == b.0);
                return v;
            }
        }
    }
    x11_active_class().map(|c| vec![(c, String::new())]).unwrap_or_default()
}

fn x11_active_class() -> Option<String> {
    let out = Command::new("xprop").args(["-root", "_NET_ACTIVE_WINDOW"]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let id = s.split_whitespace().last()?.trim().to_string();
    if !id.starts_with("0x") || id == "0x0" {
        return None;
    }
    let out = Command::new("xprop").args(["-id", &id, "WM_CLASS"]).output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    // WM_CLASS(STRING) = "instance", "Class"
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
                        if let Some(rest) = line.strip_prefix("activewindow>>") {
                            notify(&sh, rest.split(',').next().unwrap_or("").to_string());
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
}
