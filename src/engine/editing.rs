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
    pub fn create_default_profile(&mut self, model: &Model) -> String {
        let mut p = Profile::new(&self.paths.profiles(), &crate::tr!("Default Profile"), model.model_id);
        let page_id = p.manifest.pages.pages[0].clone();
        let page = p.pages.get_mut(&page_id).unwrap();
        // Icon-only keys using the bundled starter icons, so the starter profile looks tidy and
        // every key is self-explanatory.
        let mk = |uuid: &str, settings: Value, icon: &str| {
            let d = builtin::def(uuid).unwrap();
            let mut a = ActionInstance::new(uuid, d.name, d.states);
            a.settings = settings;
            if let Some(url) = crate::resources::starter_icon_data_url(icon) {
                for st in a.states.iter_mut() {
                    st.image = Some(url.clone());
                }
            }
            a
        };
        // Everything works on a stock Linux desktop with no setup: media keys via
        // playerctl/wpctl/pactl, xdg-open, loginctl; brightness and sleep are the deck's.
        let media = || {
            [
                mk(MULTIMEDIA, json!({"actionIdx": 0}), "previous"),
                mk(MULTIMEDIA, json!({"actionIdx": 1}), "play"),
                mk(MULTIMEDIA, json!({"actionIdx": 2}), "next"),
                mk(MULTIMEDIA, json!({"actionIdx": 4}), "mute"),
                mk(MULTIMEDIA, json!({"actionIdx": 6}), "volume-down"),
                mk(MULTIMEDIA, json!({"actionIdx": 5}), "volume-up"),
            ]
        };
        let sleep = || mk(SLEEP, json!({}), "sleep");
        let brightness = || mk(BRIGHTNESS, json!({"actionIdx": BRIGHTNESS_CYCLE}), "brightness");
        let (cols, rows) = (model.cols as usize, model.rows as usize);
        let mut keys: Vec<((usize, usize), ActionInstance)> = Vec::new();
        if rows == 1 {
            // Pedal: previous, play/pause, next.
            keys.extend(media().into_iter().take(cols).enumerate().map(|(i, a)| ((i, 0), a)));
        } else if rows == 2 && cols >= 4 {
            // Neo and +: the media remote plus the deck controls, row by row.
            let all = media().into_iter().chain([sleep(), brightness()]);
            keys.extend(all.enumerate().map(|(i, a)| ((i % cols, i / cols), a)));
        } else {
            // The media remote as a 3x2 block: all of a Mini; top left on the bigger decks,
            // which also get desktop shortcuts bottom left and the deck controls bottom
            // right. The remaining keys are left free for the user.
            keys.extend(media().into_iter().enumerate().map(|(i, a)| ((i % 3, i / 3), a)));
            if rows >= 3 && cols >= 5 {
                let last = rows - 1;
                keys.push(((0, last), mk(OPEN, json!({"path": "~"}), "home")));
                keys.push(((1, last), mk(OPEN, json!({"path": "loginctl lock-session"}), "lock")));
                keys.push(((2, last), mk(TIMER, json!({"seconds": 300}), "timer")));
                keys.push(((cols - 2, last), sleep()));
                keys.push(((cols - 1, last), brightness()));
            }
        }
        for ((c, r), a) in keys {
            page.controller_mut("Keypad").actions.insert(format!("{c},{r}"), a);
        }
        if model.encoders > 0 {
            page.controller_mut("Encoder").actions.insert("0,0".into(), brightness());
            page.controller_mut("Encoder").actions.insert("1,0".into(), mk(MULTIMEDIA, json!({"actionIdx": 4}), "volume-up"));
        }
        if model.kind == Kind::Neo {
            let d = builtin::def(DIGITAL_TIME).unwrap();
            let mut a = ActionInstance::new(DIGITAL_TIME, d.name, 1);
            a.settings = json!({"layout": "digital_time_01", "theme": "", "24HourFormat": true});
            page.controller_mut("Neo").actions.insert(NEO_INFOBAR.into(), a);
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
                json!({"id": d.id, "name": d.model.name, "model": d.model.model_id, "kind": format!("{:?}", d.model.kind).to_lowercase(),
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
                       "default_page": p.manifest.pages.default, "folders": p.folders(), "app": p.app_identifier()})
            })
            .collect();
        json!({"devices": devices, "profiles": profiles, "active_app": self.active_app,
               "settings": {"cmd_as_ctrl": self.settings.cmd_as_ctrl, "platform": self.settings.platform, "language": self.settings.language,
                            "ui_language": crate::i18n::current(), "ui_languages": crate::i18n::ui_languages(),
                            "touch_color": self.settings.touch_color, "injector": self.settings.injector,
                            "injector_active": self.input.backend_name(), "injector_choices": Injector::CHOICES}})
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
            let folder = inst.settings.get("ProfileUUID").and_then(|v| v.as_str()).map(|s| s.to_lowercase());
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
