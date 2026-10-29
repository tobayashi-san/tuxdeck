//! Converting profiles from the official app on Windows/macOS into regular Linux
//! profiles. For each action whose plugin is missing this decides whether the same
//! plugin can be installed from the catalogue, a built-in action, another plugin or a
//! keyboard shortcut does the same, or the key needs a command of the user's own.

use serde_json::{json, Value};

/// What a key turns into when its plugin is replaced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum To {
    /// The built-in Multimedia action with this `actionIdx`.
    Media(u8),
    /// An action of the rule's replacement plugin (suffix after its id).
    Action(&'static str),
    /// A keyboard shortcut: evdev code, Qt key code, Ctrl, Shift.
    Hotkey(u32, u32, bool, bool),
}

pub struct Rule {
    pub group: &'static str,
    pub name: &'static str,
    /// Action UUIDs starting with this belong to the rule.
    pub prefix: &'static str,
    /// Catalogue plugin that provides the replacement actions.
    pub plugin: Option<(&'static str, &'static str)>,
    /// Action suffix (after `prefix`) → replacement. Unlisted actions have no counterpart.
    pub map: &'static [(&'static str, To)],
}

const OBS: (&str, &str) = ("de.felitendo.obs-tools", "OBS Tools");
const DISCORD: (&str, &str) = ("me.amankhanna.oadiscord", "Discord");
const SYSTEM: (&str, &str) = ("me.amankhanna.oasystem", "System Information");
const MPRIS: (&str, &str) = ("me.amankhanna.oampris", "Linux Media");

pub const RULES: &[Rule] = &[
    Rule {
        group: "spotify",
        name: "Spotify (BarRaider)",
        prefix: "com.barraider.spotify",
        plugin: Some(MPRIS),
        map: &[
            ("playpause", To::Media(1)),
            ("next", To::Media(2)),
            ("prev", To::Media(0)),
            ("volup", To::Media(5)),
            ("voldown", To::Media(6)),
            ("voltoggle", To::Media(4)),
            ("shuffle", To::Action("shuffle")),
            ("repeat", To::Action("repeat")),
        ],
    },
    Rule {
        group: "obs",
        name: "OBS Studio (Elgato)",
        prefix: "com.elgato.obsstudio.",
        plugin: Some(OBS),
        map: &[
            ("record", To::Action("recordtoggle")),
            ("stream", To::Action("streamtoggle")),
            ("scene", To::Action("smartscene")),
            ("source", To::Action("sourcevisibility")),
            ("mixeraudio", To::Action("mutetoggle")),
            ("scenecollection", To::Action("profile")),
            ("profile", To::Action("profile")),
            ("replaybuffer", To::Action("instantreplay")),
            ("studiomode", To::Action("studiomode")),
            ("virtualcamera", To::Action("virtualcam")),
            ("screenshot", To::Action("screenshot")),
            ("filter", To::Action("filtertoggle")),
        ],
    },
    Rule {
        group: "discord",
        name: "Discord (Elgato)",
        prefix: "com.elgato.discord.",
        plugin: Some(DISCORD),
        map: &[
            ("mute", To::Action("togglemute")),
            ("deafen", To::Action("toggledeafen")),
            ("pushto.mute", To::Action("pushtomute")),
            ("pushto.talk", To::Action("pushtotalk")),
            ("volumecontrolbutton", To::Action("volumecontrol")),
            ("volumecontrol", To::Action("volumecontrol")),
            ("soundboard", To::Action("soundboard")),
            ("notifications", To::Action("notifications")),
            ("channel.text", To::Action("textchannel")),
            ("channel.voice", To::Action("voicechannel")),
        ],
    },
    Rule {
        group: "teams",
        name: "Microsoft Teams",
        prefix: "com.microsoft.teams.button.",
        plugin: None,
        // Teams' own shortcuts (desktop app and teams.microsoft.com); Teams needs the focus.
        map: &[
            ("togglemute", To::Hotkey(50, 77, true, true)),
            ("togglevideo", To::Hotkey(24, 79, true, true)),
            ("leavecall", To::Hotkey(35, 72, true, true)),
            ("togglehand", To::Hotkey(37, 75, true, true)),
        ],
    },
    Rule { group: "cpu", name: "CPU (Elgato)", prefix: "com.elgato.cpu.", plugin: Some(SYSTEM), map: &[("cpu", To::Action("cpu"))] },
    Rule { group: "twitch", name: "Twitch (Elgato)", prefix: "com.elgato.twitch.", plugin: None, map: &[] },
    Rule { group: "youtube", name: "YouTube (Elgato)", prefix: "com.elgato.youtube.", plugin: None, map: &[] },
    Rule { group: "youtube-stats", name: "YouTube statistics", prefix: "com.nefrit.youtube.", plugin: None, map: &[] },
    Rule { group: "slack", name: "Slack (Elgato)", prefix: "com.elgato.slack.", plugin: None, map: &[] },
    Rule { group: "worldclock", name: "World Clock (Elgato)", prefix: "com.elgato.worldclock.", plugin: None, map: &[] },
    Rule { group: "stocks", name: "Stocks", prefix: "com.exension.stocks.", plugin: None, map: &[] },
];

