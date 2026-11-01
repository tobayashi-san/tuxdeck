mod apps;
mod backup;
mod builtin;
mod catalogue;
mod compat;
mod device;
mod engine;
mod i18n;
mod iconpacks;
mod input;
mod keymap;
mod layout;
mod linuxcheck;
mod net;
mod packages;
mod paths;
mod peer;
mod plugins;
mod profile;
mod render;
mod resources;
mod sdk;
mod smart;
mod strip;
mod urlhandler;
mod web;
mod web_plugins;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use engine::{DevCmd, DevEvent, Engine, Shared};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

#[derive(Parser)]
#[command(name = "tuxdeck", version, about = "TuxDeck – Linux software for Elgato Stream Deck devices")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the daemon with the web UI (default).
    Run {
        /// Do not open the configuration UI in the browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Open the configuration UI (starts the daemon in the background if needed).
    Ui,
    /// Import icons, layouts and default profiles from the official installer
    /// (Stream_Deck_*.pkg or an extracted "Stream Deck.app").
    ImportElgato {
        source: PathBuf,
        /// Also import the official default profile for these models (e.g. 20GBJ9901 for Neo).
        #[arg(long)]
        default_profile: Vec<String>,
    },
    /// Import a .streamDeckProfile file.
    ImportProfile { file: PathBuf },
    /// Install a .streamDeckPlugin file.
    InstallPlugin { file: PathBuf },
    /// Install a .streamDeckIconPack file.
    InstallIconPack { file: PathBuf },
    /// List connected devices.
    List,
    /// Hardware test: fill keys and print input events.
    Probe {
        #[arg(default_value_t = 10)]
        seconds: u64,
    },
    /// Print udev rules for device / uinput access.
    Udev,
    /// Handle a streamdeck:// link (registered as URL handler).
    OpenUrl { url: String },
}

fn main() -> Result<()> {
    // usvg warns about every flaw in plugin SVGs (e.g. unparseable font-family values or
    // transforms), on every render: not actionable for users.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,hidapi=warn,usvg=error".into()))
        .init();
    let cli = Cli::parse();
    let paths = paths::Paths::new();
    match cli.cmd.unwrap_or(Cmd::Run { no_browser: false }) {
        Cmd::Run { no_browser } => run(paths, !no_browser),
        Cmd::Ui => open_ui(&paths),
        Cmd::ImportElgato { source, default_profile } => {
            let dst = resources::import_elgato(&paths, &source)?;
            println!("Resources imported to {}", dst.display());
            for m in default_profile {
                let f = resources::default_profile_for(&paths, &m).with_context(|| format!("no default profile for model {m}"))?;
                let p = packages::import_profile(&std::fs::read(&f)?, &paths.profiles())?;
                println!("Default profile imported: {}", p.dir.display());
            }
            Ok(())
        }
        Cmd::ImportProfile { file } => {
            // A running service imports it itself (and shows it right away).
            if let Some(r) = import_via_service(&paths, &file, "profile")? {
                println!("Profile \"{}\" imported ({})", r["name"].as_str().unwrap_or_default(), r["id"].as_str().unwrap_or_default());
                return Ok(());
            }
            // Like importing in the UI: new action ids, nothing left behind on errors.
            let mut p = packages::import_profile(&std::fs::read(&file)?, &paths.profiles())?;
            if engine::looks_imported(&p, &engine::Engine::scan_registry(&paths, &paths::Settings::load(&paths))) {
                p.set_imported(true);
                p.save_manifest()?;
            }
            println!("Profile \"{}\" imported ({})", p.manifest.name, p.id);
            Ok(())
        }
        Cmd::InstallPlugin { file } => {
            if let Some(r) = import_via_service(&paths, &file, "plugin")? {
                println!("Plugin {} installed", r["id"].as_str().unwrap_or_default());
                if r["runs_on_linux"] == false {
                    println!("It does not run on Linux: {}", r["reason"].as_str().unwrap_or_default());
                }
                return Ok(());
            }
            let id = plugins::install_archive(&std::fs::read(&file)?, &paths.user_plugins())?;
            println!("Plugin {id} installed");
            Ok(())
        }
        Cmd::InstallIconPack { file } => {
            let id = iconpacks::install_archive(&std::fs::read(&file)?, &paths.icon_packs())?;
            println!("Icon pack {id} installed");
            Ok(())
        }
        Cmd::List => {
            // A test instance with a simulated deck lists that one and leaves real
            // hardware alone (like `run`).
            if let Some(kinds) = virtual_device() {
                for kind in kinds.split(',').map(str::trim).filter(|k| !k.is_empty()) {
                    println!("{}  serial VIRTUAL-{}  firmware virtual  (simulated)", virtual_model(kind)?.name, kind.to_uppercase());
                }
                return Ok(());
            }
            let api = hidapi::HidApi::new()?;
            for info in device::enumerate(&api) {
                match device::Deck::inspect(&api, info) {
                    Ok(d) => println!("{}  serial {}  firmware {}  {}", d.model.name, short_serial(&d.serial), d.firmware, d.path),
                    Err(e) => println!("{:04x}:{:04x} {}: {e:#}", info.vendor_id(), info.product_id(), info.path().to_string_lossy()),
                }
            }
            for info in device::unsupported(&api) {
                let name = info.product_string().filter(|s| !s.is_empty()).unwrap_or("Elgato device");
                println!("{name}  {:04x}:{:04x}  not supported by TuxDeck  {}", info.vendor_id(), info.product_id(), info.path().to_string_lossy());
            }
            Ok(())
        }
        Cmd::Probe { seconds } => probe(seconds),
        Cmd::OpenUrl { url } => open_url_cmd(&paths, &url),
        Cmd::Udev => {
            print!("{}", include_str!("../packaging/70-tuxdeck.rules"));
            Ok(())
        }
    }
}

