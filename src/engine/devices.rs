//! Devices connecting and going away, brightness, profile and page navigation.

use super::*;

impl Engine {
    pub fn device_connected(&mut self, id: String, model: &'static Model, firmware: String, tx: std::sync::mpsc::Sender<DevCmd>) {
        let ds = self.settings.devices.get(&id).cloned();
        let profile = ds
            .as_ref()
            .map(|d| d.profile.clone())
            .filter(|p| self.profiles.get(p).is_some_and(|x| model.accepts_profile_model(&x.manifest.device.model)))
            .or_else(|| self.profiles.values().find(|p| p.manifest.device.model == model.model_id).map(|p| p.id.clone()))
            .or_else(|| self.profiles.values().find(|p| model.accepts_profile_model(&p.manifest.device.model)).map(|p| p.id.clone()))
            .unwrap_or_else(|| self.create_default_profile(model));
        let brightness = ds.map(|d| d.brightness).unwrap_or(70);
        let _ = tx.send(DevCmd::Brightness(brightness));
        let n_buttons = model.key_count() + model.touch_points as usize;
        let dev = DevState {
            id: id.clone(),
            model,
            firmware,
            tx: Some(tx),
            profile: String::new(),
            page: String::new(),
            stack: vec![],
            brightness,
            sleeping: false,
            smart_base: None,
            sent: HashMap::new(),
            buttons: vec![false; n_buttons],
            encoders: vec![false; model.encoders as usize],
            pressed: HashMap::new(),
            animated: false,
            plugin_return: None,
            visible: BTreeMap::new(),
        };
        let reconnect = self.devices.contains_key(&id);
        if let Some(old) = self.devices.get_mut(&id) {
            old.tx = dev.tx;
            old.sent.clear();
            old.firmware = dev.firmware;
            old.buttons = dev.buttons;
            old.pressed.clear();
            // A replugged deck shows its boot logo at the stored brightness: show the
            // keys instead of keeping it "asleep" in that state.
            old.sleeping = false;
        } else {
            self.devices.insert(id.clone(), dev);
        }
        self.log("info", crate::tr!("{device} connected ({id})", device = model.name, id = id));
        if !reconnect {
            self.set_profile(&id, &profile);
            self.apply_smart(&id);
        } else {
            // Plugins got willDisappear on disconnect.
            self.update_visibility(&id);
            self.refresh(&id);
        }
        let info = self.device_info(&id);
        self.broadcast_plugins(|_| Some(json!({"event": "deviceDidConnect", "device": id, "deviceInfo": info})));
        self.emit(json!({"event": "state"}));
    }

    pub fn device_disconnected(&mut self, id: &str) {
        if let Some(d) = self.devices.get_mut(id) {
            d.tx = None;
            let vis: Vec<_> = std::mem::take(&mut d.visible).into_iter().collect();
            for (ctx, (plugin, action, payload)) in vis {
                self.send_plugin(&plugin, json!({"event": "willDisappear", "action": action, "context": ctx, "device": id, "payload": payload}));
            }
            self.log("info", crate::tr!("{id} disconnected", id = id));
        }
        self.broadcast_plugins(|_| Some(json!({"event": "deviceDidDisconnect", "device": id})));
        self.emit(json!({"event": "state"}));
    }

    pub fn device_info(&self, id: &str) -> Value {
        let Some(d) = self.devices.get(id) else { return Value::Null };
        let ty = match d.model.kind {
            Kind::Original | Kind::Mk2 => 0,
            Kind::Mini => 1,
            Kind::Xl => 2,
            Kind::Pedal => 5,
            Kind::Plus => 7,
            Kind::Neo => 9,
        };
        json!({"name": d.model.name, "type": ty, "size": {"columns": d.model.cols, "rows": d.model.rows}})
    }

    pub(super) fn send_dev(&self, id: &str, cmd: DevCmd) {
        if let Some(tx) = self.devices.get(id).and_then(|d| d.tx.as_ref()) {
            let _ = tx.send(cmd);
        }
    }

    pub fn set_brightness(&mut self, id: &str, b: u8) {
        let b = b.min(100);
        if let Some(d) = self.devices.get_mut(id) {
            d.brightness = b;
            d.sleeping = false;
        }
        self.send_dev(id, DevCmd::Brightness(b));
        self.settings.devices.entry(id.to_string()).or_insert(DeviceSettings { profile: String::new(), brightness: b }).brightness = b;
        self.settings.save(&self.paths);
        self.emit(json!({"event": "state"}));
    }

    /// Switch profiles on behalf of the user (UI, action, plugin): remembered for the
    /// next start and ends a temporary smart profile switch.
    pub fn set_profile(&mut self, dev: &str, profile_id: &str) {
        if !self.profile_fits(dev, profile_id) {
            tracing::warn!("profile {profile_id} does not fit device {dev}");
            return;
        }
        if let Some(d) = self.devices.get_mut(dev) {
            d.smart_base = None;
        }
        self.apply_profile(dev, profile_id, true);
    }

