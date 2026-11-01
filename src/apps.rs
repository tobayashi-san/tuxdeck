//! The Linux counterpart of applications named in imported profiles: Windows paths
//! (`C:\Program Files\obs-studio\bin\64bit\obs64.exe`), macOS bundles
//! (`/Applications/Slack.app`) or just the key's title ("Calculator"). Matched against
//! the installed desktop entries (including Flatpak), with a few well-known aliases and
//! the web version as a fallback.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct DesktopApp {
    /// Desktop file id without `.desktop` (`com.obsproject.Studio`).
    pub id: String,
    pub name: String,
    pub exec: String,
    keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    App { id: String, name: String, exec: String },
    Url(String),
    Dir(PathBuf),
}

impl Target {
    /// Short description for the UI ("Steam", "outlook.office.com", "~/Documents").
    pub fn label(&self) -> String {
        match self {
            Target::App { name, .. } => name.clone(),
            Target::Url(u) => u.trim_start_matches("https://").trim_end_matches('/').to_string(),
            Target::Dir(d) => match dirs::home_dir().and_then(|h| d.strip_prefix(h).ok().map(Path::to_path_buf)) {
                Some(rel) => format!("~/{}", rel.display()),
                None => d.display().to_string(),
            },
        }
    }
}

/// A path that cannot exist on Linux: a Windows path or a macOS application bundle.
pub fn is_foreign(path: &str) -> bool {
    let p = path.trim().trim_matches('"');
    let b = p.as_bytes();
    (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || p.starts_with("\\\\")
        || ((p.starts_with("/Applications/") || p.starts_with("/System/Applications/") || p.starts_with("/System/Library/")) && !Path::new(p).exists())
}

/// Lowercase letters and digits only: "Microsoft\nOutlook" → "microsoftoutlook".
fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Folder names in Windows/macOS paths that say nothing about the application.
const GENERIC: &[&str] = &[
    "c",
    "programfiles",
    "programfilesx86",
    "programdata",
    "users",
    "appdata",
    "local",
    "roaming",
    "microsoft",
    "windows",
    "startmenu",
    "programs",
    "bin",
    "64bit",
    "32bit",
    "x64",
    "x86",
    "root",
    "application",
    "applications",
    "system",
    "library",
    "contents",
    "macos",
    "office16",
    "common",
];

/// Names a key may refer to, most specific first: the program, its folder, the title.
pub fn candidates(path: &str, title: &str) -> Vec<String> {
    let mut out = Vec::new();
    let p = path.trim().trim_matches('"');
    let parts: Vec<&str> = p.split(['\\', '/']).filter(|s| !s.is_empty()).collect();
    for (i, part) in parts.iter().enumerate().rev() {
        let lower = part.to_ascii_lowercase();
        let stem = [".app", ".exe", ".lnk"].iter().find(|x| lower.ends_with(*x)).map_or(*part, |x| &part[..part.len() - x.len()]);
        let n = norm(stem);
        // Version folders like "app-1.0.9041".
        let versioned = stem.chars().filter(|c| c.is_ascii_digit()).count() * 2 > n.len();
        if n.is_empty() || GENERIC.contains(&n.as_str()) || versioned || (i > 0 && norm(parts[i - 1]) == "users") {
            continue;
        }
        out.push(stem.to_string());
        if out.len() >= 2 {
            break;
        }
    }
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if !title.is_empty() {
        out.push(title);
    }
    let mut seen = HashSet::new();
    out.retain(|c| seen.insert(norm(c)));
    out
}

/// Well-known applications: names they go by → desktop ids/names to try → web version.
struct Alias {
    names: &'static [&'static str],
    apps: &'static [&'static str],
    url: Option<&'static str>,
    /// Otherwise the default application for this MIME type (a "Chrome" key opens your
    /// browser, "Notepad" your text editor).
    mime: Option<&'static str>,
}

const fn a(names: &'static [&'static str], apps: &'static [&'static str], url: Option<&'static str>) -> Alias {
    Alias { names, apps, url, mime: None }
}

const fn m(names: &'static [&'static str], apps: &'static [&'static str], mime: &'static str) -> Alias {
    Alias { names, apps, url: None, mime: Some(mime) }
}

