//! Translations of user-visible daemon messages. Source strings are English; the
//! dictionaries are the same files the web UI uses (`ui/i18n/<lang>.json`).
//!
//! Like the official app, the language follows the system (`LANGUAGE`, `LC_ALL`,
//! `LC_MESSAGES`, `LANG`) unless the user picks one in the settings. The UI languages
//! are English plus every `ui/i18n/<lang>.json` embedded in the binary, so adding a
//! translation needs no code change. Plugins bring their own translations for the
//! languages of the official app ([`PLUGIN_LANGUAGES`]).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, RwLock};

/// Languages of the official app; plugins ship `<lang>.json` files for these.
pub const PLUGIN_LANGUAGES: &[&str] = &["en", "de", "es", "fr", "ja", "ko", "zh_CN", "zh_TW"];

static CURRENT: RwLock<String> = RwLock::new(String::new());
type Dict = std::sync::Arc<HashMap<String, String>>;
static DICTS: OnceLock<Mutex<HashMap<String, Dict>>> = OnceLock::new();

/// Languages with a UI translation: English (the source) and all embedded dictionaries.
pub fn ui_languages() -> Vec<String> {
    let mut v: Vec<String> =
        crate::web::Assets::iter().filter_map(|p| p.strip_prefix("i18n/").and_then(|f| f.strip_suffix(".json")).map(str::to_string)).collect();
    v.push("en".into());
    v.sort();
    v.dedup();
    v
}

fn dict(lang: &str) -> Dict {
    let cache = DICTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
    c.entry(lang.to_string())
        .or_insert_with(|| {
            let d = crate::web::Assets::get(&format!("i18n/{lang}.json")).and_then(|f| serde_json::from_slice(&f.data).ok()).unwrap_or_default();
            std::sync::Arc::new(d)
        })
        .clone()
}

/// Language code from a POSIX locale such as `de_CH.UTF-8` or `zh_TW` (`C` → `en`).
pub fn from_locale(locale: &str) -> Option<String> {
    let l = locale.split(['.', '@']).next().unwrap_or("").trim();
    if l.is_empty() || l == "C" || l == "POSIX" {
        return None;
    }
    let mut parts = l.split(['_', '-']);
    let lang = parts.next()?.to_lowercase();
    let region = parts.next().unwrap_or("").to_uppercase();
    Some(match (lang.as_str(), region.as_str()) {
        ("zh", "TW" | "HK" | "MO") => "zh_TW".into(),
        ("zh", _) => "zh_CN".into(),
        _ => lang,
    })
}

/// Language of the session (first entry of `LANGUAGE`, then `LC_ALL`, `LC_MESSAGES`, `LANG`).
pub fn system_language() -> String {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    env("LANGUAGE")
        .and_then(|v| v.split(':').find_map(from_locale))
        .or_else(|| ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|k| env(k).and_then(|v| from_locale(&v))))
        .unwrap_or_else(|| "en".into())
}

/// Effective language for the setting value (`auto` or empty = system language).
pub fn resolve(setting: &str) -> String {
    if setting.is_empty() || setting == "auto" {
        system_language()
    } else {
        setting.to_string()
    }
}

/// Language used for the UI (falls back to English).
pub fn ui_language(lang: &str) -> String {
    if ui_languages().iter().any(|l| l == lang) {
        lang.to_string()
    } else {
        "en".into()
    }
}

/// Language passed to plugins and used for their manifests (falls back to English).
pub fn plugin_language(lang: &str) -> &'static str {
    PLUGIN_LANGUAGES.iter().copied().find(|l| *l == lang).unwrap_or("en")
}

pub fn set_language(lang: &str) {
    if let Ok(mut c) = CURRENT.write() {
        *c = ui_language(lang);
    }
}

pub fn current() -> String {
    CURRENT.read().map(|c| if c.is_empty() { "en".to_string() } else { c.clone() }).unwrap_or_else(|_| "en".into())
}

/// Translate an English source string into the current language.
pub fn tr(s: &str) -> String {
    let lang = current();
    if lang == "en" {
        return s.to_string();
    }
    dict(&lang).get(s).cloned().unwrap_or_else(|| s.to_string())
}

/// How a program ended, in words of the current language ("exit code 1", "signal 9").
pub fn exit_status(st: &std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (st.code(), st.signal()) {
        (Some(c), _) => crate::tr!("exit code {code}", code = c),
        (None, Some(s)) => crate::tr!("signal {signal}", signal = s),
        _ => st.to_string(),
    }
}

/// `tr!("Plugin \"{name}\" installed", name = n)`: translate, then fill `{name}`.
#[macro_export]
macro_rules! tr {
    ($s:expr) => {
        $crate::i18n::tr($s)
    };
    ($s:expr, $($k:ident = $v:expr),+ $(,)?) => {{
        let mut out = $crate::i18n::tr($s);
        $( out = out.replace(concat!("{", stringify!($k), "}"), &$v.to_string()); )+
        out
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales() {
        assert_eq!(from_locale("de_CH.UTF-8").as_deref(), Some("de"));
        assert_eq!(from_locale("zh_TW.UTF-8").as_deref(), Some("zh_TW"));
        assert_eq!(from_locale("zh_SG").as_deref(), Some("zh_CN"));
        assert_eq!(from_locale("C.UTF-8"), None);
        assert_eq!(ui_language("fr"), "en");
        assert_eq!(plugin_language("fr"), "fr");
    }

    #[test]
    fn german_dictionary_is_valid() {
        assert!(ui_languages().contains(&"de".to_string()));
        assert!(dict("de").len() > 100, "ui/i18n/de.json must parse");
    }
}
