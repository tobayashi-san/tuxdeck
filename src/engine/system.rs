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

/// Called with the exit status of a watched process and how long it ran.
pub type OnExit = Box<dyn FnOnce(std::process::ExitStatus, Duration) + Send>;

/// A program started for the user. Quitting it later signals exactly this program
/// (its scope or process group), not every process that happens to share its name.
#[derive(Clone, Debug)]
pub struct Launched {
    pub pid: u32,
    /// The transient systemd scope it runs in.
    pub unit: Option<String>,
    /// Start time of the process (`/proc/<pid>/stat`), to recognise a reused pid.
    pub started: Option<u64>,
}

/// Start time of a process in clock ticks since boot.
fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // After the command name in parentheses: field 22 is the start time.
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

/// Spawn a process without leaving zombies behind.
pub fn spawn_detached(cmd: Command) -> Result<()> {
    spawn_watched(cmd, None).map(drop)
}

/// Spawn a process without leaving zombies behind and hand its exit status to `on_exit`.
/// Returns the process id (`None` in dry-run mode).
fn spawn_watched(mut cmd: Command, on_exit: Option<OnExit>) -> Result<Option<u32>> {
    if input::dry_run() {
        tracing::info!("DRYRUN exec {cmd:?}");
        return Ok(None);
    }
    let started = Instant::now();
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    let pid = child.id();
    std::thread::spawn(move || {
        if let (Ok(status), Some(f)) = (child.wait(), on_exit) {
            f(status, started.elapsed());
        }
    });
    Ok(Some(pid))
}

/// Start a program for the user (Open, Open Application, links) and report how it ended.
/// Under the systemd user service it gets its own transient scope: otherwise it would
/// stay in tuxdeck.service's cgroup and systemd would kill it whenever the service
/// restarts or stops.
pub fn launch(mut cmd: Command, on_exit: Option<OnExit>) -> Result<Option<Launched>> {
    if scopes_available() {
        let (c, unit) = in_scope(&cmd);
        return Ok(spawn_watched(c, on_exit)?.map(|pid| Launched { pid, unit: Some(unit), started: start_time(pid) }));
    }
    // Its own process group, so quitting it also reaches the programs it started.
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    Ok(spawn_watched(cmd, on_exit)?.map(|pid| Launched { pid, unit: None, started: start_time(pid) }))
}

/// Signal a program started by [`launch`] and everything it started. False if it is gone.
pub fn quit_launched(l: &Launched, signal: i32) -> bool {
    match &l.unit {
        Some(unit) => Command::new("systemctl")
            .args(["--user", "kill", "--quiet"])
            .arg(format!("--signal={signal}"))
            .arg(unit)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success()),
        // The pid stays taken while the group has members. A process with that pid that
        // started later is another program: the group is gone.
        None => {
            if start_time(l.pid).is_some_and(|t| l.started != Some(t)) {
                return false;
            }
            (unsafe { libc::kill(-(l.pid as i32), signal) }) == 0
        }
    }
}

/// Why a program that was started for a key ended right away, or `None` if it didn't fail.
pub fn exit_problem(program: &str, status: std::process::ExitStatus, ran: Duration) -> Option<String> {
    if status.success() || ran > Duration::from_secs(5) {
        return None;
    }
    Some(match status.code()? {
        3 | 4 if program.starts_with("xdg-open ") => {
            crate::tr!("no application is set up to open {target}", target = program.trim_start_matches("xdg-open "))
        }
        127 => crate::tr!("{program}: command not found", program = program),
        126 => crate::tr!("{program} is not executable", program = program),
        code => crate::tr!("{program} exited with code {code}", program = program, code = code),
    })
}