const ALIASES: &[Alias] = &[
    a(&["obs64", "obs32", "obs", "obsstudio"], &["com.obsproject.Studio", "obs"], None),
    m(&["chrome", "googlechrome"], &["google-chrome", "com.google.Chrome", "chromium", "org.chromium.Chromium", "chromium-browser"], "x-scheme-handler/https"),
    m(
        &["msedge", "microsoftedge", "edge", "safari", "iexplore", "internetexplorer", "browser", "webbrowser"],
        &["microsoft-edge", "com.microsoft.Edge"],
        "x-scheme-handler/https",
    ),
    m(&["firefox", "mozillafirefox"], &["firefox", "org.mozilla.firefox"], "x-scheme-handler/https"),
    a(&["outlook", "microsoftoutlook"], &[], Some("https://outlook.office.com/mail/")),
    a(
        &["winword", "word", "microsoftword"],
        &["libreoffice-writer", "org.libreoffice.LibreOffice.writer", "onlyoffice-desktopeditors"],
        Some("https://www.office.com/launch/word"),
    ),
    a(
        &["excel", "microsoftexcel"],
        &["libreoffice-calc", "org.libreoffice.LibreOffice.calc", "onlyoffice-desktopeditors"],
        Some("https://www.office.com/launch/excel"),
    ),
    a(
        &["powerpnt", "powerpoint", "microsoftpowerpoint"],
        &["libreoffice-impress", "org.libreoffice.LibreOffice.impress"],
        Some("https://www.office.com/launch/powerpoint"),
    ),
    a(&["teams", "msteams", "microsoftteams"], &["teams-for-linux", "com.github.IsmaelMartinez.teams_for_linux"], Some("https://teams.microsoft.com/")),
    a(&["slack"], &["slack", "com.slack.Slack"], Some("https://app.slack.com/")),
    a(&["discord"], &["discord", "com.discordapp.Discord", "vesktop", "dev.vencord.Vesktop"], Some("https://discord.com/app")),
    a(&["spotify"], &["spotify", "com.spotify.Client"], Some("https://open.spotify.com/")),
    a(&["zoom", "zoomus"], &["Zoom", "us.zoom.Zoom", "zoom"], Some("https://app.zoom.us/wc")),
    a(&["figma"], &["figma-linux", "io.github.Figma_Linux.figma_linux"], Some("https://www.figma.com/files")),
    a(&["photoshop", "adobephotoshop"], &["gimp", "org.gimp.GIMP", "krita", "org.kde.krita"], Some("https://www.photopea.com/")),
    a(&["chatgpt"], &[], Some("https://chatgpt.com/")),
    a(&["steam"], &["steam", "com.valvesoftware.Steam"], None),
    a(&["epicgameslauncher", "epicgames", "epic", "heroic"], &["heroic", "com.heroicgameslauncher.hgl"], None),
    a(&["battlenet", "blizzard", "battlenetlauncher"], &["lutris", "net.lutris.Lutris", "bottles", "com.usebottles.bottles"], None),
    a(&["xbox"], &[], Some("https://www.xbox.com/play")),
    a(&["code", "vscode", "visualstudiocode"], &["code", "com.visualstudio.code", "codium", "com.vscodium.codium"], None),
    m(
        &["notepad", "textedit", "notepadplusplus", "editor", "texteditor"],
        &[
            "org.gnome.TextEditor",
            "gnome-text-editor",
            "org.kde.kate",
            "org.kde.kwrite",
            "mousepad",
            "org.xfce.mousepad",
            "gedit",
            "org.gnome.gedit",
            "xed",
            "pluma",
        ],
        "text/plain",
    ),
    a(
        &["calc", "calculator", "rechner"],
        &["org.gnome.Calculator", "gnome-calculator", "org.kde.kcalc", "kcalc", "qalculate-gtk", "io.github.Qalculate", "galculator", "mate-calc"],
        None,
    ),
    m(
        &["explorer", "fileexplorer", "finder", "files", "filemanager"],
        &["org.gnome.Nautilus", "org.kde.dolphin", "thunar", "nemo", "pcmanfm"],
        "inode/directory",
    ),
    a(
        &["cmd", "powershell", "terminal", "windowsterminal", "wt"],
        &[
            "org.gnome.Ptyxis",
            "org.gnome.Console",
            "org.gnome.Terminal",
            "org.kde.konsole",
            "kitty",
            "Alacritty",
            "foot",
            "com.mitchellh.ghostty",
            "org.wezfurlong.wezterm",
            "xfce4-terminal",
        ],
        None,
    ),
    a(&["vlc"], &["vlc", "org.videolan.VLC"], None),
    a(&["telegram"], &["org.telegram.desktop", "telegramdesktop"], Some("https://web.telegram.org/")),
    a(&["whatsapp"], &[], Some("https://web.whatsapp.com/")),
    a(&["thunderbird"], &["thunderbird", "org.mozilla.Thunderbird", "net.thunderbird.Thunderbird"], None),
];

