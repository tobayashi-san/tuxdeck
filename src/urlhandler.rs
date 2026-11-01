//! `streamdeck://` URL handler registration (deep links of plugins).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Find a helper tool even when PATH is minimal (systemd user services).
fn tool(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(["/run/current-system/sw/bin", "/usr/bin", "/bin", "/usr/local/bin"].iter().map(PathBuf::from));
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.is_file())
}

const HANDLER: &str = "tuxdeck-url-handler.desktop";
const SCHEME: &str = "x-scheme-handler/streamdeck";

/// Add `<mime>=<desktop>` to the `[Default Applications]` group of a mimeapps.list.
fn set_default_in(path: &Path) -> Result<()> {
    // A file that can't be read as text is the user's: leave it alone.
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("not changing {}", path.display())),
    };
    let line = format!("{SCHEME}={HANDLER}");
    // Already the default (the same line in e.g. `[Added Associations]` doesn't count).
    let mut group = "";
    if text.lines().map(str::trim).any(|l| {
        if l.starts_with('[') {
            group = l;
        }
        group == "[Default Applications]" && l == line
    }) {
        return Ok(());
    }
    let mut out = Vec::new();
    let mut in_defaults = false;
    let mut done = false;
    for l in text.lines() {
        if l.trim().starts_with('[') {
            if in_defaults && !done {
                out.push(line.clone());
                done = true;
            }
            in_defaults = l.trim() == "[Default Applications]";
        }
        if in_defaults && l.trim_start().starts_with(&format!("{SCHEME}=")) {
            continue;
        }
        out.push(l.to_string());
    }
    if !done {
        if !in_defaults {
            out.push("[Default Applications]".into());
        }
        out.push(line);
    }
    // A link (home-manager) or a read-only file is managed elsewhere: replacing it would
    // break that.
    if let Ok(m) = std::fs::symlink_metadata(path) {
        if m.file_type().is_symlink() || m.permissions().readonly() {
            anyhow::bail!("{} is read-only or a link", path.display());
        }
    }
    // Written whole or not at all (the file holds all default applications).
    crate::profile::write_atomic(path, (out.join("\n") + "\n").as_bytes())?;
    Ok(())
}

/// Quote a program path for the `Exec` key (Desktop Entry spec: double quotes with `"`,
/// `` ` ``, `$` and `\` escaped, `%` doubled; then backslashes doubled for the string value).
fn desktop_exec_arg(path: &str) -> String {
    if path.chars().all(|c| c.is_ascii_alphanumeric() || "/._+-".contains(c)) {
        return path.to_string();
    }
    let mut q = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                q.push('\\');
                q.push(c);
            }
            '%' => q.push_str("%%"),
            c => q.push(c),
        }
    }
    q.push('"');
    q.replace('\\', "\\\\")
}