    /// Whether the profile was made for the device's model (or a compatible one).
    pub fn profile_fits(&self, dev: &str, profile_id: &str) -> bool {
        match (self.devices.get(dev), self.profiles.get(profile_id)) {
            (Some(d), Some(p)) => d.model.accepts_profile_model(&p.manifest.device.model),
            _ => false,
        }
    }

    /// Focus changed to the application with window class `app`.
    pub fn active_app_changed(&mut self, app: &str) {
        if self.active_app == app {
            return;
        }
        self.active_app = app.to_string();
        self.input.run(input::Job::Focus(app.to_string()), None);
        let ids: Vec<String> = self.devices.keys().cloned().collect();
        for id in ids {
            self.apply_smart(&id);
        }
        self.emit(json!({"event": "state"}));
    }

    /// Show the smart profile of the focused application, or return to the profile
    /// that was active before.
    pub(super) fn apply_smart(&mut self, dev: &str) {
        let Some(d) = self.devices.get(dev) else { return };
        let model = d.model;
        let target = self
            .profiles
            .values()
            .filter(|p| model.accepts_profile_model(&p.manifest.device.model))
            .find(|p| p.app_identifier().map(|a| crate::smart::app_matches(a, &self.active_app)).unwrap_or(false))
            .map(|p| (p.id.clone(), p.manifest.name.clone()));
        let (current, base) = (d.profile.clone(), d.smart_base.clone());
        match target {
            Some((t, name)) if t != current => {
                self.apply_profile(dev, &t, false);
                self.devices.get_mut(dev).unwrap().smart_base = Some(base.unwrap_or(current));
                self.log("info", crate::tr!("Smart Profile \"{name}\" for {app}", name = name, app = self.active_app));
            }
            Some(_) => {}
            None => {
                if let Some(b) = base {
                    self.devices.get_mut(dev).unwrap().smart_base = None;
                    if self.profiles.contains_key(&b) {
                        self.apply_profile(dev, &b, false);
                    }
                }
            }
        }
    }

    pub(super) fn apply_profile(&mut self, dev: &str, profile_id: &str, persist: bool) {
        let Some(p) = self.profiles.get(profile_id) else { return };
        let page = p.manifest.pages.current.clone();
        if let Some(d) = self.devices.get_mut(dev) {
            d.profile = profile_id.to_string();
            d.page = page;
            d.stack.clear();
        }
        if persist {
            let b = self.devices.get(dev).map(|d| d.brightness).unwrap_or(70);
            self.settings.devices.insert(dev.to_string(), DeviceSettings { profile: profile_id.to_string(), brightness: b });
            self.settings.save(&self.paths);
        }
        self.page_changed(dev);
    }

    /// Navigate to a top-level page or a folder of the device's profile.
    pub fn goto_page(&mut self, dev: &str, page: &str) {
        let Some(d) = self.devices.get(dev) else { return };
        let Some(p) = self.profiles.get_mut(&d.profile) else { return };
        if !p.pages.contains_key(page) {
            return;
        }
        let mut stack = Vec::new();
        if p.manifest.pages.pages.iter().any(|x| x == page) {
            if p.manifest.pages.current != page {
                p.manifest.pages.current = page.to_string();
                let _ = p.save_manifest();
            }
        } else {
            // Folder: rebuild the parent chain.
            let mut cur = page.to_string();
            while let Some(parent) = p.parent_of(&cur) {
                if stack.contains(&parent) || stack.len() > 32 {
                    break;
                }
                stack.insert(0, parent.clone());
                if p.manifest.pages.pages.contains(&parent) {
                    break;
                }
                cur = parent;
            }
            if stack.is_empty() {
                stack.push(p.manifest.pages.current.clone());
            }
        }
        let d = self.devices.get_mut(dev).unwrap();
        d.page = page.to_string();
        d.stack = stack;
        self.page_changed(dev);
    }

    pub(super) fn page_changed(&mut self, dev: &str) {
        self.update_visibility(dev);
        self.refresh(dev);
        self.emit(json!({"event": "state"}));
    }

    pub(super) fn top_pages(&self, dev: &str) -> (Vec<String>, Option<usize>) {
        let Some(d) = self.devices.get(dev) else { return (vec![], None) };
        let Some(p) = self.profiles.get(&d.profile) else { return (vec![], None) };
        let pages = p.manifest.pages.pages.clone();
        let idx = if d.stack.is_empty() { pages.iter().position(|x| *x == d.page) } else { None };
        (pages, idx)
    }

    pub(super) fn page_step(&mut self, dev: &str, delta: i64) {
        let (pages, idx) = self.top_pages(dev);
        let Some(i) = idx else { return };
        let n = i as i64 + delta;
        if n >= 0 && (n as usize) < pages.len() {
            self.goto_page(dev, &pages[n as usize].clone());
        }
    }
}
