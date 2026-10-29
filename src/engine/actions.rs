//! Running actions: plugin key events, built-in actions, Key Logic, multi actions.

use super::*;

/// Key Logic timing of the official app (KA_KeyLogic): a single-shot 300 ms
/// double-click timer and a 500 ms long-press timer, both started on the first press.
pub(super) const KEY_LOGIC_DOUBLE: Duration = Duration::from_millis(300);

pub(super) const KEY_LOGIC_HOLD: Duration = Duration::from_millis(500);

#[derive(Default)]
pub(super) struct KeyLogicState {
    pressed: bool,
    double_timer: bool,
    hold_timer: bool,
    /// Bumped whenever the timers are stopped, so pending timeouts become stale.
    generation: u64,
    /// Where the key was pressed (top level).
    loc: Option<Loc>,
    /// Nested action that got keyDown and waits for the physical release.
    held: Option<(Loc, ActionInstance, Instant)>,
}

#[derive(Clone, Copy)]
pub(super) enum KeyLogicTimer {
    Double,
    Hold,
}

/// Nested action lists. Multi actions are stored as `"Actions": [{"Actions": [...]}, ...]`;
/// Key Logic as three slots `"Actions": [action | {"Actions": []}, ...]` (click, double
/// click, long press). Both are exposed as lists; Key Logic lists hold at most one action.
/// Title of an action's current state (names the application of imported Open keys).
pub fn inst_title(inst: &ActionInstance) -> String {
    inst.states.get(inst.state).or(inst.states.first()).and_then(|s| s.title.clone()).filter(|t| !t.trim().is_empty()).unwrap_or_default()
}