/// Folders by name: "Docs Folder" → the documents folder.
fn special_dir(name: &str) -> Option<PathBuf> {
    let n = norm(name);
    let n = n.strip_suffix("folder").unwrap_or(&n);
    // Without ~/.config/user-dirs.dirs the XDG folders are unknown: try the usual names.
    let or_home = |xdg: Option<PathBuf>, name: &str| xdg.or_else(|| dirs::home_dir().map(|h| h.join(name))).filter(|d| d.is_dir());
    match n {
        "docs" | "documents" | "mydocuments" | "dokumente" => or_home(dirs::document_dir(), "Documents"),
        "downloads" => or_home(dirs::download_dir(), "Downloads"),
        "desktop" => or_home(dirs::desktop_dir(), "Desktop"),
        "pictures" | "photos" => or_home(dirs::picture_dir(), "Pictures"),
        "music" => or_home(dirs::audio_dir(), "Music"),
        "videos" | "movies" => or_home(dirs::video_dir(), "Videos"),
        "home" | "userfolder" => dirs::home_dir(),
        _ => None,
    }
}

/// Resolve a key to what it should open on Linux. Names that only resemble the key's
/// ("Spotify" → "Spotify Tray") count too: the conversion dialog shows what was found.
pub fn resolve(path: &str, title: &str) -> Option<Target> {
    resolve_in(&installed(), path, title, true)
}

/// [`resolve`] with known names only (sample profiles: a program that isn't installed
/// gets no command rather than one with a similar name).
pub fn resolve_exact(path: &str, title: &str) -> Option<Target> {
    resolve_in(&installed(), path, title, false)
}

/// Names that may mean a folder: the path itself when it doesn't name a program, and
/// then the title. Not the folders a program is in (`Desktop\App.lnk` is the program).
fn folder_names(path: &str, title: &str) -> Vec<String> {
    let p = path.trim().trim_matches('"').trim_end_matches(['\\', '/']);
    let last = p.rsplit(['\\', '/']).next().unwrap_or("");
    let program = [".app", ".exe", ".lnk", ".bat", ".cmd", ".url"].iter().any(|x| last.to_ascii_lowercase().ends_with(x));
    match (p.is_empty(), program) {
        (true, _) => vec![title.to_string()],
        (false, false) => vec![last.to_string(), title.to_string()],
        (false, true) => vec![],
    }
}

/// The words of a name in lower case: "SteamLink" and "Steam Link" → ["steam", "link"].
fn words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if !c.is_alphanumeric() {
            prev_lower = false;
            out.push(String::new());
            continue;
        }
        if c.is_uppercase() && prev_lower {
            out.push(String::new());
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        match out.last_mut() {
            Some(w) => w.extend(c.to_lowercase()),
            None => out.push(c.to_lowercase().collect()),
        }
    }
    out.retain(|w| !w.is_empty());
    out
}

