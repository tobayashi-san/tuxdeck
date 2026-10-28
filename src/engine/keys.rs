//! Input from the decks: keys, dials, touch strip, wake-up.

use super::*;

impl Engine {
    pub fn on_input(&mut self, dev: &str, input: Input) {
        let Some(d) = self.devices.get_mut(dev) else { return };
        let model = d.model;
        match input {
            Input::Buttons(states) => {
                let prev = std::mem::replace(&mut d.buttons, states.clone());
                let keys = model.key_count();
                for (i, &down) in states.iter().enumerate() {
                    if prev.get(i).copied().unwrap_or(false) == down {
                        continue;
                    }
                    let slot = if i < keys {
                        Slot::new("Keypad", &format!("{},{}", i % model.cols as usize, i / model.cols as usize))
                    } else if i == keys {
                        Slot::new("Neo", NEO_LEFT)
                    } else {
                        Slot::new("Neo", NEO_RIGHT)
                    };
                    self.on_key(dev, &slot, down);
                }
            }
            Input::EncoderPress(states) => {
                let prev = std::mem::replace(&mut d.encoders, states.clone());
                for (i, &down) in states.iter().enumerate() {
                    if prev.get(i).copied().unwrap_or(false) != down {
                        self.on_dial(dev, i, if down { "dialDown" } else { "dialUp" }, 0);
                    }
                }
            }
            Input::EncoderTurn(ticks) => {
                for (i, &t) in ticks.iter().enumerate() {
                    if t != 0 {
                        self.on_dial(dev, i, "dialRotate", t as i32);
                    }
                }
            }
            Input::Touch { x, y, kind, x2, .. } => {
                if kind == 3 {
                    if self.wake(dev) {
                        return;
                    }
                    // Swipe across the strip: change pages like the official app.
                    let dx = x2 as i32 - x as i32;
                    if dx.abs() >= 40 {
                        self.page_step(dev, if dx < 0 { 1 } else { -1 });
                    }
                    return;
                }
                let (sw, sh) = model.screen.unwrap_or((800, 100));
                let (x, y) = (x.min(sw.saturating_sub(1) as u16), y.min(sh.saturating_sub(1) as u16));
                let w = (sw / model.encoders.max(1) as u32).max(1);
                let i = ((x as u32 / w) as usize).min(model.encoders.max(1) as usize - 1);
                let rel = (x as u32 - i as u32 * w) as u16;
                self.on_touch(dev, i, rel, y, kind == 2);
            }
        }
    }

    pub(super) fn wake(&mut self, dev: &str) -> bool {
        if let Some(d) = self.devices.get_mut(dev) {
            if d.sleeping {
                d.sleeping = false;
                d.sent.clear();
                let b = d.brightness;
                self.send_dev(dev, DevCmd::Brightness(b));
                self.refresh(dev);
                self.log("info", crate::tr!("{device}: woke up", device = dev));
                self.emit(json!({"event": "state"}));
                return true;
            }
        }
        false
    }

    pub fn on_key(&mut self, dev: &str, slot: &Slot, down: bool) {
        let Some((profile, page, held)) = self.track_press(dev, slot, down) else { return };
        self.emit(json!({"event": "input", "device": dev, "controller": slot.controller, "coord": slot.coord, "down": down}));
        let inst = self.profiles.get(&profile).and_then(|p| p.pages.get(&page)).and_then(|p| p.action(&slot.controller, &slot.coord)).cloned();
        let Some(inst) = inst else {
            // Unassigned Neo touch points navigate between pages.
            if slot.controller == "Neo" && down {
                if slot.coord == NEO_LEFT {
                    self.page_step(dev, -1);
                } else if slot.coord == NEO_RIGHT {
                    self.page_step(dev, 1);
                }
            }
            return;
        };
        let loc = Loc { device: dev.into(), profile, page, slot: slot.clone(), nested: None };
        self.trigger(&loc, &inst, down, held);
    }

    /// Book-keeping for a key/dial press or release. Returns where the action lives
    /// (the page of the press for a release) and how long it was held, or `None` if
    /// the event must be ignored: a press that woke the deck, its release, or a
    /// release without a press.
    pub(super) fn track_press(&mut self, dev: &str, slot: &Slot, down: bool) -> Option<(String, String, Duration)> {
        let d = self.devices.get(dev)?;
        let (profile, page) = (d.profile.clone(), d.page.clone());
        if down {
            let swallowed = self.wake(dev);
            let press = Press { at: Instant::now(), profile: profile.clone(), page: page.clone(), swallowed };
            self.devices.get_mut(dev)?.pressed.insert(slot.clone(), press);
            (!swallowed).then_some((profile, page, Duration::ZERO))
        } else {
            let p = self.devices.get_mut(dev)?.pressed.remove(slot)?;
            (!p.swallowed).then(|| (p.profile, p.page, p.at.elapsed()))
        }
    }

