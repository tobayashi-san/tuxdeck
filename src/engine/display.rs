//! What the decks show: key specs, info bar, touch strip, refresh, periodic work and
//! previews for the UI.

use super::*;

pub enum PreviewJob {
    Info(InfoContent, Option<std::path::PathBuf>),
    Svg(String, u32, u32),
    Key(Box<KeySpec>, u32),
}

impl Engine {
    /// Devices currently showing a page.
    pub(super) fn devices_showing(&self, profile: &str, page: &str) -> Vec<String> {
        self.devices.values().filter(|d| d.profile == profile && d.page == page).map(|d| d.id.clone()).collect()
    }

    pub fn page_modified(&mut self, profile: &str, page: &str) {
        for d in self.devices_showing(profile, page) {
            self.update_visibility(&d);
            self.refresh(&d);
        }
        self.emit(json!({"event": "page", "profile": profile, "page": page}));
    }

    pub fn refresh_context(&mut self, ctx: &str) {
        if let Some(loc) = self.contexts.get(ctx).cloned() {
            if self.devices.get(&loc.device).is_some_and(|d| d.profile == loc.profile && d.page == loc.page) {
                self.refresh(&loc.device);
            }
            self.emit(json!({"event": "page", "profile": loc.profile, "page": loc.page}));
        }
    }

    pub fn refresh_all(&mut self) {
        let ids: Vec<String> = self.devices.keys().cloned().collect();
        for id in ids {
            self.refresh(&id);
        }
    }

    /// Push changed key/screen images and touch point colors to the device.
    pub fn refresh(&mut self, dev: &str) {
        let Some(d) = self.devices.get(dev) else { return };
        if d.tx.is_none() || d.sleeping {
            return;
        }
        let model = d.model;
        let (profile, page) = (d.profile.clone(), d.page.clone());
        let mut cmds = Vec::new();
        let mut sent = d.sent.clone();
        let mut animated = false;
        if model.key_px > 0 {
            for i in 0..model.key_count() {
                let slot = Slot::new("Keypad", &format!("{},{}", i % model.cols as usize, i / model.cols as usize));
                let mut spec = self.slot_spec(dev, &profile, &page, &slot);
                if let Some(img) = &spec.image {
                    let (frame, ticking) = render::animation_frame(img);
                    animated |= ticking;
                    if let Some(f) = frame {
                        spec.image = Some(f);
                    }
                }
                let h = spec_hash(&spec);
                if sent.get(&slot.key()) != Some(&h) {
                    sent.insert(slot.key(), h);
                    cmds.push(DevCmd::Key(i as u8, Box::new(spec)));
                }
            }
        }
        if model.kind == Kind::Neo {
            let (key, content) = self.info_bar(dev, &profile, &page);
            if sent.get("screen") != Some(&key) {
                sent.insert("screen".into(), key);
                cmds.push(DevCmd::Screen(Box::new(content), self.paths.layouts()));
            }
            for t in 0..model.touch_points {
                let color = self.touch_color(dev, t);
                let h = spec_hash(&color);
                let k = format!("touch{t}");
                if sent.get(&k) != Some(&h) {
                    sent.insert(k, h);
                    cmds.push(DevCmd::Touch(t, color));
                }
            }
        }
        if let (Some((sw, _)), true) = (model.screen, model.encoders > 0) {
            let seg_w = sw / model.encoders as u32;
            for i in 0..model.encoders as usize {
                let svg = self.strip_svg(dev, &profile, &page, i);
                let h = spec_hash(&svg);
                let k = format!("strip{i}");
                if sent.get(&k) != Some(&h) {
                    sent.insert(k, h);
                    cmds.push(DevCmd::Strip { x: (i as u32 * seg_w) as u16, w: seg_w as u16, svg });
                }
            }
        }
        if let Some(d) = self.devices.get_mut(dev) {
            d.sent = sent;
            d.animated = animated;
        }
        for c in cmds {
            self.send_dev(dev, c);
        }
    }

