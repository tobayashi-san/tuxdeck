//! "Works on Linux" checks of plugins: `linux-check.mjs` (embedded) runs a plugin in an
//! isolated test instance of this app and reports what works. Results are kept per plugin
//! UUID in `linux-checks.json` and shown in the plugin catalogue.

use crate::paths::Paths;
use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const SCRIPT: &str = include_str!("linux-check.mjs");
/// A check runs a plugin for a few seconds per action; give up after this.
const TIMEOUT: Duration = Duration::from_secs(300);

pub enum Source<'a> {
    /// An installed plugin folder.
    Dir(&'a Path),
    /// A downloaded `.streamDeckPlugin`.
    Archive(&'a Path),
}

fn store_file(paths: &Paths) -> PathBuf {
    paths.data.join("linux-checks.json")
}

fn store(paths: &Paths) -> &'static Mutex<Map<String, Value>> {
    static STORE: OnceLock<Mutex<Map<String, Value>>> = OnceLock::new();
    STORE.get_or_init(|| {
        let m = std::fs::read(store_file(paths)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Mutex::new(m)
    })
}

/// Results published in the public checks repository (fetched by `fetch_remote`).
fn remote(paths: &Paths) -> &'static Mutex<Map<String, Value>> {
    static REMOTE: OnceLock<Mutex<Map<String, Value>>> = OnceLock::new();
    REMOTE.get_or_init(|| {
        let m = std::fs::read(paths.data.join("linux-checks-remote.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.get("plugins").and_then(|p| p.as_object()).cloned())
            .unwrap_or_default();
        Mutex::new(m)
    })
}

/// Local results not yet published.
static DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn running() -> &'static Mutex<HashSet<String>> {
    static RUNNING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    RUNNING.get_or_init(Default::default)
}

/// All results (by plugin UUID): this computer's and the published ones, and the checks
/// in progress.
pub fn results(paths: &Paths) -> Value {
    let running: Vec<String> = running().lock().iter().cloned().collect();
    serde_json::json!({"results": store(paths).lock().clone(), "community": remote(paths).lock().clone(), "running": running})
}

/// Short form for the catalogue list (own result first, else the published one).
pub fn summary(paths: &Paths, uuid: &str) -> Option<Value> {
    let local = store(paths).lock().get(uuid).cloned().map(|r| (r, "local"));
    let (r, source) = local.or_else(|| remote(paths).lock().get(uuid).cloned().map(|r| (r, "community")))?;
    Some(serde_json::json!({"verdict": r["verdict"], "version": r["version"], "tested_at": r["tested_at"], "source": source}))
}

/// A stored result of this computer (for skipping plugins in bulk runs).
pub fn local(paths: &Paths, uuid: &str) -> Option<Value> {
    store(paths).lock().get(uuid).cloned()
}

/// Fetch the published results (`index.json` of the checks repository).
pub async fn fetch_remote(paths: &Paths, repo: &str) -> Result<usize> {
    let url = format!("https://raw.githubusercontent.com/{repo}/main/index.json");
    let r = crate::net::http().get(&url).timeout(crate::net::SHORT).send().await?.error_for_status()?;
    let v: Value = r.json().await?;
    let plugins = v.get("plugins").and_then(|p| p.as_object()).cloned().context("no plugins in index.json")?;
    let n = plugins.len();
    let _ = crate::profile::write_atomic(&paths.data.join("linux-checks-remote.json"), &serde_json::to_vec(&v)?);
    *remote(paths).lock() = plugins;
    Ok(n)
}

pub fn mark_dirty() {
    DIRTY.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Publish this computer's results to the checks repository (GitHub contents API through
/// the `gh` CLI, which must be signed in with write access). Returns the number of
/// plugins published.
pub fn publish(paths: &Paths, repo: &str) -> Result<usize> {
    use base64::Engine as _;
    if !DIRTY.swap(false, std::sync::atomic::Ordering::Relaxed) {
        return Ok(0);
    }
    let res = (|| -> Result<usize> {
        let gh = resolve("gh");
        let api = format!("repos/{repo}/contents/index.json");
        let current = Command::new(&gh).args(["api", &api]).output()?;
        let (mut index, sha) = if current.status.success() {
            let v: Value = serde_json::from_slice(&current.stdout)?;
            let raw = v["content"].as_str().unwrap_or("").replace('\n', "");
            let content = base64::engine::general_purpose::STANDARD.decode(raw)?;
            (serde_json::from_slice::<Value>(&content)?, v["sha"].as_str().map(str::to_string))
        } else {
            (serde_json::json!({"format": 1, "plugins": {}}), None)
        };
        let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
        let local = store(paths).lock().clone();
        let plugins = index["plugins"].as_object_mut().context("bad index.json")?;
        let mut n = 0;
        // Results marked `local_only` stay on this computer.
        for (uuid, r) in local.into_iter().filter(|(_, r)| r["local_only"] != true) {
            let newer = plugins.get(&uuid).and_then(|p| p["tested_at"].as_str()).is_none_or(|t| r["tested_at"].as_str().is_some_and(|l| l > t));
            if newer {
                plugins.insert(uuid, sanitize(&r, &home));
                n += 1;
            }
        }
        if n == 0 {
            return Ok(0);
        }
        index["updated"] = Value::String(chrono::Utc::now().to_rfc3339());
        let mut body = serde_json::json!({
            "message": format!("Linux checks: {n} plugin(s)"),
            "content": base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec_pretty(&index)?),
        });
        if let Some(sha) = sha {
            body["sha"] = Value::String(sha);
        }
        let mut child = Command::new(&gh)
            .args(["api", "--method", "PUT", &api, "--input", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        std::io::Write::write_all(child.stdin.as_mut().context("stdin")?, &serde_json::to_vec(&body)?)?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!("gh api: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        *remote(paths).lock() = index["plugins"].as_object().cloned().unwrap_or_default();
        Ok(n)
    })();
    if res.is_err() {
        mark_dirty();
    }
    res
}

/// Only what's needed for the badge and the report; no local paths.
fn sanitize(r: &Value, home: &str) -> Value {
    let clean = |s: &str| {
        let mut s = s.to_string();
        if !home.is_empty() {
            s = s.replace(home, "~");
        }
        // Temporary test folders.
        while let Some(i) = s.find("/tmp/sd-linux-check-") {
            let end = s[i..].find([' ', '\'', '"', ')']).map(|e| i + e).unwrap_or(s.len());
            s.replace_range(i..end, "…");
        }
        s
    };
    let checks: Vec<Value> = r["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| serde_json::json!({"id": c["id"], "ok": c["ok"], "detail": clean(c["detail"].as_str().unwrap_or(""))}))
        .collect();
    let mut out = serde_json::json!({"name": r["name"], "version": r["version"], "verdict": r["verdict"], "tested_at": r["tested_at"],
                                     "app_version": r["app_version"], "sandboxed": r["sandboxed"], "checks": checks});
    for k in ["source", "release"] {
        if !r[k].is_null() {
            out[k] = r[k].clone();
        }
    }
    out
}

pub fn is_running(key: &str) -> bool {
    running().lock().contains(key)
}

/// Mark a check as started (`false` if one is already running for `key`).
pub fn begin(key: &str) -> bool {
    running().lock().insert(key.to_string())
}

pub fn end(key: &str) {
    running().lock().remove(key);
}

/// `TUXDECK_NO_SANDBOX` (tests, development): no sandbox, and plugins that are not
/// installed are started for the check too.
fn no_sandbox() -> bool {
    std::env::var_os("TUXDECK_NO_SANDBOX").is_some_and(|v| !v.is_empty())
}

/// bubblewrap for sandboxing the plugin under test: `TUXDECK_BWRAP`, `bwrap` on the
/// PATH or `<data>/tools/bwrap-root/bin/bwrap` (e.g. a Nix GC root).
fn bwrap(paths: &Paths) -> Option<PathBuf> {
    // `TUXDECK_BWRAP=none`: as if there were none (tests).
    if no_sandbox() || std::env::var_os("TUXDECK_BWRAP").is_some_and(|v| v == "none") {
        return None;
    }
    let candidates = std::env::var_os("TUXDECK_BWRAP")
        .map(PathBuf::from)
        .into_iter()
        .chain(std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).map(|d| d.join("bwrap")).collect::<Vec<_>>()))
        .chain([paths.data.join("tools/bwrap-root/bin/bwrap")]);
    candidates.into_iter().find(|p| p.is_file())
}

/// System folders the check needs (programs, libraries, Node, the test browser). Everything
/// else – the user's files, other drives (/mnt, /media, /run/media), /srv, the D-Bus
/// sockets – stays outside the sandbox.
const SYSTEM_DIRS: &[&str] =
    &["/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/opt", "/nix/store", "/run/current-system", "/run/opengl-driver", "/sys"];

/// bubblewrap arguments: system folders and `binds` read-only, own network (only its
/// own loopback: the test instances, the plugin and the browser talk to each other, but
/// nothing on the host – like a running TuxDeck – or the internet is reachable), own
/// IPC, hostname and process namespaces.
fn sandbox_args(binds: &[PathBuf]) -> Vec<std::ffi::OsString> {
    // Only the environment the check needs (no tokens or other secrets of the session).
    let mut a: Vec<std::ffi::OsString> = vec!["--clearenv".into()];
    for k in ["PATH", "LANG", "TUXDECK_BROWSER"] {
        if let Some(v) = std::env::var_os(k) {
            a.extend(["--setenv".into(), k.into(), v]);
        }
    }
    for d in SYSTEM_DIRS.iter().map(Path::new).filter(|d| d.exists()) {
        a.extend(["--ro-bind".into(), d.into(), d.into()]);
    }
    for s in [
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/run/user/sandbox",
        "--setenv",
        "XDG_RUNTIME_DIR",
        "/run/user/sandbox",
        "--setenv",
        "HOME",
        "/tmp",
        "--unsetenv",
        "WAYLAND_DISPLAY",
        "--unsetenv",
        "DISPLAY",
        "--unsetenv",
        "DBUS_SESSION_BUS_ADDRESS",
        "--unsetenv",
        "DBUS_SYSTEM_BUS_ADDRESS",
        "--unshare-net",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-pid",
        "--unshare-cgroup-try",
        "--die-with-parent",
        "--new-session",
    ] {
        a.push(s.into());
    }
    for b in binds {
        a.extend(["--ro-bind".into(), b.into(), b.into()]);
    }
    a
}

/// A program on the PATH as a real path (symlinks resolved, e.g. into /nix/store), so it
/// stays visible in the sandbox where /home is hidden.
fn resolve(program: &str) -> PathBuf {
    let found = if program.contains('/') {
        Some(PathBuf::from(program))
    } else {
        std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(program)).find(|p| p.is_file()))
    };
    found.and_then(|p| p.canonicalize().ok()).unwrap_or_else(|| PathBuf::from(program))
}

/// Run the check (blocking, up to a few minutes) and store the result. The plugin runs in
/// a bubblewrap sandbox (see `sandbox_args`) if bubblewrap is there.
pub fn run(paths: &Paths, node: &str, source: Source, key: &str, extra: Value) -> Result<Value> {
    let tools = paths.data.join("tools");
    std::fs::create_dir_all(&tools)?;
    let script = tools.join("linux-check.mjs");
    std::fs::write(&script, SCRIPT)?;
    let exe = std::env::current_exe()?.canonicalize()?;
    let node = resolve(node);
    let elgato = paths.elgato().canonicalize().ok().filter(|p| p.is_dir());
    let (flag, plugin) = match source {
        Source::Dir(d) => ("--plugin-dir", d.canonicalize()?),
        Source::Archive(f) => ("--plugin", f.canonicalize()?),
    };
    // With bubblewrap the plugin runs in a sandbox. Without it, an installed plugin is
    // tested in the isolated test instances only (it runs with the user's rights anyway
    // once used); a plugin that is not installed is never started, only its files are
    // checked (or on explicit request: tests, development).
    let sandbox = bwrap(paths);
    if sandbox.is_none() && matches!(source, Source::Archive(_)) && !no_sandbox() {
        let Source::Archive(file) = source else { unreachable!() };
        return static_check(paths, file, key, extra);
    }
    let mut cmd = match &sandbox {
        Some(bw) => {
            let mut binds = vec![exe.clone(), script.clone(), plugin.clone(), node.clone()];
            binds.extend(elgato.clone());
            // Debug builds read the UI from the source tree.
            if cfg!(debug_assertions) {
                binds.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/ui")));
            }
            let mut c = Command::new(bw);
            // The test instances start the plugin with this Node (the PATH may lead
            // through folders that are not in the sandbox).
            c.args(sandbox_args(&binds)).arg("--setenv").arg("TUXDECK_NODE").arg(&node).arg(&node);
            c
        }
        None => {
            // Only what the check needs from the session (no tokens or other secrets),
            // and a HOME of its own; the test instances get their own HOME and data too.
            let mut c = Command::new(&node);
            c.env_clear();
            for k in ["PATH", "LANG", "TUXDECK_BROWSER"] {
                if let Some(v) = std::env::var_os(k) {
                    c.env(k, v);
                }
            }
            c.env("HOME", std::env::temp_dir()).env("TUXDECK_NODE", &node);
            c
        }
    };
    cmd.arg(&script).arg("--bin").arg(&exe).arg(flag).arg(&plugin);
    if let Some(e) = &elgato {
        cmd.arg("--elgato").arg(e);
    }
    use std::os::unix::process::CommandExt;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context(crate::tr!("Node.js is needed for the Linux check"))?;
    let mut stdout = child.stdout.take().context("no output")?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    let start = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if start.elapsed() > TIMEOUT {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            bail!(crate::tr!("The Linux check took too long and was stopped."));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let out = reader.join().map_err(|_| anyhow!("reader failed"))?;
    let mut report: Value = out.lines().rev().find_map(|l| serde_json::from_str(l).ok()).with_context(|| crate::tr!("the Linux check gave no result"))?;
    report["sandboxed"] = Value::Bool(sandbox.is_some());
    // Where the package came from and its release (to test updates again).
    if let Value::Object(x) = extra {
        for (k, v) in x {
            report[k.as_str()] = v;
        }
    }
    report["app_version"] = Value::String(env!("CARGO_PKG_VERSION").into());
    if report["verdict"] == "error" {
        // The check script's own failures, in the user's language.
        let msg = match report["error"].as_str().unwrap_or("") {
            "the test instance did not start" => crate::tr!("the test instance did not start"),
            "the plugin was not installed" => crate::tr!("the plugin could not be installed in the test instance"),
            "" => crate::tr!("the Linux check failed"),
            other => other.to_string(),
        };
        bail!("{msg}");
    }
    report["uuid"].as_str().with_context(|| crate::tr!("the Linux check gave no result"))?;
    // Stored under the plugin or catalogue entry that was checked, not under the UUID the
    // package claims (it could claim another plugin's).
    let mut s = store(paths).lock();
    s.insert(key.to_string(), report.clone());
    mark_dirty();
    let _ = crate::profile::write_atomic(&store_file(paths), &serde_json::to_vec_pretty(&*s)?);
    Ok(report)
}

/// The check of a plugin that is not installed, without bubblewrap: its files only (is
/// its program one that runs on Linux), nothing is started. Kept on this computer.
fn static_check(paths: &Paths, file: &Path, key: &str, extra: Value) -> Result<Value> {
    let scratch = paths.data.join("tools").join(format!("inspect-{}", uuid::Uuid::new_v4().simple()));
    let plugin = std::fs::read(file).map_err(anyhow::Error::from).and_then(|b| crate::plugins::inspect_archive(&b, &scratch));
    let _ = std::fs::remove_dir_all(&scratch);
    let plugin = plugin?;
    let (verdict, check) = match &plugin.runtime {
        crate::plugins::Runtime::Unsupported(why) => ("unsupported", serde_json::json!({"id": "linux_runtime", "ok": false, "detail": why})),
        _ => ("static", serde_json::json!({"id": "linux_runtime", "ok": true})),
    };
    let mut report = serde_json::json!({
        "uuid": plugin.uuid, "name": plugin.name(), "version": plugin.manifest.version, "verdict": verdict,
        "checks": [check], "static": true, "sandboxed": false, "local_only": true,
        "tested_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "app_version": env!("CARGO_PKG_VERSION"),
    });
    if let Value::Object(x) = extra {
        for (k, v) in x {
            report[k.as_str()] = v;
        }
    }
    let mut s = store(paths).lock();
    s.insert(key.to_string(), report.clone());
    let _ = crate::profile::write_atomic(&store_file(paths), &serde_json::to_vec_pretty(&*s)?);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_is_offline_and_hides_the_users_files() {
        let a: Vec<String> = sandbox_args(&[PathBuf::from("/home/u/plugin")]).iter().map(|x| x.to_string_lossy().into_owned()).collect();
        for flag in ["--unshare-net", "--unshare-ipc", "--unshare-pid", "--die-with-parent"] {
            assert!(a.iter().any(|x| x == flag), "{flag}");
        }
        let bound: Vec<&str> = a.windows(3).filter(|w| w[0] == "--ro-bind").map(|w| w[1].as_str()).collect();
        assert!(bound.contains(&"/home/u/plugin"));
        for hidden in ["/", "/home", "/mnt", "/media", "/run/media", "/srv", "/run/dbus", "/run/user", "/var"] {
            assert!(!bound.contains(&hidden), "{hidden} must not be visible");
        }
    }

    /// A `.streamDeckPlugin` whose program is `code`.
    fn plugin_zip(uuid: &str, code: &str) -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file(format!("{uuid}.sdPlugin/manifest.json"), o).unwrap();
            let m = serde_json::json!({"Name": "Static", "UUID": uuid, "Version": "1.2", "Author": "t", "SDKVersion": 2, "CodePath": code, "Icon": "i",
                                       "OS": [{"Platform": "mac", "MinimumVersion": "10.15"}], "Actions": [{"UUID": format!("{uuid}.a"), "Name": "A", "Icon": "i", "States": [{"Image": "i"}]}]});
            z.write_all(m.to_string().as_bytes()).unwrap();
            z.start_file(format!("{uuid}.sdPlugin/{code}"), o).unwrap();
            z.write_all(b"MZ").unwrap();
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn plugins_that_are_not_installed_get_their_files_checked_without_bubblewrap() {
        let dir = std::env::temp_dir().join(format!("sd-linuxcheck-static-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = Paths { data: dir.join("data"), config: dir.join("config") };
        std::fs::create_dir_all(&paths.data).unwrap();
        for (uuid, code, verdict, ok) in [("com.t.node", "plugin.js", "static", true), ("com.t.win", "plugin.exe", "unsupported", false)] {
            let file = dir.join(format!("{uuid}.streamDeckPlugin"));
            std::fs::write(&file, plugin_zip(uuid, code)).unwrap();
            let r = static_check(&paths, &file, &format!("cat:{uuid}"), serde_json::json!({"source": "openaction"})).unwrap();
            assert_eq!((r["uuid"].as_str(), r["verdict"].as_str(), r["version"].as_str()), (Some(uuid), Some(verdict), Some("1.2")));
            assert_eq!(r["checks"][0]["ok"], ok, "{r}");
            // Not a test that ran: stays on this computer.
            assert_eq!((r["static"].as_bool(), r["local_only"].as_bool(), r["source"].as_str()), (Some(true), Some(true), Some("openaction")));
            assert_eq!(store(&paths).lock()[&format!("cat:{uuid}")]["verdict"], verdict);
        }
        // Nothing unpacked is left behind.
        assert!(std::fs::read_dir(paths.data.join("tools")).map(|d| d.count()).unwrap_or(0) == 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn published_results_contain_no_local_paths() {
        let r = serde_json::json!({
            "name": "X", "version": "1.0", "verdict": "issues", "tested_at": "2026-10-28T12:00:00Z", "uuid": "com.x",
            "checks": [{"id": "log", "ok": false, "detail": "Error: ENOENT open '/tmp/sd-linux-check-AbC12/neo-2/data/x.json' in /home/someone/.config"}],
            "error": "internal", "duration_ms": 1234, "source": "openaction", "release": "v1.0", "local_only": false,
        });
        let out = sanitize(&r, "/home/someone");
        let detail = out["checks"][0]["detail"].as_str().unwrap();
        assert!(!detail.contains("/tmp/sd-linux-check-") && !detail.contains("/home/someone"), "{detail}");
        assert!(detail.contains("ENOENT") && detail.contains("~/.config"), "{detail}");
        // Only the fields the badge and the report need.
        assert!(out.get("duration_ms").is_none() && out.get("error").is_none());
        assert!(out.get("local_only").is_none());
        assert_eq!((out["release"].as_str(), out["verdict"].as_str()), (Some("v1.0"), Some("issues")));
    }
}