/// Hand a package to the running service (`None` if it isn't running).
fn import_via_service(paths: &paths::Paths, file: &std::path::Path, kind: &str) -> Result<Option<serde_json::Value>> {
    let port = paths::Settings::load(paths).ui_port;
    if !daemon_running(port) {
        return Ok(None);
    }
    let bytes = std::fs::read(file)?;
    // The service takes any package; this command only its own kind.
    let found = match packages::classify(&bytes, file.file_name().and_then(|f| f.to_str()))? {
        packages::Package::Plugin => "plugin",
        packages::Package::Profile => "profile",
        packages::Package::IconPack => "icon pack",
    };
    if found != kind {
        anyhow::bail!("{} is a {found}, not a {kind}", file.display());
    }
    let name = file.file_name().map(|f| f.to_string_lossy().replace(|c: char| !c.is_ascii_graphic() && c != ' ', "_")).unwrap_or_default();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let r = net::local()
            .post(format!("http://127.0.0.1:{port}/api/import"))
            .header("x-tuxdeck-token", paths.api_token()?)
            .header("x-filename", name)
            .body(bytes)
            .send()
            .await?;
        let ok = r.status().is_success();
        let v: serde_json::Value = r.json().await.unwrap_or_default();
        if ok {
            Ok(Some(v))
        } else {
            anyhow::bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or("error"))
        }
    })
}

fn daemon_running(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(300)).is_ok()
}

/// Open the UI in an app window (Chromium-based browsers) or the default browser.
fn open_ui(paths: &paths::Paths) -> Result<()> {
    let s = paths::Settings::load(paths);
    start_daemon_if_needed(s.ui_port)?;
    // Without a code (e.g. an older daemon still running) the UI explains how to get in.
    let url = match login_code(paths, s.ui_port) {
        Ok(code) => format!("http://127.0.0.1:{}/?code={code}", s.ui_port),
        Err(e) => {
            tracing::warn!("no login code: {e:#}");
            format!("http://127.0.0.1:{}/", s.ui_port)
        }
    };
    if let Some(b) = plugins::html_browser() {
        let mut c = std::process::Command::new(b);
        c.arg(format!("--app={url}")).arg("--class=tuxdeck");
        // Own browser profile, so this starts an independent browser process instead of
        // handing the window to an already-running one (which would ignore the flags below).
        c.arg(format!("--user-data-dir={}", paths.data.join("ui-browser").display()));
        // Chromium ignores --class for the Wayland app_id of an --app window (it derives one
        // from the URL), so a Wayland taskbar shows the browser's name and icon. Run the
        // window through XWayland, where --class sets WM_CLASS and the desktop entry
        // (StartupWMClass=tuxdeck) supplies the name and icon — this is the same mechanism
        // KDE, GNOME and wlroots bars use. Only when an X server is reachable (DISPLAY set,
        // i.e. a real X11 session or Wayland with XWayland); a pure-Wayland session without
        // XWayland keeps the browser's native window. Override via TUXDECK_UI_OZONE.
        let ozone = std::env::var("TUXDECK_UI_OZONE").unwrap_or_else(|_| if std::env::var_os("DISPLAY").is_some() { "x11".into() } else { String::new() });
        if !ozone.is_empty() {
            c.arg(format!("--ozone-platform={ozone}"));
        }
        if engine::spawn_detached(c).is_ok() {
            return Ok(());
        }
    }
    engine::open_url(&url)
}