fn resolve_in(apps: &[DesktopApp], path: &str, title: &str, similar: bool) -> Option<Target> {
    let names = candidates(path, title);
    let find = |want: &str| {
        let w = norm(want);
        apps.iter().find(|a| norm(&a.id) == w).or_else(|| apps.iter().find(|a| a.keys.contains(&w)))
    };
    for name in folder_names(path, title) {
        if let Some(d) = special_dir(&name) {
            return Some(Target::Dir(d));
        }
    }
    let mut url = None;
    for name in &names {
        let n = norm(name);
        if let Some(alias) = ALIASES.iter().find(|al| al.names.contains(&n.as_str())) {
            if let Some(app) = alias.apps.iter().find_map(|w| find(w)) {
                return Some(app.target());
            }
            if let Some(app) = alias.mime.and_then(default_app).and_then(|id| find(&id)) {
                return Some(app.target());
            }
            url = url.or(alias.url);
        }
    }
    // Any installed application with that name, then one whose name has these words.
    for name in &names {
        if let Some(app) = find(name) {
            return Some(app.target());
        }
    }
    if similar {
        for name in &names {
            let want = words(name);
            if want.concat().len() < 4 {
                continue;
            }
            let has = |s: &str| words(s).windows(want.len()).any(|w| w == want.as_slice());
            if let Some(app) = apps.iter().find(|a| has(&a.name) || has(a.id.rsplit('.').next().unwrap_or(&a.id))) {
                return Some(app.target());
            }
        }
    }
    url.map(|u| Target::Url(u.to_string()))
}

/// The Linux application ids of a well-known Windows/macOS program (`obs64` →
/// `com.obsproject.Studio`, `obs`), lower case.
pub fn linux_names(program: &str) -> Vec<String> {
    let n = norm(program);
    ALIASES.iter().filter(|al| al.names.contains(&n.as_str())).flat_map(|al| al.apps.iter().map(|a| a.to_lowercase())).collect()
}

/// Desktop id of the default application for a MIME type (`xdg-mime query default`).
fn default_app(mime: &str) -> Option<String> {
    if cfg!(test) {
        return None;
    }
    let out = std::process::Command::new("xdg-mime").args(["query", "default", mime]).output().ok()?;
    let id = String::from_utf8_lossy(&out.stdout).trim().trim_end_matches(".desktop").to_string();
    (!id.is_empty()).then_some(id)
}

impl DesktopApp {
    fn target(&self) -> Target {
        Target::App { id: self.id.clone(), name: self.name.clone(), exec: self.exec.clone() }
    }
}

/// The command line of a desktop entry's `Exec` without field codes (`%U`, `%f`, …).
pub fn exec_command(exec: &str) -> String {
    let mut out = String::new();
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            // Field codes are dropped, "%%" is a literal "%".
            if let Some('%') = chars.next() {
                out.push('%');
            }
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The command a key stores for a desktop entry: its `Exec` without field codes, with a
/// program given by absolute path (as in the Nix store) shortened to its name when that
/// name on `PATH` is the same file, so the key survives the next system update.
pub fn launch_command(exec: &str) -> String {
    short_program(&exec_command(exec), &std::env::var("PATH").unwrap_or_default())
}

fn short_program(cmd: &str, path: &str) -> String {
    let (program, args) = cmd.split_once(' ').unwrap_or((cmd, ""));
    let p = Path::new(program);
    let (Some(name), Ok(real)) = (p.file_name().filter(|_| p.is_absolute()), p.canonicalize()) else { return cmd.to_string() };
    // The first one on PATH is what a shell runs.
    match std::env::split_paths(path).map(|d| d.join(name)).find(|c| c.exists()) {
        Some(c) if c.canonicalize().is_ok_and(|c| c == real) => format!("{} {args}", name.to_string_lossy()).trim_end().to_string(),
        _ => cmd.to_string(),
    }
}

/// Installed desktop applications (rescanned at most every 30 seconds).
pub fn installed() -> Arc<Vec<DesktopApp>> {
    type Cached = Option<(Instant, Arc<Vec<DesktopApp>>)>;
    static CACHE: OnceLock<Mutex<Cached>> = OnceLock::new();
    let mut c = CACHE.get_or_init(|| Mutex::new(None)).lock().unwrap();
    if let Some((t, apps)) = c.as_ref() {
        if t.elapsed() < Duration::from_secs(30) {
            return apps.clone();
        }
    }
    let apps = Arc::new(scan(&data_dirs()));
    *c = Some((Instant::now(), apps.clone()));
    apps
}

/// The command line of a desktop entry, given as a path or as a file name / desktop id
/// (`firefox.desktop`), without field codes.
pub fn desktop_command(entry: &str) -> Option<String> {
    let p = Path::new(entry);
    if p.is_absolute() {
        let text = std::fs::read_to_string(p).ok()?;
        let id = p.file_stem()?.to_string_lossy();
        return parse_entry(&id, &text).map(|a| launch_command(&a.exec));
    }
    let id = entry.strip_suffix(".desktop").unwrap_or(entry);
    installed().iter().find(|a| a.id == id).map(|a| launch_command(&a.exec))
}

/// The file of a desktop entry given as a path or as a file name / desktop id.
pub fn desktop_file(entry: &str) -> Option<PathBuf> {
    let p = Path::new(entry);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    let name = if entry.ends_with(".desktop") { entry.to_string() } else { format!("{entry}.desktop") };
    data_dirs().into_iter().map(|d| d.join("applications").join(&name)).find(|f| f.is_file())
}

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = dirs::data_dir() {
        dirs.push(d.clone());
        dirs.push(d.join("flatpak/exports/share"));
    }
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let sys = if sys.is_empty() { "/usr/local/share:/usr/share".to_string() } else { sys };
    dirs.extend(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    dirs
}

/// Read the desktop entries; an id found earlier (user data dir) hides later ones.
fn scan(data_dirs: &[PathBuf]) -> Vec<DesktopApp> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for dir in data_dirs {
        let root = dir.join("applications");
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                let Some(rel) = p.strip_prefix(&root).ok().and_then(|r| r.to_str()) else { continue };
                let Some(id) = rel.strip_suffix(".desktop").map(|r| r.replace('/', "-")) else { continue };
                if !seen.insert(id.clone()) {
                    continue;
                }
                if let Some(app) = std::fs::read_to_string(&p).ok().and_then(|s| parse_entry(&id, &s)) {
                    out.push(app);
                }
            }
        }
    }
    out
}