    pub fn touch_color(&self, dev: &str, t: u8) -> [u8; 3] {
        let Some(d) = self.devices.get(dev) else { return [0, 0, 0] };
        let coord = if t == 0 { NEO_LEFT } else { NEO_RIGHT };
        let page = self.profiles.get(&d.profile).and_then(|p| p.pages.get(&d.page));
        if page.and_then(|p| p.action("Neo", coord)).is_some() {
            return parse_color(&self.settings.touch_color);
        }
        let (pages, idx) = self.top_pages(dev);
        let lit = match idx {
            Some(i) => {
                if t == 0 {
                    i > 0
                } else {
                    i + 1 < pages.len()
                }
            }
            None => false,
        };
        if lit {
            parse_color(&self.settings.touch_color)
        } else {
            [0, 0, 0]
        }
    }

    /// Info bar content and its change key. Rendering is done by the caller only
    /// when the key changed.
    pub(super) fn info_bar(&self, dev: &str, profile: &str, page: &str) -> (u64, InfoContent) {
        let pm = self.profiles.get(profile).and_then(|p| p.pages.get(page));
        let inst = pm.and_then(|p| p.action("Neo", NEO_INFOBAR));
        let pages = self.profiles.get(profile).map(|p| p.manifest.pages.pages.clone()).unwrap_or_default();
        let idx = pages.iter().position(|p| p == page);
        let page_info = || PageInfo {
            number: idx.map(|i| i + 1).unwrap_or(0),
            count: pages.len(),
            name: pm.map(|p| p.name.clone()).unwrap_or_default(),
            icon: pm.filter(|p| !p.icon.is_empty()).and_then(|p| self.resolve_page_icon(profile, page, &p.icon)),
        };
        match inst {
            Some(a) if a.uuid == DIGITAL_TIME => {
                let now = chrono::Local::now();
                let key = spec_hash(&(a.settings.to_string(), now.format("%Y%m%d%H%M").to_string(), crate::i18n::current()));
                (key, InfoContent::Clock { settings: a.settings.clone() })
            }
            Some(a) if a.uuid == PAGINATION => {
                let pi = page_info();
                let key = spec_hash(&(a.settings.to_string(), pi.number, pi.count, pi.name.clone(), page));
                (key, InfoContent::Pagination { settings: a.settings.clone(), page: pi })
            }
            Some(a) => {
                let ctx = context_id(dev, &a.action_id);
                let rt = self.runtime.get(&ctx);
                let t = rt.and_then(|r| r.title(a.state)).or_else(|| a.current_state().and_then(|s| s.title.clone())).unwrap_or_default();
                match rt.and_then(|r| r.layout.as_ref().map(|l| (l, &r.feedback))) {
                    // Plugins draw the info bar like a Plus touch strip segment: a
                    // 232x50 layout, centered on the 248x58 screen.
                    Some((layout, values)) => {
                        let plugin_dir = self.registry.plugin_for_action(&a.uuid).map(|p| p.dir.clone());
                        let defaults: Map<String, Value> = [("title".to_string(), json!(t))].into_iter().collect();
                        let inner = strip::feedback_svg(
                            &strip::Segment {
                                layout,
                                values,
                                defaults: &defaults,
                                title_style: None,
                                show_title: true,
                                background: None,
                                plugin_dir: plugin_dir.as_deref(),
                            },
                            232,
                            50,
                        );
                        // Nested <svg> element, not <image>: usvg drops images inside image-embedded SVGs.
                        let inner = inner.replacen("<svg ", r#"<svg x="8" y="4" "#, 1);
                        let svg = format!(
                            r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="248" height="58"><rect width="248" height="58" fill="#000"/>{inner}</svg>"##
                        );
                        (spec_hash(&svg), InfoContent::Svg(svg))
                    }
                    None => (spec_hash(&("text", &t)), InfoContent::Text(t)),
                }
            }
            None if idx.is_none() => {
                // Inside a folder: show the folder's title.
                let t = self.folder_title(profile, page);
                (spec_hash(&("folder", &t)), InfoContent::Text(t))
            }
            None if pages.len() > 1 => {
                // Default: page indicator when there are several pages.
                let pi = page_info();
                let key = spec_hash(&("default", pi.number, pi.count, pi.name.clone(), page));
                (key, InfoContent::Pagination { settings: json!({"layout": "pagination_01", "theme": "black"}), page: pi })
            }
            None => (0, InfoContent::Black),
        }
    }

    pub(super) fn folder_title(&self, profile: &str, page: &str) -> String {
        let Some(p) = self.profiles.get(profile) else { return crate::i18n::tr("Folder") };
        p.parent_of(page)
            .and_then(|parent| {
                p.pages.get(&parent)?.iter_actions().find_map(|(_, _, a)| {
                    (a.uuid == OPEN_CHILD && a.settings.get("ProfileUUID").and_then(|v| v.as_str()).map(|s| s.eq_ignore_ascii_case(page)).unwrap_or(false))
                        .then(|| a.current_state().and_then(|s| s.title.clone()).unwrap_or_default())
                })
            })
            .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| crate::i18n::tr("Folder"))
    }