/// Whether `systemd-run --user --scope` works here (checked once).
pub fn scopes_available() -> bool {
    static SCOPES: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SCOPES.get_or_init(|| {
        let ok = std::env::var_os("INVOCATION_ID").is_some()
            && !input::dry_run()
            && Command::new("systemd-run")
                .args(["--user", "--scope", "--collect", "--quiet", "--", "true"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
        tracing::info!("programs start in their own systemd scope: {ok}");
        ok
    })
}

/// `cmd` wrapped in `systemd-run --user --scope` (unit `app-tuxdeck-<program>-<random>.scope`,
/// named like the desktop launchers do).
fn in_scope(cmd: &Command) -> (Command, String) {
    let program = std::path::Path::new(cmd.get_program()).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    // `sh -c "<command line>"`: name the scope after the command line's program.
    let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    let shown = match (program.as_str(), args.first().map(String::as_str)) {
        ("sh", Some("-c")) => match args.get(1).and_then(|l| l.split_whitespace().find(|w| *w != "exec")) {
            // `exec "$0" <args>` with the program path as $0 (Open Application).
            Some("\"$0\"") => args.get(2).cloned().unwrap_or_default(),
            Some(w) => w.to_string(),
            None => "sh".to_string(),
        },
        _ => program,
    };
    let name: String = std::path::Path::new(shown.trim_matches('"'))
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '.' { c } else { '_' })
        .take(40)
        .collect();
    let unit = format!("app-tuxdeck-{}-{}.scope", if name.is_empty() { "program" } else { &name }, &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let mut c = Command::new("systemd-run");
    c.args(["--user", "--scope", "--collect", "--quiet"]).arg(format!("--unit={unit}")).arg("--").arg(cmd.get_program()).args(cmd.get_args());
    for (k, v) in cmd.get_envs() {
        match v {
            Some(v) => c.env(k, v),
            None => c.env_remove(k),
        };
    }
    if let Some(d) = cmd.get_current_dir() {
        c.current_dir(d);
    }
    (c, unit)
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
    launch(url_opener(url), None).map(drop)
}

/// `xdg-open <url>` (a browser that isn't running yet becomes its child, so it is
/// started like a program).
pub(super) fn url_opener(url: &str) -> Command {
    let mut c = Command::new("xdg-open");
    c.arg(url);
    c
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
    fn quick_failures() {
        let exit = |code: i32| std::process::ExitStatus::from_raw(code << 8);
        let quick = Duration::from_millis(200);
        assert_eq!(exit_problem("nope", exit(127), quick).as_deref(), Some("nope: command not found"));
        assert_eq!(exit_problem("x.sh", exit(126), quick).as_deref(), Some("x.sh is not executable"));
        assert_eq!(exit_problem("xdg-open mailto:", exit(3), quick).as_deref(), Some("no application is set up to open mailto:"));
        assert_eq!(exit_problem("tool", exit(2), quick).as_deref(), Some("tool exited with code 2"));
        assert_eq!(exit_problem("tool", exit(0), quick), None);
        assert_eq!(exit_problem("tool", exit(1), Duration::from_secs(60)), None);
        // Killed by a signal (quit from the deck): not an error.
        assert_eq!(exit_problem("tool", std::process::ExitStatus::from_raw(libc::SIGTERM), quick), None);
    }

    #[test]
    fn launched_programs_quit_with_their_group() {
        if input::dry_run() || scopes_available() {
            return;
        }
        let mut c = Command::new("sh");
        c.args(["-c", "sleep 30 & wait"]);
        let (tx, rx) = std::sync::mpsc::channel();
        let l = launch(c, Some(Box::new(move |st, _| tx.send(st).unwrap()))).unwrap().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(quit_launched(&l, libc::SIGTERM));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap().signal(), Some(libc::SIGTERM));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!quit_launched(&l, libc::SIGTERM));
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

    #[test]
    fn programs_get_a_named_scope() {
        let argv = |c: &Command| std::iter::once(c.get_program()).chain(c.get_args()).map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
        let mut sh = Command::new("sh");
        sh.arg("-c").arg("exec \"$0\" --x").arg("/opt/My App/obs");
        sh.env("A", "1");
        let (c, unit) = in_scope(&sh);
        let a = argv(&c);
        assert_eq!(a[5], format!("--unit={unit}"));
        assert_eq!(a[..5], ["systemd-run", "--user", "--scope", "--collect", "--quiet"]);
        assert!(a[5].starts_with("--unit=app-tuxdeck-obs-") && a[5].ends_with(".scope"), "{}", a[5]);
        assert_eq!(a[6..], ["--", "sh", "-c", "exec \"$0\" --x", "/opt/My App/obs"]);
        assert!(c.get_envs().any(|(k, v)| k == "A" && v == Some("1".as_ref())));
        let mut sh = Command::new("sh");
        sh.arg("-c").arg("flatpak run org.x.Y --new");
        assert!(argv(&in_scope(&sh).0)[5].starts_with("--unit=app-tuxdeck-flatpak-"));
        assert!(argv(&in_scope(&Command::new("/usr/bin/my prog!")).0)[5].starts_with("--unit=app-tuxdeck-my_prog_-"));
    }

    /// Start a process that waits on its stdin, once it runs its own program (right after
    /// the fork it still is a copy of the test binary, which the name search won't find).
    fn waiter(cmd: &mut Command) -> std::process::Child {
        for _ in 0..20 {
            match cmd.stdin(Stdio::piped()).spawn() {
                // Another test thread may still hold the fresh script open while forking.
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => std::thread::sleep(Duration::from_millis(50)),
                r => {
                    let child = r.unwrap();
                    let me = std::fs::read_link("/proc/self/exe").ok();
                    for _ in 0..200 {
                        if std::fs::read_link(format!("/proc/{}/exe", child.id())).ok() != me {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    return child;
                }
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
