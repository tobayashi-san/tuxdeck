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
    let Some(mut cmd) = audio_command(&path, volume, has_cmd) else {
        bail!(crate::tr!("no audio player found (pw-play, ffplay, mpv or paplay)"));
    };
    Ok(cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?)
}

/// Command line of the first available player for `path` at `volume` (0–100).
fn audio_command(path: &str, volume: f64, has: impl Fn(&str) -> bool) -> Option<Command> {
    let v = volume.clamp(0.0, 100.0);
    let mut c;
    if has("pw-play") {
        c = Command::new("pw-play");
        c.arg(format!("--volume={}", v / 100.0));
    } else if has("ffplay") {
        c = Command::new("ffplay");
        c.args(["-nodisp", "-autoexit", "-loglevel", "quiet", "-volume"]).arg(format!("{}", v as i64));
    } else if has("mpv") {
        c = Command::new("mpv");
        c.args(["--no-video", "--really-quiet"]).arg(format!("--volume={}", v as i64));
    } else if has("paplay") {
        c = Command::new("paplay");
        c.arg(format!("--volume={}", (v / 100.0 * 65536.0) as i64));
    } else {
        return None;
    }
    c.arg(path);
    Some(c)
}

pub(super) fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sd-system-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn argv(c: &Command) -> Vec<String> {
        std::iter::once(c.get_program()).chain(c.get_args()).map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn home_expansion() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_home("~"), home.to_string_lossy());
        assert_eq!(expand_home("~/Music/a b.mp3"), home.join("Music/a b.mp3").to_string_lossy());
        for p in ["/usr/bin/x", "relative/~/x", "~other/x", "", "~x"] {
            assert_eq!(expand_home(p), p);
        }
    }

    #[test]
    fn executables() {
        let d = tmp("exec");
        let (script, text) = (d.join("run.sh"), d.join("notes.txt"));
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        std::fs::write(&text, "hi").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o744)).unwrap();
        std::fs::set_permissions(&text, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(is_executable(&script));
        assert!(!is_executable(&text));
        assert!(!is_executable(&d.join("missing")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn commands_on_path() {
        assert!(has_cmd("sh"));
        assert!(!has_cmd("tuxdeck-no-such-tool-4711"));
    }

    #[test]
    fn audio_players() {
        let only = |name: &'static str| move |c: &str| c == name;
        assert_eq!(argv(&audio_command("/a.wav", 50.0, |_| true).unwrap()), ["pw-play", "--volume=0.5", "/a.wav"]);
        assert_eq!(argv(&audio_command("/a.wav", 250.0, only("pw-play")).unwrap()), ["pw-play", "--volume=1", "/a.wav"]);
        assert_eq!(
            argv(&audio_command("/a.wav", 80.6, only("ffplay")).unwrap()),
            ["ffplay", "-nodisp", "-autoexit", "-loglevel", "quiet", "-volume", "80", "/a.wav"]
        );
        assert_eq!(argv(&audio_command("/a.wav", -5.0, only("mpv")).unwrap()), ["mpv", "--no-video", "--really-quiet", "--volume=0", "/a.wav"]);
        assert_eq!(argv(&audio_command("/a.wav", 50.0, only("paplay")).unwrap()), ["paplay", "--volume=32768", "/a.wav"]);
        assert!(audio_command("/a.wav", 50.0, |_| false).is_none());
    }

    #[test]
    fn detached_processes() {
        if input::dry_run() {
            return;
        }
        assert!(spawn_detached(Command::new("true")).is_ok());
        assert!(spawn_detached(Command::new("/nonexistent/tuxdeck-test")).is_err());
    }

    /// Start a process that waits on its stdin.
    fn waiter(cmd: &mut Command) -> std::process::Child {
        for _ in 0..20 {
            match cmd.stdin(Stdio::piped()).spawn() {
                // Another test thread may still hold the fresh script open while forking.
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => std::thread::sleep(Duration::from_millis(50)),
                r => return r.unwrap(),
            }
        }
        panic!("could not start {cmd:?}");
    }

    #[test]
    fn kill_by_name() {
        if input::dry_run() {
            return;
        }
        let pid = std::process::id();
        let assert_killed = |mut child: std::process::Child| assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));

        // argv[0] file name, also of NixOS wrappers (`.name-wrapped`).
        let name = format!("tuxdeck-kill-test-{pid}");
        let child = waiter(Command::new("sh").arg0(format!("/some/bin/{name}")).args(["-c", "read x"]));
        assert_eq!(kill_processes(&format!("{name}-other"), libc::SIGTERM).unwrap(), 0);
        assert_eq!(kill_processes(&name, libc::SIGTERM).unwrap(), 1);
        assert_killed(child);
        let child = waiter(Command::new("sh").arg0(format!(".{name}-w-wrapped")).args(["-c", "read x"]));
        assert_eq!(kill_processes(&format!("{name}-w"), libc::SIGTERM).unwrap(), 1);
        assert_killed(child);

        // Process name (comm, cut to 15 characters) of a script.
        let d = tmp("kill");
        let script_name = format!("t{pid}-kill-test-script");
        let script = d.join(&script_name);
        std::fs::write(&script, "#!/bin/sh\nread x\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let child = waiter(&mut Command::new(&script));
        assert_eq!(kill_processes(&script_name, libc::SIGTERM).unwrap(), 1);
        assert_killed(child);
        let _ = std::fs::remove_dir_all(&d);
    }
}
