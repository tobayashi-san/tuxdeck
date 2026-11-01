//! Keyboard/media injection for the built-in Hotkey, Text and Multimedia actions.
//!
//! Backends, in order of preference:
//! 1. `/dev/uinput` virtual keyboard (works on X11 and every Wayland compositor,
//!    needs write access to /dev/uinput),
//! 2. Hyprland `hyprctl dispatch sendshortcut`,
//! 3. `wtype` (wlroots compositors), 4. `xdotool` (X11).

use anyhow::{bail, Result};
use evdev::{uinput::VirtualDevice, AttributeSet, EventType, InputEvent, KeyCode};
use serde_json::Value;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Test mode (`TUXDECK_DRY_RUN=1`): log every side effect instead of executing it.
pub fn dry_run() -> bool {
    static D: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *D.get_or_init(|| std::env::var_os("TUXDECK_DRY_RUN").is_some())
}

pub const MOD_SHIFT: u32 = 1;
pub const MOD_CTRL: u32 = 2;
pub const MOD_ALT: u32 = 4;
pub const MOD_SUPER: u32 = 8;

/// One key combination (Elgato `Hotkeys[]` entry, normalized).
#[derive(Debug, Clone, PartialEq)]
pub struct Combo {
    pub mods: u32,
    pub key: Option<KeyCode>,
    /// xkb keysym name for command-line backends.
    pub keysym: Option<String>,
    /// Physical key (evdev code) of hotkeys recorded on Linux. Sent as keycode where
    /// possible, so the active layout applies like for a real key press (QWERTZ etc.).
    pub code: Option<u16>,
}

const QT_UNSET: i64 = 33554431; // 0x01FFFFFF = Qt::Key_unknown

impl Combo {
    /// Parse an Elgato hotkey object. `cmd_as_ctrl` maps the macOS ⌘ modifier to Ctrl.
    pub fn from_elgato(v: &Value, cmd_as_ctrl: bool) -> Option<Combo> {
        let qt = v.get("QTKeyCode").and_then(|x| x.as_i64()).unwrap_or(QT_UNSET);
        let linux = v.get("LinuxCode").and_then(|x| x.as_i64()).unwrap_or(-1);
        let flag = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
        let m = v.get("KeyModifiers").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        let mut mods = 0;
        if flag("KeyShift") || m & 1 != 0 {
            mods |= MOD_SHIFT;
        }
        if flag("KeyCtrl") || m & 2 != 0 {
            mods |= MOD_CTRL;
        }
        if flag("KeyOption") || m & 4 != 0 {
            mods |= MOD_ALT;
        }
        // Hotkeys recorded here carry a LinuxCode: their Meta is Super. ⌘ of profiles
        // made on macOS becomes Ctrl if configured.
        let recorded_here = linux > 0;
        if flag("KeyCmd") || flag("KeyWindows") || m & 8 != 0 {
            mods |= if cmd_as_ctrl && !recorded_here && !flag("KeyWindows") { MOD_CTRL } else { MOD_SUPER };
        }
        let (mut key, mut keysym) = qt_key(qt);
        // Elgato marks keypad keys with this modifier bit.
        if m & 0x4000 != 0 {
            if let Some((k, sym)) = keypad_key(qt) {
                key = Some(k);
                keysym = Some(sym.to_string());
            }
        }
        let code = recorded_here.then_some(linux as u16);
        if let Some(c) = code {
            key = Some(KeyCode::new(c));
        }
        if key.is_none() && keysym.is_none() && mods == 0 {
            return None;
        }
        Some(Combo { mods, key, keysym, code })
    }
}

/// Keypad variant of a Qt key code (digits and operators).
fn keypad_key(qt: i64) -> Option<(KeyCode, &'static str)> {
    use KeyCode as K;
    Some(match u8::try_from(qt).ok()? {
        b'0' => (K::KEY_KP0, "KP_0"),
        b'1' => (K::KEY_KP1, "KP_1"),
        b'2' => (K::KEY_KP2, "KP_2"),
        b'3' => (K::KEY_KP3, "KP_3"),
        b'4' => (K::KEY_KP4, "KP_4"),
        b'5' => (K::KEY_KP5, "KP_5"),
        b'6' => (K::KEY_KP6, "KP_6"),
        b'7' => (K::KEY_KP7, "KP_7"),
        b'8' => (K::KEY_KP8, "KP_8"),
        b'9' => (K::KEY_KP9, "KP_9"),
        b'+' => (K::KEY_KPPLUS, "KP_Add"),
        b'-' => (K::KEY_KPMINUS, "KP_Subtract"),
        b'*' => (K::KEY_KPASTERISK, "KP_Multiply"),
        b'/' => (K::KEY_KPSLASH, "KP_Divide"),
        b'.' | b',' => (K::KEY_KPDOT, "KP_Decimal"),
        b'=' => (K::KEY_KPEQUAL, "KP_Equal"),
        _ => return None,
    })
}