    /// Press or release dial `idx` (keeps the stored encoder state consistent).
    pub fn dial_press(&mut self, dev: &str, idx: usize, down: bool) {
        let Some(d) = self.devices.get(dev) else { return };
        let mut states = d.encoders.clone();
        if idx < states.len() {
            states[idx] = down;
            self.on_input(dev, Input::EncoderPress(states));
        }
    }

    pub(super) fn on_dial(&mut self, dev: &str, idx: usize, event: &str, ticks: i32) {
        let slot = Slot::new("Encoder", &format!("{idx},0"));
        let (profile, page) = if event == "dialRotate" {
            if self.wake(dev) {
                return;
            }
            let Some(d) = self.devices.get(dev) else { return };
            (d.profile.clone(), d.page.clone())
        } else {
            let Some((profile, page, _)) = self.track_press(dev, &slot, event == "dialDown") else { return };
            self.emit(json!({"event": "input", "device": dev, "controller": "Encoder", "coord": slot.coord, "down": event == "dialDown"}));
            (profile, page)
        };
        let Some(d) = self.devices.get(dev) else { return };
        let pressed = d.encoders.get(idx).copied().unwrap_or(false);
        let Some(inst) = self.profiles.get(&profile).and_then(|p| p.pages.get(&page)).and_then(|p| p.action("Encoder", &slot.coord)).cloned() else { return };
        let ctx = context_id(dev, &inst.action_id);
        if let Some(pl) = self.plugin_of(&inst.uuid).map(|p| p.uuid.clone()) {
            let mut payload = self.action_payload(&slot, &inst, false);
            if event == "dialRotate" {
                payload["ticks"] = json!(ticks);
                payload["pressed"] = json!(pressed);
            }
            self.send_plugin(&pl, json!({"event": event, "action": inst.uuid, "context": ctx, "device": dev, "payload": payload}));
        } else if event == "dialRotate" {
            match inst.uuid.as_str() {
                BRIGHTNESS => {
                    let b = (self.devices[dev].brightness as i32 + ticks * 5).clamp(0, 100) as u8;
                    self.set_brightness(dev, b);
                }
                MULTIMEDIA => {
                    let idx = if ticks > 0 { 5 } else { 6 };
                    for _ in 0..ticks.unsigned_abs() {
                        self.input.run(input::Job::Media(idx), None);
                    }
                }
                _ => {}
            }
            self.refresh(dev);
        } else if event == "dialDown" {
            let loc = Loc { device: dev.into(), profile, page, slot, nested: None };
            self.trigger(&loc, &inst, true, Duration::ZERO);
            self.trigger(&loc, &inst, false, Duration::ZERO);
        }
    }

    pub(super) fn on_touch(&mut self, dev: &str, idx: usize, x: u16, y: u16, hold: bool) {
        if self.wake(dev) {
            return;
        }
        let Some(d) = self.devices.get(dev) else { return };
        let slot = Slot::new("Encoder", &format!("{idx},0"));
        self.emit(json!({"event": "input", "device": dev, "controller": "Encoder", "coord": slot.coord, "touch": true, "down": true}));
        let Some(inst) = self.profiles.get(&d.profile).and_then(|p| p.pages.get(&d.page)).and_then(|p| p.action("Encoder", &slot.coord)).cloned() else {
            return;
        };
        if let Some(pl) = self.plugin_of(&inst.uuid).map(|p| p.uuid.clone()) {
            let mut payload = self.action_payload(&slot, &inst, false);
            payload["tapPos"] = json!([x, y]);
            payload["hold"] = json!(hold);
            let ctx = context_id(dev, &inst.action_id);
            self.send_plugin(&pl, json!({"event": "touchTap", "action": inst.uuid, "context": ctx, "device": dev, "payload": payload}));
        } else {
            // Built-in actions: a tap works like pressing the dial.
            let loc = Loc { device: dev.into(), profile: d.profile.clone(), page: d.page.clone(), slot, nested: None };
            self.trigger(&loc, &inst, true, Duration::ZERO);
            self.trigger(&loc, &inst, false, Duration::ZERO);
        }
    }
}