fn parse_entry(id: &str, text: &str) -> Option<DesktopApp> {
    let mut in_main = false;
    let (mut name, mut exec, mut wm_class, mut kind) = (String::new(), String::new(), String::new(), String::new());
    let mut hidden = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "Name" => name = v.trim().to_string(),
            "Exec" => exec = v.trim().to_string(),
            "StartupWMClass" => wm_class = v.trim().to_string(),
            "Type" => kind = v.trim().to_string(),
            "NoDisplay" | "Hidden" => hidden |= v.trim() == "true",
            _ => {}
        }
    }
    if kind != "Application" || exec.is_empty() || hidden || name.is_empty() {
        return None;
    }
    // Program name: the first word of Exec, or the app id of `flatpak run … <id>`.
    let cmd = exec_command(&exec);
    let words: Vec<&str> = cmd.split_whitespace().filter(|w| *w != "env" && !w.contains('=')).collect();
    let program = if words.first().is_some_and(|w| w.ends_with("flatpak")) {
        words.iter().rev().find(|w| !w.starts_with('-')).copied().unwrap_or("")
    } else {
        words.first().copied().unwrap_or("")
    };
    let program = Path::new(program).file_name().and_then(|f| f.to_str()).unwrap_or(program);
    let last = id.rsplit('.').next().unwrap_or(id);
    let mut keys: Vec<String> = [id, last, &name, program, &wm_class].iter().map(|s| norm(s)).filter(|s| !s.is_empty()).collect();
    keys.dedup();
    Some(DesktopApp { id: id.to_string(), name, exec, keys })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, name: &str, exec: &str) -> DesktopApp {
        parse_entry(id, &format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\n")).unwrap()
    }

    fn apps() -> Vec<DesktopApp> {
        vec![
            app("com.obsproject.Studio", "OBS Studio", "/usr/bin/flatpak run --branch=stable --command=obs com.obsproject.Studio"),
            app("steam", "Steam", "/usr/bin/steam %U"),
            app("org.gnome.Calculator", "Calculator", "gnome-calculator"),
            app("discord", "Discord", "Discord"),
            app("brave-browser", "Brave Web Browser", "brave %U"),
        ]
    }

    #[test]
    fn windows_paths_and_titles() {
        let a = apps();
        let id = |p: &str, t: &str| match resolve_in(&a, p, t, true) {
            Some(Target::App { id, .. }) => id,
            Some(Target::Url(u)) => u,
            Some(Target::Dir(d)) => d.display().to_string(),
            None => "-".into(),
        };
        assert_eq!(id(r#""C:\Program Files\obs-studio\bin\64bit\obs64.exe""#, "OBS\nStudio"), "com.obsproject.Studio");
        assert_eq!(id(r#""C:\Program Files (x86)\Steam\Steam.exe""#, "Steam"), "steam");
        assert_eq!(id(r#""C:\Users\Jared\AppData\Local\Discord\app-1.0.9041\Discord.exe""#, "Discord"), "discord");
        assert_eq!(id("", "Calculator"), "org.gnome.Calculator");
        assert_eq!(id("/System/Applications/Calculator.app", ""), "org.gnome.Calculator");
        assert_eq!(id(r#""C:\Program Files\Microsoft Office\root\Office16\OUTLOOK.EXE""#, "Microsoft\nOutlook"), "https://outlook.office.com/mail/");
        assert_eq!(id("", "ChatGPT"), "https://chatgpt.com/");
        assert_eq!(id("", "Nintendo"), "-");
        // A program on the desktop or in Downloads is the program (here: its web version),
        // not that folder.
        assert_eq!(id(r#"C:\Users\Jared\Desktop\Spotify.lnk"#, "Spotify"), "https://open.spotify.com/");
        assert_eq!(id(r#"C:\Users\Jared\Downloads\SomeTool.exe"#, ""), "-");
        // Similar names only as whole words; known names only for sample profiles.
        assert_eq!(id("", "Brave"), "brave-browser");
        assert_eq!(id("", "Bra"), "-");
        let link = vec![app("com.valvesoftware.SteamLink", "Steam Link", "flatpak run com.valvesoftware.SteamLink")];
        assert!(resolve_in(&link, "", "Steam", false).is_none());
        assert!(matches!(resolve_in(&link, "", "Steam", true), Some(Target::App { .. })));
        assert!(resolve_in(&link, "", "Stea", true).is_none());
    }

    #[test]
    fn name_words() {
        assert_eq!(words("SteamLink"), ["steam", "link"]);
        assert_eq!(words("Brave Web-Browser 2"), ["brave", "web", "browser", "2"]);
        assert_eq!(words("OBS"), ["obs"]);
        assert_eq!(linux_names("obs64"), ["com.obsproject.studio", "obs"]);
    }

    #[test]
    fn candidates_skip_generic_folders() {
        assert_eq!(candidates(r#""C:\Users\Jared\AppData\Local\slack\slack.exe""#, "Slack"), vec!["slack"]);
        assert_eq!(candidates(r#""C:\ProgramData\Microsoft\Windows\Start Menu\Programs\battlenet""#, "Battle.net"), vec!["battlenet"]);
        assert_eq!(candidates("/Applications/Google Chrome.app", ""), vec!["Google Chrome"]);
    }

    #[test]
    fn foreign_paths() {
        assert!(is_foreign(r#""C:\Program Files\x.exe""#));
        assert!(is_foreign("/Applications/Nope Nope.app"));
        assert!(!is_foreign("/usr/bin/firefox"));
        assert!(!is_foreign("~/Documents"));
    }

    #[test]
    fn exec_field_codes() {
        assert_eq!(exec_command("steam %U"), "steam");
        assert_eq!(exec_command("foo --name=%c 100%%"), "foo --name= 100%");
    }

    #[test]
    fn store_paths_become_names() {
        let d = std::env::temp_dir().join(format!("tuxdeck-apps-test-{}", std::process::id()));
        let (store, bin, other) = (d.join("store/brave-1/bin"), d.join("profile/bin"), d.join("other/bin"));
        for x in [&store, &bin, &other] {
            std::fs::create_dir_all(x).unwrap();
        }
        std::fs::write(store.join("brave"), "").unwrap();
        std::fs::write(other.join("brave"), "").unwrap();
        std::os::unix::fs::symlink(store.join("brave"), bin.join("brave")).unwrap();
        let exe = store.join("brave").display().to_string();
        let path = |dirs: &[&PathBuf]| std::env::join_paths(dirs).unwrap().into_string().unwrap();
        assert_eq!(short_program(&format!("{exe} --incognito"), &path(&[&bin])), "brave --incognito");
        assert_eq!(short_program(&exe, &path(&[&bin])), "brave");
        // Another brave comes first on PATH, or none is there: keep the full path.
        assert_eq!(short_program(&exe, &path(&[&other, &bin])), exe);
        assert_eq!(short_program(&exe, ""), exe);
        assert_eq!(short_program("alacritty -e top", &path(&[&bin])), "alacritty -e top");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