/// Map a Qt::Key code to an evdev key (US layout) and an xkb keysym name.
pub fn qt_key(qt: i64) -> (Option<KeyCode>, Option<String>) {
    use KeyCode as K;
    let ascii_key = |c: u8| -> Option<KeyCode> {
        Some(match c {
            b'A' => K::KEY_A,
            b'B' => K::KEY_B,
            b'C' => K::KEY_C,
            b'D' => K::KEY_D,
            b'E' => K::KEY_E,
            b'F' => K::KEY_F,
            b'G' => K::KEY_G,
            b'H' => K::KEY_H,
            b'I' => K::KEY_I,
            b'J' => K::KEY_J,
            b'K' => K::KEY_K,
            b'L' => K::KEY_L,
            b'M' => K::KEY_M,
            b'N' => K::KEY_N,
            b'O' => K::KEY_O,
            b'P' => K::KEY_P,
            b'Q' => K::KEY_Q,
            b'R' => K::KEY_R,
            b'S' => K::KEY_S,
            b'T' => K::KEY_T,
            b'U' => K::KEY_U,
            b'V' => K::KEY_V,
            b'W' => K::KEY_W,
            b'X' => K::KEY_X,
            b'Y' => K::KEY_Y,
            b'Z' => K::KEY_Z,
            b'0' => K::KEY_0,
            b'1' => K::KEY_1,
            b'2' => K::KEY_2,
            b'3' => K::KEY_3,
            b'4' => K::KEY_4,
            b'5' => K::KEY_5,
            b'6' => K::KEY_6,
            b'7' => K::KEY_7,
            b'8' => K::KEY_8,
            b'9' => K::KEY_9,
            b' ' => K::KEY_SPACE,
            b'-' => K::KEY_MINUS,
            b'=' => K::KEY_EQUAL,
            b'[' => K::KEY_LEFTBRACE,
            b']' => K::KEY_RIGHTBRACE,
            b'\\' => K::KEY_BACKSLASH,
            b';' => K::KEY_SEMICOLON,
            b'\'' => K::KEY_APOSTROPHE,
            b'`' => K::KEY_GRAVE,
            b',' => K::KEY_COMMA,
            b'.' => K::KEY_DOT,
            b'/' => K::KEY_SLASH,
            _ => return None,
        })
    };
    if (0x20..0x7f).contains(&qt) {
        let c = qt as u8;
        let sym = match c {
            b' ' => "space".to_string(),
            b'-' => "minus".into(),
            b'=' => "equal".into(),
            b'[' => "bracketleft".into(),
            b']' => "bracketright".into(),
            b'\\' => "backslash".into(),
            b';' => "semicolon".into(),
            b'\'' => "apostrophe".into(),
            b'`' => "grave".into(),
            b',' => "comma".into(),
            b'.' => "period".into(),
            b'/' => "slash".into(),
            b'+' => "plus".into(),
            b'#' => "numbersign".into(),
            b'!' => "exclam".into(),
            b'"' => "quotedbl".into(),
            b'$' => "dollar".into(),
            b'%' => "percent".into(),
            b'&' => "ampersand".into(),
            b'(' => "parenleft".into(),
            b')' => "parenright".into(),
            b'*' => "asterisk".into(),
            b':' => "colon".into(),
            b'<' => "less".into(),
            b'>' => "greater".into(),
            b'?' => "question".into(),
            b'@' => "at".into(),
            b'^' => "asciicircum".into(),
            b'_' => "underscore".into(),
            b'{' => "braceleft".into(),
            b'|' => "bar".into(),
            b'}' => "braceright".into(),
            b'~' => "asciitilde".into(),
            // xkb keysyms of letters are lower case ("Z" would be Shift+z for some tools).
            c => (c as char).to_ascii_lowercase().to_string(),
        };
        return (ascii_key(c.to_ascii_uppercase()), Some(sym));
    }
    // Latin-1 characters of European layouts (recorded from the layout: ä, ß, é, …): the
    // keysym of the lower-case letter; the physical key comes from LinuxCode.
    if (0xa0..=0xff).contains(&qt) {
        let c = char::from_u32(qt as u32).map(|c| c.to_lowercase().next().unwrap_or(c)).unwrap_or(' ');
        let sym = match c {
            'ä' => "adiaeresis",
            'ö' => "odiaeresis",
            'ü' => "udiaeresis",
            'ß' => "ssharp",
            'é' => "eacute",
            'è' => "egrave",
            'ê' => "ecircumflex",
            'à' => "agrave",
            'â' => "acircumflex",
            'ç' => "ccedilla",
            'ñ' => "ntilde",
            'å' => "aring",
            'ø' => "oslash",
            'æ' => "ae",
            'ò' => "ograve",
            'ù' => "ugrave",
            'ì' => "igrave",
            '§' => "section",
            '°' => "degree",
            '´' => "acute",
            '¨' => "diaeresis",
            '£' => "sterling",
            '¿' => "questiondown",
            '¡' => "exclamdown",
            _ => return (None, None),
        };
        return (None, Some(sym.to_string()));
    }
    let f = |n: i64| -> (Option<KeyCode>, Option<String>) {
        let codes = [
            K::KEY_F1,
            K::KEY_F2,
            K::KEY_F3,
            K::KEY_F4,
            K::KEY_F5,
            K::KEY_F6,
            K::KEY_F7,
            K::KEY_F8,
            K::KEY_F9,
            K::KEY_F10,
            K::KEY_F11,
            K::KEY_F12,
            K::KEY_F13,
            K::KEY_F14,
            K::KEY_F15,
            K::KEY_F16,
            K::KEY_F17,
            K::KEY_F18,
            K::KEY_F19,
            K::KEY_F20,
            K::KEY_F21,
            K::KEY_F22,
            K::KEY_F23,
            K::KEY_F24,
        ];
        (codes.get(n as usize).copied(), Some(format!("F{}", n + 1)))
    };
    if (0x0100_0030..0x0100_0030 + 24).contains(&qt) {
        return f(qt - 0x0100_0030);
    }
    let (k, s) = match qt {
        0x0100_0000 => (K::KEY_ESC, "Escape"),
        0x0100_0001 => (K::KEY_TAB, "Tab"),
        0x0100_0003 => (K::KEY_BACKSPACE, "BackSpace"),
        0x0100_0004 => (K::KEY_ENTER, "Return"),
        0x0100_0005 => (K::KEY_KPENTER, "KP_Enter"),
        0x0100_0006 => (K::KEY_INSERT, "Insert"),
        0x0100_0007 => (K::KEY_DELETE, "Delete"),
        0x0100_0008 => (K::KEY_PAUSE, "Pause"),
        0x0100_0009 => (K::KEY_SYSRQ, "Print"),
        0x0100_0010 => (K::KEY_HOME, "Home"),
        0x0100_0011 => (K::KEY_END, "End"),
        0x0100_0012 => (K::KEY_LEFT, "Left"),
        0x0100_0013 => (K::KEY_UP, "Up"),
        0x0100_0014 => (K::KEY_RIGHT, "Right"),
        0x0100_0015 => (K::KEY_DOWN, "Down"),
        0x0100_0016 => (K::KEY_PAGEUP, "Prior"),
        0x0100_0017 => (K::KEY_PAGEDOWN, "Next"),
        0x0100_0024 => (K::KEY_CAPSLOCK, "Caps_Lock"),
        0x0100_0025 => (K::KEY_NUMLOCK, "Num_Lock"),
        0x0100_0026 => (K::KEY_SCROLLLOCK, "Scroll_Lock"),
        0x0100_0055 => (K::KEY_MENU, "Menu"),
        0x0100_0070 => (K::KEY_VOLUMEDOWN, "XF86AudioLowerVolume"),
        0x0100_0071 => (K::KEY_MUTE, "XF86AudioMute"),
        0x0100_0072 => (K::KEY_VOLUMEUP, "XF86AudioRaiseVolume"),
        0x0100_0080 => (K::KEY_PLAYCD, "XF86AudioPlay"),
        0x0100_0081 => (K::KEY_STOPCD, "XF86AudioStop"),
        0x0100_0082 => (K::KEY_PREVIOUSSONG, "XF86AudioPrev"),
        0x0100_0083 => (K::KEY_NEXTSONG, "XF86AudioNext"),
        0x0100_0086 => (K::KEY_PLAYPAUSE, "XF86AudioPlay"),
        _ => return (None, None),
    };
    (Some(k), Some(s.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Backend {
    Uinput,
    Hyprland,
    Wtype,
    Xdotool,
    None,
}

pub struct Injector {
    dev: Option<VirtualDevice>,
    backend: Backend,
    /// Hyprland >= 0.55 uses a Lua dispatcher API (`hl.dsp.*`); `None` = not probed yet.
    hypr_lua: Option<bool>,
    /// A combo held down that ran one of Hyprland's own key bindings (see `hypr_bind`).
    hypr_bound: Option<Combo>,
    /// Window class of the focused application (terminals paste with Ctrl+Shift+V).
    pub focused_class: String,
}

/// Something to type or press, executed in order on the input thread.
pub enum Job {
    Tap(Combo),
    Down(Combo),
    Up(Combo),
    /// Paste (or type key by key) and optionally press Enter afterwards.
    Text {
        text: String,
        paste: bool,
        enter: bool,
    },
    Media(i64),
    /// Window class of the focused application changed.
    Focus(String),
}

type OnError = Box<dyn FnOnce(anyhow::Error) + Send>;

enum Msg {
    Job(Job, Option<OnError>),
    Backend(String, std::sync::mpsc::Sender<&'static str>),
}

/// Key injection on its own thread: external tools (hyprctl, playerctl) and the delays
/// between keystrokes must not block the engine, which is shared by everything else.
pub struct InputWorker {
    tx: std::sync::mpsc::Sender<Msg>,
    active: std::sync::Arc<parking_lot::Mutex<&'static str>>,
}

impl InputWorker {
    pub fn start(pref: &str) -> InputWorker {
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        let mut inj = Injector::with_preference(pref);
        let active = std::sync::Arc::new(parking_lot::Mutex::new(inj.backend_name()));
        let shown = active.clone();
        let _ = std::thread::Builder::new().name("input".into()).spawn(move || {
            for msg in rx {
                match msg {
                    Msg::Backend(pref, reply) => {
                        let focused = std::mem::take(&mut inj.focused_class);
                        inj = Injector::with_preference(&pref);
                        inj.focused_class = focused;
                        *shown.lock() = inj.backend_name();
                        let _ = reply.send(inj.backend_name());
                    }
                    Msg::Job(job, on_error) => {
                        let r = match job {
                            Job::Tap(c) => inj.tap(&c),
                            Job::Down(c) => inj.combo_down(&c),
                            Job::Up(c) => inj.combo_up(&c),
                            Job::Text { text, paste, enter } => (if text.is_empty() { Ok(()) } else { inj.text(&text, paste) }).and_then(|_| {
                                if enter {
                                    std::thread::sleep(Duration::from_millis(40));
                                    inj.enter()?;
                                }
                                Ok(())
                            }),
                            Job::Media(idx) => media(&mut inj, idx),
                            Job::Focus(class) => {
                                inj.focused_class = class;
                                Ok(())
                            }
                        };
                        if let (Err(e), Some(f)) = (r, on_error) {
                            f(e);
                        }
                    }
                }
            }
        });
        InputWorker { tx, active }
    }

    /// Queue a job; `on_error` runs on the input thread (it must not wait for locks the
    /// caller may hold).
    pub fn run(&self, job: Job, on_error: Option<OnError>) {
        let _ = self.tx.send(Msg::Job(job, on_error));
    }

    /// Switch the backend (after the queued jobs); returns the one now active.
    pub fn set_backend(&self, pref: &str) -> &'static str {
        let (reply, answer) = std::sync::mpsc::channel();
        let _ = self.tx.send(Msg::Backend(pref.to_string(), reply));
        answer.recv_timeout(Duration::from_secs(3)).unwrap_or_else(|_| self.backend_name())
    }

    pub fn backend_name(&self) -> &'static str {
        *self.active.lock()
    }
}

/// Terminal emulators use Ctrl+Shift+V to paste (Ctrl+V is passed to the program).
fn is_terminal(class: &str) -> bool {
    let c = class.to_lowercase();
    let exact = ["st", "st-256color", "rio", "foot", "footclient", "kgx", "xterm", "urxvt", "tilda"];
    let contains = [
        "kitty",
        "alacritty",
        "wezterm",
        "konsole",
        "terminal",
        "ghostty",
        "terminator",
        "tilix",
        "org.gnome.console",
        "termite",
        "blackbox",
        "ptyxis",
        "yakuake",
        "guake",
    ];
    exact.contains(&c.as_str()) || contains.iter().any(|t| c.contains(t))
}

/// The running Hyprland instance. The signature in our environment goes stale when
/// Hyprland is restarted while the service keeps running: then use the newest live one.
pub fn hypr_instance() -> Option<String> {
    let env = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok();
    let mut roots = vec![];
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
        roots.push(std::path::PathBuf::from(rt).join("hypr"));
    }
    roots.push(std::path::PathBuf::from("/tmp/hypr"));
    let live = |sig: &str| roots.iter().any(|r| owned(&r.join(sig).join(".socket.sock")));
    if let Some(sig) = env.as_deref().filter(|s| live(s)) {
        return Some(sig.to_string());
    }
    let newest = roots
        .iter()
        .filter_map(|r| std::fs::read_dir(r).ok())
        .flatten()
        .flatten()
        .filter(|d| owned(&d.path().join(".socket.sock")))
        .max_by_key(|d| d.metadata().and_then(|m| m.modified()).ok())
        .map(|d| d.file_name().to_string_lossy().into_owned());
    // Keep the configured one if nothing better is found (e.g. no runtime dir access).
    newest.or(env)
}

/// A Hyprland socket of this user (`/tmp/hypr` is writable for everyone).
pub fn owned(socket: &std::path::Path) -> bool {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
    let me = unsafe { libc::getuid() };
    // No symlinks, and the instance folder must be ours too (it could be swapped otherwise).
    let mine = |p: &std::path::Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.uid() == me && !m.file_type().is_symlink());
    std::fs::symlink_metadata(socket).is_ok_and(|m| m.file_type().is_socket()) && mine(socket) && socket.parent().is_some_and(mine)
}

