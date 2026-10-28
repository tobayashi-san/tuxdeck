//! Plugin SDK: action contexts, visibility (willAppear/willDisappear), messages to and
//! from plugins and property inspectors.

use super::*;

impl Engine {
    pub(super) fn sdk_controller(slot: &Slot) -> &'static str {
        match (slot.controller.as_str(), slot.coord.as_str()) {
            ("Encoder", _) => "Encoder",
            // The Neo info bar; its touch points behave like keys.
            ("Neo", NEO_INFOBAR) => "Neo",
            _ => "Keypad",
        }
    }

    pub fn action_payload(&self, slot: &Slot, inst: &ActionInstance, nested: bool) -> Value {
        let (c, r) = slot.col_row();
        let mut p = json!({
            "settings": inst.settings,
            "coordinates": {"column": c, "row": r},
            "controller": Self::sdk_controller(slot),
            "isInMultiAction": nested,
        });
        if inst.states.len() > 1 {
            p["state"] = json!(inst.state);
        }
        p
    }

    pub(super) fn plugin_of(&self, action_uuid: &str) -> Option<&plugins::Plugin> {
        self.registry.plugin_for_action(action_uuid).filter(|p| !matches!(p.runtime, PluginRuntime::Native | PluginRuntime::Unsupported(_)))
    }

    pub(super) fn update_visibility(&mut self, dev: &str) {
        let Some(d) = self.devices.get(dev) else { return };
        let (profile, page) = (d.profile.clone(), d.page.clone());
        let mut now: BTreeMap<String, (String, String, Value)> = BTreeMap::new();
        // Forget contexts of actions that were removed from this page. (Page ids are
        // only unique per profile: an imported copy keeps them.)
        self.contexts.retain(|_, l| !(l.device == dev && l.profile == profile && l.page == page));
        if let Some(pm) = self.profiles.get(&profile).and_then(|p| p.pages.get(&page)) {
            for (ctrl, coord, inst) in pm.iter_actions() {
                let slot = Slot::new(ctrl, coord);
                let mut items = vec![(inst.clone(), None)];
                for (li, list) in multi_lists(inst).iter().enumerate() {
                    for (ei, sub) in list.iter().enumerate() {
                        items.push((sub.clone(), Some((li, ei))));
                    }
                }
                for (a, nested) in items {
                    let ctx = context_id(dev, &a.action_id);
                    self.contexts.insert(ctx.clone(), Loc { device: dev.into(), profile: profile.clone(), page: page.clone(), slot: slot.clone(), nested });
                    if let Some(pl) = self.plugin_of(&a.uuid) {
                        now.insert(ctx, (pl.uuid.clone(), a.uuid.clone(), self.action_payload(&slot, &a, nested.is_some())));
                    }
                }
            }
        }
        let old = std::mem::take(&mut self.devices.get_mut(dev).unwrap().visible);
        for (ctx, (plugin, action, payload)) in &old {
            let same = now.get(ctx).map(|n| n.2.get("coordinates") == payload.get("coordinates")).unwrap_or(false);
            if !same {
                self.send_plugin(plugin, json!({"event": "willDisappear", "action": action, "context": ctx, "device": dev, "payload": payload}));
                self.runtime.remove(ctx);
            }
        }
        for (ctx, (plugin, action, payload)) in &now {
            let same = old.get(ctx).map(|o| o.2.get("coordinates") == payload.get("coordinates")).unwrap_or(false);
            if !same {
                self.send_plugin(plugin, json!({"event": "willAppear", "action": action, "context": ctx, "device": dev, "payload": payload}));
                self.send_title_params(ctx);
            }
        }
        self.devices.get_mut(dev).unwrap().visible = now;
    }

    pub fn send_title_params(&mut self, ctx: &str) {
        let Some((loc, inst)) = self.lookup(ctx) else { return };
        let Some(pl) = self.plugin_of(&inst.uuid).map(|p| p.uuid.clone()) else { return };
        let st = inst.current_state().cloned().unwrap_or_default();
        let (c, r) = loc.slot.col_row();
        self.send_plugin(
            &pl,
            json!({"event": "titleParametersDidChange", "action": inst.uuid, "context": ctx, "device": loc.device,
                   "payload": {"coordinates": {"column": c, "row": r}, "settings": inst.settings, "state": inst.state,
                               "title": st.title.clone().unwrap_or_default(),
                               "titleParameters": {"fontFamily": st.font_family.clone().unwrap_or_default(), "fontSize": st.font_size.unwrap_or(12.0),
                                                   "fontStyle": st.font_style.clone().unwrap_or_default(), "fontUnderline": st.font_underline.unwrap_or(false),
                                                   "showTitle": st.show_title.unwrap_or(true), "titleAlignment": st.title_alignment.clone().unwrap_or("bottom".into()),
                                                   "titleColor": st.title_color.clone().unwrap_or("#ffffff".into())}}}),
        );
    }

    /// Find the instance behind a context. The slot must still hold that very action:
    /// a context of a removed action must not reach whatever was placed there later.
    pub fn lookup(&self, ctx: &str) -> Option<(Loc, ActionInstance)> {
        let loc = self.contexts.get(ctx)?.clone();
        let inst = self.instance_at(&loc)?;
        (context_id(&loc.device, &inst.action_id) == ctx).then_some((loc, inst))
    }

    pub(super) fn loc_of(&self, ctx: &str) -> Option<Loc> {
        self.lookup(ctx).map(|(loc, _)| loc)
    }

    pub(super) fn instance_at(&self, loc: &Loc) -> Option<ActionInstance> {
        let inst = self.profiles.get(&loc.profile)?.pages.get(&loc.page)?.action(&loc.slot.controller, &loc.slot.coord)?;
        match loc.nested {
            None => Some(inst.clone()),
            Some((li, ei)) => multi_lists(inst).get(li)?.get(ei).cloned(),
        }
    }

    /// Mutate an instance (top-level or nested) and persist the page.
    pub(super) fn with_instance<R>(&mut self, loc: &Loc, f: impl FnOnce(&mut ActionInstance) -> R) -> Option<R> {
        let p = self.profiles.get_mut(&loc.profile)?;
        let page = p.pages.get_mut(&loc.page)?;
        let inst = page.action_mut(&loc.slot.controller, &loc.slot.coord)?;
        let r = match loc.nested {
            None => f(inst),
            Some((li, ei)) => {
                let entry = nested_entry_mut(inst, li, ei)?;
                let mut sub: ActionInstance = serde_json::from_value(entry.clone()).ok()?;
                let r = f(&mut sub);
                *entry = serde_json::to_value(&sub).ok()?;
                r
            }
        };
        let _ = p.save_page(&loc.page);
        Some(r)
    }

    pub fn send_plugin(&mut self, plugin: &str, msg: Value) {
        let s = msg.to_string();
        let conn = self.plugin_conns.entry(plugin.to_string()).or_default();
        match &conn.tx {
            Some(tx) if tx.send(s.clone()).is_ok() => {}
            _ => {
                if conn.pending.len() >= 1000 {
                    conn.pending.remove(0);
                }
                conn.pending.push(s);
            }
        }
    }

    /// A plugin (re)connected: bring it up to date like the official app does at plugin
    /// start. Input and visibility events queued while it was down are stale; the
    /// current state is sent instead.
    /// SDK activity of a plugin and where its actions are shown (Linux check).
    pub fn plugin_diagnostics(&mut self, uuid: &str) -> Value {
        // HTML plugins run in the shared plugin host: no process of their own (null).
        let running = self.plugin_conns.get_mut(uuid).and_then(|c| c.child.as_mut()).map(|c| matches!(c.try_wait(), Ok(None)));
        let (connected, restarts) = self.plugin_conns.get(uuid).map(|c| (c.tx.is_some(), c.restarts)).unwrap_or_default();
        let shown: Vec<Value> = self
            .devices
            .values()
            .flat_map(|d| d.visible.iter().filter(|(_, (p, _, _))| p == uuid).map(move |(ctx, (_, action, payload))| (d.id.clone(), ctx, action, payload)))
            .map(|(dev, ctx, action, payload)| json!({"device": dev, "context": ctx, "action": action, "controller": payload["controller"], "coordinates": payload["coordinates"]}))
            .collect();
        json!({"running": running, "connected": connected, "restarts": restarts, "stats": self.plugin_stats.get(uuid), "shown": shown})
    }

    pub fn plugin_registered(&mut self, uuid: &str, tx: UnboundedSender<String>) {
        self.plugin_stats.entry(uuid.to_string()).or_default().registrations += 1;
        let conn = self.plugin_conns.entry(uuid.to_string()).or_default();
        let keep: Vec<String> = conn
            .pending
            .drain(..)
            .filter(|m| {
                let ev = serde_json::from_str::<Value>(m).ok().and_then(|v| v.get("event").and_then(|e| e.as_str()).map(str::to_string));
                matches!(ev.as_deref(), Some("didReceiveDeepLink" | "didReceiveGlobalSettings"))
            })
            .collect();
        conn.tx = Some(tx);
        let devs: Vec<String> = self.devices.values().filter(|d| d.tx.is_some()).map(|d| d.id.clone()).collect();
        for d in &devs {
            let info = self.device_info(d);
            self.send_plugin(uuid, json!({"event": "deviceDidConnect", "device": d, "deviceInfo": info}));
        }
        for m in keep {
            if let Some(tx) = self.plugin_conns.get(uuid).and_then(|c| c.tx.as_ref()) {
                let _ = tx.send(m);
            }
        }
        for d in &devs {
            let visible: Vec<(String, String, Value)> = self.devices[d]
                .visible
                .iter()
                .filter(|(_, (pl, _, _))| pl == uuid)
                .map(|(ctx, (_, action, payload))| (ctx.clone(), action.clone(), payload.clone()))
                .collect();
            for (ctx, action, payload) in visible {
                self.send_plugin(uuid, json!({"event": "willAppear", "action": action, "context": ctx, "device": d, "payload": payload}));
                self.send_title_params(&ctx);
            }
        }
        let pis: Vec<String> = self.pis.keys().cloned().collect();
        for ctx in pis {
            if let Some((loc, inst)) = self.lookup(&ctx) {
                if self.plugin_of(&inst.uuid).is_some_and(|p| p.uuid == uuid) {
                    self.send_plugin(uuid, json!({"event": "propertyInspectorDidAppear", "action": inst.uuid, "context": ctx, "device": loc.device}));
                }
            }
        }
    }

    pub fn broadcast_plugins(&mut self, f: impl Fn(&str) -> Option<Value>) {
        let ids: Vec<String> = self.plugin_conns.iter().filter(|(_, c)| c.tx.is_some()).map(|(k, _)| k.clone()).collect();
        for id in ids {
            if let Some(m) = f(&id) {
                self.send_plugin(&id, m);
            }
        }
    }

    pub fn send_pi(&mut self, ctx: &str, msg: Value) {
        if let Some(tx) = self.pis.get(ctx) {
            let _ = tx.send(msg.to_string());
        }
    }

    /// Context for a slot (used by the UI to open a property inspector).
    pub fn ensure_context(&mut self, profile: &str, page: &str, slot: &Slot) -> Option<(String, Loc, ActionInstance)> {
        let inst = self.profiles.get(profile)?.pages.get(page)?.action(&slot.controller, &slot.coord)?.clone();
        let dev = self
            .devices
            .values()
            .find(|d| d.profile == profile && d.page == page)
            .or_else(|| self.devices.values().find(|d| d.profile == profile))
            .or_else(|| self.devices.values().next())
            .map(|d| d.id.clone())
            .unwrap_or_else(|| "virtual".into());
        let ctx = context_id(&dev, &inst.action_id);
        let loc = Loc { device: dev, profile: profile.into(), page: page.into(), slot: slot.clone(), nested: None };
        self.contexts.insert(ctx.clone(), loc.clone());
        Some((ctx, loc, inst))
    }

    /// Info JSON passed to plugins and property inspectors (`-info` / inInfo).
    pub fn app_info(&self, plugin: &plugins::Plugin) -> Value {
        let devices: Vec<Value> = self
            .devices
            .values()
            .map(|d| {
                let mut i = self.device_info(&d.id);
                i["id"] = json!(d.id);
                i
            })
            .collect();
        json!({
            "application": {"font": ".AppleSystemUIFont", "language": crate::i18n::plugin_language(&crate::i18n::resolve(&self.settings.language)), "platform": self.settings.platform,
                            "platformVersion": "15.0.0", "version": "7.6.0.23012"},
            "plugin": {"uuid": plugin.uuid, "version": plugin.manifest.version},
            "devicePixelRatio": 2,
            "colors": {"buttonPressedBackgroundColor": "#303030FF", "buttonPressedBorderColor": "#646464FF", "buttonPressedTextColor": "#969696FF",
                       "disabledColor": "#007AFF7F", "highlightColor": "#007AFFFF", "mouseDownColor": "#2EA8FFFF"},
            "devices": devices,
        })
    }

    pub fn set_settings_from(&mut self, ctx: &str, settings: Value, from_pi: bool) {
        let Some(loc) = self.loc_of(ctx) else { return };
        self.with_instance(&loc, |i| i.settings = settings.clone());
        let Some((_, inst)) = self.lookup(ctx) else { return };
        let payload = self.action_payload(&loc.slot, &inst, loc.nested.is_some());
        let msg = json!({"event": "didReceiveSettings", "action": inst.uuid, "context": ctx, "device": loc.device, "payload": payload});
        if from_pi {
            if let Some(pl) = self.plugin_of(&inst.uuid).map(|p| p.uuid.clone()) {
                self.send_plugin(&pl, msg);
            }
        } else {
            self.send_pi(ctx, msg);
        }
        if let Some(d) = self.devices.get_mut(&loc.device) {
            if let Some(v) = d.visible.get_mut(ctx) {
                v.2 = payload;
            }
        }
        self.emit(json!({"event": "page", "profile": loc.profile, "page": loc.page}));
    }

    /// Handle a message coming from a plugin (`from_pi == false`) or a property
    /// inspector (`from_pi == true`). `owner` is the plugin UUID or PI context.
    pub fn handle_sdk(&mut self, owner: &str, from_pi: bool, msg: Value) {
        let event = msg.get("event").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let ctx = msg.get("context").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let payload = msg.get("payload").cloned().unwrap_or(Value::Null);
        let plugin_uuid = if from_pi {
            self.lookup(owner).and_then(|(_, i)| self.registry.plugin_for_action(&i.uuid).map(|p| p.uuid.clone())).unwrap_or_default()
        } else {
            owner.to_string()
        };
        if !from_pi {
            let stats = self.plugin_stats.entry(plugin_uuid.clone()).or_default();
            *stats.events.entry(event.clone()).or_default() += 1;
            if !ctx.is_empty() && ctx != plugin_uuid {
                *stats.contexts.entry(ctx.clone()).or_default() += 1;
            }
        }
        // Context-bound events may only address the sender's own actions.
        const CONTEXT_EVENTS: &[&str] = &[
            "setSettings",
            "getSettings",
            "setTitle",
            "setImage",
            "setState",
            "showOk",
            "showAlert",
            "sendToPropertyInspector",
            "setFeedback",
            "setFeedbackLayout",
            "setTriggerDescription",
        ];
        if CONTEXT_EVENTS.contains(&event.as_str()) {
            let own = if from_pi {
                ctx == owner
            } else {
                self.lookup(&ctx).and_then(|(_, i)| self.registry.plugin_for_action(&i.uuid).map(|p| p.uuid == plugin_uuid)).unwrap_or(false)
            };
            if !own {
                tracing::debug!("ignoring {event} from {owner} for foreign or unknown context {ctx}");
                return;
            }
        }
        match event.as_str() {
            "setSettings" => self.set_settings_from(&ctx, payload, from_pi),
            "getSettings" => {
                if let Some((loc, inst)) = self.lookup(&ctx) {
                    let p = self.action_payload(&loc.slot, &inst, loc.nested.is_some());
                    let m = json!({"event": "didReceiveSettings", "action": inst.uuid, "context": ctx, "device": loc.device, "payload": p});
                    if from_pi {
                        self.send_pi(&ctx, m);
                    } else {
                        self.send_plugin(owner, m);
                    }
                }
            }
            "setGlobalSettings" => {
                self.settings.global_settings.insert(plugin_uuid.clone(), payload.clone());
                self.settings.save(&self.paths);
                let m = json!({"event": "didReceiveGlobalSettings", "payload": {"settings": payload}});
                if from_pi {
                    self.send_plugin(&plugin_uuid, m);
                } else {
                    let ctxs: Vec<String> = self.pis.keys().cloned().collect();
                    for c in ctxs {
                        if self.lookup(&c).and_then(|(_, i)| self.registry.plugin_for_action(&i.uuid).map(|p| p.uuid == plugin_uuid)).unwrap_or(false) {
                            self.send_pi(&c, m.clone());
                        }
                    }
                }
            }
            "getGlobalSettings" => {
                let s = self.settings.global_settings.get(&plugin_uuid).cloned().unwrap_or(json!({}));
                let m = json!({"event": "didReceiveGlobalSettings", "payload": {"settings": s}});
                if from_pi {
                    self.send_pi(owner, m);
                } else {
                    self.send_plugin(owner, m);
                }
            }
            "openUrl" => {
                if let Some(u) = payload.get("url").and_then(|v| v.as_str()) {
                    let _ = open_url(u);
                }
            }
            "logMessage" => {
                let m = payload.get("message").and_then(|v| v.as_str()).unwrap_or("");
                tracing::info!(target: "plugin", "[{plugin_uuid}] {m}");
                // Also in the plugin's own log, like the official app.
                if !plugin_uuid.is_empty() && !plugin_uuid.contains(['/', '\\']) {
                    use std::io::Write as _;
                    let path = self.paths.logs().join(format!("{plugin_uuid}.log"));
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                        let _ = writeln!(f, "{} {m}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"));
                    }
                }
            }
            "setTitle" => {
                let title = payload.get("title").and_then(|v| v.as_str()).map(str::to_string);
                let target = payload.get("target").and_then(|v| v.as_i64()).unwrap_or(0);
                if target != 2 {
                    let state = payload.get("state").and_then(|v| v.as_u64()).map(|s| s as usize);
                    let rt = self.runtime.entry(ctx.clone()).or_default();
                    // Without a state the title applies to all states.
                    if state.is_none() {
                        rt.titles.clear();
                    }
                    match title {
                        Some(t) => rt.titles.insert(state, t),
                        None => rt.titles.remove(&state),
                    };
                    self.refresh_context(&ctx);
                }
            }
            "setImage" => {
                let target = payload.get("target").and_then(|v| v.as_i64()).unwrap_or(0);
                if target != 2 {
                    let state = payload.get("state").and_then(|v| v.as_u64()).map(|s| s as usize);
                    let img = payload.get("image").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
                    let data = img.and_then(|i| {
                        if i.starts_with("data:") {
                            ImageData::from_data_url(i)
                        } else {
                            // Relative path inside the plugin directory.
                            let pl = self.registry.plugin(&plugin_uuid)?;
                            plugins::resolve_image(&pl.dir, i).and_then(|p| ImageData::from_file(&p))
                        }
                    });
                    let rt = self.runtime.entry(ctx.clone()).or_default();
                    // Without a state the image applies to all states.
                    if state.is_none() {
                        rt.images.clear();
                    }
                    match data {
                        Some(d) => {
                            rt.images.insert(state, d);
                        }
                        None => {
                            rt.images.remove(&state);
                        }
                    }
                    self.refresh_context(&ctx);
                }
            }
            "setState" => {
                if let (Some(loc), Some(st)) = (self.loc_of(&ctx), payload.get("state").and_then(|v| v.as_u64())) {
                    self.with_instance(&loc, |i| {
                        if (st as usize) < i.states.len().max(1) {
                            i.state = st as usize
                        }
                    });
                    self.refresh_context(&ctx);
                }
            }
            "showOk" | "showAlert" => {
                let o = if event == "showOk" { Overlay::Ok } else { Overlay::Alert };
                self.runtime.entry(ctx.clone()).or_default().overlay = Some((o, Instant::now()));
                self.refresh_context(&ctx);
            }
            "sendToPropertyInspector" => {
                let action = self.lookup(&ctx).map(|(_, i)| i.uuid).unwrap_or_default();
                self.send_pi(&ctx, json!({"event": "sendToPropertyInspector", "action": action, "context": ctx, "payload": payload}));
            }
            "sendToPlugin" => {
                let action =
                    msg.get("action").and_then(|v| v.as_str()).map(str::to_string).or_else(|| self.lookup(&ctx).map(|(_, i)| i.uuid)).unwrap_or_default();
                if !plugin_uuid.is_empty() {
                    self.send_plugin(&plugin_uuid, json!({"event": "sendToPlugin", "action": action, "context": ctx, "payload": payload}));
                }
            }
            "switchToProfile" => {
                // Plugins may only switch to the profiles they ship; without a profile
                // the device returns to the one before the plugin's switch.
                let dev = msg.get("device").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let Some(pl) = self.registry.plugin(&plugin_uuid).cloned() else { return };
                match payload.get("profile").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                    None => {
                        let prev = self.devices.get_mut(&dev).and_then(|d| d.plugin_return.take());
                        if let Some(prev) = prev.filter(|p| self.profile_fits(&dev, p)) {
                            self.set_profile(&dev, &prev);
                        }
                    }
                    Some(name) if pl.manifest.profiles.iter().any(|p| p.name == name) => {
                        let ids = self.settings.plugin_profiles.get(&format!("{}/{name}", pl.uuid)).cloned().unwrap_or_default();
                        let Some(id) = ids.into_iter().find(|id| self.profile_fits(&dev, id)) else {
                            tracing::warn!("[{plugin_uuid}] profile {name:?} is not installed for this device");
                            return;
                        };
                        let current = self.devices.get(&dev).map(|d| d.profile.clone());
                        if let (Some(d), Some(cur)) = (self.devices.get_mut(&dev), current) {
                            if cur != id {
                                d.plugin_return.get_or_insert(cur);
                            }
                        }
                        self.set_profile(&dev, &id);
                        // Optional page, indexed from 0.
                        if let Some(n) = payload.get("page").and_then(|v| v.as_u64()) {
                            let page = self.profiles.get(&id).and_then(|p| p.manifest.pages.pages.get(n as usize).cloned());
                            if let Some(page) = page {
                                self.goto_page(&dev, &page);
                            }
                        }
                    }
                    Some(name) => tracing::warn!("[{plugin_uuid}] switchToProfile: {name:?} is not one of the plugin's profiles"),
                }
            }
            "setFeedback" => {
                if let Value::Object(o) = payload {
                    let rt = self.runtime.entry(ctx.clone()).or_default();
                    for (k, v) in o {
                        // Objects patch the previous value of that item.
                        match (rt.feedback.get_mut(&k), v) {
                            (Some(Value::Object(old)), Value::Object(new)) => old.extend(new),
                            (_, v) => {
                                rt.feedback.insert(k, v);
                            }
                        }
                    }
                    self.refresh_context(&ctx);
                }
            }
            "setFeedbackLayout" => {
                let layout = payload.get("layout").cloned().unwrap_or(Value::Null);
                let dir = self.registry.plugin(&plugin_uuid).map(|p| p.dir.clone());
                let r = layout.as_str().unwrap_or("").to_string();
                let loaded = match &layout {
                    // A layout definition sent inline (e.g. the weather plugin's info bar).
                    Value::Object(o) if o.get("items").is_some_and(Value::is_array) => Some(layout.clone()),
                    // Plugins send "" on actions without a touch strip.
                    _ if r.is_empty() => return,
                    _ => strip::load_layout(&r, dir.as_deref()),
                };
                match loaded {
                    Some(l) => {
                        let rt = self.runtime.entry(ctx.clone()).or_default();
                        rt.layout = Some(l);
                        rt.feedback.clear();
                        self.refresh_context(&ctx);
                    }
                    None => tracing::warn!("[{plugin_uuid}] unknown touch strip layout {r:?}"),
                }
            }
            "setTriggerDescription" => {}
            other => tracing::debug!("unhandled SDK event {other} from {owner}"),
        }
    }
}
