//! Making imported profiles work on Linux. Profiles from the official app often use
//! plugins that don't exist here; for each action this decides whether TuxDeck does it
//! itself, the same plugin can be installed from the catalogue, another plugin (or a
//! keyboard shortcut) does the same, or there is nothing on Linux.

use serde_json::{json, Value};

/// What a key turns into when its plugin is replaced.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum To {
    /// TuxDeck runs it without the plugin (see `spotify_command`).
    BuiltIn,
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
    /// Shown for actions without a counterpart.
    pub hint: Option<&'static str>,
}

const OBS: (&str, &str) = ("de.felitendo.obs-tools", "OBS Tools");
const DISCORD: (&str, &str) = ("me.amankhanna.oadiscord", "Discord");
const SYSTEM: (&str, &str) = ("me.amankhanna.oasystem", "System Information");

pub const RULES: &[Rule] = &[
    Rule {
        group: "spotify",
        name: "Spotify (BarRaider)",
        prefix: "com.barraider.spotify",
        plugin: None,
        map: &[
            ("playpause", To::BuiltIn),
            ("next", To::BuiltIn),
            ("prev", To::BuiltIn),
            ("shuffle", To::BuiltIn),
            ("repeat", To::BuiltIn),
            ("volup", To::BuiltIn),
            ("voldown", To::BuiltIn),
            ("voltoggle", To::BuiltIn),
        ],
        hint: Some("Liking songs and playing playlists needs the Spotify web API: \"Essentials for Spotify\" in the catalogue can do it."),
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
        hint: Some("OBS Tools has no counterpart for this action."),
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
        hint: Some("The Discord plugin has no counterpart for this action."),
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
        hint: Some("Teams has no keyboard shortcut for this."),
    },
    Rule { group: "cpu", name: "CPU (Elgato)", prefix: "com.elgato.cpu.", plugin: Some(SYSTEM), map: &[("cpu", To::Action("cpu"))], hint: None },
    Rule {
        group: "twitch",
        name: "Twitch (Elgato)",
        prefix: "com.elgato.twitch.",
        plugin: None,
        map: &[],
        hint: Some("Elgato's Twitch plugin only exists for Windows and macOS. \"Twitch Viewer Deck\" and \"Twitchat\" in the catalogue cover parts of it."),
    },
    Rule {
        group: "youtube",
        name: "YouTube (Elgato)",
        prefix: "com.elgato.youtube.",
        plugin: None,
        map: &[],
        hint: Some("Elgato's YouTube plugin only exists for Windows and macOS."),
    },
    Rule {
        group: "youtube-stats",
        name: "YouTube statistics",
        prefix: "com.nefrit.youtube.",
        plugin: None,
        map: &[],
        hint: Some("Not in the catalogue. \"YouTube Ticker\" shows channel statistics."),
    },
    Rule {
        group: "slack",
        name: "Slack (Elgato)",
        prefix: "com.elgato.slack.",
        plugin: None,
        map: &[],
        hint: Some("Elgato's Slack plugin only exists for Windows and macOS."),
    },
    Rule {
        group: "worldclock",
        name: "World Clock (Elgato)",
        prefix: "com.elgato.worldclock.",
        plugin: None,
        map: &[],
        hint: Some("Elgato's World Clock only exists for Windows and macOS. \"Clocks\" in the catalogue shows other time zones."),
    },
    Rule {
        group: "stocks",
        name: "Stocks",
        prefix: "com.exension.stocks.",
        plugin: None,
        map: &[],
        hint: Some("The Stocks plugin only ships programs for Windows and macOS."),
    },
];

/// How one action of a profile can be made to work.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    BuiltIn,
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
            Some((_, To::BuiltIn)) => (Some(r), Status::BuiltIn),
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
        _ => None,
    }
}

/// Settings of a built-in Hotkey action sending one shortcut.
pub fn hotkey_settings(to: To) -> Value {
    let To::Hotkey(linux, qt, ctrl, shift) = to else { return json!({}) };
    let hk = json!({"KeyCmd": false, "KeyCtrl": ctrl, "KeyOption": false, "KeyShift": shift,
                    "KeyModifiers": (shift as u32) | (ctrl as u32) << 1, "NativeCode": -1, "QTKeyCode": qt, "VKeyCode": qt, "LinuxCode": linux});
    let empty = crate::builtin::empty_hotkey();
    json!({"Coalesce": true, "Hotkeys": [hk, empty.clone(), empty.clone(), empty]})
}

/// playerctl arguments for the Spotify actions TuxDeck runs itself. `Loop` and `Mute`
/// depend on the current state, see the engine.
pub enum SpotifyCmd {
    Args(&'static [&'static str]),
    Loop,
    Mute,
}

pub fn spotify_command(uuid: &str) -> Option<SpotifyCmd> {
    Some(match uuid.strip_prefix("com.barraider.spotify")? {
        "playpause" => SpotifyCmd::Args(&["play-pause"]),
        "next" => SpotifyCmd::Args(&["next"]),
        "prev" => SpotifyCmd::Args(&["previous"]),
        "shuffle" => SpotifyCmd::Args(&["shuffle", "Toggle"]),
        "volup" => SpotifyCmd::Args(&["volume", "0.1+"]),
        "voldown" => SpotifyCmd::Args(&["volume", "0.1-"]),
        "repeat" => SpotifyCmd::Loop,
        "voltoggle" => SpotifyCmd::Mute,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification() {
        assert_eq!(classify("com.barraider.spotifyplaypause", false).1, Status::BuiltIn);
        assert_eq!(classify("com.barraider.spotifylikesong", false).1, Status::Unavailable);
        assert_eq!(classify("com.elgato.obsstudio.record", false).1, Status::Replace(To::Action("recordtoggle")));
        assert_eq!(classify("com.elgato.obsstudio.record.pause", false).1, Status::Unavailable);
        assert_eq!(classify("com.elgato.twitch.marker", true).1, Status::Unavailable);
        assert_eq!(classify("com.jk.weather.action", true).1, Status::Install);
        assert_eq!(classify("com.example.thing", false).1, Status::Unknown);
        let r = rule_for("com.elgato.discord.mute").unwrap();
        assert_eq!(replacement_uuid(r, To::Action("togglemute")).unwrap(), "me.amankhanna.oadiscord.togglemute");
        assert!(spotify_command("com.barraider.spotifynext").is_some());
    }

    #[test]
    fn teams_hotkey() {
        let s = hotkey_settings(To::Hotkey(50, 77, true, true));
        assert_eq!(s["Hotkeys"][0]["KeyModifiers"], 3);
        assert_eq!(s["Hotkeys"][0]["LinuxCode"], 50);
    }
}