/// Register `streamdeck://` links with the desktop: plugins use them as OAuth redirect
/// (`streamdeck://plugins/message/<uuid>/…`). Idempotent. Works with a home-manager
/// managed (read-only) mimeapps.list by writing the desktop-specific
/// `<desktop>-mimeapps.list`, which takes precedence.
pub fn register_url_handler() -> Result<()> {
    if std::env::var_os("TUXDECK_NO_URL_HANDLER").is_some() {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let data = dirs::data_dir().context("no data dir")?;
    let config = dirs::config_dir().context("no config dir")?;
    register_in(&exe, &data, &config, std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref())
}

/// `register_url_handler` for the XDG data/config directories `data`, `config` and the
/// colon separated `XDG_CURRENT_DESKTOP` list `desktops`.
fn register_in(exe: &Path, data: &Path, config: &Path, desktops: Option<&str>) -> Result<()> {
    let apps = data.join("applications");
    std::fs::create_dir_all(&apps)?;
    let desktop = apps.join(HANDLER);
    let content = format!(
        "[Desktop Entry]\nType=Application\nName=TuxDeck (Links)\nNoDisplay=true\nExec={} open-url %u\nMimeType={SCHEME};\nTerminal=false\n",
        desktop_exec_arg(&exe.to_string_lossy())
    );
    if std::fs::read_to_string(&desktop).ok().as_deref() != Some(content.as_str()) {
        std::fs::write(&desktop, content)?;
    }
    let cache_ok = std::fs::read_to_string(apps.join("mimeinfo.cache")).map(|c| c.contains(&format!("{SCHEME}={HANDLER}"))).unwrap_or(false);
    if !cache_ok {
        match tool("update-desktop-database") {
            Some(t) => {
                let _ = std::process::Command::new(t).arg(&apps).status();
            }
            None => tracing::warn!("update-desktop-database not found"),
        }
    }
    let main = config.join("mimeapps.list");
    let main_writable = std::fs::symlink_metadata(&main).map(|m| !m.file_type().is_symlink()).unwrap_or(true);
    if main_writable {
        set_default_in(&main)?;
    }
    // Desktop specific lists override the main one (XDG mime apps spec).
    if let Some(desktops) = desktops {
        for d in desktops.split(':').filter(|d| !d.is_empty()) {
            let p = config.join(format!("{}-mimeapps.list", d.to_lowercase()));
            let writable = std::fs::symlink_metadata(&p).map(|m| !m.file_type().is_symlink()).unwrap_or(true);
            if writable {
                set_default_in(&p)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_exec_quoting() {
        assert_eq!(desktop_exec_arg("/home/a/.local/bin/tuxdeck"), "/home/a/.local/bin/tuxdeck");
        assert_eq!(desktop_exec_arg("/home/a b/bin/sd"), "\"/home/a b/bin/sd\"");
        assert_eq!(desktop_exec_arg("/x/100%/\"q\""), "\"/x/100%%/\\\\\"q\\\\\"\"");
    }

    #[test]
    fn mimeapps_merge() {
        let dir = std::env::temp_dir().join(format!("sd-mime-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x-mimeapps.list");
        std::fs::write(&p, "[Default Applications]\ntext/html=brave.desktop\n[Added Associations]\nfoo/bar=a.desktop\n").unwrap();
        set_default_in(&p).unwrap();
        set_default_in(&p).unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert_eq!(t.matches("x-scheme-handler/streamdeck=").count(), 1);
        assert!(t.find("x-scheme-handler/streamdeck").unwrap() < t.find("[Added Associations]").unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sd-url-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Contents of a mimeapps.list after `set_default_in`.
    fn merged(dir: &Path, before: Option<&str>) -> String {
        let p = dir.join("mimeapps.list");
        let _ = std::fs::remove_file(&p);
        if let Some(b) = before {
            std::fs::write(&p, b).unwrap();
        }
        set_default_in(&p).unwrap();
        std::fs::read_to_string(&p).unwrap()
    }

    #[test]
    fn mimeapps_cases() {
        let d = tmp("cases");
        let ours = "x-scheme-handler/streamdeck=tuxdeck-url-handler.desktop";
        assert_eq!(merged(&d, None), format!("[Default Applications]\n{ours}\n"));
        assert_eq!(merged(&d, Some("")), format!("[Default Applications]\n{ours}\n"));
        // Another handler is replaced; other entries and groups stay.
        assert_eq!(
            merged(&d, Some("[Added Associations]\nx-scheme-handler/streamdeck=other.desktop;\n[Default Applications]\nx-scheme-handler/streamdeck=other.desktop\ntext/html=firefox.desktop\n")),
            format!("[Added Associations]\nx-scheme-handler/streamdeck=other.desktop;\n[Default Applications]\ntext/html=firefox.desktop\n{ours}\n")
        );
        // No default group yet.
        assert_eq!(
            merged(&d, Some("[Added Associations]\nfoo/bar=a.desktop")),
            format!("[Added Associations]\nfoo/bar=a.desktop\n[Default Applications]\n{ours}\n")
        );
        // The line under another group doesn't make it the default.
        assert_eq!(merged(&d, Some(&format!("[Added Associations]\n{ours}\n"))), format!("[Added Associations]\n{ours}\n[Default Applications]\n{ours}\n"));
        // Already set: left exactly as it is.
        let set = format!("# mine\n[Default Applications]\n  {ours}  \ntext/html=firefox.desktop\n\n[Removed Associations]\n");
        assert_eq!(merged(&d, Some(&set)), set);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn mimeapps_read_only() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let d = tmp("ro");
        let p = d.join("mimeapps.list");
        std::fs::write(&p, "[Default Applications]\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert!(set_default_in(&p).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "[Default Applications]\n");
        // Not text: left as it is.
        let bin = d.join("latin1-mimeapps.list");
        std::fs::write(&bin, b"[Default Applications]\ntext/plain=caf\xe9.desktop\n").unwrap();
        assert!(set_default_in(&bin).is_err());
        assert_eq!(std::fs::read(&bin).unwrap(), b"[Default Applications]\ntext/plain=caf\xe9.desktop\n");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn registration() {
        let d = tmp("register");
        let (data, config) = (d.join("data"), d.join("config"));
        let apps = data.join("applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        // Up to date cache: update-desktop-database isn't needed.
        std::fs::write(apps.join("mimeinfo.cache"), "[MIME Cache]\nx-scheme-handler/streamdeck=tuxdeck-url-handler.desktop;\n").unwrap();
        // home-manager: the main list is a read-only link into the store.
        let store = d.join("store-mimeapps.list");
        std::fs::write(&store, "[Default Applications]\n").unwrap();
        std::os::unix::fs::symlink(&store, config.join("mimeapps.list")).unwrap();

        register_in(Path::new("/opt/Tux Deck/tuxdeck"), &data, &config, Some("Hyprland::GNOME")).unwrap();
        let desktop = std::fs::read_to_string(apps.join(HANDLER)).unwrap();
        assert!(desktop.starts_with("[Desktop Entry]\nType=Application\n"));
        assert!(desktop.contains("\nExec=\"/opt/Tux Deck/tuxdeck\" open-url %u\n"));
        assert!(desktop.contains("\nMimeType=x-scheme-handler/streamdeck;\n"));
        assert!(desktop.contains("\nNoDisplay=true\n"));
        assert_eq!(std::fs::read_to_string(&store).unwrap(), "[Default Applications]\n");
        for list in ["hyprland-mimeapps.list", "gnome-mimeapps.list"] {
            assert!(std::fs::read_to_string(config.join(list)).unwrap().contains("x-scheme-handler/streamdeck=tuxdeck-url-handler.desktop"), "{list}");
        }
        assert!(!config.join("-mimeapps.list").exists());

        // Idempotent; a regular main list is written too.
        std::fs::remove_file(config.join("mimeapps.list")).unwrap();
        register_in(Path::new("/opt/Tux Deck/tuxdeck"), &data, &config, None).unwrap();
        assert_eq!(std::fs::read_to_string(apps.join(HANDLER)).unwrap(), desktop);
        assert!(std::fs::read_to_string(config.join("mimeapps.list")).unwrap().contains("streamdeck=tuxdeck-url-handler.desktop"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
