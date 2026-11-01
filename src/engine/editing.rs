//! Profile/page editing for the web UI: page JSON, adding and removing actions,
//! default profiles.

use super::*;

impl Engine {
    /// Everything on disk was replaced by a backup: load it and show it on the decks.
    pub fn after_restore(&mut self) {
        crate::i18n::set_language(&crate::i18n::resolve(&self.settings.language));
        self.input.set_backend(&self.settings.injector);
        self.runtime.clear();
        self.reload_profiles();
        self.rescan_plugins();
        let devs: Vec<(String, &'static Model)> = self.devices.values().map(|d| (d.id.clone(), d.model)).collect();
        for (id, model) in devs {
            let saved = self.settings.devices.get(&id).cloned();
            let profile = saved
                .as_ref()
                .map(|d| d.profile.clone())
                .filter(|p| self.profile_fits(&id, p))
                .or_else(|| self.profiles.values().find(|p| model.accepts_profile_model(&p.manifest.device.model)).map(|p| p.id.clone()))
                .unwrap_or_else(|| self.create_default_profile(model));
            if let Some(d) = self.devices.get_mut(&id) {
                d.stack.clear();
            }
            self.apply_profile(&id, &profile, true);
            if let Some(b) = saved.map(|d| d.brightness) {
                self.set_brightness(&id, b);
            }
        }
        self.emit(json!({"event": "iconpacks"}));
        self.emit(json!({"event": "state"}));
    }

    /// Create a profile with Linux-friendly defaults laid out for the model's keys.
    /// The starter profile of a new deck: TuxDeck's "Standard" profile. Everything works
    /// on a stock Linux desktop with no setup (media keys via playerctl/wpctl/pactl,
    /// xdg-open, loginctl; brightness and sleep are the deck's). The + also gets
    /// brightness and volume on its first two dials.
    pub fn create_default_profile(&mut self, model: &Model) -> String {
        let t = templates::TEMPLATES.iter().find(|t| t.id == templates::STANDARD).unwrap();
        let mut p = self.profile_from_template(t, model, &crate::tr!("Default Profile"));
        if model.encoders > 0 {
            let first = p.manifest.pages.pages[0].clone();
            let page = p.pages.get_mut(&first).unwrap();
            let dial = |uuid: &str, settings: Value| {
                let d = builtin::def(uuid).unwrap();
                let mut a = ActionInstance::new(uuid, d.name, d.states);
                a.settings = settings;
                a
            };
            page.controller_mut("Encoder").actions.insert("0,0".into(), dial(BRIGHTNESS, json!({"actionIdx": BRIGHTNESS_CYCLE})));
            page.controller_mut("Encoder").actions.insert("1,0".into(), dial(MULTIMEDIA, json!({"actionIdx": 4})));
        }
        let _ = p.save_all();
        let id = p.id.clone();
        self.profiles.insert(id.clone(), p);
        id
    }

    pub fn state_json(&self) -> Value {
        let devices: Vec<Value> = self
            .devices
            .values()
            .map(|d| {
                json!({"id": d.id, "name": crate::i18n::tr(d.model.name), "model": d.model.model_id, "kind": format!("{:?}", d.model.kind).to_lowercase(),
                       "connected": d.tx.is_some(), "cols": d.model.cols, "rows": d.model.rows, "key_px": d.model.key_px,
                       "touch_points": d.model.touch_points, "screen": d.model.screen.map(|s| json!([s.0, s.1])),
                       "encoders": d.model.encoders, "firmware": d.firmware, "profile": d.profile, "page": d.page,
                       "folder_depth": d.stack.len(), "brightness": d.brightness, "sleeping": d.sleeping,
                       "smart": d.smart_base.is_some()})
            })
            .collect();
        let profiles: Vec<Value> = self
            .profiles
            .values()
            .map(|p| {
                json!({"id": p.id, "name": p.manifest.name, "model": p.manifest.device.model, "pages": p.manifest.pages.pages,
                       "default_page": p.manifest.pages.default, "folders": p.folders(), "app": p.app_identifier(),
                       "device": self.deck_named(&p.manifest.device.uuid)})
            })
            .collect();
        json!({"devices": devices, "profiles": profiles, "active_app": self.active_app,
               "settings": {"cmd_as_ctrl": self.settings.cmd_as_ctrl, "platform": self.settings.platform, "language": self.settings.language,
                            "ui_language": crate::i18n::current(), "ui_languages": crate::i18n::ui_languages(),
                            "touch_color": self.settings.touch_color, "injector": self.settings.injector,
                            "injector_active": self.input.backend_name(), "injector_choices": Injector::CHOICES,
                            "plugin_port": self.plugin_port}})
    }

    pub fn page_json(&self, profile: &str, page: &str) -> Option<Value> {
        let pm = self.profiles.get(profile)?.pages.get(page)?;
        let mut controllers = Vec::new();
        for c in &pm.controllers {
            let kind = profile::normalize_controller(&c.kind);
            let mut actions = Map::new();
            for (coord, a) in &c.actions {
                let mut v = serde_json::to_value(a).ok()?;
                // Expose multi action lists as settings for the generic form editor.
                for (i, list) in multi_lists(a).iter().enumerate() {
                    if !v["Settings"].is_object() {
                        v["Settings"] = json!({});
                    }
                    v["Settings"][format!("_actions{i}")] = serde_json::to_value(list).ok()?;
                }
                actions.insert(coord.clone(), v);
            }
            if let Some(existing) = controllers.iter_mut().find(|x: &&mut Value| x["Type"] == kind) {
                if let Some(obj) = existing["Actions"].as_object_mut() {
                    obj.extend(actions);
                }
            } else {
                controllers.push(json!({"Type": kind, "Actions": actions}));
            }
        }
        let parent = self.profiles.get(profile)?.parent_of(page);
        Some(json!({"id": page, "name": pm.name, "profile": profile, "parent": parent, "controllers": controllers}))
    }

    pub fn page_mut(&mut self, profile: &str, page: &str) -> Result<&mut PageManifest> {
        self.profiles.get_mut(profile).and_then(|p| p.pages.get_mut(page)).ok_or_else(|| anyhow!(crate::tr!("page not found")))
    }

    pub fn save_page(&mut self, profile: &str, page: &str) {
        if let Some(p) = self.profiles.get(profile) {
            if let Err(e) = p.save_page(page) {
                self.log("error", crate::tr!("Saving failed: {error}", error = format!("{e:#}")));
            }
        }
        self.page_modified(profile, page);
    }

    pub fn new_instance(&mut self, profile: &str, uuid: &str) -> Result<ActionInstance> {
        let (name, states) = if let Some(d) = builtin::def(uuid) {
            (d.name.to_string(), d.states)
        } else {
            let pl = self.registry.plugin_for_action(uuid).with_context(|| crate::tr!("unknown action"))?;
            let a = pl.action(uuid).unwrap();
            (pl.action_name(a), a.states.len().max(1))
        };
        let mut inst = ActionInstance::new(uuid, &name, states);
        inst.settings = builtin::default_settings(uuid);
        if let Some(pl) = self.registry.plugin_for_action(uuid) {
            inst.plugin = Some(json!({"Name": pl.name(), "UUID": pl.uuid, "Version": pl.manifest.version}));
        }
        if uuid == OPEN_CHILD {
            // Folders are separate pages with a "parent folder" key at 0,0.
            let folder = uuid::Uuid::new_v4().to_string();
            let mut page = PageManifest::default();
            let back = ActionInstance::new(BACK_TO_PARENT, "Parent Folder", 1);
            page.controller_mut("Keypad").actions.insert("0,0".into(), back);
            let p = self.profiles.get_mut(profile).with_context(|| crate::tr!("profile not found"))?;
            p.pages.insert(folder.clone(), page);
            p.save_page(&folder)?;
            inst.settings = json!({"ProfileUUID": folder});
        }
        Ok(inst)
    }

    /// Remove an instance; folders (and their sub folders) are deleted as well. Devices
    /// that were showing a deleted folder go back to the profile's current page.
    pub fn remove_instance(&mut self, profile: &str, inst: &ActionInstance) {
        self.remove_instance_inner(profile, inst, &mut HashSet::new());
        let Some(p) = self.profiles.get(profile) else { return };
        let fallback = p.manifest.pages.current.clone();
        let stale: Vec<String> = self
            .devices
            .values()
            .filter(|d| d.profile == profile && (!p.pages.contains_key(&d.page) || d.stack.iter().any(|s| !p.pages.contains_key(s))))
            .map(|d| d.id.clone())
            .collect();
        for d in stale {
            self.goto_page(&d, &fallback);
        }
    }

    pub(super) fn remove_instance_inner(&mut self, profile: &str, inst: &ActionInstance, visited: &mut HashSet<String>) {
        if inst.uuid == OPEN_CHILD {
            // Folders may (via edited settings) point at each other or at themselves.
            // In the form page ids have (the parser also takes `{…}`, `urn:uuid:…`, no hyphens).
            let folder =
                inst.settings.get("ProfileUUID").and_then(|v| v.as_str()).and_then(|s| uuid::Uuid::parse_str(s).ok()).map(|u| u.hyphenated().to_string());
            // A folder key pointing at a top-level page must not delete that page's keys.
            let top_level = |f: &String| self.profiles.get(profile).is_some_and(|p| p.manifest.pages.pages.contains(f));
            if let Some(folder) = folder.filter(|f| !top_level(f) && visited.insert(f.clone())) {
                let children: Vec<ActionInstance> = self
                    .profiles
                    .get(profile)
                    .and_then(|p| p.pages.get(&folder))
                    .map(|pg| pg.iter_actions().map(|(_, _, a)| a.clone()).collect())
                    .unwrap_or_default();
                for c in children {
                    self.remove_instance_inner(profile, &c, visited);
                }
                if let Some(p) = self.profiles.get_mut(profile) {
                    if !p.manifest.pages.pages.contains(&folder) {
                        p.delete_page_dir(&folder);
                        p.pages.remove(&folder);
                    }
                }
            }
        }
        let ids: Vec<String> = self.devices.keys().map(|d| context_id(d, &inst.action_id)).collect();
        for c in ids {
            self.runtime.remove(&c);
        }
    }
}

impl Engine {
    /// Put the top-level pages of a profile in a new order (the same pages, no more, no
    /// less). "Go to Page" keys keep pointing at the same page: their page numbers follow.
    pub fn reorder_pages(&mut self, profile: &str, order: &[String]) -> Result<()> {
        let p = self.profiles.get_mut(profile).ok_or_else(|| anyhow!(crate::tr!("profile not found")))?;
        let old = p.manifest.pages.pages.clone();
        let mut a = old.iter().map(|s| s.to_lowercase()).collect::<Vec<_>>();
        let mut b = order.iter().map(|s| s.to_lowercase()).collect::<Vec<_>>();
        a.sort();
        b.sort();
        if a != b {
            bail!(crate::tr!("the new order must contain exactly the pages of the profile"));
        }
        // The stored ids (their spelling) in the new order.
        let new: Vec<String> = order.iter().map(|o| old.iter().find(|x| x.eq_ignore_ascii_case(o)).cloned().unwrap_or_else(|| o.clone())).collect();
        if new == old {
            return Ok(());
        }
        let renumber: Vec<usize> = old.iter().map(|id| new.iter().position(|x| x == id).unwrap_or(0) + 1).collect();
        let mut changed = Vec::new();
        for (page_id, page) in p.pages.iter_mut() {
            let mut any = false;
            for c in page.controllers.iter_mut() {
                for inst in c.actions.values_mut() {
                    let Ok(mut v) = serde_json::to_value(&*inst) else { continue };
                    if renumber_goto(&mut v, &renumber) {
                        if let Ok(back) = serde_json::from_value(v) {
                            *inst = back;
                            any = true;
                        }
                    }
                }
            }
            if any {
                changed.push(page_id.clone());
            }
        }
        p.manifest.pages.pages = new;
        p.save_manifest()?;
        for page in &changed {
            if let Err(e) = p.save_page(page) {
                tracing::warn!("saving page {page}: {e:#}");
            }
        }
        // The decks stay on their pages; page numbers (info bar) change.
        let devs: Vec<String> = self.devices.values().filter(|d| d.profile == profile).map(|d| d.id.clone()).collect();
        for d in devs {
            self.refresh(&d);
        }
        for page in &changed {
            self.emit(json!({"event": "page", "profile": profile, "page": page}));
        }
        self.emit(json!({"event": "state"}));
        Ok(())
    }
}

/// Renumber the "Go to Page" actions in an action instance (also inside multi actions and
/// Key Logic): `renumber[n - 1]` is the new number of the page that had number `n`. A
/// number past the last page meant the last page. Returns whether something changed.
fn renumber_goto(v: &mut Value, renumber: &[usize]) -> bool {
    let mut changed = false;
    match v {
        Value::Object(o) => {
            if o.get("UUID").and_then(|u| u.as_str()) == Some(PAGE_GOTO) && !renumber.is_empty() {
                let n = o.get("Settings").and_then(|s| s.get("PageIndex")).and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize;
                let to = renumber[n.min(renumber.len()) - 1];
                if to != n {
                    if let Some(s) = o.entry("Settings").or_insert_with(|| json!({})).as_object_mut() {
                        s.insert("PageIndex".into(), json!(to));
                        changed = true;
                    }
                }
            }
            for x in o.values_mut() {
                changed |= renumber_goto(x, renumber);
            }
        }
        Value::Array(a) => {
            for x in a {
                changed |= renumber_goto(x, renumber);
            }
        }
        _ => {}
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goto_page_numbers_follow_their_page() {
        // Pages A B C become C A B: A 1→2, B 2→3, C 3→1.
        let renumber = [2, 3, 1];
        let mut key = json!({"UUID": PAGE_GOTO, "Settings": {"PageIndex": 3}});
        assert!(renumber_goto(&mut key, &renumber));
        assert_eq!(key["Settings"]["PageIndex"], 1);
        // Inside a multi action / Key Logic, next to other actions.
        let mut multi = json!({"UUID": MULTI, "Settings": {}, "Actions": [{"Actions": [
            {"UUID": PAGE_GOTO, "Settings": {"PageIndex": 1}},
            {"UUID": HOTKEY, "Settings": {"PageIndex": 1}},
            {"UUID": PAGE_GOTO, "Settings": {}}
        ]}]});
        assert!(renumber_goto(&mut multi, &renumber));
        let list = &multi["Actions"][0]["Actions"];
        assert_eq!(
            (list[0]["Settings"]["PageIndex"].clone(), list[1]["Settings"]["PageIndex"].clone(), list[2]["Settings"]["PageIndex"].clone()),
            (json!(2), json!(1), json!(2))
        );
        // Past the last page meant the last page (C).
        let mut far = json!({"UUID": PAGE_GOTO, "Settings": {"PageIndex": 9}});
        assert!(renumber_goto(&mut far, &renumber));
        assert_eq!(far["Settings"]["PageIndex"], 1);
        // Unchanged numbers: nothing to save.
        let mut same = json!({"UUID": PAGE_GOTO, "Settings": {"PageIndex": 2}});
        assert!(!renumber_goto(&mut same, &[1, 2]));
    }
}
