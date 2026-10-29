//! Converting a profile from the official app on Windows/macOS into a regular Linux
//! profile: Open keys get the Linux program, the web version or the folder; actions of
//! missing plugins become built-in actions, another plugin's or a keyboard shortcut
//! (rules in `crate::compat`). Keys without a counterpart keep their title and icon and
//! become an Open action for a command of the user's own. Titles and images stay.

use super::*;
use crate::apps::{self, Target};
use crate::compat::{self, Status, To};

/// Visit every action instance in `v`, including those inside multi actions. The
/// `Plugin` object of an instance (`{"Name", "UUID", "Version"}` of its plugin) also has
/// a UUID but is no action.
fn walk(v: &mut Value, f: &mut dyn FnMut(&mut Map<String, Value>)) {
    match v {
        Value::Object(o) => {
            if o.get("UUID").is_some_and(Value::is_string) {
                f(o);
            }
            for (k, x) in o.iter_mut() {
                if k != "Plugin" {
                    walk(x, f);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| walk(x, f)),
        _ => {}
    }
}

/// Title of the current state of a serialized action instance, on one line.
fn title_of(o: &Map<String, Value>) -> String {
    let state = o.get("State").and_then(Value::as_u64).unwrap_or(0) as usize;
    let states = o.get("States").and_then(Value::as_array);
    let pick = |i: usize| states.and_then(|s| s.get(i)).and_then(|s| s.get("Title")).and_then(Value::as_str).filter(|t| !t.trim().is_empty());
    pick(state).or_else(|| pick(0)).unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ")
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

/// An Open action from Windows/macOS: a path that can't exist here, or no path but a
/// title naming the application.
fn foreign_open(path: &str, title: &str) -> bool {
    apps::is_foreign(path) || (path.trim().trim_matches('"').is_empty() && !title.is_empty())
}

/// What happens to one key when converting.
enum Plan {
    Open(Target),
    Replace {
        group: &'static compat::Rule,
        to: To,
    },
    /// Same plugin from the catalogue.
    Install(String, String),
    /// No counterpart: an Open action with an empty command.
    Manual(String),
}

impl Engine {
    fn plan_key(&self, o: &Map<String, Value>, catalogue: &[crate::catalogue::Entry]) -> Option<Plan> {
        let uuid = o["UUID"].as_str().unwrap_or_default();
        let title = title_of(o);
        if let Some(path) = open_path(uuid, o.get("Settings").unwrap_or(&Value::Null)) {
            if !foreign_open(&path, &title) {
                return None;
            }
            return match apps::resolve(&path, &title) {
                Some(t) => Some(Plan::Open(t)),
                // An Open key without a command already waits for one of the user's own.
                None if !apps::is_foreign(&path) => None,
                None => Some(Plan::Manual(if title.is_empty() { apps::candidates(&path, "").into_iter().next().unwrap_or(path) } else { title })),
            };
        }
        if builtin::def(uuid).is_some() || self.registry.plugin_for_action(uuid).is_some() {
            return None;
        }
        let cat = catalogue.iter().filter(|e| uuid.starts_with(&format!("{}.", e.id))).max_by_key(|e| e.id.len());
        let name = || {
            let n = o.get("Name").and_then(Value::as_str).unwrap_or(uuid);
            if title.is_empty() || title == n {
                n.to_string()
            } else {
                format!("{n} ({title})")
            }
        };
        Some(match compat::classify(uuid, cat.is_some()) {
            (Some(r), Status::Replace(to)) => Plan::Replace { group: r, to },
            (_, Status::Install) => {
                let e = cat.unwrap();
                Plan::Install(e.id.clone(), e.name.clone())
            }
            _ => Plan::Manual(name()),
        })
    }

    /// What converting a profile would do (`needed`: false if there is nothing to do).
    pub fn conversion_plan(&self, profile: &str, catalogue: &[crate::catalogue::Entry]) -> Result<Value> {
        let p = self.profiles.get(profile).with_context(|| crate::tr!("profile not found"))?;
        let (mut programs, mut web, mut folders, mut manual) = (vec![], vec![], vec![], vec![]);
        let mut replaced: Vec<Value> = vec![];
        let mut installs: Vec<(String, String)> = vec![];
        let count = |list: &mut Vec<Value>, key: &str, label: &str| match list.iter_mut().find(|x| x["key"] == key) {
            Some(x) => x["keys"] = json!(x["keys"].as_u64().unwrap_or(0) + 1),
            None => list.push(json!({"key": key, "name": label, "keys": 1})),
        };
        for page in p.pages.values() {
            for c in &page.controllers {
                for inst in c.actions.values() {
                    let Ok(mut v) = serde_json::to_value(inst) else { continue };
                    walk(&mut v, &mut |o| {
                        let title = title_of(o);
                        match self.plan_key(o, catalogue) {
                            Some(Plan::Open(t)) => {
                                let item = json!({"title": title, "target": t.label()});
                                match t {
                                    Target::App { .. } => programs.push(item),
                                    Target::Url(_) => web.push(item),
                                    Target::Dir(_) => folders.push(item),
                                }
                            }
                            Some(Plan::Replace { group, to }) => {
                                let (key, label) = match (to, group.plugin) {
                                    (To::Media(_), _) => ("media".to_string(), crate::tr!("Multimedia")),
                                    (To::Hotkey(..), _) => (format!("{}:hotkey", group.group), crate::tr!("{app} shortcuts", app = group.name)),
                                    (_, Some((id, name))) => {
                                        if self.registry.plugin(id).is_none() && !installs.iter().any(|(i, _)| i == id) {
                                            installs.push((id.to_string(), name.to_string()));
                                        }
                                        (id.to_string(), name.to_string())
                                    }
                                    _ => return,
                                };
                                count(&mut replaced, &key, &label);
                            }
                            Some(Plan::Install(id, name)) => {
                                if !installs.iter().any(|(i, _)| *i == id) {
                                    installs.push((id.clone(), name.clone()));
                                }
                                count(&mut replaced, &id, &name);
                            }
                            Some(Plan::Manual(name)) => manual.push(json!(name)),
                            None => {}
                        }
                    });
                }
            }
        }
        let needed = !(programs.is_empty() && web.is_empty() && folders.is_empty() && replaced.is_empty() && manual.is_empty());
        Ok(json!({"profile": profile, "needed": needed, "programs": programs, "web": web, "folders": folders, "replaced": replaced,
                  "manual": manual, "installs": installs.iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>()}))
    }

    /// Convert a profile (the plugins it needs are installed already). Returns how many
    /// keys changed and how many need a command of the user's own.
    pub fn convert_profile(&mut self, profile: &str, catalogue: &[crate::catalogue::Entry]) -> Result<Value> {
        let name_of = |e: &Engine, uuid: &str| -> String {
            builtin::def(uuid)
                .map(|d| crate::i18n::tr(d.name))
                .or_else(|| e.registry.plugin_for_action(uuid).and_then(|p| p.action(uuid).map(|a| p.action_name(a))))
                .unwrap_or_else(|| uuid.rsplit('.').next().unwrap_or(uuid).to_string())
        };
        let (mut changed, mut manual) = (0, 0);
        let page_ids: Vec<String> = self.profiles.get(profile).with_context(|| crate::tr!("profile not found"))?.pages.keys().cloned().collect();
        let mut pages_changed = Vec::new();
        for page_id in page_ids {
            let mut controllers = self.profiles[profile].pages[&page_id].controllers.clone();
            let mut page_changed = false;
            for c in controllers.iter_mut() {
                for inst in c.actions.values_mut() {
                    let Ok(mut v) = serde_json::to_value(&*inst) else { continue };
                    let mut n = 0;
                    walk(&mut v, &mut |o| {
                        let (uuid, settings) = match self.plan_key(o, catalogue) {
                            None => return,
                            Some(Plan::Open(Target::App { exec, .. })) => (OPEN.to_string(), json!({"path": apps::exec_command(&exec)})),
                            Some(Plan::Open(Target::Url(url))) => (WEBSITE.to_string(), json!({"openInBrowser": true, "path": url})),
                            Some(Plan::Open(t @ Target::Dir(_))) => (OPEN.to_string(), json!({"path": t.label()})),
                            Some(Plan::Replace { group, to }) => match compat::replacement_uuid(group, to) {
                                Some(u) if builtin::def(&u).is_some() || self.registry.plugin_for_action(&u).is_some() => (u, compat::replacement_settings(to)),
                                _ => {
                                    manual += 1;
                                    (OPEN.to_string(), json!({"path": ""}))
                                }
                            },
                            // Installed by now; a failed install leaves the key as it was.
                            Some(Plan::Install(..)) => return,
                            Some(Plan::Manual(_)) => {
                                manual += 1;
                                (OPEN.to_string(), json!({"path": ""}))
                            }
                        };
                        o.insert("Name".into(), json!(name_of(self, &uuid)));
                        o.insert("Settings".into(), settings);
                        o.insert("UUID".into(), json!(uuid));
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
        self.log("info", crate::tr!("Profile converted for Linux: {n} keys", n = changed));
        Ok(json!({"changed": changed, "manual": manual}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_visits_nested_actions_but_not_plugin_info() {
        // A multi action made on Linux: the instance and its nested action carry the
        // `Plugin` object of their (built-in) plugin, which has a UUID of its own.
        let mut v = json!({
            "UUID": "com.elgato.streamdeck.multiactions.routine", "Name": "Multi Action",
            "Plugin": {"Name": "Multi Action", "UUID": "com.elgato.streamdeck.multiactions", "Version": "1"},
            "Settings": {"Routine": [{
                "UUID": "com.elgato.streamdeck.system.website", "Settings": {"path": "https://example.org"},
                "Plugin": {"Name": "System", "UUID": "com.elgato.streamdeck.system", "Version": "1"}
            }]}
        });
        let mut seen = vec![];
        walk(&mut v, &mut |o| seen.push(o["UUID"].as_str().unwrap().to_string()));
        assert_eq!(seen, ["com.elgato.streamdeck.multiactions.routine", "com.elgato.streamdeck.system.website"]);
    }
}