/// How one action of a profile can be made to work.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Replace(To),
    Install,
    Unavailable,
    Unknown,
}

pub fn rule_for(uuid: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| uuid.starts_with(r.prefix))
}

/// Classify an action whose plugin is not installed. `catalogue` tells whether a
/// catalogue plugin of that id exists (its id is a prefix of the action UUID).
pub fn classify(uuid: &str, in_catalogue: bool) -> (Option<&'static Rule>, Status) {
    if let Some(r) = rule_for(uuid) {
        let suffix = &uuid[r.prefix.len()..];
        return match r.map.iter().find(|(s, _)| *s == suffix) {
            Some((_, to)) => (Some(r), Status::Replace(*to)),
            None => (Some(r), Status::Unavailable),
        };
    }
    (None, if in_catalogue { Status::Install } else { Status::Unknown })
}

/// The replacement action UUID of a rule mapping.
pub fn replacement_uuid(rule: &Rule, to: To) -> Option<String> {
    match (to, rule.plugin) {
        (To::Action(a), Some((plugin, _))) => Some(format!("{plugin}.{a}")),
        (To::Hotkey(..), _) => Some(crate::builtin::HOTKEY.to_string()),
        (To::Media(_), _) => Some(crate::builtin::MULTIMEDIA.to_string()),
        _ => None,
    }
}

/// Settings of the replacement action (built-in ones need theirs; plugin actions start
/// with none, the old settings belong to another plugin).
pub fn replacement_settings(to: To) -> Value {
    if let To::Media(idx) = to {
        return json!({"actionIdx": idx});
    }
    let To::Hotkey(linux, qt, ctrl, shift) = to else { return json!({}) };
    let hk = json!({"KeyCmd": false, "KeyCtrl": ctrl, "KeyOption": false, "KeyShift": shift,
                    "KeyModifiers": (shift as u32) | (ctrl as u32) << 1, "NativeCode": -1, "QTKeyCode": qt, "VKeyCode": qt, "LinuxCode": linux});
    let empty = crate::builtin::empty_hotkey();
    json!({"Coalesce": true, "Hotkeys": [hk, empty.clone(), empty.clone(), empty]})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(classify("com.barraider.spotifyplaypause", false).1, Status::Replace(To::Media(1)));
        assert_eq!(classify("com.barraider.spotifyshuffle", false).1, Status::Replace(To::Action("shuffle")));
        assert_eq!(classify("com.barraider.spotifylikesong", false).1, Status::Unavailable);
        assert_eq!(classify("com.elgato.obsstudio.record", false).1, Status::Replace(To::Action("recordtoggle")));
        assert_eq!(classify("com.elgato.obsstudio.record.pause", false).1, Status::Unavailable);
        assert_eq!(classify("com.elgato.twitch.marker", true).1, Status::Unavailable);
        assert_eq!(classify("com.jk.weather.action", true).1, Status::Install);
        assert_eq!(classify("com.example.thing", false).1, Status::Unknown);
        let r = rule_for("com.elgato.discord.mute").unwrap();
        assert_eq!(replacement_uuid(r, To::Action("togglemute")).unwrap(), "me.amankhanna.oadiscord.togglemute");
    }

    #[test]
    fn teams_hotkey() {
        assert_eq!(replacement_settings(To::Media(2))["actionIdx"], 2);
        let s = replacement_settings(To::Hotkey(50, 77, true, true));
        assert_eq!(s["Hotkeys"][0]["KeyModifiers"], 3);
        assert_eq!(s["Hotkeys"][0]["LinuxCode"], 50);
    }
}