/// `hyprctl` talking to the current Hyprland instance.
pub fn hyprctl_command() -> Command {
    let mut c = Command::new("hyprctl");
    if let Some(sig) = hypr_instance() {
        c.env("HYPRLAND_INSTANCE_SIGNATURE", sig);
    }
    c
}

fn hyprctl(expr: &str) -> Result<()> {
    if let Some(r) = exec_fake("hyprctl", &["dispatch", expr], None) {
        return r;
    }
    if dry_run() {
        tracing::info!("DRYRUN hyprctl dispatch {expr}");
        return Ok(());
    }
    let out = hyprctl_command().arg("dispatch").arg(expr).stderr(Stdio::null()).output()?;
    check_hyprctl(out.status.success(), &out.stdout)
}

/// `hyprctl dispatch` exits with 0 on failures too: only `ok` (or nothing) is success.
/// 0.55+ answers `error: …`, older versions e.g. `Invalid dispatcher` to the Lua syntax.
fn check_hyprctl(success: bool, stdout: &[u8]) -> Result<()> {
    let text = String::from_utf8_lossy(stdout);
    let reply = text.trim();
    if !success || !(reply.is_empty() || reply.eq_ignore_ascii_case("ok")) {
        bail!("hyprctl: {reply}");
    }
    Ok(())
}

/// Unit tests never start external tools: the command goes to the thread's fake instead.
#[cfg(test)]
fn exec_fake(cmd: &str, args: &[&str], input: Option<&str>) -> Option<Result<()>> {
    Some(tests::fake_exec(cmd, args, input))
}

#[cfg(not(test))]
fn exec_fake(_: &str, _: &[&str], _: Option<&str>) -> Option<Result<()>> {
    None
}

fn lua_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn has_cmd(name: &str) -> bool {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file())).unwrap_or(false)
}

fn run(cmd: &str, args: &[&str]) -> Result<()> {
    if let Some(r) = exec_fake(cmd, args, None) {
        return r;
    }
    if dry_run() {
        tracing::info!("DRYRUN run {cmd} {}", args.join(" "));
        return Ok(());
    }
    let st = Command::new(cmd).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status()?;
    if !st.success() {
        bail!("{cmd} failed ({st})");
    }
    Ok(())
}

/// What the session offers the command-line backends (display servers, tools on PATH).
#[derive(Debug, Clone, Copy, Default)]
struct Desktop {
    wayland: bool,
    x11: bool,
    hyprland: bool,
    hyprctl: bool,
    wtype: bool,
    xdotool: bool,
    wl_copy: bool,
    xclip: bool,
    /// GNOME or KDE: their Wayland sessions have no virtual-keyboard protocol for wtype.
    gnome_kde: bool,
}

impl Desktop {
    fn detect() -> Desktop {
        Desktop {
            wayland: std::env::var_os("WAYLAND_DISPLAY").is_some(),
            x11: std::env::var_os("DISPLAY").is_some(),
            hyprland: hypr_instance().is_some(),
            hyprctl: has_cmd("hyprctl"),
            wtype: has_cmd("wtype"),
            xdotool: has_cmd("xdotool"),
            // Dry run exercises the clipboard path without the tools.
            wl_copy: dry_run() || has_cmd("wl-copy"),
            xclip: dry_run() || has_cmd("xclip"),
            gnome_kde: std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| {
                let d = d.to_uppercase();
                d.contains("GNOME") || d.contains("KDE")
            }),
        }
    }

    /// Can the command-line backend `name` (see `Injector::CHOICES`) be used?
    fn usable(&self, name: &str) -> bool {
        match name {
            "hyprctl" => self.hyprland && self.hyprctl,
            "wtype" => self.wayland && self.wtype && !self.gnome_kde,
            // In a Wayland session xdotool only reaches XWayland windows.
            "xdotool" => self.xdotool && self.x11 && !self.wayland,
            _ => false,
        }
    }

    /// Clipboard tool and its arguments.
    fn clipboard(&self) -> Option<(&'static str, &'static [&'static str])> {
        if self.wayland && self.wl_copy {
            Some(("wl-copy", &[]))
        } else if self.xclip && self.x11 {
            Some(("xclip", &["-selection", "clipboard"]))
        } else {
            None
        }
    }
}

/// The command-line backend chosen in the settings, if it is usable.
fn preferred_backend(pref: &str, d: &Desktop) -> Option<Backend> {
    let backend = match pref {
        "hyprctl" => Backend::Hyprland,
        "wtype" => Backend::Wtype,
        "xdotool" => Backend::Xdotool,
        _ => return None,
    };
    d.usable(pref).then_some(backend)
}

/// Best command-line backend when uinput is unavailable.
fn fallback_backend(d: &Desktop) -> Backend {
    if d.usable("hyprctl") {
        Backend::Hyprland
    } else if d.usable("wtype") {
        Backend::Wtype
    } else if d.usable("xdotool") {
        Backend::Xdotool
    } else {
        Backend::None
    }
}

/// How text is typed when it is not pasted.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Typing {
    /// Key by key on the virtual keyboard, with the session's layout.
    Uinput,
    Wtype,
    Xdotool,
    Hyprland,
    Paste,
    /// Nothing can type here (and pasting instead would put the text on the clipboard,
    /// which typing is meant to avoid).
    Unavailable,
}

fn typing_method(paste: bool, backend: Backend, d: &Desktop, keymap: bool) -> Typing {
    if paste {
        return Typing::Paste;
    }
    if backend == Backend::Uinput && keymap {
        Typing::Uinput
    } else if d.usable("wtype") {
        Typing::Wtype
    } else if d.usable("xdotool") {
        Typing::Xdotool
    } else if backend == Backend::Hyprland || d.usable("hyprctl") {
        Typing::Hyprland
    } else {
        Typing::Unavailable
    }
}

const MOD_NAMES: [(u32, &str); 4] = [(MOD_CTRL, "ctrl"), (MOD_SHIFT, "shift"), (MOD_ALT, "alt"), (MOD_SUPER, "super")];

fn mod_names(mods: u32) -> Vec<&'static str> {
    MOD_NAMES.iter().filter(|(m, _)| mods & m != 0).map(|(_, n)| *n).collect()
}

/// Hyprland's key of a combo: the physical key if recorded on Linux (xkb code = evdev + 8),
/// otherwise the keysym.
fn hypr_key(c: &Combo) -> String {
    c.code.map(|k| format!("code:{}", k + 8)).or_else(|| c.keysym.clone()).unwrap_or_default()
}

fn hypr_mods(c: &Combo) -> String {
    mod_names(c.mods).iter().map(|n| n.to_uppercase()).collect::<Vec<_>>().join(" ")
}

/// Lua dispatcher expression (Hyprland >= 0.55); `state` is `down`/`up`, `None` taps.
fn hypr_lua_expr(c: &Combo, state: Option<&str>) -> String {
    let (mods, key) = (lua_str(&hypr_mods(c)), lua_str(&hypr_key(c)));
    match state {
        Some(st) => format!("hl.dsp.send_key_state({{ mods = {mods}, key = {key}, state = {} }})", lua_str(st)),
        None => format!("hl.dsp.send_shortcut({{ mods = {mods}, key = {key} }})"),
    }
}

/// Legacy `sendshortcut` dispatcher of older Hyprland versions.
fn hypr_legacy_expr(c: &Combo) -> String {
    format!("sendshortcut {}, {},", hypr_mods(c), hypr_key(c))
}

/// One of Hyprland's key bindings (`hyprctl binds -j`).
#[derive(Debug, PartialEq)]
struct HyprBind {
    dispatcher: String,
    arg: String,
}

impl HyprBind {
    /// The `hyprctl dispatch` argument that runs it. Lua configs (0.55+) bind Lua functions
    /// kept in the registry under the number in `arg`; `dispatch` evaluates
    /// `hl.dispatch(<expr>)`, which calls a function. Older configs name a dispatcher.
    fn dispatch_expr(&self) -> String {
        if self.dispatcher == "__lua" {
            format!("debug.getregistry()[{}]", self.arg.trim().parse::<i64>().unwrap_or(0))
        } else if self.arg.is_empty() {
            self.dispatcher.clone()
        } else {
            format!("{} {}", self.dispatcher, self.arg)
        }
    }
}