/// The end of a serial number: enough to tell decks apart in `list`/`probe` output that
/// people paste into public bug reports.
fn short_serial(serial: &str) -> String {
    let n = serial.chars().count();
    if n <= 4 {
        return serial.to_string();
    }
    format!("…{}", serial.chars().skip(n - 4).collect::<String>())
}

/// A one-time login code from the running daemon (the browser must not get the API token:
/// its command line is visible to every user on the machine).
fn login_code(paths: &paths::Paths, port: u16) -> Result<String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let r = net::local().post(format!("http://127.0.0.1:{port}/api/login-code")).header("x-tuxdeck-token", paths.api_token()?).send().await?;
        let v: serde_json::Value = r.error_for_status()?.json().await?;
        v["code"].as_str().map(str::to_string).context("no login code")
    })
}

/// Exit status when the UI port is taken (see packaging/tuxdeck.service).
const EXIT_PORT_IN_USE: i32 = 3;

fn start_daemon_if_needed(port: u16) -> Result<()> {
    if daemon_running(port) {
        return Ok(());
    }
    // The installed service, if there is one: a daemon of our own would hold the port
    // and keep the service from starting later.
    let unit = std::process::Command::new("systemctl")
        .args(["--user", "start", "tuxdeck.service"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if unit
        && (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(100));
            daemon_running(port)
        })
    {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let mut c = std::process::Command::new(exe);
    c.args(["run", "--no-browser"]);
    use std::os::unix::process::CommandExt;
    c.process_group(0).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    c.spawn().context("could not start the daemon")?;
    for _ in 0..50 {
        if daemon_running(port) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("the daemon does not respond")
}

/// `tuxdeck open-url streamdeck://…`: forward the link to the daemon.
fn open_url_cmd(paths: &paths::Paths, url: &str) -> Result<()> {
    let s = paths::Settings::load(paths);
    i18n::set_language(&i18n::resolve(&s.language));
    start_daemon_if_needed(s.ui_port)?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let res: Result<serde_json::Value> = rt.block_on(async {
        let r = net::local()
            .post(format!("http://127.0.0.1:{}/api/deeplink", s.ui_port))
            .header("x-tuxdeck-token", paths.api_token()?)
            .json(&serde_json::json!({"url": url}))
            .send()
            .await?;
        let ok = r.status().is_success();
        let v: serde_json::Value = r.json().await.unwrap_or_default();
        if ok {
            Ok(v)
        } else {
            Err(anyhow::anyhow!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or("error")))
        }
    });
    let (summary, body) = match &res {
        Ok(_) => ("TuxDeck".to_string(), String::new()),
        Err(e) => (tr!("TuxDeck – error"), format!("{e:#}")),
    };
    if !body.is_empty() {
        let _ = std::process::Command::new("notify-send").args(["-a", "TuxDeck", &summary, &body]).status();
    }
    if matches!(&res, Ok(v) if v["action"] == "open_store") {
        open_ui(paths)?;
    }
    res.map(|_| ())
}

fn probe(seconds: u64) -> Result<()> {
    if virtual_device().is_some() {
        anyhow::bail!("probe tests real hardware: not with TUXDECK_VIRTUAL_DEVICE (TUXDECK_ALLOW_HARDWARE=1 allows it)");
    }
    let api = hidapi::HidApi::new()?;
    let infos = device::enumerate(&api);
    let info = infos.first().context("no Stream Deck found")?;
    // The service reads the same input and would run the keys' actions.
    let _lock = DeviceLock::acquire(&info.path().to_string_lossy())
        .context("TuxDeck is using this deck: stop it first (systemctl --user stop tuxdeck) and start it again afterwards")?;
    let deck = device::Deck::open(&api, info)?;
    println!("{} serial {} firmware {}", deck.model.name, short_serial(&deck.serial), deck.firmware);
    deck.set_brightness(70)?;
    // The Pedal has no key displays.
    for k in (0..deck.model.key_count()).filter(|_| deck.model.key_px > 0) {
        let spec = render::KeySpec { background: Some(format!("hsl({}, 70%, 35%)", k * 45)), big_text: Some((k + 1).to_string()), ..Default::default() };
        deck.set_key_image(k as u8, &render::encode_device(&render::render_key(&spec, deck.model.key_px), deck.model.key_format))?;
    }
    if let Some((w, h)) = deck.model.screen {
        let img = layout::render_info_bar(None, &layout::InfoContent::Text("TuxDeck".into()), w, h);
        deck.set_screen_image(&render::encode_device(&img, deck.model.screen_format))?;
    }
    for t in 0..deck.model.touch_points {
        deck.set_touch_point_color(t, if t == 0 { [255, 0, 0] } else { [0, 0, 255] })?;
    }
    println!("Press keys… ({seconds} s)");
    let end = std::time::Instant::now() + Duration::from_secs(seconds);
    while std::time::Instant::now() < end {
        if let Some(i) = deck.read_input(100)? {
            println!("{i:?}");
        }
    }
    deck.reset()?;
    Ok(())
}

/// Owns one deck: executes queued commands and forwards input events.
fn run_deck(deck: device::Deck, events: UnboundedSender<DevEvent>, alive: Arc<AtomicBool>) {
    let id = if deck.serial.is_empty() { deck.path.clone() } else { deck.serial.clone() };
    let (tx, rx) = std::sync::mpsc::channel::<DevCmd>();
    let _ = events.send(DevEvent::Connected { id: id.clone(), model: deck.model, firmware: deck.firmware.clone(), tx });
    'outer: loop {
        // Coalesce queued images: only the newest image per key matters.
        let mut keys: HashMap<u8, Box<render::KeySpec>> = HashMap::new();
        let mut screen = None;
        let mut strip: std::collections::BTreeMap<u16, (u16, String)> = Default::default();
        let mut other = Vec::new();
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                DevCmd::Key(k, d) => {
                    keys.insert(k, d);
                }
                DevCmd::Screen(content, layouts) => screen = Some((content, layouts)),
                DevCmd::Strip { x, w, svg } => {
                    strip.insert(x, (w, svg));
                }
                c => other.push(c),
            }
        }
        for c in other {
            let r = match c {
                DevCmd::Touch(t, rgb) => deck.set_touch_point_color(t, rgb),
                DevCmd::Brightness(b) => deck.set_brightness(b),
                DevCmd::Reset => deck.reset(),
                _ => Ok(()),
            };
            if let Err(e) = r {
                tracing::warn!("device write failed: {e:#}");
                break 'outer;
            }
        }
        let mut keys: Vec<_> = keys.into_iter().collect();
        keys.sort_by_key(|k| k.0);
        for (k, spec) in keys {
            let fmt = deck.model.key_format;
            let data = render::cached(&spec, deck.model.key_px, render::format_tag(fmt), |img| render::encode_device(img, fmt));
            if let Err(e) = deck.set_key_image(k, &data) {
                tracing::warn!("device write failed: {e:#}");
                break 'outer;
            }
        }
        // Rendering happens here, not in the engine (which holds the shared lock).
        if let (Some((content, layouts)), Some((w, h))) = (screen, deck.model.screen) {
            let img = layout::render_info_bar(layouts.as_deref(), &content, w, h);
            if let Err(e) = deck.set_screen_image(&render::encode_device_jpeg(&img)) {
                tracing::warn!("device write failed: {e:#}");
                break 'outer;
            }
        }
        let strip_h = deck.model.screen.map(|s| s.1 as u16).unwrap_or(0);
        for (x, (w, svg)) in strip {
            let data = render::encode_device(&render::rasterize(&svg, w as u32, strip_h as u32), deck.model.screen_format);
            if let Err(e) = deck.set_lcd_region(x, 0, w, strip_h, &data) {
                tracing::warn!("device write failed: {e:#}");
                break 'outer;
            }
        }
        match deck.read_input(15) {
            Ok(Some(input)) => {
                let _ = events.send(DevEvent::Input { id: id.clone(), input });
            }
            Ok(None) => {}
            Err(e) => {
                tracing::info!("device read ended: {e:#}");
                break;
            }
        }
        if !alive.load(Ordering::Relaxed) {
            let _ = deck.reset();
            break;
        }
    }
    alive.store(false, Ordering::Relaxed);
    let _ = events.send(DevEvent::Disconnected { id });
}