    pub(super) fn resolve_page_icon(&self, profile: &str, page: &str, icon: &str) -> Option<ImageData> {
        let p = self.profiles.get(profile)?;
        for base in [p.page_dir(page), p.dir.clone()] {
            if let Some(d) = ImageData::from_file(&base.join(icon)) {
                return Some(d);
            }
        }
        None
    }

    /// Rendering description of a slot (also used for UI previews).
    pub fn slot_spec(&self, dev: &str, profile: &str, page: &str, slot: &Slot) -> KeySpec {
        self.slot_spec_state(dev, profile, page, slot, None)
    }

    pub fn slot_spec_state(&self, dev: &str, profile: &str, page: &str, slot: &Slot, state: Option<usize>) -> KeySpec {
        let Some(inst) = self.profiles.get(profile).and_then(|p| p.pages.get(page)).and_then(|p| p.action(&slot.controller, &slot.coord)) else {
            return KeySpec::default();
        };
        let ctx = context_id(dev, &inst.action_id);
        let rt = self.runtime.get(&ctx);
        let state_idx = state.unwrap_or(inst.state).min(inst.states.len().saturating_sub(1));
        let st = inst.states.get(state_idx).cloned().unwrap_or_default();
        let plugin = self.registry.plugin_for_action(&inst.uuid);
        let mdef = plugin.and_then(|p| p.action(&inst.uuid));
        let mstate = mdef.and_then(|a| a.states.get(state_idx).or(a.states.first()));

        let mut spec = KeySpec { ..Default::default() };
        // User-chosen key background, shown behind the (possibly transparent) image.
        spec.background = st.background.clone().filter(|s| !s.is_empty());
        // Image precedence: plugin override > user image > built-in variant > manifest default.
        spec.image = rt
            .and_then(|r| r.images.get(&Some(state_idx)).or(r.images.get(&None)).cloned())
            .or_else(|| {
                let img = st.image.as_deref().filter(|s| !s.is_empty())?;
                if img.starts_with("data:") {
                    return ImageData::from_data_url(img);
                }
                let p = self.profiles.get(profile)?;
                ImageData::from_file(&p.page_dir(page).join(img)).or_else(|| ImageData::from_file(&p.dir.join(img)))
            })
            .or_else(|| {
                let pl = plugin?;
                let rel = builtin::variant_image(&inst.uuid, &inst.settings)?;
                plugins::resolve_image(&pl.dir, rel).and_then(|p| ImageData::from_file(&p))
            })
            .or_else(|| {
                let pl = plugin?;
                let rel = mstate.and_then(|s| s.image.clone()).or_else(|| mdef.and_then(|a| a.icon.clone()))?;
                plugins::resolve_image(&pl.dir, &rel).and_then(|p| ImageData::from_file(&p))
            });

        // Title: setTitle (until the plugin resets it) > user title > manifest state title.
        let user_title = st.title.clone().filter(|t| !t.is_empty());
        let mut text = rt.and_then(|r| r.title(state_idx)).or(user_title).or_else(|| mstate.and_then(|s| s.title.clone())).unwrap_or_default();
        match inst.uuid.as_str() {
            PAGE_INDICATOR => {
                let (_, idx) = self.top_pages(dev);
                spec.big_text = Some(idx.map(|i| (i + 1).to_string()).unwrap_or_else(|| "–".into()));
                spec.image = None;
            }
            TIMER => {
                let secs = inst.settings.get("seconds").and_then(|v| v.as_u64()).unwrap_or(300);
                let rem = rt.and_then(|r| r.timer.as_ref()).map(|t| t.remaining().as_secs()).unwrap_or(secs);
                text = format!("{:02}:{:02}", rem / 60, rem % 60);
            }
            _ => {}
        }
        let show = st.show_title.or(mstate.and_then(|s| s.show_title)).unwrap_or(true);
        if show && !text.is_empty() {
            let style = st.font_style.clone().or(mstate.and_then(|s| s.font_style.clone())).unwrap_or_default();
            spec.title = Some(Title {
                text,
                align: st.title_alignment.clone().or(mstate.and_then(|s| s.title_alignment.clone())).unwrap_or_else(|| "bottom".into()),
                size: st
                    .font_size
                    .map(|f| f as u32)
                    .or_else(|| {
                        mstate.and_then(|s| s.font_size.as_ref()).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).map(|f| f as u32)
                    })
                    .unwrap_or(12),
                color: st.title_color.clone().or(mstate.and_then(|s| s.title_color.clone())).unwrap_or_else(|| "#ffffff".into()),
                family: st.font_family.clone().unwrap_or_default(),
                bold: style.contains("Bold"),
                italic: style.contains("Italic"),
                underline: st.font_underline.unwrap_or(false),
                outline: st.outline_thickness.map(|o| o as u32).unwrap_or(2),
            });
        }
        if spec.image.is_none() && spec.big_text.is_none() {
            if let Some(d) = builtin::def(&inst.uuid) {
                spec.image = Some(ImageData::svg(render::fallback_icon_svg(d.glyph, d.color)));
            } else if spec.background.is_none() {
                spec.background = Some("#202020".into());
            }
        }
        if let Some((o, t)) = rt.and_then(|r| r.overlay) {
            if t.elapsed() < Duration::from_millis(1500) {
                spec.overlay = Some(o);
            }
        }
        spec
    }

    /// Preview for the UI. Expensive rasterization happens in `PreviewJob::render`
    /// so the engine lock is not held while rendering.
    pub fn preview_job(&self, profile: &str, page: &str, slot: &Slot, size: u32, state: Option<usize>) -> PreviewJob {
        let dev = self
            .devices
            .values()
            .find(|d| d.profile == profile && d.page == page)
            .or_else(|| self.devices.values().find(|d| d.profile == profile))
            .map(|d| d.id.clone())
            .unwrap_or_default();
        if slot.controller == "Neo" {
            if slot.coord == NEO_INFOBAR {
                let (_, content) = self.info_bar(&dev, profile, page);
                return PreviewJob::Info(content, self.paths.layouts());
            }
            let has = self.profiles.get(profile).and_then(|p| p.pages.get(page)).and_then(|p| p.action("Neo", &slot.coord)).is_some();
            if !has {
                let t = if slot.coord == NEO_LEFT { 0 } else { 1 };
                let c = if dev.is_empty() { [90, 90, 90] } else { self.touch_color(&dev, t) };
                let svg = format!(
                    r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64"><circle cx="32" cy="32" r="22" fill="#{:02x}{:02x}{:02x}" stroke="#555" stroke-width="3"/></svg>"##,
                    c[0], c[1], c[2]
                );
                return PreviewJob::Svg(svg, 64, 64);
            }
        }
        if slot.controller == "Encoder" {
            let idx = slot.col_row().0 as usize;
            return PreviewJob::Svg(self.strip_svg(&dev, profile, page, idx), strip::SEG_W, strip::SEG_H);
        }
        PreviewJob::Key(Box::new(self.slot_spec_state(&dev, profile, page, slot, state)), size)
    }

    /// SVG of the touch strip segment above dial `idx` (black when unassigned).
    pub fn strip_svg(&self, dev: &str, profile: &str, page: &str, idx: usize) -> String {
        let coord = format!("{idx},0");
        let empty = Value::Object(Map::new());
        let Some(inst) = self.profiles.get(profile).and_then(|p| p.pages.get(page)).and_then(|p| p.action("Encoder", &coord)) else {
            let l = json!({"items": []});
            return strip::segment_svg(&strip::Segment {
                layout: &l,
                values: empty.as_object().unwrap(),
                defaults: empty.as_object().unwrap(),
                title_style: None,
                show_title: false,
                background: None,
                plugin_dir: None,
            });
        };
        let ctx = context_id(dev, &inst.action_id);
        let rt = self.runtime.get(&ctx);
        let st = inst.current_state().cloned().unwrap_or_default();
        let plugin = self.registry.plugin_for_action(&inst.uuid);
        let mdef = plugin.and_then(|p| p.action(&inst.uuid));
        let enc = mdef.and_then(|a| a.encoder.clone()).unwrap_or(Value::Null);
        let enc_str = |k: &str| enc.get(k).or_else(|| enc.get(k.to_lowercase())).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
        let plugin_dir = plugin.map(|p| p.dir.clone());
        let mut defaults = Map::new();
        // Title: setTitle > user title > manifest state title > action name.
        let name = builtin::def(&inst.uuid)
            .map(|d| crate::i18n::tr(d.name))
            .or_else(|| plugin.zip(mdef).map(|(p, a)| p.action_name(a)))
            .unwrap_or_else(|| inst.name.clone());
        let title = rt
            .and_then(|r| r.title(inst.state))
            .or_else(|| st.title.clone().filter(|t| !t.is_empty()))
            .or_else(|| mdef.and_then(|a| a.states.get(inst.state).or(a.states.first())).and_then(|s| s.title.clone()).filter(|t| !t.is_empty()))
            .unwrap_or(name);
        defaults.insert("title".into(), json!(title));
        // Icon: setImage > user image > Encoder.Icon > state image > action icon.
        let user_img = st.image.as_deref().filter(|s| !s.is_empty()).and_then(|img| {
            if img.starts_with("data:") {
                return ImageData::from_data_url(img);
            }
            let p = self.profiles.get(profile)?;
            ImageData::from_file(&p.page_dir(page).join(img)).or_else(|| ImageData::from_file(&p.dir.join(img)))
        });
        let from_plugin =
            |rel: Option<String>| -> Option<ImageData> { plugins::resolve_image(plugin_dir.as_deref()?, &rel?).and_then(|p| ImageData::from_file(&p)) };
        let icon = rt
            .and_then(|r| r.images.get(&Some(inst.state)).or(r.images.get(&None)).cloned())
            .or(user_img)
            .or_else(|| from_plugin(enc_str("Icon")))
            .or_else(|| from_plugin(builtin::variant_image(&inst.uuid, &inst.settings).map(str::to_string)))
            .or_else(|| from_plugin(mdef.and_then(|a| a.states.get(inst.state).or(a.states.first())).and_then(|s| s.image.clone())))
            .or_else(|| from_plugin(mdef.and_then(|a| a.icon.clone())))
            .or_else(|| builtin::def(&inst.uuid).map(|d| ImageData::svg(render::fallback_icon_svg(d.glyph, d.color))));
        if let Some(i) = icon {
            let href = i.to_href();
            for k in ["icon", "icon1"] {
                defaults.insert(k.into(), json!(href));
            }
        }
        let mut layout_ref = enc_str("layout").unwrap_or_else(|| "$X1".into());
        if inst.uuid == BRIGHTNESS {
            let b = self.devices.get(dev).map(|d| d.brightness).unwrap_or(70);
            layout_ref = "$B1".into();
            defaults.insert("value".into(), json!(format!("{b} %")));
            defaults.insert("indicator".into(), json!(b));
        }
        let layout = rt
            .and_then(|r| r.layout.clone())
            .or_else(|| strip::load_layout(&layout_ref, plugin_dir.as_deref()))
            .unwrap_or_else(|| strip::builtin_layout("$X1").unwrap());
        let background = from_plugin(enc_str("background"));
        let title_style = (st.font_size.is_some() || st.title_color.is_some() || st.font_style.is_some() || st.font_family.is_some()).then(|| {
            let style = st.font_style.clone().unwrap_or_default();
            Title {
                text: String::new(),
                align: String::new(),
                size: st.font_size.map(|f| f as u32).unwrap_or(12),
                color: st.title_color.clone().unwrap_or_else(|| "#ffffff".into()),
                family: st.font_family.clone().unwrap_or_default(),
                bold: style.contains("Bold") || st.font_style.is_none(),
                italic: style.contains("Italic"),
                underline: false,
                outline: 0,
            }
        });
        let values = rt.map(|r| r.feedback.clone()).unwrap_or_default();
        strip::segment_svg(&strip::Segment {
            layout: &layout,
            values: &values,
            defaults: &defaults,
            title_style: title_style.as_ref(),
            show_title: st.show_title.unwrap_or(true),
            background: background.as_ref(),
            plugin_dir: plugin_dir.as_deref(),
        })
    }

    /// Advance animated GIFs (called often; only decks showing one are refreshed).
    pub fn animate(&mut self) {
        let ids: Vec<String> = self.devices.values().filter(|d| d.animated && !d.sleeping).map(|d| d.id.clone()).collect();
        for id in ids {
            self.refresh(&id);
        }
    }

    pub fn has_animations(&self) -> bool {
        self.devices.values().any(|d| d.animated && !d.sleeping)
    }

    /// Periodic work: clock, timers, overlays.
    pub fn tick(&mut self) {
        // The monotonic clock stands still while the system sleeps, the wall clock doesn't.
        let (mono, wall) = (Instant::now(), std::time::SystemTime::now());
        let slept = wall.duration_since(self.clocks.1).unwrap_or_default().saturating_sub(mono.duration_since(self.clocks.0));
        self.clocks = (mono, wall);
        if slept > Duration::from_secs(10) {
            tracing::info!("system woke up (slept {} s)", slept.as_secs());
            self.broadcast_plugins(|_| Some(json!({"event": "systemDidWakeUp"})));
            // The decks may have been reset while suspended: send everything again.
            for d in self.devices.values_mut() {
                d.sent.clear();
            }
        }
        let expired: Vec<String> = self
            .runtime
            .iter_mut()
            .filter_map(|(ctx, r)| match r.overlay {
                Some((_, t)) if t.elapsed() >= Duration::from_millis(1500) => {
                    r.overlay = None;
                    Some(ctx.clone())
                }
                _ => None,
            })
            .collect();
        let mut finished = Vec::new();
        for (ctx, r) in self.runtime.iter_mut() {
            if let Some(t) = &r.timer {
                if t.started.is_some() && t.remaining().is_zero() {
                    finished.push(ctx.clone());
                }
            }
            r.audio.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        }
        for ctx in finished {
            let alarm = self.lookup(&ctx).and_then(|(_, i)| i.settings.get("alarm").and_then(|v| v.as_str()).map(str::to_string)).filter(|s| !s.is_empty());
            if let Some(r) = self.runtime.get_mut(&ctx) {
                r.timer = None;
                r.overlay = Some((Overlay::Alert, Instant::now()));
                if let Some(a) = alarm {
                    if let Ok(c) = play_audio(&a, 100.0) {
                        r.audio.push(c);
                    }
                }
            }
        }
        let _ = expired;
        self.refresh_all();
    }
}