/// `hyprctl binds -j` of the running Hyprland.
#[cfg(not(test))]
fn hypr_binds() -> Option<String> {
    if dry_run() {
        return None;
    }
    let out = hyprctl_command().args(["binds", "-j"]).stderr(Stdio::null()).output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
fn hypr_binds() -> Option<String> {
    tests::FAKE_BINDS.with(|b| b.borrow().clone())
}

/// The binding of the main key map that a real press of `c` would trigger. Hyprland's
/// modmask: Shift 1, Ctrl 4, Alt 8, Super 64 (Caps Lock and Num Lock are ignored, as by
/// Hyprland). The key matches by keysym name (case-insensitive) or by key code.
fn find_hypr_bind(json: &str, c: &Combo) -> Option<HyprBind> {
    let binds: Vec<Value> = serde_json::from_str(json).ok()?;
    let mask = (if c.mods & MOD_SHIFT != 0 { 1 } else { 0 })
        | (if c.mods & MOD_CTRL != 0 { 4 } else { 0 })
        | (if c.mods & MOD_ALT != 0 { 8 } else { 0 })
        | (if c.mods & MOD_SUPER != 0 { 64 } else { 0 });
    let sym = c.keysym.as_deref().unwrap_or("");
    let xkb_code = c.code.map(|k| k as i64 + 8).or_else(|| c.key.map(|k| k.code() as i64 + 8));
    binds.iter().find_map(|b| {
        let s = |k: &str| b.get(k).and_then(|v| v.as_str()).unwrap_or("");
        let flag = |k: &str| b.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        let modmask = b.get("modmask").and_then(|v| v.as_i64()).unwrap_or(0) & !(2 | 16);
        let keycode = b.get("keycode").and_then(|v| v.as_i64()).unwrap_or(0);
        let key = s("key");
        let same_key = if keycode > 0 { Some(keycode) == xkb_code } else { !key.is_empty() && !sym.is_empty() && key.eq_ignore_ascii_case(sym) };
        let usable = s("submap").is_empty() && !flag("mouse") && !flag("catch_all") && !flag("release") && !s("dispatcher").is_empty();
        (usable && same_key && modmask == mask).then(|| HyprBind { dispatcher: s("dispatcher").to_string(), arg: s("arg").to_string() })
    })
}

fn wtype_args(c: &Combo) -> Vec<String> {
    let names: Vec<&str> = mod_names(c.mods).into_iter().map(|n| if n == "super" { "logo" } else { n }).collect();
    let mut args: Vec<String> = Vec::new();
    for n in &names {
        args.extend(["-M".to_string(), n.to_string()]);
    }
    if let Some(sym) = c.keysym.as_deref().filter(|s| !s.is_empty()) {
        args.extend(["-k".to_string(), sym.to_string()]);
    }
    for n in names.iter().rev() {
        args.extend(["-m".to_string(), n.to_string()]);
    }
    args
}

fn xdotool_combo(c: &Combo) -> String {
    let mut parts = mod_names(c.mods);
    if let Some(sym) = c.keysym.as_deref().filter(|s| !s.is_empty()) {
        parts.push(sym);
    }
    parts.join("+")
}

/// uinput press order: modifiers first, then the key (released in reverse).
fn uinput_keys(c: &Combo) -> Vec<KeyCode> {
    let mut keys = Injector::mod_keys(c.mods);
    keys.extend(c.key);
    keys
}

impl Injector {
    /// Backends that can be chosen in the settings (`auto` picks the best available).
    pub const CHOICES: &'static [&'static str] = &["auto", "uinput", "hyprctl", "wtype", "xdotool"];

    #[allow(dead_code)]
    pub fn new() -> Injector {
        Self::with_preference("auto")
    }

    /// Create an injector, preferring the backend `pref` (see `CHOICES`) when it is usable.
    pub fn with_preference(pref: &str) -> Injector {
        if dry_run() {
            // Exercise the command line path; every call is logged, nothing is sent.
            return Injector { dev: None, backend: Backend::Hyprland, hypr_lua: Some(true), hypr_bound: None, focused_class: String::new() };
        }
        let desktop = Desktop::detect();
        match preferred_backend(pref, &desktop) {
            Some(backend) => {
                tracing::info!("key injection backend: {backend:?} (setting)");
                return Injector { dev: None, backend, hypr_lua: None, hypr_bound: None, focused_class: String::new() };
            }
            None if matches!(pref, "hyprctl" | "wtype" | "xdotool") => tracing::warn!("input backend {pref} is not available, choosing automatically"),
            None => {}
        }
        let dev = (|| -> Result<VirtualDevice> {
            let mut keys = AttributeSet::<KeyCode>::new();
            for code in 1..=248u16 {
                keys.insert(KeyCode::new(code));
            }
            Ok(VirtualDevice::builder()?.name("TuxDeck virtual keyboard").with_keys(&keys)?.build()?)
        })();
        let backend = match &dev {
            Ok(_) => Backend::Uinput,
            Err(e) => {
                tracing::info!("uinput unavailable ({e}); falling back to compositor tools");
                fallback_backend(&desktop)
            }
        };
        tracing::info!("key injection backend: {backend:?}");
        Injector { dev: dev.ok(), backend, hypr_lua: None, hypr_bound: None, focused_class: String::new() }
    }

    pub fn backend_name(&self) -> &'static str {
        match self.backend {
            Backend::Uinput => "uinput",
            Backend::Hyprland => "hyprctl",
            Backend::Wtype => "wtype",
            Backend::Xdotool => "xdotool",
            Backend::None => "none",
        }
    }

    fn mod_keys(mods: u32) -> Vec<KeyCode> {
        let mut v = Vec::new();
        if mods & MOD_CTRL != 0 {
            v.push(KeyCode::KEY_LEFTCTRL);
        }
        if mods & MOD_SHIFT != 0 {
            v.push(KeyCode::KEY_LEFTSHIFT);
        }
        if mods & MOD_ALT != 0 {
            v.push(KeyCode::KEY_LEFTALT);
        }
        if mods & MOD_SUPER != 0 {
            v.push(KeyCode::KEY_LEFTMETA);
        }
        v
    }

    fn emit(&mut self, keys: &[KeyCode], down: bool) -> Result<()> {
        let dev = self.dev.as_mut().unwrap();
        let ev: Vec<InputEvent> = keys.iter().map(|k| InputEvent::new(EventType::KEY.0, k.code(), down as i32)).collect();
        dev.emit(&ev)?;
        Ok(())
    }

    /// Hyprland: a combo that is one of Hyprland's own key bindings runs that binding (keys
    /// sent with `sendshortcut` only reach the focused window, so Hyprland itself would not
    /// react: workspaces, launchers, screenshots). Other combos go to the focused window:
    /// Lua API first, legacy `sendshortcut` syntax as fallback.
    fn hypr_shortcut(&mut self, c: &Combo, state: Option<&str>) -> Result<()> {
        if state == Some("up") && self.hypr_bound.as_ref() == Some(c) {
            self.hypr_bound = None;
            return Ok(());
        }
        if state != Some("up") {
            if let Some(b) = hypr_binds().and_then(|j| find_hypr_bind(&j, c)) {
                tracing::debug!("{c:?} is Hyprland's binding {} {}", b.dispatcher, b.arg);
                if state == Some("down") {
                    self.hypr_bound = Some(c.clone());
                }
                return hyprctl(&b.dispatch_expr());
            }
        }
        if self.hypr_lua != Some(false) {
            match hyprctl(&hypr_lua_expr(c, state)) {
                Ok(()) => {
                    self.hypr_lua = Some(true);
                    return Ok(());
                }
                Err(e) if self.hypr_lua == Some(true) => return Err(e),
                // Only an older Hyprland that doesn't know the Lua dispatcher switches
                // to the legacy syntax (not e.g. a failed connection).
                Err(e) if !format!("{e:#}").to_lowercase().contains("dispatcher") => return Err(e),
                Err(_) => self.hypr_lua = Some(false),
            }
        }
        if state == Some("up") {
            return Ok(());
        }
        hyprctl(&hypr_legacy_expr(c))
    }

    /// Press a combo and keep it held (uinput / Hyprland); other backends tap on press.
    pub fn combo_down(&mut self, c: &Combo) -> Result<()> {
        if self.backend == Backend::Hyprland {
            return self.hypr_shortcut(c, Some("down"));
        }
        if self.backend == Backend::Uinput {
            for k in uinput_keys(c) {
                self.emit(&[k], true)?;
            }
            Ok(())
        } else {
            self.tap_cli(c)
        }
    }

    pub fn combo_up(&mut self, c: &Combo) -> Result<()> {
        if self.backend == Backend::Hyprland {
            return self.hypr_shortcut(c, Some("up"));
        }
        if self.backend == Backend::Uinput {
            for k in uinput_keys(c).into_iter().rev() {
                self.emit(&[k], false)?;
            }
        }
        Ok(())
    }

    pub fn tap(&mut self, c: &Combo) -> Result<()> {
        if self.backend == Backend::Hyprland {
            return self.hypr_shortcut(c, None);
        }
        self.combo_down(c)?;
        if self.backend == Backend::Uinput {
            std::thread::sleep(Duration::from_millis(15));
        }
        self.combo_up(c)
    }

    fn tap_cli(&mut self, c: &Combo) -> Result<()> {
        match self.backend {
            Backend::Hyprland => self.hypr_shortcut(c, None),
            Backend::Wtype => run("wtype", &wtype_args(c).iter().map(|s| s.as_str()).collect::<Vec<_>>()),
            Backend::Xdotool => run("xdotool", &["key", &xdotool_combo(c)]),
            _ => bail!(crate::tr!("no way to send keys here: enable /dev/uinput (see the handbook) or install wtype (wlroots) or xdotool (X11)")),
        }
    }

    /// Insert text. `paste` uses the clipboard + Ctrl+V like the official app.
    pub fn text(&mut self, text: &str, paste: bool) -> Result<()> {
        let keymap = if !paste && self.backend == Backend::Uinput { crate::keymap::current() } else { None };
        self.text_on(&Desktop::detect(), text, paste, keymap.as_deref())
    }

    fn text_on(&mut self, d: &Desktop, text: &str, paste: bool, keymap: Option<&crate::keymap::Keymap>) -> Result<()> {
        match typing_method(paste, self.backend, d, keymap.is_some()) {
            Typing::Uinput => self.type_uinput(keymap.unwrap(), text),
            Typing::Wtype => run("wtype", &["--", text]),
            Typing::Xdotool => run("xdotool", &["type", "--", text]),
            Typing::Hyprland => self.type_hyprland(d, text),
            Typing::Paste => self.paste(d, text),
            Typing::Unavailable => {
                bail!(crate::tr!("text can't be typed here: enable /dev/uinput (see the handbook) or turn off \"Type instead of paste\""))
            }
        }
    }

    /// Type text on the virtual keyboard. Every character must be on the layout (no dead
    /// keys or compose sequences): nothing is typed otherwise.
    fn type_uinput(&mut self, keymap: &crate::keymap::Keymap, text: &str) -> Result<()> {
        let strokes: Vec<Option<crate::keymap::Stroke>> = text
            .chars()
            .filter(|c| *c != '\r')
            .map(|c| match c {
                '\n' => Some(crate::keymap::Stroke { code: KeyCode::KEY_ENTER.code(), shift: false, altgr: false }),
                c => keymap.stroke(c),
            })
            .collect();
        let missing: String = text.chars().filter(|c| *c != '\r' && *c != '\n').filter(|c| keymap.stroke(*c).is_none()).collect();
        if !missing.is_empty() {
            bail!(crate::tr!(
                "{chars} can't be typed with the keyboard layout {layout}: turn off \"Type instead of paste\"",
                chars = missing,
                layout = keymap.name
            ));
        }
        for s in strokes.into_iter().flatten() {
            let mut keys = Vec::new();
            if s.shift {
                keys.push(KeyCode::KEY_LEFTSHIFT);
            }
            if s.altgr {
                keys.push(KeyCode::KEY_RIGHTALT);
            }
            keys.push(KeyCode::new(s.code));
            for k in &keys {
                self.emit(&[*k], true)?;
            }
            std::thread::sleep(Duration::from_millis(2));
            for k in keys.iter().rev() {
                self.emit(&[*k], false)?;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    /// Type text key by key through Hyprland (keysyms are resolved with the active
    /// keyboard layout). Characters without a plain keysym are pasted.
    fn type_hyprland(&mut self, d: &Desktop, text: &str) -> Result<()> {
        let mut pending = String::new();
        for ch in text.chars() {
            let combo = match ch {
                'a'..='z' | '0'..='9' => Some((0, ch.to_string())),
                'A'..='Z' => Some((MOD_SHIFT, ch.to_ascii_lowercase().to_string())),
                ' ' => Some((0, "space".into())),
                '\n' => Some((0, "Return".into())),
                '\t' => Some((0, "Tab".into())),
                _ => None,
            };
            match combo {
                Some((mods, sym)) => {
                    if !pending.is_empty() {
                        self.paste(d, &std::mem::take(&mut pending))?;
                    }
                    self.hypr_shortcut(&Combo { mods, key: None, keysym: Some(sym), code: None }, None)?;
                }
                None => pending.push(ch),
            }
        }
        if !pending.is_empty() {
            self.paste(d, &pending)?;
        }
        Ok(())
    }

    fn paste(&mut self, d: &Desktop, text: &str) -> Result<()> {
        // Clipboard + paste shortcut.
        let Some((tool, args)) = d.clipboard() else { bail!(crate::tr!("no clipboard tool found: install wl-clipboard (Wayland) or xclip (X11)")) };
        pipe(tool, args, text)?;
        std::thread::sleep(Duration::from_millis(60));
        let mods = if is_terminal(&self.focused_class) { MOD_CTRL | MOD_SHIFT } else { MOD_CTRL };
        let v = Combo { mods, key: Some(KeyCode::KEY_V), keysym: Some("v".into()), code: None };
        self.tap(&v)
    }

    pub fn enter(&mut self) -> Result<()> {
        self.tap(&Combo { mods: 0, key: Some(KeyCode::KEY_ENTER), keysym: Some("Return".into()), code: None })
    }

    pub fn key(&mut self, key: KeyCode, sym: &str) -> Result<()> {
        self.tap(&Combo { mods: 0, key: Some(key), keysym: Some(sym.into()), code: None })
    }
}

fn pipe(cmd: &str, args: &[&str], input: &str) -> Result<()> {
    use std::io::Write;
    if let Some(r) = exec_fake(cmd, args, Some(input)) {
        return r;
    }
    if dry_run() {
        tracing::info!("DRYRUN pipe {cmd} {} <<< {input:?}", args.join(" "));
        return Ok(());
    }
    let mut child = Command::new(cmd).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    child.stdin.take().unwrap().write_all(input.as_bytes())?;
    // wl-copy and xclip fork into the background and keep serving the clipboard; they
    // exit successfully once they own it. Otherwise the paste shortcut would insert
    // whatever was on the clipboard before.
    let st = child.wait()?;
    if !st.success() {
        bail!(crate::tr!("{tool} could not take the clipboard ({status})", tool = cmd, status = st));
    }
    Ok(())
}

/// Multimedia action (`Settings.actionIdx` of `com.elgato.streamdeck.system.multimedia`).
/// Index mapping recovered from the official default profiles' icons.
pub fn media(inj: &mut Injector, idx: i64) -> Result<()> {
    let player = |a: &str| run("playerctl", &[a]);
    let vol = |a: &[&str]| run("wpctl", a).or_else(|_| run("pactl", &pactl_equiv(a)));
    let r = match idx {
        0 => player("previous"),
        1 => player("play-pause"),
        2 => player("next"),
        3 => player("stop"),
        4 => vol(&["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]),
        5 => vol(&["set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", "5%+"]),
        6 => vol(&["set-volume", "@DEFAULT_AUDIO_SINK@", "5%-"]),
        _ => bail!(crate::tr!("unsupported multimedia action {idx}", idx = idx)),
    };
    if r.is_ok() {
        return Ok(());
    }
    // Fall back to media keys.
    let (k, s) = match idx {
        0 => (KeyCode::KEY_PREVIOUSSONG, "XF86AudioPrev"),
        1 => (KeyCode::KEY_PLAYPAUSE, "XF86AudioPlay"),
        2 => (KeyCode::KEY_NEXTSONG, "XF86AudioNext"),
        3 => (KeyCode::KEY_STOPCD, "XF86AudioStop"),
        4 => (KeyCode::KEY_MUTE, "XF86AudioMute"),
        5 => (KeyCode::KEY_VOLUMEUP, "XF86AudioRaiseVolume"),
        _ => (KeyCode::KEY_VOLUMEDOWN, "XF86AudioLowerVolume"),
    };
    inj.key(k, s)
}

fn pactl_equiv<'a>(wp: &[&'a str]) -> Vec<&'a str> {
    match wp.first() {
        Some(&"set-mute") => vec!["set-sink-mute", "@DEFAULT_SINK@", "toggle"],
        _ if wp.last() == Some(&"5%+") => vec!["set-sink-volume", "@DEFAULT_SINK@", "+5%"],
        _ => vec!["set-sink-volume", "@DEFAULT_SINK@", "-5%"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    type Responder = Box<dyn FnMut(&[String]) -> Result<()>>;

    thread_local! {
        /// Commands "run" on this test thread and the fake that answers them.
        static FAKE: RefCell<(Vec<Vec<String>>, Option<Responder>)> = RefCell::new((Vec::new(), None));
        /// What `hyprctl binds -j` answers on this test thread.
        pub(super) static FAKE_BINDS: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    pub(super) fn fake_exec(cmd: &str, args: &[&str], input: Option<&str>) -> Result<()> {
        let mut argv: Vec<String> = std::iter::once(cmd).chain(args.iter().copied()).map(str::to_string).collect();
        if let Some(i) = input {
            argv.push(format!("<<< {i}"));
        }
        FAKE.with(|f| {
            let mut f = f.borrow_mut();
            f.0.push(argv.clone());
            match f.1.as_mut() {
                Some(answer) => answer(&argv),
                None => Ok(()),
            }
        })
    }

    /// Run `f` and return the commands it executed (argv joined with spaces).
    fn record(answer: impl FnMut(&[String]) -> Result<()> + 'static, f: impl FnOnce()) -> Vec<String> {
        FAKE.with(|x| *x.borrow_mut() = (Vec::new(), Some(Box::new(answer))));
        f();
        let log = FAKE.with(|x| std::mem::take(&mut *x.borrow_mut()).0);
        log.into_iter().map(|a| a.join(" ")).collect()
    }

    fn ok(_: &[String]) -> Result<()> {
        Ok(())
    }

    fn inj(backend: Backend) -> Injector {
        Injector { dev: None, backend, hypr_lua: None, hypr_bound: None, focused_class: String::new() }
    }

    fn combo(mods: u32, key: Option<KeyCode>, keysym: &str, code: Option<u16>) -> Combo {
        Combo { mods, key, keysym: (!keysym.is_empty()).then(|| keysym.to_string()), code }
    }

    const ALL: Desktop =
        Desktop { wayland: true, x11: true, hyprland: true, hyprctl: true, wtype: true, xdotool: true, wl_copy: true, xclip: true, gnome_kde: false };
    const NONE: Desktop =
        Desktop { wayland: false, x11: false, hyprland: false, hyprctl: false, wtype: false, xdotool: false, wl_copy: false, xclip: false, gnome_kde: false };

    #[test]
    fn qt_keys() {
        assert_eq!(qt_key('A' as i64), (Some(KeyCode::KEY_A), Some("a".into())));
        assert_eq!(qt_key('a' as i64), (Some(KeyCode::KEY_A), Some("a".into())));
        assert_eq!(qt_key('7' as i64), (Some(KeyCode::KEY_7), Some("7".into())));
        assert_eq!(qt_key(' ' as i64), (Some(KeyCode::KEY_SPACE), Some("space".into())));
        // Layout characters of European keyboards.
        assert_eq!(qt_key(0xdc), (None, Some("udiaeresis".into())));
        assert_eq!(qt_key(0xdf), (None, Some("ssharp".into())));
        assert_eq!(qt_key('/' as i64), (Some(KeyCode::KEY_SLASH), Some("slash".into())));
        assert_eq!(qt_key('.' as i64), (Some(KeyCode::KEY_DOT), Some("period".into())));
        // Shifted symbols have no unshifted US key, only a keysym.
        assert_eq!(qt_key('+' as i64), (None, Some("plus".into())));
        assert_eq!(qt_key('~' as i64), (None, Some("asciitilde".into())));
        assert_eq!(qt_key(0x0100_0030), (Some(KeyCode::KEY_F1), Some("F1".into())));
        assert_eq!(qt_key(0x0100_0030 + 23), (Some(KeyCode::KEY_F24), Some("F24".into())));
        assert_eq!(qt_key(0x0100_0000), (Some(KeyCode::KEY_ESC), Some("Escape".into())));
        assert_eq!(qt_key(0x0100_0004), (Some(KeyCode::KEY_ENTER), Some("Return".into())));
        assert_eq!(qt_key(0x0100_0016), (Some(KeyCode::KEY_PAGEUP), Some("Prior".into())));
        assert_eq!(qt_key(0x0100_0072), (Some(KeyCode::KEY_VOLUMEUP), Some("XF86AudioRaiseVolume".into())));
        assert_eq!(qt_key(0x0100_0086), (Some(KeyCode::KEY_PLAYPAUSE), Some("XF86AudioPlay".into())));
        assert_eq!(qt_key(0x7f), (None, None));
        assert_eq!(qt_key(QT_UNSET), (None, None));
        assert_eq!(qt_key(0x0100_0020), (None, None)); // Qt::Key_Shift alone
    }

    #[test]
    fn keypad_keys() {
        assert_eq!(keypad_key('5' as i64), Some((KeyCode::KEY_KP5, "KP_5")));
        assert_eq!(keypad_key(',' as i64), Some((KeyCode::KEY_KPDOT, "KP_Decimal")));
        assert_eq!(keypad_key('A' as i64), None);
        assert_eq!(keypad_key(0x0100_0004), None);
    }

    #[test]
    fn elgato_hotkeys() {
        // ⌘C from a macOS profile.
        let cmd_c = json!({"KeyCmd": true, "QTKeyCode": 67, "NativeCode": 8});
        assert_eq!(Combo::from_elgato(&cmd_c, true).unwrap(), combo(MOD_CTRL, Some(KeyCode::KEY_C), "c", None));
        assert_eq!(Combo::from_elgato(&cmd_c, false).unwrap().mods, MOD_SUPER);
        // The Windows key always stays Super.
        let win_e = json!({"KeyWindows": true, "QTKeyCode": 69});
        assert_eq!(Combo::from_elgato(&win_e, true).unwrap().mods, MOD_SUPER);
        // Recorded on Linux: Meta is Super and the physical key wins over the Qt key.
        let linux = json!({"KeyCmd": true, "KeyShift": true, "QTKeyCode": 90, "LinuxCode": 21});
        let c = Combo::from_elgato(&linux, true).unwrap();
        assert_eq!(c.mods, MOD_SUPER | MOD_SHIFT);
        assert_eq!((c.key, c.code, c.keysym.as_deref()), (Some(KeyCode::KEY_Y), Some(21), Some("z")));
        // Windows style modifier bit mask.
        let mask = json!({"KeyModifiers": 1 | 2 | 4 | 8, "QTKeyCode": 0x0100_0007});
        let c = Combo::from_elgato(&mask, false).unwrap();
        assert_eq!(c.mods, MOD_SHIFT | MOD_CTRL | MOD_ALT | MOD_SUPER);
        assert_eq!(c.key, Some(KeyCode::KEY_DELETE));
        assert_eq!(Combo::from_elgato(&mask, true).unwrap().mods, MOD_SHIFT | MOD_CTRL | MOD_ALT);
        // Keypad flag.
        let kp = json!({"KeyModifiers": 0x4000, "QTKeyCode": 43});
        assert_eq!(Combo::from_elgato(&kp, true).unwrap(), combo(0, Some(KeyCode::KEY_KPPLUS), "KP_Add", None));
        let kp_letter = json!({"KeyModifiers": 0x4000, "QTKeyCode": 65});
        assert_eq!(Combo::from_elgato(&kp_letter, true).unwrap().key, Some(KeyCode::KEY_A));
        // Modifier only, and nothing at all.
        assert_eq!(Combo::from_elgato(&json!({"KeyOption": true}), true).unwrap(), combo(MOD_ALT, None, "", None));
        assert_eq!(Combo::from_elgato(&json!({}), true), None);
        assert_eq!(Combo::from_elgato(&json!({"QTKeyCode": QT_UNSET, "LinuxCode": -1}), true), None);
        assert_eq!(Combo::from_elgato(&json!({"LinuxCode": 30}), true).unwrap(), combo(0, Some(KeyCode::KEY_A), "", Some(30)));
    }

    #[test]
    fn uinput_order() {
        let c = combo(MOD_SUPER | MOD_ALT | MOD_SHIFT | MOD_CTRL, Some(KeyCode::KEY_T), "t", None);
        assert_eq!(uinput_keys(&c), vec![KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_LEFTALT, KeyCode::KEY_LEFTMETA, KeyCode::KEY_T]);
        assert_eq!(uinput_keys(&combo(0, None, "", None)), vec![]);
    }

    #[test]
    fn hyprland_own_bindings() {
        // As `hyprctl binds -j` of a Lua config (0.55) and of an older config.
        let binds = r#"[
            {"modmask": 64, "submap": "", "key": "T", "keycode": 0, "mouse": false, "release": false, "catch_all": false, "dispatcher": "__lua", "arg": "36"},
            {"modmask": 66, "submap": "", "key": "Return", "keycode": 0, "mouse": false, "release": false, "catch_all": false, "dispatcher": "exec", "arg": "kitty"},
            {"modmask": 64, "submap": "resize", "key": "R", "keycode": 0, "mouse": false, "release": false, "catch_all": false, "dispatcher": "exec", "arg": "x"},
            {"modmask": 64, "submap": "", "key": "", "keycode": 18, "mouse": false, "release": false, "catch_all": false, "dispatcher": "exec", "arg": "files"},
            {"modmask": 64, "submap": "", "key": "mouse:272", "keycode": 0, "mouse": true, "release": false, "catch_all": false, "dispatcher": "movewindow", "arg": ""}
        ]"#;
        // Super+T recorded here (QWERTZ etc.: keysym from the layout, code of the key).
        let super_t = Combo::from_elgato(&json!({"KeyCmd": true, "KeyModifiers": 8, "QTKeyCode": 84, "LinuxCode": 20}), true).unwrap();
        let b = find_hypr_bind(binds, &super_t).unwrap();
        assert_eq!(b.dispatch_expr(), "debug.getregistry()[36]");
        // Caps Lock in the mask is ignored; a dispatcher of an older config is named.
        assert_eq!(find_hypr_bind(binds, &combo(MOD_SUPER, None, "Return", None)).unwrap().dispatch_expr(), "exec kitty");
        // By key code (xkb code = evdev + 8).
        assert_eq!(find_hypr_bind(binds, &combo(MOD_SUPER, Some(KeyCode::new(10)), "9", Some(10))).unwrap().arg, "files");
        // Other submaps, other modifiers and mouse bindings don't count.
        assert_eq!(find_hypr_bind(binds, &combo(MOD_SUPER, None, "r", None)), None);
        assert_eq!(find_hypr_bind(binds, &combo(MOD_SUPER | MOD_SHIFT, None, "t", None)), None);
        assert_eq!(find_hypr_bind(binds, &combo(0, None, "t", None)), None);
        assert_eq!(find_hypr_bind("not json", &super_t), None);

        // The injector runs the binding instead of sending keys to the window; held keys
        // run it once on press, the release sends nothing.
        FAKE_BINDS.with(|f| *f.borrow_mut() = Some(binds.to_string()));
        let mut i = inj(Backend::Hyprland);
        let log = record(ok, || {
            i.tap(&super_t).unwrap();
            i.combo_down(&super_t).unwrap();
            i.combo_up(&super_t).unwrap();
            i.tap(&combo(MOD_CTRL, Some(KeyCode::KEY_C), "c", Some(46))).unwrap();
        });
        assert_eq!(
            log,
            vec![
                "hyprctl dispatch debug.getregistry()[36]",
                "hyprctl dispatch debug.getregistry()[36]",
                r#"hyprctl dispatch hl.dsp.send_shortcut({ mods = "CTRL", key = "code:54" })"#,
            ]
        );
        FAKE_BINDS.with(|f| *f.borrow_mut() = None);
    }

    #[test]
    fn hyprland_expressions() {
        // Physical key: xkb code = evdev + 8.
        let c = combo(MOD_CTRL | MOD_SHIFT, Some(KeyCode::KEY_C), "c", Some(46));
        assert_eq!(hypr_key(&c), "code:54");
        assert_eq!(hypr_lua_expr(&c, None), r#"hl.dsp.send_shortcut({ mods = "CTRL SHIFT", key = "code:54" })"#);
        assert_eq!(hypr_lua_expr(&c, Some("down")), r#"hl.dsp.send_key_state({ mods = "CTRL SHIFT", key = "code:54", state = "down" })"#);
        assert_eq!(hypr_legacy_expr(&c), "sendshortcut CTRL SHIFT, code:54,");
        let c = combo(MOD_SUPER | MOD_ALT, None, "Return", None);
        assert_eq!(hypr_lua_expr(&c, Some("up")), r#"hl.dsp.send_key_state({ mods = "ALT SUPER", key = "Return", state = "up" })"#);
        assert_eq!(hypr_legacy_expr(&combo(0, None, "XF86AudioPlay", None)), "sendshortcut , XF86AudioPlay,");
        assert_eq!(hypr_key(&combo(MOD_CTRL, None, "", None)), "");
        assert_eq!(lua_str(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn cli_arguments() {
        let c = combo(MOD_CTRL | MOD_SUPER, Some(KeyCode::KEY_L), "l", Some(38));
        assert_eq!(wtype_args(&c), ["-M", "ctrl", "-M", "logo", "-k", "l", "-m", "logo", "-m", "ctrl"]);
        assert_eq!(xdotool_combo(&c), "ctrl+super+l");
        let c = combo(MOD_SHIFT | MOD_ALT, None, "", None);
        assert_eq!(wtype_args(&c), ["-M", "shift", "-M", "alt", "-m", "alt", "-m", "shift"]);
        assert_eq!(xdotool_combo(&c), "shift+alt");
        let c = combo(0, None, "F5", None);
        assert_eq!(wtype_args(&c), ["-k", "F5"]);
        assert_eq!(xdotool_combo(&c), "F5");
    }

    #[test]
    fn hyprctl_replies() {
        assert!(check_hyprctl(true, b"ok\n").is_ok());
        assert!(check_hyprctl(true, b"").is_ok());
        let e = check_hyprctl(true, b"  error: invalid dispatcher\n").unwrap_err();
        assert_eq!(e.to_string(), "hyprctl: error: invalid dispatcher");
        assert!(check_hyprctl(false, b"ok").is_err());
        // Real replies (exit code 0): Hyprland <= 0.54 to the Lua syntax, 0.55 to errors.
        assert_eq!(check_hyprctl(true, b"Invalid dispatcher\n").unwrap_err().to_string(), "hyprctl: Invalid dispatcher");
        assert!(check_hyprctl(true, b"Error: [string \"return hl.dispatch(...)\"]:1: bad\n").is_err());
        assert!(check_hyprctl(true, b"error: [string \"return hl.dispatch(hl.dsp.x())\"]:1: attempt to call a nil value\n").is_err());
    }

    #[test]
    fn backend_selection() {
        let hypr = Desktop { wayland: true, hyprland: true, hyprctl: true, ..NONE };
        let wlroots = Desktop { wayland: true, wtype: true, xdotool: true, ..NONE };
        let x11 = Desktop { x11: true, xdotool: true, ..NONE };
        assert_eq!(preferred_backend("hyprctl", &hypr), Some(Backend::Hyprland));
        assert_eq!(preferred_backend("hyprctl", &Desktop { hyprctl: false, ..hypr }), None);
        assert_eq!(preferred_backend("hyprctl", &Desktop { hyprland: false, ..hypr }), None);
        assert_eq!(preferred_backend("wtype", &wlroots), Some(Backend::Wtype));
        assert_eq!(preferred_backend("wtype", &Desktop { wayland: false, ..wlroots }), None);
        assert_eq!(preferred_backend("xdotool", &x11), Some(Backend::Xdotool));
        assert_eq!(preferred_backend("xdotool", &hypr), None);
        for pref in ["auto", "uinput", "", "bogus"] {
            assert_eq!(preferred_backend(pref, &ALL), None, "{pref}");
        }
        assert_eq!(fallback_backend(&ALL), Backend::Hyprland);
        assert_eq!(fallback_backend(&wlroots), Backend::Wtype);
        // wtype needs a wlroots-like Wayland session, xdotool an X11 one.
        assert_eq!(fallback_backend(&Desktop { gnome_kde: true, ..wlroots }), Backend::None);
        assert_eq!(fallback_backend(&Desktop { wayland: false, ..wlroots }), Backend::None);
        assert_eq!(fallback_backend(&Desktop { x11: true, ..wlroots }), Backend::Wtype);
        assert_eq!(fallback_backend(&Desktop { wtype: false, x11: true, ..wlroots }), Backend::None);
        assert_eq!(fallback_backend(&x11), Backend::Xdotool);
        assert_eq!(fallback_backend(&NONE), Backend::None);
        for (b, n) in
            [(Backend::Uinput, "uinput"), (Backend::Hyprland, "hyprctl"), (Backend::Wtype, "wtype"), (Backend::Xdotool, "xdotool"), (Backend::None, "none")]
        {
            assert_eq!(inj(b).backend_name(), n);
            assert!(n == "none" || Injector::CHOICES.contains(&n));
        }
    }

    #[test]
    fn typing_and_clipboard_choice() {
        assert_eq!(typing_method(true, Backend::Hyprland, &ALL, true), Typing::Paste);
        // The virtual keyboard types with the session's layout, before any tool.
        assert_eq!(typing_method(false, Backend::Uinput, &ALL, true), Typing::Uinput);
        assert_eq!(typing_method(false, Backend::Uinput, &ALL, false), Typing::Wtype);
        assert_eq!(typing_method(false, Backend::Uinput, &Desktop { wtype: false, ..ALL }, false), Typing::Hyprland);
        let x11 = Desktop { x11: true, xdotool: true, ..NONE };
        assert_eq!(typing_method(false, Backend::Uinput, &x11, false), Typing::Xdotool);
        // XWayland: xdotool can't type into Wayland windows; GNOME/KDE don't let wtype.
        assert_eq!(typing_method(false, Backend::Uinput, &Desktop { wayland: true, ..x11 }, false), Typing::Unavailable);
        let gnome = Desktop { wayland: true, wtype: true, wl_copy: true, gnome_kde: true, ..NONE };
        assert_eq!(typing_method(false, Backend::Uinput, &gnome, false), Typing::Unavailable);
        assert_eq!(typing_method(false, Backend::Uinput, &gnome, true), Typing::Uinput);
        assert_eq!(typing_method(false, Backend::Hyprland, &NONE, false), Typing::Hyprland);
        assert_eq!(typing_method(false, Backend::Uinput, &NONE, false), Typing::Unavailable);

        assert_eq!(ALL.clipboard(), Some(("wl-copy", &[][..])));
        assert_eq!(Desktop { wl_copy: false, ..ALL }.clipboard(), Some(("xclip", &["-selection", "clipboard"][..])));
        assert_eq!(Desktop { wayland: false, ..ALL }.clipboard().unwrap().0, "xclip");
        assert_eq!(Desktop { wayland: true, wl_copy: false, ..NONE }.clipboard(), None);
        // xclip needs an X server.
        assert_eq!(Desktop { x11: false, wl_copy: false, ..ALL }.clipboard(), None);
        assert_eq!(NONE.clipboard(), None);
    }

    #[test]
    fn terminals() {
        for t in [
            "kitty",
            "Alacritty",
            "org.wezfurlong.wezterm",
            "foot",
            "st-256color",
            "com.mitchellh.ghostty",
            "gnome-terminal-server",
            "org.gnome.Console",
            "org.gnome.Ptyxis",
            "org.kde.yakuake",
        ] {
            assert!(is_terminal(t), "{t}");
        }
        for t in ["firefox", "", "steam", "code"] {
            assert!(!is_terminal(t), "{t}");
        }
    }

    #[test]
    fn cli_backends_tap() {
        let c = combo(MOD_CTRL | MOD_SUPER, Some(KeyCode::KEY_L), "l", Some(38));
        let log = record(ok, || inj(Backend::Wtype).tap(&c).unwrap());
        assert_eq!(log, ["wtype -M ctrl -M logo -k l -m logo -m ctrl"]);
        let log = record(ok, || inj(Backend::Xdotool).tap(&c).unwrap());
        assert_eq!(log, ["xdotool key ctrl+super+l"]);
        // Command-line tools can't hold a key: down taps, up does nothing.
        let log = record(ok, || {
            let mut i = inj(Backend::Xdotool);
            i.combo_down(&c).unwrap();
            i.combo_up(&c).unwrap();
        });
        assert_eq!(log, ["xdotool key ctrl+super+l"]);
        let log = record(ok, || assert!(inj(Backend::None).tap(&c).is_err()));
        assert!(log.is_empty());
        let log = record(|_| anyhow::bail!("exit 1"), || assert!(inj(Backend::Wtype).tap(&c).is_err()));
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn hyprland_lua_and_legacy() {
        let c = combo(MOD_CTRL, Some(KeyCode::KEY_S), "s", Some(31));
        let lua_tap = r#"hyprctl dispatch hl.dsp.send_shortcut({ mods = "CTRL", key = "code:39" })"#;
        let legacy = "hyprctl dispatch sendshortcut CTRL, code:39,";

        let mut i = inj(Backend::Hyprland);
        assert_eq!(record(ok, || i.tap(&c).unwrap()), [lua_tap]);
        assert_eq!(i.hypr_lua, Some(true));

        // Old Hyprland: the Lua dispatcher is unknown -> legacy syntax, remembered.
        let old = |a: &[String]| check_hyprctl(true, if a[2].starts_with("hl.dsp") { b"Invalid dispatcher\n" } else { b"ok\n" });
        let mut i = inj(Backend::Hyprland);
        assert_eq!(record(old, || i.tap(&c).unwrap()), [lua_tap, legacy]);
        assert_eq!(i.hypr_lua, Some(false));
        assert_eq!(record(old, || i.tap(&c).unwrap()), [legacy]);
        // Legacy has no key state: down taps, up is a no-op.
        assert_eq!(record(old, || i.combo_down(&c).unwrap()), [legacy]);
        assert!(record(old, || i.combo_up(&c).unwrap()).is_empty());

        // Other failures (e.g. no connection) don't switch the syntax.
        let mut i = inj(Backend::Hyprland);
        let log = record(|_| anyhow::bail!("hyprctl: Couldn't connect"), || assert!(i.tap(&c).is_err()));
        assert_eq!(log, [lua_tap]);
        assert_eq!(i.hypr_lua, None);
        // Once Lua worked, a dispatcher error is a real error.
        i.hypr_lua = Some(true);
        let log = record(|_| anyhow::bail!("hyprctl: Invalid dispatcher"), || assert!(i.tap(&c).is_err()));
        assert_eq!(log, [lua_tap]);

        let mut i = inj(Backend::Hyprland);
        let log = record(ok, || {
            i.combo_down(&c).unwrap();
            i.combo_up(&c).unwrap();
        });
        assert_eq!(
            log,
            [
                r#"hyprctl dispatch hl.dsp.send_key_state({ mods = "CTRL", key = "code:39", state = "down" })"#,
                r#"hyprctl dispatch hl.dsp.send_key_state({ mods = "CTRL", key = "code:39", state = "up" })"#
            ]
        );
    }

    #[test]
    fn text_typing() {
        let wl = Desktop { wayland: true, wtype: true, wl_copy: true, ..NONE };
        assert_eq!(record(ok, || inj(Backend::Uinput).text_on(&wl, "-n hi", false, None).unwrap()), ["wtype -- -n hi"]);
        let x11 = Desktop { x11: true, xdotool: true, xclip: true, ..NONE };
        assert_eq!(record(ok, || inj(Backend::Uinput).text_on(&x11, "a b", false, None).unwrap()), ["xdotool type -- a b"]);

        // Hyprland: plain keys as shortcuts, the rest pasted in runs.
        let hypr = Desktop { wayland: true, hyprland: true, hyprctl: true, wl_copy: true, ..NONE };
        let mut i = inj(Backend::Hyprland);
        i.hypr_lua = Some(true);
        let log = record(ok, || i.text_on(&hypr, "Hi 2ü€\t", false, None).unwrap());
        let key = |m: &str, k: &str| format!(r#"hyprctl dispatch hl.dsp.send_shortcut({{ mods = "{m}", key = "{k}" }})"#);
        assert_eq!(log, [key("SHIFT", "h"), key("", "i"), key("", "space"), key("", "2"), "wl-copy <<< ü€".into(), key("CTRL", "v"), key("", "Tab")]);
        let log = record(ok, || i.text_on(&hypr, "a€", false, None).unwrap());
        assert_eq!(log, [key("", "a"), "wl-copy <<< €".into(), key("CTRL", "v")]);
    }

    #[test]
    fn text_paste() {
        let wl = Desktop { wayland: true, wl_copy: true, wtype: true, ..NONE };
        let mut i = inj(Backend::Wtype);
        assert_eq!(record(ok, || i.text_on(&wl, "hello", true, None).unwrap()), ["wl-copy <<< hello", "wtype -M ctrl -k v -m ctrl"]);
        // Terminals paste with Ctrl+Shift+V.
        i.focused_class = "kitty".into();
        assert_eq!(record(ok, || i.text_on(&wl, "ls", true, None).unwrap()), ["wl-copy <<< ls", "wtype -M ctrl -M shift -k v -m shift -m ctrl"]);
        let x11 = Desktop { x11: true, xclip: true, xdotool: true, ..NONE };
        let mut i = inj(Backend::Xdotool);
        assert_eq!(record(ok, || i.text_on(&x11, "x", true, None).unwrap()), ["xclip -selection clipboard <<< x", "xdotool key ctrl+v"]);
        // Nothing to type with: an error, not the clipboard (typing is meant to avoid it).
        let log = record(ok, || assert!(i.text_on(&Desktop { xdotool: false, ..x11 }, "y", false, None).is_err()));
        assert!(log.is_empty());
        let log = record(ok, || assert!(inj(Backend::Uinput).text_on(&NONE, "z", false, None).is_err()));
        assert!(log.is_empty());
        // A failing clipboard tool stops before the shortcut.
        assert_eq!(record(|_| anyhow::bail!("no"), || assert!(i.text_on(&x11, "x", true, None).is_err())), ["xclip -selection clipboard <<< x"]);
        assert_eq!(record(ok, || i.enter().unwrap()), ["xdotool key Return"]);
    }

    #[test]
    fn media_actions() {
        let mut i = inj(Backend::Xdotool);
        for (idx, cmd) in [(0, "playerctl previous"), (1, "playerctl play-pause"), (2, "playerctl next"), (3, "playerctl stop")] {
            assert_eq!(record(ok, || media(&mut i, idx).unwrap()), [cmd]);
        }
        assert_eq!(record(ok, || media(&mut i, 4).unwrap()), ["wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"]);
        assert_eq!(record(ok, || media(&mut i, 5).unwrap()), ["wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 5%+"]);
        assert_eq!(record(ok, || media(&mut i, 6).unwrap()), ["wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-"]);
        // No PipeWire: pactl.
        let no_wpctl = |a: &[String]| if a[0] == "wpctl" { anyhow::bail!("not found") } else { Ok(()) };
        assert_eq!(record(no_wpctl, || media(&mut i, 4).unwrap())[1], "pactl set-sink-mute @DEFAULT_SINK@ toggle");
        assert_eq!(record(no_wpctl, || media(&mut i, 5).unwrap())[1], "pactl set-sink-volume @DEFAULT_SINK@ +5%");
        assert_eq!(record(no_wpctl, || media(&mut i, 6).unwrap())[1], "pactl set-sink-volume @DEFAULT_SINK@ -5%");
        // No player / sound tool: media keys.
        let only_keys = |a: &[String]| if a[0] == "xdotool" { Ok(()) } else { anyhow::bail!("failed") };
        assert_eq!(record(only_keys, || media(&mut i, 1).unwrap()), ["playerctl play-pause", "xdotool key XF86AudioPlay"]);
        assert_eq!(record(only_keys, || media(&mut i, 6).unwrap()).last().unwrap(), "xdotool key XF86AudioLowerVolume");
        assert_eq!(record(only_keys, || media(&mut i, 5).unwrap()).len(), 3);
        assert!(record(ok, || assert!(media(&mut i, 7).is_err())).is_empty());
        assert!(record(ok, || assert!(media(&mut i, -1).is_err())).is_empty());
    }
}
