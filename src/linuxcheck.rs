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
    let r = crate::net::http().get(&url).send().await?.error_for_status()?;
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

/// bubblewrap for sandboxing the plugin under test: `TUXDECK_BWRAP`, `bwrap` on the
/// PATH or `<data>/tools/bwrap-root/bin/bwrap` (e.g. a Nix GC root).
fn bwrap(paths: &Paths) -> Option<PathBuf> {
    if std::env::var_os("TUXDECK_NO_SANDBOX").is_some() {
        return None;
    }
    let candidates = std::env::var_os("TUXDECK_BWRAP")
        .map(PathBuf::from)
        .into_iter()
        .chain(std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).map(|d| d.join("bwrap")).collect::<Vec<_>>()))
        .chain([paths.data.join("tools/bwrap-root/bin/bwrap")]);
    candidates.into_iter().find(|p| p.is_file())
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

/// Run the check (blocking, up to a few minutes) and store the result. With bubblewrap
/// the plugin can't see the user's files: /home and /run/user are empty in there, only
/// what the check needs is mounted read-only.
pub fn run(paths: &Paths, node: &str, source: Source, extra: Value) -> Result<Value> {
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
    let sandbox = bwrap(paths);
    let mut cmd = match &sandbox {
        Some(bw) => {
            let mut c = Command::new(bw);
            c.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp", "--tmpfs", "/home", "--tmpfs", "/run/user"]);
            c.args(["--dir", "/run/user/sandbox", "--setenv", "XDG_RUNTIME_DIR", "/run/user/sandbox", "--setenv", "HOME", "/tmp"]);
            c.args(["--unsetenv", "WAYLAND_DISPLAY", "--unsetenv", "DISPLAY", "--unsetenv", "DBUS_SESSION_BUS_ADDRESS"]);
            c.args(["--unshare-pid", "--die-with-parent", "--new-session"]);
            let mut binds = vec![exe.clone(), script.clone(), plugin.clone(), node.clone()];
            binds.extend(elgato.clone());
            // Debug builds read the UI from the source tree.
            if cfg!(debug_assertions) {
                binds.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/ui")));
            }
            for b in binds {
                c.arg("--ro-bind").arg(&b).arg(&b);
            }
            c.arg(&node);
            c
        }
        None => Command::new(&node),
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
    let mut report: Value = out.lines().rev().find_map(|l| serde_json::from_str(l).ok()).context("the Linux check gave no result")?;
    report["sandboxed"] = Value::Bool(sandbox.is_some());
    // Where the package came from and its release (to test updates again).
    if let Value::Object(x) = extra {
        for (k, v) in x {
            report[k.as_str()] = v;
        }
    }
    report["app_version"] = Value::String(env!("CARGO_PKG_VERSION").into());
    if report["verdict"] == "error" {
        bail!("{}", report["error"].as_str().unwrap_or("Linux check failed"));
    }
    let uuid = report["uuid"].as_str().context("no plugin UUID in the result")?.to_string();
    let mut s = store(paths).lock();
    s.insert(uuid, report.clone());
    mark_dirty();
    let _ = crate::profile::write_atomic(&store_file(paths), &serde_json::to_vec_pretty(&*s)?);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

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
