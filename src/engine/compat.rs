//! Imported profiles on Linux: the report behind "Make this profile work on Linux", the
//! replacements it offers (rules in `crate::compat`) and the Linux counterpart of
//! applications from Windows and macOS profiles (`crate::apps`).

use super::*;
use crate::apps::{self, Target};
use crate::compat::{self, SpotifyCmd, Status, To};

/// Visit every action instance in `v`, including those inside multi actions.
fn walk(v: &mut Value, f: &mut dyn FnMut(&mut Map<String, Value>)) {
    match v {
        Value::Object(o) => {
            if o.get("UUID").is_some_and(Value::is_string) {
                f(o);
            }
            for (_, x) in o.iter_mut() {
                walk(x, f);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| walk(x, f)),
        _ => {}
    }
}

/// Title of the current state of a serialized action instance.
fn title_of(o: &Map<String, Value>) -> String {
    let state = o.get("State").and_then(Value::as_u64).unwrap_or(0) as usize;
    let states = o.get("States").and_then(Value::as_array);
    let pick = |i: usize| states.and_then(|s| s.get(i)).and_then(|s| s.get("Title")).and_then(Value::as_str).filter(|t| !t.trim().is_empty());
    pick(state).or_else(|| pick(0)).unwrap_or("").to_string()
}

/// The program an Open / Open Application action points to.
fn open_path(uuid: &str, settings: &Value) -> Option<String> {
    let s = |k: &str| settings.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match uuid {
        OPEN => Some(s("path")),
        OPEN_APP => Some(if s("exec").is_empty() { s("app_name") } else { s("exec") }),
        _ => None,
    }
}

/// An Open action that needs the Linux counterpart: a Windows/macOS path, or no path but
/// a title naming the application.
pub fn needs_counterpart(path: &str, title: &str) -> bool {
    apps::is_foreign(path) || (path.trim().trim_matches('"').is_empty() && !title.trim().is_empty())
}

impl Engine {
    /// What a profile needs to work on Linux: missing plugins grouped by what can be done
    /// about them, and the applications its Open keys start here.
    pub fn compat_report(&self, profile: &str, catalogue: &[crate::catalogue::Entry]) -> Result<Value> {
        let p = self.profiles.get(profile).with_context(|| crate::tr!("profile not found"))?;
        let mut groups: Vec<Value> = Vec::new();
        let mut apps_out: Vec<Value> = Vec::new();
        let mut page_ids: Vec<&String> = p.pages.keys().collect();
        page_ids.sort();
        for page_id in page_ids {
            let page = &p.pages[page_id];
            for c in &page.controllers {
                for (coord, inst) in &c.actions {
                    let Ok(mut v) = serde_json::to_value(inst) else { continue };
                    walk(&mut v, &mut |o| {
                        let uuid = o["UUID"].as_str().unwrap_or_default().to_string();
                        let title = title_of(o);
                        if let Some(path) = open_path(&uuid, o.get("Settings").unwrap_or(&Value::Null)) {
                            if needs_counterpart(&path, &title) {
                                let target = apps::resolve(&path, &title);
                                apps_out.push(json!({"page": page_id, "coord": coord, "title": title.split_whitespace().collect::<Vec<_>>().join(" "), "path": path,
                                    "target": target.as_ref().map(Target::label),
                                    "kind": match target { Some(Target::App { .. }) => "app", Some(Target::Url(_)) => "web", Some(Target::Dir(_)) => "folder", None => "none" }}));
                            }
                            return;
                        }
                        if builtin::def(&uuid).is_some() || self.registry.plugin_for_action(&uuid).is_some() {
                            return;
                        }
                        let cat = catalogue.iter().filter(|e| uuid.starts_with(&format!("{}.", e.id))).max_by_key(|e| e.id.len());
                        let (rule, status) = compat::classify(&uuid, cat.is_some());
                        let (gid, name, plugin, hint) = match (rule, cat) {
                            (Some(r), _) => (
                                r.group.to_string(),
                                r.name.to_string(),
                                r.plugin.map(|(id, n)| json!({"id": id, "name": n, "installed": self.registry.plugin(id).is_some()})),
                                r.hint.map(crate::i18n::tr),
                            ),
                            (None, Some(e)) => {
                                (format!("install:{}", e.id), e.name.clone(), Some(json!({"id": e.id, "name": e.name, "installed": false})), None)
                            }
                            (None, None) => {
                                let guess = uuid.rsplit_once('.').map(|(a, _)| a).unwrap_or(&uuid).to_string();
                                (format!("unknown:{guess}"), guess, None, None)
                            }
                        };
                        let idx = groups.iter().position(|g| g["group"] == gid.as_str()).unwrap_or_else(|| {
                            groups.push(json!({"group": gid, "name": name, "plugin": plugin, "hint": hint,
                                               "keys": 0, "builtin": 0, "replace": 0, "install": 0, "unavailable": 0, "unknown": 0}));
                            groups.len() - 1
                        });
                        let g = &mut groups[idx];
                        g["keys"] = json!(g["keys"].as_u64().unwrap_or(0) + 1);
                        let k = match status {
                            Status::BuiltIn => "builtin",
                            Status::Replace(_) => "replace",
                            Status::Install => "install",
                            Status::Unavailable => "unavailable",
                            Status::Unknown => "unknown",
                        };
                        g[k] = json!(g[k].as_u64().unwrap_or(0) + 1);
                    });
                }
            }
        }
        Ok(json!({"profile": profile, "groups": groups, "apps": apps_out}))
    }

    /// Replace the actions of a rule's group by their Linux counterparts (the replacement
    /// plugin has to be installed already). Returns how many keys were changed.
    pub fn apply_compat(&mut self, profile: &str, group: &str) -> Result<usize> {
        let rule = compat::RULES.iter().find(|r| r.group == group).with_context(|| crate::tr!("nothing to replace"))?;
        let name_of = |e: &Engine, uuid: &str| -> String {
            builtin::def(uuid)
                .map(|d| d.name.to_string())
                .or_else(|| e.registry.plugin_for_action(uuid).and_then(|p| p.action(uuid).map(|a| p.action_name(a))))
                .unwrap_or_else(|| uuid.rsplit('.').next().unwrap_or(uuid).to_string())
        };
        let mut changed = 0;
        let mut pages_changed = Vec::new();
        let page_ids: Vec<String> = self.profiles.get(profile).with_context(|| crate::tr!("profile not found"))?.pages.keys().cloned().collect();
        for page_id in page_ids {
            let mut page_changed = false;
            let mut controllers = self.profiles[profile].pages[&page_id].controllers.clone();
            for c in controllers.iter_mut() {
                for inst in c.actions.values_mut() {
                    let Ok(mut v) = serde_json::to_value(&*inst) else { continue };
                    let mut n = 0;
                    walk(&mut v, &mut |o| {
                        let uuid = o["UUID"].as_str().unwrap_or_default().to_string();
                        if self.registry.plugin_for_action(&uuid).is_some() {
                            return;
                        }
                        let (Some(r), Status::Replace(to)) = compat::classify(&uuid, false) else { return };
                        if r.group != rule.group {
                            return;
                        }
                        let Some(new) = compat::replacement_uuid(r, to) else { return };
                        o.insert("Name".into(), json!(name_of(self, &new)));
                        o.insert("Settings".into(), if matches!(to, To::Hotkey(..)) { compat::hotkey_settings(to) } else { json!({}) });
                        o.insert("UUID".into(), json!(new));
                        o.remove("Plugin");
                        n += 1;
                    });
                    if n > 0 {
                        if let Ok(new) = serde_json::from_value(v) {
                            *inst = new;
                            changed += n;
                            page_changed = true;
                        }
                    }
                }
            }
            if page_changed {
                self.profiles.get_mut(profile).unwrap().pages.get_mut(&page_id).unwrap().controllers = controllers;
                pages_changed.push(page_id);
            }
        }
        for page in &pages_changed {
            self.save_page(profile, page);
            self.emit(json!({"event": "page", "profile": profile, "page": page}));
        }
        let devs: Vec<String> = self.devices.iter().filter(|(_, d)| d.profile == profile).map(|(id, _)| id.clone()).collect();
        for dev in devs {
            self.update_visibility(&dev);
            self.refresh(&dev);
        }
        Ok(changed)
    }

    /// What happens with a key from a Windows/macOS profile here, shown in the editor:
    /// which application it opens, or what can be done about its missing plugin.
    pub fn linux_info(&self, inst: &ActionInstance) -> Option<Value> {
        let title = super::actions::inst_title(inst);
        if let Some(path) = open_path(&inst.uuid, &inst.settings) {
            if !needs_counterpart(&path, &title) {
                return None;
            }
            return Some(match apps::resolve(&path, &title) {
                Some(t) => json!({"status": "ok", "text": crate::tr!("On Linux this opens {target}.", target = t.label())}),
                None => {
                    let name = if title.is_empty() {
                        apps::candidates(&path, "").into_iter().next().unwrap_or(path.clone())
                    } else {
                        title.split_whitespace().collect::<Vec<_>>().join(" ")
                    };
                    json!({"status": "missing", "text": crate::tr!("No Linux application found for {app}. Install it, or set a program here.", app = name)})
                }
            });
        }
        if builtin::def(&inst.uuid).is_some() || self.registry.plugin_for_action(&inst.uuid).is_some() {
            return None;
        }
        let (rule, status) = compat::classify(&inst.uuid, false);
        let rule = rule?;
        Some(match status {
            Status::BuiltIn => json!({"status": "ok", "text": crate::tr!("TuxDeck does this itself with playerctl (Spotify desktop app).")}),
            Status::Replace(_) => {
                let with = rule.plugin.map(|(_, n)| n.to_string()).unwrap_or_else(|| crate::tr!("a keyboard shortcut"));
                json!({"status": "fix", "text": crate::tr!("Can be replaced with {replacement}: \"Make it work on Linux\" above the deck.", replacement = with)})
            }
            _ => json!({"status": "missing", "text": rule.hint.map(crate::i18n::tr).unwrap_or_else(|| crate::tr!("This plugin is not available on Linux."))}),
        })
    }

    /// Open the Linux counterpart of an application from a Windows/macOS profile.
    pub(super) fn open_counterpart(&self, path: &str, title: &str) -> Result<()> {
        let name = if title.trim().is_empty() {
            apps::candidates(path, "").into_iter().next().unwrap_or_else(|| path.to_string())
        } else {
            title.split_whitespace().collect::<Vec<_>>().join(" ")
        };
        match apps::resolve(path, title) {
            Some(Target::App { id, exec, .. }) => {
                tracing::info!("{name}: opening {id}");
                let mut c = Command::new("sh");
                c.arg("-c").arg(apps::exec_command(&exec));
                spawn_detached(c)
            }
            Some(Target::Url(url)) => open_url(&url),
            Some(Target::Dir(dir)) => open_url(&dir.to_string_lossy()),
            None => bail!(crate::tr!("no Linux application found for {app}", app = name)),
        }
    }

    /// Spotify actions of BarRaider's plugin, done with playerctl.
    pub(super) fn run_spotify(&mut self, cmd: SpotifyCmd) -> Result<()> {
        let args: Vec<String> = match cmd {
            SpotifyCmd::Args(a) => a.iter().map(|s| s.to_string()).collect(),
            SpotifyCmd::Loop => {
                let next = match playerctl(&["loop"])?.trim() {
                    "None" => "Playlist",
                    "Playlist" => "Track",
                    _ => "None",
                };
                vec!["loop".into(), next.into()]
            }
            SpotifyCmd::Mute => {
                let vol: f64 = playerctl(&["volume"])?.trim().parse().unwrap_or(0.0);
                if vol > 0.001 {
                    self.spotify_volume = Some(vol);
                    vec!["volume".into(), "0".into()]
                } else {
                    vec!["volume".into(), format!("{:.2}", self.spotify_volume.take().unwrap_or(0.5))]
                }
            }
        };
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        playerctl(&a).map(|_| ())
    }
}

/// Run `playerctl -p spotify …` and return its output.
fn playerctl(args: &[&str]) -> Result<String> {
    if input::dry_run() {
        tracing::info!("DRYRUN playerctl -p spotify {}", args.join(" "));
        return Ok(String::new());
    }
    let out = Command::new("playerctl")
        .arg("-p")
        .arg("spotify")
        .args(args)
        .output()
        .map_err(|_| anyhow!(crate::tr!("{program} is not installed", program = "playerctl")))?;
    if !out.status.success() {
        bail!(crate::tr!("Spotify is not running"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
