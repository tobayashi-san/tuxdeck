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
    let exact = ["st", "st-256color", "rio", "foot", "footclient", "kgx", "xterm", "urxvt"];
    let contains = ["kitty", "alacritty", "wezterm", "konsole", "terminal", "ghostty", "terminator", "tilix", "org.gnome.console", "termite", "blackbox"];
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
    let live = |sig: &str| roots.iter().any(|r| r.join(sig).join(".socket.sock").exists());
    if let Some(sig) = env.as_deref().filter(|s| live(s)) {
        return Some(sig.to_string());
    }
    let newest = roots
        .iter()
        .filter_map(|r| std::fs::read_dir(r).ok())
        .flatten()
        .flatten()
        .filter(|d| d.path().join(".socket.sock").exists())
        .max_by_key(|d| d.metadata().and_then(|m| m.modified()).ok())
        .map(|d| d.file_name().to_string_lossy().into_owned());
    // Keep the configured one if nothing better is found (e.g. no runtime dir access).
    newest.or(env)
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
    if dry_run() {
        tracing::info!("DRYRUN hyprctl dispatch {expr}");
        return Ok(());
    }
    let out = hyprctl_command().arg("dispatch").arg(expr).stderr(Stdio::null()).output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || text.trim_start().starts_with("error") {
        bail!("hyprctl: {}", text.trim());
    }
    Ok(())
}

fn lua_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn has_cmd(name: &str) -> bool {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file())).unwrap_or(false)
}