/// The simulated device kind (`TUXDECK_VIRTUAL_DEVICE`) when real hardware must be left
/// alone, i.e. unless `TUXDECK_ALLOW_HARDWARE` is set.
fn virtual_device() -> Option<String> {
    std::env::var("TUXDECK_VIRTUAL_DEVICE").ok().filter(|_| std::env::var_os("TUXDECK_ALLOW_HARDWARE").is_none())
}

/// The simulated model: a kind (`neo`, `original` = 1st generation, `original2`), a model
/// number (`20GAA9902`) or a USB product id (`0x006d`).
fn virtual_model(kind: &str) -> Result<&'static device::Model> {
    let k = kind.trim().to_ascii_lowercase();
    let pid = u16::from_str_radix(k.trim_start_matches("0x"), 16).ok().filter(|_| k.starts_with("0x"));
    device::MODELS
        .iter()
        .find(|m| match k.as_str() {
            "original2" => m.model_id == "20GAA9902",
            _ => format!("{:?}", m.kind).eq_ignore_ascii_case(&k) || m.model_id.eq_ignore_ascii_case(&k) || pid == Some(m.pid),
        })
        .with_context(|| {
            format!("TUXDECK_VIRTUAL_DEVICE: unknown model {kind:?} (neo, mk2, xl, plus, pedal, original, original2, mini, a model number or 0x<product id>)")
        })
}

