//! Processes and the async runtime: spawning, signalling, opening URLs, audio.

use super::*;

pub(super) static RUNTIME: std::sync::OnceLock<tokio::runtime::Handle> = std::sync::OnceLock::new();

/// Register the daemon's runtime, so async work can be started from plain threads too
/// (device readers, Key Logic timers).
pub fn set_runtime(handle: tokio::runtime::Handle) {
    let _ = RUNTIME.set(handle);
}

/// `tokio::spawn` that works from any thread.
pub fn spawn_task(f: impl std::future::Future<Output = ()> + Send + 'static) {
    match RUNTIME.get().cloned().or_else(|| tokio::runtime::Handle::try_current().ok()) {
        Some(h) => drop(h.spawn(f)),
        None => tracing::error!("no async runtime to run the task on"),
    }
}

/// Spawn a process without leaving zombies behind.
pub fn spawn_detached(mut cmd: Command) -> Result<()> {
    if input::dry_run() {
        tracing::info!("DRYRUN exec {cmd:?}");
        return Ok(());
    }
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Signal every process of the given program (like `pkill -x`, but also for names
/// longer than 15 characters and for NixOS-wrapped programs, whose process name is
/// `.name-wrapped`). Matches the process name or the file name of argv[0].
pub fn kill_processes(name: &str, signal: i32) -> Result<usize> {
    if input::dry_run() {
        tracing::info!("DRYRUN kill -{signal} {name}");
        return Ok(0);
    }
    let me = std::process::id();
    let wrapped = format!(".{name}-wrapped");
    let short = |n: &str| n.chars().take(15).collect::<String>();
    let mut n = 0;
    for entry in std::fs::read_dir("/proc")?.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|p| p.parse::<u32>().ok()) else { continue };
        if pid == me {
            continue;
        }
        let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        let comm = comm.trim_end();
        let argv0 = std::fs::read(entry.path().join("cmdline")).unwrap_or_default();
        let argv0 = argv0.split(|b| *b == 0).next().map(|a| String::from_utf8_lossy(a).into_owned()).unwrap_or_default();
        let base = std::path::Path::new(&argv0).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        let matches = base == name || base == wrapped || comm == short(name) || comm == short(&wrapped);
        if matches && unsafe { libc::kill(pid as i32, signal) } == 0 {
            n += 1;
        }
    }
    Ok(n)
}

pub fn open_url(url: &str) -> Result<()> {
    let mut c = Command::new("xdg-open");
    c.arg(url);
    spawn_detached(c)
}

pub(super) fn expand_home(p: &str) -> String {
    if p == "~" {
        if let Some(h) = dirs::home_dir() {
            return h.to_string_lossy().into_owned();
        }
    }
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest).to_string_lossy().into_owned();
        }
    }
    p.to_string()
}

pub(super) fn has_cmd(name: &str) -> bool {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file())).unwrap_or(false)
}

pub(super) fn play_audio(path: &str, volume: f64) -> Result<Child> {
    let path = expand_home(path);
    if input::dry_run() {
        tracing::info!("DRYRUN play_audio {path} volume={volume}");
        return Ok(Command::new("sleep").arg("3").stdin(Stdio::null()).spawn()?);
    }
    let v = volume.clamp(0.0, 100.0);
    let mut cmd = if has_cmd("pw-play") {
        let mut c = Command::new("pw-play");
        c.arg(format!("--volume={}", v / 100.0)).arg(&path);
        c
    } else if has_cmd("ffplay") {
        let mut c = Command::new("ffplay");
        c.args(["-nodisp", "-autoexit", "-loglevel", "quiet", "-volume"]).arg(format!("{}", v as i64)).arg(&path);
        c
    } else if has_cmd("mpv") {
        let mut c = Command::new("mpv");
        c.args(["--no-video", "--really-quiet"]).arg(format!("--volume={}", v as i64)).arg(&path);
        c
    } else if has_cmd("paplay") {
        let mut c = Command::new("paplay");
        c.arg(format!("--volume={}", (v / 100.0 * 65536.0) as i64)).arg(&path);
        c
    } else {
        bail!(crate::tr!("no audio player found (pw-play, ffplay, mpv or paplay)"));
    };
    Ok(cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?)
}

pub(super) fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}