pub fn multi_lists(inst: &ActionInstance) -> Vec<Vec<ActionInstance>> {
    let key_logic = inst.uuid == KEY_LOGIC;
    if !key_logic && ![MULTI, MULTI_SWITCH, MULTI_RANDOM].contains(&inst.uuid.as_str()) {
        return vec![];
    }
    let parse = |a: &Vec<Value>| a.iter().filter_map(|x| serde_json::from_value(x.clone()).ok()).collect::<Vec<ActionInstance>>();
    let mut out: Vec<Vec<ActionInstance>> = inst
        .extra
        .get("Actions")
        .and_then(|v| v.as_array())
        .map(|lists| {
            lists
                .iter()
                .map(|l| {
                    if key_logic && l.get("UUID").is_some() {
                        return parse(&vec![l.clone()]);
                    }
                    l.get("Actions").and_then(|a| a.as_array()).map(parse).unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default();
    let want = match inst.uuid.as_str() {
        MULTI_SWITCH => 2,
        KEY_LOGIC => 3,
        _ => 1,
    };
    while out.len() < want {
        out.push(vec![]);
    }
    out
}

pub fn set_multi_lists(inst: &mut ActionInstance, lists: Vec<Vec<Value>>) {
    let v: Vec<Value> = if inst.uuid == KEY_LOGIC {
        let mut v: Vec<Value> = lists.into_iter().take(3).map(|l| l.into_iter().next().unwrap_or_else(|| json!({"Actions": []}))).collect();
        v.resize(3, json!({"Actions": []}));
        v
    } else {
        lists.into_iter().map(|l| json!({"Actions": l})).collect()
    };
    inst.extra.insert("Actions".into(), Value::Array(v));
}

/// Copy of an action with fresh ActionIDs (nested multi action entries included), so
/// the copy gets its own plugin contexts. The stored JSON is kept as is otherwise.
pub fn with_new_ids(inst: &ActionInstance) -> ActionInstance {
    fn renew(v: &mut Value) {
        match v {
            Value::Array(a) => a.iter_mut().for_each(renew),
            Value::Object(o) => {
                if o.contains_key("ActionID") {
                    o.insert("ActionID".into(), json!(uuid::Uuid::new_v4().to_string()));
                }
                if let Some(a) = o.get_mut("Actions") {
                    renew(a);
                }
            }
            _ => {}
        }
    }
    let mut c = inst.clone();
    c.action_id = uuid::Uuid::new_v4().to_string();
    if let Some(a) = c.extra.get_mut("Actions") {
        renew(a);
    }
    c
}

/// Stored JSON of nested action `(li, ei)` (see [`multi_lists`]).
pub(super) fn nested_entry_mut(inst: &mut ActionInstance, li: usize, ei: usize) -> Option<&mut Value> {
    let key_logic = inst.uuid == KEY_LOGIC;
    let list = inst.extra.get_mut("Actions")?.as_array_mut()?.get_mut(li)?;
    if key_logic && list.get("UUID").is_some() {
        return if ei == 0 { Some(list) } else { None };
    }
    list.get_mut("Actions")?.as_array_mut()?.get_mut(ei)
}

impl Engine {
    /// Execute an action instance (key down or up).
    pub fn trigger(&mut self, loc: &Loc, inst: &ActionInstance, down: bool, held: Duration) {
        let ctx = context_id(&loc.device, &inst.action_id);
        self.contexts.insert(ctx.clone(), loc.clone());
        // Inside a multi action the configured state is what the user wants, not a toggle.
        let in_multi = loc.nested.is_some() && self.instance_at(&Loc { nested: None, ..loc.clone() }).is_some_and(|p| p.uuid != KEY_LOGIC);
        if let Some(pl) = self.plugin_of(&inst.uuid).map(|p| p.uuid.clone()) {
            let mut payload = self.action_payload(&loc.slot, inst, loc.nested.is_some());
            if in_multi {
                if let Some(o) = payload.as_object_mut() {
                    o.remove("coordinates");
                }
                if inst.states.len() > 1 {
                    payload["userDesiredState"] = json!(inst.state);
                }
            }
            let event = if down { "keyDown" } else { "keyUp" };
            self.send_plugin(&pl, json!({"event": event, "action": inst.uuid, "context": ctx, "device": loc.device, "payload": payload}));
            // Automatic state toggling for multi-state actions.
            if !down && inst.states.len() > 1 && !in_multi {
                let auto =
                    !self.registry.plugin_for_action(&inst.uuid).and_then(|p| p.action(&inst.uuid)).and_then(|a| a.disable_automatic_states).unwrap_or(false);
                if auto {
                    let n = inst.states.len();
                    self.with_instance(loc, |i| i.state = (i.state + 1) % n);
                    self.refresh_context(&ctx);
                }
            }
            return;
        }
        if let Err(e) = self.run_builtin(loc, inst, &ctx, down, held) {
            let name = builtin::def(&inst.uuid).map(|d| crate::i18n::tr(d.name)).unwrap_or_else(|| inst.name.clone());
            self.action_failed(&ctx, Some(name), &e);
        }
    }

    /// Key Logic press/release, following KA_KeyLogic of the official app: the first
    /// press starts both timers; a second press while both run is a double click; the
    /// hold timer fires the long press if the key is still down; the double-click timer
    /// fires the click if the key was released (and the hold timer still runs).
    pub(super) fn key_logic_input(&mut self, loc: &Loc, down: bool) {
        let key = (loc.device.clone(), loc.slot.clone());
        let st = self.key_logic.entry(key.clone()).or_default();
        if !down {
            st.pressed = false;
            if let Some((l, a, t)) = st.held.take() {
                self.trigger(&l, &a, false, t.elapsed());
            }
            return;
        }
        st.pressed = true;
        st.loc = Some(loc.clone());
        if !st.double_timer && !st.hold_timer {
            st.double_timer = true;
            st.hold_timer = true;
            let generation = st.generation;
            let Some(sh) = self.self_ref.as_ref().and_then(|w| w.upgrade()) else { return };
            for (timer, delay) in [(KeyLogicTimer::Double, KEY_LOGIC_DOUBLE), (KeyLogicTimer::Hold, KEY_LOGIC_HOLD)] {
                let (sh, key) = (sh.clone(), key.clone());
                let _ = std::thread::Builder::new().name("key-logic".into()).spawn(move || {
                    std::thread::sleep(delay);
                    sh.lock().key_logic_timeout(&key, generation, timer);
                });
            }
        } else if st.double_timer && st.hold_timer {
            st.double_timer = false;
            st.hold_timer = false;
            st.generation += 1;
            self.key_logic_fire(&key, 1, true);
        }
    }

    pub(super) fn key_logic_timeout(&mut self, key: &(String, Slot), generation: u64, timer: KeyLogicTimer) {
        let Some(st) = self.key_logic.get_mut(key).filter(|s| s.generation == generation) else { return };
        let fire = match timer {
            KeyLogicTimer::Hold => {
                st.hold_timer = false;
                st.pressed.then_some((2, true))
            }
            KeyLogicTimer::Double => {
                st.double_timer = false;
                (!st.pressed && st.hold_timer).then_some((0, false))
            }
        };
        if let Some((idx, hold)) = fire {
            st.double_timer = false;
            st.hold_timer = false;
            st.generation += 1;
            self.key_logic_fire(key, idx, hold);
        }
    }

    /// Run slot `idx` (0 click, 1 double click, 2 long press). Held slots get keyUp on
    /// the physical release; an empty slot shows the alert overlay.
    pub(super) fn key_logic_fire(&mut self, key: &(String, Slot), idx: usize, hold: bool) {
        let Some(loc) = self.key_logic.get(key).and_then(|s| s.loc.clone()) else { return };
        let Some(inst) = self.instance_at(&loc) else { return };
        let Some(sub) = multi_lists(&inst).get(idx).and_then(|l| l.first()).cloned() else {
            let ctx = context_id(&loc.device, &inst.action_id);
            self.runtime.entry(ctx.clone()).or_default().overlay = Some((Overlay::Alert, Instant::now()));
            self.refresh_context(&ctx);
            return;
        };
        let sub_loc = Loc { nested: Some((idx, 0)), ..loc };
        self.trigger(&sub_loc, &sub, true, Duration::ZERO);
        if hold {
            if let Some(st) = self.key_logic.get_mut(key) {
                st.held = Some((sub_loc, sub, Instant::now()));
            }
        } else {
            self.trigger(&sub_loc, &sub, false, Duration::ZERO);
        }
    }

    pub(super) fn hotkeys(&self, settings: &Value) -> Vec<Combo> {
        settings
            .get("Hotkeys")
            .and_then(|h| h.as_array())
            .map(|a| a.iter().filter_map(|v| Combo::from_elgato(v, self.settings.cmd_as_ctrl)).collect())
            .unwrap_or_default()
    }

    /// Key injection runs on the input thread; a failure shows the alert on the key
    /// like any other action error.
    pub(super) fn inject(&self, ctx: &str, job: input::Job) {
        let (weak, ctx) = (self.self_ref.clone(), ctx.to_string());
        let on_error = move |err: anyhow::Error| {
            // Not on the input thread: the engine may be waiting for it (backend switch).
            std::thread::spawn(move || {
                if let Some(sh) = weak.and_then(|w| w.upgrade()) {
                    sh.lock().action_failed(&ctx, None, &err);
                }
            });
        };
        self.input.run(job, Some(Box::new(on_error)));
    }

    /// Log an action error and show the alert overlay on its key.
    pub(super) fn action_failed(&mut self, ctx: &str, name: Option<String>, err: &anyhow::Error) {
        let name = name.or_else(|| self.lookup(ctx).map(|(_, i)| builtin::def(&i.uuid).map(|d| crate::i18n::tr(d.name)).unwrap_or(i.name)));
        self.log("warn", format!("{}: {err:#}", name.unwrap_or_default()));
        self.runtime.entry(ctx.to_string()).or_default().overlay = Some((Overlay::Alert, Instant::now()));
        self.refresh_context(ctx);
    }

    pub(super) fn run_builtin(&mut self, loc: &Loc, inst: &ActionInstance, ctx: &str, down: bool, held: Duration) -> Result<()> {
        let s = &inst.settings;
        let sv = |k: &str| s.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let dev = loc.device.clone();
        match (inst.uuid.as_str(), down) {
            (HOTKEY, _) => {
                let combos = self.hotkeys(s);
                if combos.is_empty() {
                    if down {
                        bail!(crate::tr!("no hotkey set"));
                    }
                    return Ok(());
                }
                // Earlier combos are tapped, the last one follows the key (hold to repeat).
                if down {
                    for c in &combos[..combos.len() - 1] {
                        self.inject(ctx, input::Job::Tap(c.clone()));
                    }
                    self.inject(ctx, input::Job::Down(combos.last().unwrap().clone()));
                } else {
                    self.inject(ctx, input::Job::Up(combos.last().unwrap().clone()));
                }
            }
            (HOTKEY_SWITCH, true) => {
                let combos: Vec<Option<Combo>> = s
                    .get("Hotkeys")
                    .and_then(|h| h.as_array())
                    .map(|a| a.iter().map(|v| Combo::from_elgato(v, self.settings.cmd_as_ctrl)).collect())
                    .unwrap_or_default();
                if let Some(Some(c)) = combos.get(inst.state) {
                    self.inject(ctx, input::Job::Tap(c.clone()));
                }
                let n = inst.states.len().max(2);
                self.with_instance(loc, |i| {
                    while i.states.len() < 2 {
                        i.states.push(Default::default());
                    }
                    i.state = (i.state + 1) % n
                });
                self.refresh_context(ctx);
            }
            (OPEN, true) => {
                let title = inst_title(inst);
                if compat::needs_counterpart(&sv("path"), &title) {
                    return self.open_counterpart(&sv("path"), &title);
                }
                let path = expand_home(sv("path").trim().trim_matches('"'));
                if path.is_empty() {
                    bail!(crate::tr!("no path set"));
                }
                let p = std::path::Path::new(&path);
                if path.contains("://") || p.is_dir() || (p.is_file() && !is_executable(p)) {
                    open_url(&path)?;
                } else if p.is_file() {
                    spawn_detached(Command::new(&path))?;
                } else if path.starts_with("/Applications/") || path.starts_with("/System/") {
                    bail!(crate::tr!("the macOS path {path} does not exist on Linux", path = path));
                } else if path.starts_with('/') && !std::path::Path::new(path.split_whitespace().next().unwrap_or("")).exists() {
                    bail!(crate::tr!("{path} does not exist", path = path));
                } else {
                    let mut c = Command::new("sh");
                    c.arg("-c").arg(&path);
                    spawn_detached(c)?;
                }
            }
            (OPEN_APP, false) => {
                let exec = sv("exec");
                let exec = if exec.is_empty() { sv("app_name") } else { exec };
                if exec.is_empty() {
                    bail!(crate::tr!("no application set"));
                }
                if crate::apps::is_foreign(&exec) {
                    return self.open_counterpart(&exec, &inst_title(inst));
                }
                // A path to a program (may contain spaces) or a command line.
                let is_path = exec.contains('/') && std::path::Path::new(&expand_home(&exec)).exists();
                let long = held >= Duration::from_millis(600);
                let lp = s.get("long_press").and_then(|v| v.as_str()).unwrap_or("quit");
                if long && lp != "none" {
                    let program = if is_path { exec.as_str() } else { exec.split_whitespace().next().unwrap_or(&exec) };
                    let name = std::path::Path::new(program).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or(exec.clone());
                    let sig = if lp == "kill" || s.get("force_kill").and_then(|v| v.as_bool()).unwrap_or(false) { libc::SIGKILL } else { libc::SIGTERM };
                    kill_processes(&name, sig)?;
                } else if exec.ends_with(".desktop") {
                    let mut c = Command::new("gtk-launch");
                    c.arg(std::path::Path::new(&exec).file_name().unwrap());
                    spawn_detached(c)?;
                } else if is_path {
                    // Arguments stay shell syntax (quotes), the program path is passed as is.
                    let mut c = Command::new("sh");
                    c.arg("-c").arg(format!("exec \"$0\" {}", sv("args"))).arg(expand_home(&exec));
                    spawn_detached(c)?;
                } else {
                    let mut c = Command::new("sh");
                    c.arg("-c").arg(format!("{} {}", exec, sv("args")));
                    spawn_detached(c)?;
                }
            }
            (CLOSE, true) => {
                let name = sv("app_name");
                if name.is_empty() {
                    bail!(crate::tr!("no application set"));
                }
                let name = std::path::Path::new(name.trim()).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or(name);
                if kill_processes(&name, libc::SIGTERM)? == 0 && !input::dry_run() {
                    bail!(crate::tr!("{app} is not running", app = name));
                }
            }
            (WEBSITE, true) => {
                let mut url = sv("path").trim().to_string();
                if url.is_empty() {
                    bail!(crate::tr!("no URL set"));
                }
                // Add https:// only to bare host names, not to mailto:, tel:, steam: etc.
                let has_scheme = url.split_once(':').is_some_and(|(scheme, rest)| {
                    scheme.len() > 1
                        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
                        && !rest.chars().next().is_some_and(|c| c.is_ascii_digit())
                });
                if !has_scheme {
                    url = format!("https://{url}");
                }
                if s.get("openInBrowser").and_then(|v| v.as_bool()).unwrap_or(true) {
                    open_url(&url)?;
                } else {
                    // "Access in background": plain GET request, result only logged.
                    if input::dry_run() {
                        tracing::info!("DRYRUN http GET {url}");
                    } else {
                        let shared = self.self_ref.as_ref().and_then(|w| w.upgrade());
                        spawn_task(async move {
                            let r = crate::net::http().get(&url).send().await;
                            let msg = match r {
                                Ok(r) if r.status().is_success() => None,
                                Ok(r) => Some(crate::tr!("Website {url}: HTTP {status}", url = url, status = r.status())),
                                Err(e) => Some(crate::tr!("Website {url}: {error}", url = url, error = e)),
                            };
                            if let (Some(m), Some(sh)) = (msg, shared) {
                                let _ = tokio::task::spawn_blocking(move || sh.lock().log("warn", m)).await;
                            }
                        });
                    }
                }
            }
            (MULTIMEDIA, true) => {
                let idx = s.get("actionIdx").and_then(|v| v.as_i64()).unwrap_or(1);
                self.inject(ctx, input::Job::Media(idx));
            }
            (TEXT, true) => {
                let text = sv("pastedText");
                let typing = s.get("isTypingMode").and_then(|v| v.as_bool()).unwrap_or(false);
                let enter = s.get("isSendingEnter").and_then(|v| v.as_bool()).unwrap_or(false);
                self.inject(ctx, input::Job::Text { text, paste: !typing, enter });
            }
            (BRIGHTNESS, true) => {
                let idx = s.get("actionIdx").and_then(|v| v.as_i64()).unwrap_or(0) as usize;
                let cur = self.devices.get(&dev).map(|d| d.brightness as i32).unwrap_or(70);
                let b = match BRIGHTNESS_ACTIONS.get(idx) {
                    Some((_, d)) if idx < 2 => (cur + d).clamp(0, 100),
                    Some(_) if idx == BRIGHTNESS_CYCLE => BRIGHTNESS_STEPS.into_iter().find(|&s| s > cur).unwrap_or(BRIGHTNESS_STEPS[0]),
                    Some((_, v)) => *v,
                    None => cur,
                };
                self.set_brightness(&dev, b as u8);
            }
            (SLEEP, true) => {
                if let Some(d) = self.devices.get_mut(&dev) {
                    d.sleeping = true;
                }
                self.send_dev(&dev, DevCmd::Brightness(0));
                self.log("info", crate::tr!("{device}: sleeping (the next key press wakes the device)", device = dev));
                self.emit(json!({"event": "state"}));
            }
            (PAGE_NEXT, true) => self.page_step(&dev, 1),
            (PAGE_PREV, true) => self.page_step(&dev, -1),
            (PAGE_GOTO, true) => {
                let n = s.get("PageIndex").and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize;
                let (pages, _) = self.top_pages(&dev);
                if let Some(p) = pages.get(n - 1).or(pages.last()) {
                    self.goto_page(&dev, &p.clone());
                }
            }
            (OPEN_CHILD, true) => {
                let target = sv("ProfileUUID").to_lowercase();
                self.goto_page(&dev, &target);
            }
            (BACK_TO_PARENT, true) => {
                let parent = self.devices.get_mut(&dev).and_then(|d| d.stack.pop());
                if let Some(p) = parent {
                    let (stack, profile) = {
                        let d = &self.devices[&dev];
                        (d.stack.clone(), d.profile.clone())
                    };
                    let d = self.devices.get_mut(&dev).unwrap();
                    d.page = p;
                    d.stack = stack;
                    let _ = profile;
                    self.page_changed(&dev);
                }
            }
            (SWITCH_PROFILE, true) => {
                let target = sv("ProfileUUID");
                let id = self.profiles.keys().find(|k| k.eq_ignore_ascii_case(&target)).cloned().with_context(|| crate::tr!("profile not found"))?;
                if !self.profile_fits(&dev, &id) {
                    bail!(crate::tr!("The profile was made for a different device model"));
                }
                self.set_profile(&dev, &id);
            }
            (PLAY_AUDIO, _) => {
                let mode = s.get("actionType").and_then(|v| v.as_i64()).unwrap_or(0);
                let rt = self.runtime.entry(ctx.to_string()).or_default();
                rt.audio.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
                match (mode, down) {
                    (2, false) => {
                        for mut c in rt.audio.drain(..) {
                            let _ = c.kill();
                        }
                    }
                    (_, true) => {
                        if mode == 0 && !rt.audio.is_empty() {
                            for mut c in rt.audio.drain(..) {
                                let _ = c.kill();
                            }
                        } else {
                            let path = sv("path");
                            if path.is_empty() {
                                bail!(crate::tr!("no audio file set"));
                            }
                            let vol = s.get("volume").and_then(|v| v.as_f64()).unwrap_or(100.0);
                            let child = play_audio(&path, vol)?;
                            self.runtime.entry(ctx.to_string()).or_default().audio.push(child);
                        }
                    }
                    _ => {}
                }
            }
            (STOP_AUDIO, true) => {
                if input::dry_run() {
                    tracing::info!("DRYRUN stop_audio");
                }
                for r in self.runtime.values_mut() {
                    for mut c in r.audio.drain(..) {
                        let _ = c.kill();
                    }
                }
            }
            (TIMER, false) => {
                let secs = s.get("seconds").and_then(|v| v.as_u64()).unwrap_or(300);
                let rt = self.runtime.entry(ctx.to_string()).or_default();
                if held >= Duration::from_millis(600) {
                    rt.timer = None;
                } else {
                    let t = rt.timer.get_or_insert(TimerState { total: Duration::from_secs(secs), elapsed: Duration::ZERO, started: None });
                    match t.started.take() {
                        Some(st) => t.elapsed += st.elapsed(),
                        None => t.started = Some(Instant::now()),
                    }
                }
                self.refresh_context(ctx);
            }
            (MULTI | MULTI_RANDOM | MULTI_SWITCH, true) => {
                let lists = multi_lists(inst);
                let li = if inst.uuid == MULTI_SWITCH { inst.state.min(lists.len().saturating_sub(1)) } else { 0 };
                let mut list: Vec<(usize, ActionInstance)> = lists.get(li).cloned().unwrap_or_default().into_iter().enumerate().collect();
                if inst.uuid == MULTI_RANDOM && !list.is_empty() {
                    let i = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos() as usize) % list.len();
                    list = vec![list.swap_remove(i)];
                }
                if inst.uuid == MULTI_SWITCH {
                    let n = inst.states.len().max(2);
                    self.with_instance(loc, |i| i.state = (i.state + 1) % n);
                    self.refresh_context(ctx);
                }
                let shared = self.self_ref.as_ref().and_then(|w| w.upgrade()).context("engine not shared")?;
                let base = loc.clone();
                spawn_task(async move {
                    for (ei, sub) in list {
                        if sub.uuid == MULTI_DELAY {
                            let ms = sub.settings.get("delay").and_then(|v| v.as_u64()).unwrap_or(500);
                            tokio::time::sleep(Duration::from_millis(ms)).await;
                            continue;
                        }
                        let loc = Loc { nested: Some((li, ei)), ..base.clone() };
                        let sh = shared.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let mut e = sh.lock();
                            e.trigger(&loc, &sub, true, Duration::ZERO);
                            e.trigger(&loc, &sub, false, Duration::ZERO);
                        })
                        .await;
                        tokio::time::sleep(Duration::from_millis(30)).await;
                    }
                });
            }
            (KEY_LOGIC, _) => self.key_logic_input(loc, down),
            (DIGITAL_TIME | PAGINATION | PAGE_INDICATOR | MULTI_DELAY, _) => {}
            (u, true) if builtin::def(u).is_none() && self.registry.plugin_for_action(u).map(|p| p.runtime == PluginRuntime::Native).unwrap_or(false) => {
                bail!(crate::tr!("this action of the official app is not available on Linux"));
            }
            (u, true) if self.registry.plugin_for_action(u).is_none() && crate::compat::spotify_command(u).is_some() => {
                self.run_spotify(crate::compat::spotify_command(u).unwrap())?;
            }
            (u, true) if self.registry.plugin_for_action(u).is_none() => bail!(crate::tr!("the plugin for {action} is not installed", action = u)),
            (u, true) if matches!(self.registry.plugin_for_action(u).map(|p| &p.runtime), Some(PluginRuntime::Unsupported(_))) => {
                bail!(crate::tr!("the plugin does not run on Linux"))
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_logic_storage() {
        // Shape of a Key Logic key in an official default profile (Galleon 100 SD).
        let mut inst: ActionInstance = serde_json::from_value(json!({
            "ActionID": "633fada3-2709-460d-b747-921891168d2f", "Name": "Key Logic", "Settings": {}, "State": 0, "States": [{}],
            "UUID": KEY_LOGIC,
            "Actions": [
                {"ActionID": "a", "Name": "Go to Page", "Settings": {"PageIndex": 5}, "State": 0, "States": [{}], "UUID": PAGE_GOTO},
                {"Actions": []},
                {"ActionID": "b", "Name": "Go to Page", "Settings": {"PageIndex": 1}, "State": 0, "States": [{}], "UUID": PAGE_GOTO}
            ]
        }))
        .unwrap();
        let lists = multi_lists(&inst);
        assert_eq!(lists.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 0, 1]);
        assert_eq!(lists[2][0].settings["PageIndex"], 1);
        assert_eq!(nested_entry_mut(&mut inst, 2, 0).unwrap()["ActionID"], "b");
        assert!(nested_entry_mut(&mut inst, 1, 0).is_none());
        // Writing back keeps the format; extra entries per slot are dropped.
        let back: Vec<Vec<Value>> = lists.iter().map(|l| l.iter().map(|a| serde_json::to_value(a).unwrap()).collect()).collect();
        let mut two = back.clone();
        two[1] = vec![back[0][0].clone(), back[2][0].clone()];
        set_multi_lists(&mut inst, two);
        let stored = inst.extra["Actions"].as_array().unwrap();
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[1]["ActionID"], "a");
        set_multi_lists(&mut inst, vec![vec![]]);
        assert_eq!(inst.extra["Actions"], json!([{"Actions": []}, {"Actions": []}, {"Actions": []}]));
    }
}