/// Simulated deck for tests (`TUXDECK_VIRTUAL_DEVICE`, see `virtual_model`,
/// several separated by commas): accepts all device commands, input comes from the API
/// (`…/press`), `…/virtual` unplugs and plugs it in again.
fn spawn_virtual_device(kind: &str, n: usize, events: UnboundedSender<DevEvent>) {
    let model = match virtual_model(kind) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("{e:#}");
            return;
        }
    };
    let (tx, rx) = std::sync::mpsc::channel::<DevCmd>();
    let id = if n > 1 { format!("VIRTUAL-{}-{n}", kind.to_uppercase()) } else { format!("VIRTUAL-{}", kind.to_uppercase()) };
    let _ = events.send(DevEvent::Connected { id, model, firmware: "virtual".into(), tx });
    std::thread::spawn(move || {
        let (mut keys, mut screens, mut other) = (0u64, 0u64, 0u64);
        while let Ok(cmd) = rx.recv() {
            match cmd {
                DevCmd::Key(k, spec) => {
                    // Encode and packetize like a real device would, so errors show up.
                    let fmt = model.key_format;
                    let data = render::cached(&spec, model.key_px.max(1), render::format_tag(fmt), |img| render::encode_device(img, fmt));
                    let _ = device::key_image_reports(model, k, &data);
                    keys += 1
                }
                DevCmd::Screen(..) | DevCmd::Strip { .. } => screens += 1,
                _ => other += 1,
            }
            tracing::debug!("virtual device: keys={keys} screens={screens} other={other}");
        }
    });
}

/// Exclusive per-device lock (flock on a file in $XDG_RUNTIME_DIR), released
/// automatically when the owning thread or process ends.
#[allow(dead_code)] // the file is held only for its lock
struct DeviceLock(std::fs::File);

impl DeviceLock {
    fn acquire(dev_path: &str) -> Option<DeviceLock> {
        use std::os::fd::AsRawFd;
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let name = format!("tuxdeck-{}.lock", dev_path.replace(['/', ' '], "_"));
        let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(name)).ok()?;
        let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        (r == 0).then_some(DeviceLock(f))
    }
}