fn run(cmd: &str, args: &[&str]) -> Result<()> {
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
            return Injector { dev: None, backend: Backend::Hyprland, hypr_lua: Some(true), focused_class: String::new() };
        }
        let usable = |b: &str| match b {
            "hyprctl" => hypr_instance().is_some() && has_cmd("hyprctl"),
            "wtype" => std::env::var_os("WAYLAND_DISPLAY").is_some() && has_cmd("wtype"),
            "xdotool" => has_cmd("xdotool"),
            _ => false,
        };
        match pref {
            "hyprctl" | "wtype" | "xdotool" if usable(pref) => {
                let backend = match pref {
                    "hyprctl" => Backend::Hyprland,
                    "wtype" => Backend::Wtype,
                    _ => Backend::Xdotool,
                };
                tracing::info!("key injection backend: {backend:?} (setting)");
                return Injector { dev: None, backend, hypr_lua: None, focused_class: String::new() };
            }
            "hyprctl" | "wtype" | "xdotool" => tracing::warn!("input backend {pref} is not available, choosing automatically"),
            _ => {}
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
                if hypr_instance().is_some() && has_cmd("hyprctl") {
                    Backend::Hyprland
                } else if std::env::var_os("WAYLAND_DISPLAY").is_some() && has_cmd("wtype") {
                    Backend::Wtype
                } else if has_cmd("xdotool") {
                    Backend::Xdotool
                } else {
                    Backend::None
                }
            }
        };
        tracing::info!("key injection backend: {backend:?}");
        Injector { dev: dev.ok(), backend, hypr_lua: None, focused_class: String::new() }
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

    fn hypr_mods(c: &Combo) -> String {
        [(MOD_CTRL, "CTRL"), (MOD_SHIFT, "SHIFT"), (MOD_ALT, "ALT"), (MOD_SUPER, "SUPER")]
            .iter()
            .filter(|(m, _)| c.mods & m != 0)
            .map(|(_, n)| *n)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Hyprland: Lua API first, legacy `sendshortcut` syntax as fallback.
    fn hypr_shortcut(&mut self, c: &Combo, state: Option<&str>) -> Result<()> {
        let mods = Self::hypr_mods(c);
        // Hyprland's keycodes are xkb codes (evdev + 8).
        let key = c.code.map(|k| format!("code:{}", k + 8)).or_else(|| c.keysym.clone()).unwrap_or_default();
        if self.hypr_lua != Some(false) {
            let expr = match state {
                Some(st) => format!("hl.dsp.send_key_state({{ mods = {}, key = {}, state = {} }})", lua_str(&mods), lua_str(&key), lua_str(st)),
                None => format!("hl.dsp.send_shortcut({{ mods = {}, key = {} }})", lua_str(&mods), lua_str(&key)),
            };
            match hyprctl(&expr) {
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
        hyprctl(&format!("sendshortcut {mods}, {key},"))
    }

    /// Press a combo and keep it held (uinput / Hyprland); other backends tap on press.
    pub fn combo_down(&mut self, c: &Combo) -> Result<()> {
        if self.backend == Backend::Hyprland {
            return self.hypr_shortcut(c, Some("down"));
        }
        if self.backend == Backend::Uinput {
            let mut keys = Self::mod_keys(c.mods);
            keys.extend(c.key);
            for k in keys {
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
            let mut keys = Self::mod_keys(c.mods);
            keys.extend(c.key);
            for k in keys.into_iter().rev() {
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
        let names: Vec<&str> = [(MOD_CTRL, "ctrl"), (MOD_SHIFT, "shift"), (MOD_ALT, "alt"), (MOD_SUPER, "super")]
            .iter()
            .filter(|(m, _)| c.mods & m != 0)
            .map(|(_, n)| *n)
            .collect();
        let sym = c.keysym.clone().unwrap_or_default();
        match self.backend {
            Backend::Hyprland => self.hypr_shortcut(c, None),
            Backend::Wtype => {
                let mut args: Vec<String> = Vec::new();
                for n in &names {
                    args.push("-M".into());
                    args.push(if *n == "super" { "logo".into() } else { n.to_string() });
                }
                if !sym.is_empty() {
                    args.push("-k".into());
                    args.push(sym);
                }
                for n in names.iter().rev() {
                    args.push("-m".into());
                    args.push(if *n == "super" { "logo".into() } else { n.to_string() });
                }
                run("wtype", &args.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            }
            Backend::Xdotool => {
                let mut combo = names.join("+");
                if !sym.is_empty() {
                    if !combo.is_empty() {
                        combo.push('+');
                    }
                    combo.push_str(&sym);
                }
                run("xdotool", &["key", &combo])
            }
            _ => bail!("no key injection backend available (enable /dev/uinput or install wtype/xdotool)"),
        }
    }

    /// Insert text. `paste` uses the clipboard + Ctrl+V like the official app.
    pub fn text(&mut self, text: &str, paste: bool) -> Result<()> {
        if !paste {
            if has_cmd("wtype") && std::env::var_os("WAYLAND_DISPLAY").is_some() {
                return run("wtype", &["--", text]);
            }
            if has_cmd("xdotool") && std::env::var_os("DISPLAY").is_some() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
                return run("xdotool", &["type", "--", text]);
            }
            if self.backend == Backend::Hyprland || (hypr_instance().is_some() && has_cmd("hyprctl")) {
                return self.type_hyprland(text);
            }
        }
        self.paste(text)
    }

    /// Type text key by key through Hyprland (keysyms are resolved with the active
    /// keyboard layout). Characters without a plain keysym are pasted.
    fn type_hyprland(&mut self, text: &str) -> Result<()> {
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
                        self.paste(&std::mem::take(&mut pending))?;
                    }
                    self.hypr_shortcut(&Combo { mods, key: None, keysym: Some(sym), code: None }, None)?;
                }
                None => pending.push(ch),
            }
        }
        if !pending.is_empty() {
            self.paste(&pending)?;
        }
        Ok(())
    }

    fn paste(&mut self, text: &str) -> Result<()> {
        // Clipboard + paste shortcut.
        let have = |c: &str| dry_run() || has_cmd(c);
        let copied = if std::env::var_os("WAYLAND_DISPLAY").is_some() && have("wl-copy") {
            pipe("wl-copy", &[], text)
        } else if have("xclip") {
            pipe("xclip", &["-selection", "clipboard"], text)
        } else {
            bail!("no clipboard tool (wl-copy/xclip) found")
        };
        copied?;
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
    if dry_run() {
        tracing::info!("DRYRUN pipe {cmd} {} <<< {input:?}", args.join(" "));
        return Ok(());
    }
    let mut child = Command::new(cmd).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    child.stdin.take().unwrap().write_all(input.as_bytes())?;
    // wl-copy forks into the background and keeps serving the clipboard.
    let _ = child.wait();
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
        _ => bail!("unsupported multimedia action {idx}"),
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
