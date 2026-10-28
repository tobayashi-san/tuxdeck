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
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let line = format!("{SCHEME}={HANDLER}");
    if text.lines().any(|l| l.trim() == line) {
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
    std::fs::write(path, out.join("\n") + "\n")?;
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
    let apps = dirs::data_dir().context("no data dir")?.join("applications");
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
    let config = dirs::config_dir().context("no config dir")?;
    let main = config.join("mimeapps.list");
    let main_writable = std::fs::symlink_metadata(&main).map(|m| !m.file_type().is_symlink()).unwrap_or(true);
    if main_writable {
        set_default_in(&main)?;
    }
    // Desktop specific lists override the main one (XDG mime apps spec).
    if let Ok(desktops) = std::env::var("XDG_CURRENT_DESKTOP") {
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
}