/// Hot-plug: periodically enumerate decks and start a thread per new device.
fn device_manager(events: UnboundedSender<DevEvent>, running: Arc<AtomicBool>, threads: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>) {
    let mut api = match hidapi::HidApi::new() {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("hidapi: {e}");
            return;
        }
    };
    let mut warned = std::collections::HashSet::new();
    while running.load(Ordering::Relaxed) {
        let _ = api.refresh_devices();
        threads.lock().retain(|_, alive| alive.load(Ordering::Relaxed));
        let infos: Vec<_> = device::enumerate(&api).into_iter().cloned().collect();
        for info in infos {
            let path = info.path().to_string_lossy().to_string();
            if threads.lock().contains_key(&path) {
                continue;
            }
            // hidraw can be opened by several processes: make sure only one
            // TuxDeck instance drives a deck.
            let lock = match DeviceLock::acquire(&path) {
                Some(l) => l,
                None => {
                    if warned.insert(format!("lock:{path}")) {
                        tracing::warn!("{path} is already used by another TuxDeck instance");
                    }
                    continue;
                }
            };
            match device::Deck::open(&api, &info) {
                Ok(deck) => {
                    warned.remove(&path);
                    let alive = Arc::new(AtomicBool::new(true));
                    threads.lock().insert(path, alive.clone());
                    let ev = events.clone();
                    std::thread::Builder::new()
                        .name("deck".into())
                        .spawn(move || {
                            let _lock = lock;
                            run_deck(deck, ev, alive)
                        })
                        .ok();
                }
                Err(e) => {
                    if warned.insert(path.clone()) {
                        tracing::warn!("{e:#}  →  `tuxdeck udev` prints the required udev rule");
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(1500));
    }
}

fn run(paths: paths::Paths, open_browser: bool) -> Result<()> {
    render::set_extra_font_dirs(resources::layout_font_dirs(&paths));
    if let Err(e) = urlhandler::register_url_handler() {
        tracing::warn!("streamdeck:// handler not registered: {e:#}");
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    engine::set_runtime(rt.handle().clone());
    rt.block_on(async move {
        let (ev_tx, _) = tokio::sync::broadcast::channel(512);
        let mut engine = Engine::new(paths, ev_tx);
        // Find out once, off the key path, whether programs can get their own scope.
        std::thread::spawn(engine::scopes_available);
        let (ui_port, sdk_port) = (engine.settings.ui_port, engine.settings.sdk_port);
        // Bind the UI port first: a second instance must fail before touching devices.
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", ui_port)).await {
            Ok(l) => l,
            // Its own exit status: the service unit doesn't restart for this
            // (RestartPreventExitStatus).
            Err(e) => {
                eprintln!("Error: UI port {ui_port} is in use ({e}) – is TuxDeck already running?");
                std::process::exit(EXIT_PORT_IN_USE);
            }
        };
        // Plugin files (property inspectors, HTML plugins, icon packs) come from their own
        // origin without the API: next to the UI port if free, else any free port.
        let wanted = match engine.settings.plugin_port {
            0 => ui_port.saturating_add(2),
            p => p,
        };
        let free = |p: u16| p != sdk_port && p != ui_port;
        let plugin_listener = match tokio::net::TcpListener::bind(("127.0.0.1", wanted)).await {
            Ok(l) if free(wanted) => l,
            _ => tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.context("no port for the plugin files")?,
        };
        let plugin_port = plugin_listener.local_addr()?.port();
        // Keep a port chosen by chance: the plugins' browser storage belongs to it.
        if plugin_port != wanted {
            tracing::warn!("plugin files on port {plugin_port} ({wanted} is not available)");
            engine.settings.plugin_port = plugin_port;
            engine.settings.save(&engine.paths);
        }
        let shared: Shared = Arc::new(Mutex::new(engine));
        shared.lock().self_ref = Some(Arc::downgrade(&shared));
        shared.lock().plugin_port = plugin_port;
        let plugin_app = peer::guard(web::plugin_router(shared.clone()), plugin_listener.local_addr()?);
        tokio::spawn(async move {
            if let Err(e) = axum::serve(plugin_listener, plugin_app).await {
                tracing::error!("plugin file server: {e:#}");
            }
        });

        // Plugin SDK server + plugin processes
        let sh = shared.clone();
        tokio::spawn(async move {
            if let Err(e) = sdk::serve(sh, sdk_port).await {
                tracing::error!("{e:#}");
                std::process::exit(1);
            }
        });
        tokio::spawn(sdk::watchdog(shared.clone()));

        smart::spawn_watcher(shared.clone());

        // Devices
        let (dtx, mut drx) = unbounded_channel::<DevEvent>();
        if let Ok(kinds) = std::env::var("TUXDECK_VIRTUAL_DEVICE") {
            // "neo,neo": several decks; the second of a kind gets the id suffix "-2".
            let mut seen: HashMap<String, usize> = HashMap::new();
            for kind in kinds.split(',').map(str::trim).filter(|k| !k.is_empty()) {
                let n = seen.entry(kind.to_ascii_lowercase()).or_default();
                *n += 1;
                spawn_virtual_device(kind, *n, dtx.clone());
            }
        }
        let running = Arc::new(AtomicBool::new(true));
        let threads: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>> = Arc::default();
        // Test instances with a simulated deck must not take over real hardware (it
        // would show the test profile and run its actions) unless explicitly allowed.
        let hardware = std::env::var_os("TUXDECK_VIRTUAL_DEVICE").is_none() || std::env::var_os("TUXDECK_ALLOW_HARDWARE").is_some();
        if hardware {
            let (r, t) = (running.clone(), threads.clone());
            std::thread::Builder::new().name("hotplug".into()).spawn(move || device_manager(dtx, r, t))?;
        } else {
            tracing::info!("simulated device: real devices are not used (TUXDECK_ALLOW_HARDWARE=1 allows it)");
            drop(dtx);
        }
        let sh = shared.clone();
        tokio::spawn(async move {
            while let Some(ev) = drx.recv().await {
                let sh = sh.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let mut e = sh.lock();
                    match ev {
                        DevEvent::Connected { id, model, firmware, tx } => e.device_connected(id, model, firmware, tx),
                        DevEvent::Disconnected { id } => e.device_disconnected(&id),
                        DevEvent::Input { id, input } => e.on_input(&id, input),
                    }
                })
                .await;
            }
        });
        // Daily automatic backup (checked hourly, kept: the last 10).
        let paths = shared.lock().paths.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(3600));
            loop {
                iv.tick().await;
                // A first start has no profile until the deck connects: a backup without
                // one would take the day's place.
                let has_profiles =
                    |p: &paths::Paths| std::fs::read_dir(p.profiles()).is_ok_and(|rd| rd.flatten().any(|e| e.path().join("manifest.json").is_file()));
                for _ in 0..600 {
                    if has_profiles(&paths) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                let p = paths.clone();
                match tokio::task::spawn_blocking(move || backup::daily(&p)).await {
                    Ok(Ok(Some(f))) => tracing::info!("daily backup: {}", f.display()),
                    Ok(Err(e)) => tracing::warn!("daily backup failed: {e:#}"),
                    _ => {}
                }
            }
        });
        // Published Linux check results (catalogue badges)
        web_plugins::spawn_check_sync(shared.clone());
        // Animated GIF keys
        let sh = shared.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(40));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                iv.tick().await;
                if sh.lock().has_animations() {
                    let sh = sh.clone();
                    let _ = tokio::task::spawn_blocking(move || sh.lock().animate()).await;
                }
            }
        });
        // Clock / timers / overlays
        let sh = shared.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(500));
            loop {
                iv.tick().await;
                let sh = sh.clone();
                let _ = tokio::task::spawn_blocking(move || sh.lock().tick()).await;
            }
        });

        // Web UI
        let url = format!("http://127.0.0.1:{ui_port}/");
        tracing::info!("configuration UI: {url} (open it with `tuxdeck ui`)");
        if open_browser {
            let _ = engine::open_url(&format!("{url}?code={}", web::login_code()));
        }
        let app = web::router(shared.clone());
        {
            // Plugins get the connected decks in their `-info` argument: give the decks a
            // moment to connect first (hardware is opened within a few hundred ms).
            let sh = shared.clone();
            tokio::spawn(async move {
                for _ in 0..20 {
                    if sh.lock().devices.values().any(|d| d.connected()) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                let sh2 = sh.clone();
                let _ = tokio::task::spawn_blocking(move || sdk::start_plugins(&sh2)).await;
                let _ = tokio::task::spawn_blocking(move || sdk::start_html_host(&sh)).await;
            });
        }
        let local = listener.local_addr()?;
        let server = axum::serve(listener, peer::guard(app, local)).with_graceful_shutdown(async {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        });
        server.await?;

        tracing::info!("shutting down…");
        running.store(false, Ordering::Relaxed);
        for alive in threads.lock().values() {
            alive.store(false, Ordering::Relaxed);
        }
        sdk::stop_plugins(&shared);
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok::<_, anyhow::Error>(())
    })?;
    // Device threads reset their deck when stopping.
    std::process::exit(0);
}
