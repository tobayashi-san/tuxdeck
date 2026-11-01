//! Renderer for the Neo info bar layouts (`*.sdLayoutEx/manifest.json`) shipped with
//! the official app (Digital Time and Pagination). Falls back to a built-in design
//! when the original resources have not been imported.

use crate::render::{rasterize, svg_paint, xml_escape, ImageData};
use chrono::{Datelike, Local, Timelike};
use image::RgbaImage;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Size of the layout canvas used by the official layouts.
const LAYOUT_W: f32 = 232.0;
const LAYOUT_H: f32 = 50.0;

pub struct PageInfo {
    pub number: usize,
    pub count: usize,
    pub name: String,
    pub icon: Option<ImageData>,
}

pub enum InfoContent {
    Clock {
        settings: Value,
    },
    Pagination {
        settings: Value,
        page: PageInfo,
    },
    /// Plain text (plugin title on the info bar).
    Text(String),
    /// A complete SVG document (plugin feedback layout).
    Svg(String),
    Black,
}

fn layout_dir(root: &Path, group: &str, id: &str) -> Option<PathBuf> {
    let base = root.join(group);
    let want = base.join(format!("{id}.sdLayoutEx"));
    // The id comes from action settings: only plain names inside the group.
    let plain = !id.is_empty() && !id.contains(['/', '\\']) && id != "..";
    if plain && want.join("manifest.json").is_file() {
        return Some(want);
    }
    // Unknown/removed layout id: use the first available one.
    let mut all: Vec<PathBuf> = std::fs::read_dir(&base).ok()?.flatten().map(|e| e.path()).filter(|p| p.join("manifest.json").is_file()).collect();
    all.sort();
    all.into_iter().next()
}

fn substitute(v: &str, theme: &Value) -> Option<Value> {
    let key = v.strip_prefix("{{")?.strip_suffix("}}")?;
    theme.get("colors").and_then(|c| c.get(key)).or_else(|| theme.get("fonts").and_then(|f| f.get(key))).cloned()
}

fn resolve(v: Option<&Value>, theme: &Value) -> Option<Value> {
    match v {
        Some(Value::String(s)) if s.starts_with("{{") => substitute(s, theme),
        Some(x) => Some(x.clone()),
        None => None,
    }
}

fn element_text(el: &Value, settings: &Value, page: Option<&PageInfo>) -> (String, Option<ImageData>) {
    let now = Local::now();
    let h24 =
        settings.get("24HourFormat").and_then(|v| v.as_bool()).or_else(|| settings.get("standardTime").and_then(|v| v.as_bool()).map(|b| !b)).unwrap_or(true);
    let ty = el.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let s = match ty {
        "hours" => {
            let h = if h24 { now.hour() } else { now.hour12().1 };
            if h24 {
                format!("{h:02}")
            } else {
                h.to_string()
            }
        }
        "minutes" => format!("{:02}", now.minute()),
        "seconds" => format!("{:02}", now.second()),
        "colon" => ":".into(),
        "am/pm" => {
            if h24 {
                String::new()
            } else if now.hour12().0 {
                "PM".into()
            } else {
                "AM".into()
            }
        }
        "day" => {
            let long = el.get("dateFormat").and_then(|v| v.as_str()) == Some("Long");
            let names = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
            let n = crate::i18n::tr(names[now.weekday().num_days_from_monday() as usize]);
            if long {
                n
            } else {
                n.chars().take(2).collect()
            }
        }
        "date" => {
            let long = el.get("dateFormat").and_then(|v| v.as_str()) == Some("Long");
            let months = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
            let month = crate::i18n::tr(months[now.month0() as usize]);
            // Day before month as in German, English uses month first.
            match (crate::i18n::current().as_str(), long) {
                ("en", true) => format!("{month} {}", now.day()),
                ("en", false) => format!("{:02}/{:02}", now.month(), now.day()),
                (_, true) => format!("{}. {month}", now.day()),
                (_, false) => format!("{:02}.{:02}.", now.day(), now.month()),
            }
        }
        "carousel_page" => {
            let Some(p) = page else { return (String::new(), None) };
            match el.get("display").and_then(|v| v.as_str()) {
                Some("Icon") => return (String::new(), p.icon.clone()),
                Some("Name") => {
                    if p.name.is_empty() {
                        crate::tr!("Page {n}", n = p.number)
                    } else {
                        p.name.clone()
                    }
                }
                _ => p.number.to_string(),
            }
        }
        _ => String::new(),
    };
    (s, None)
}

/// Render a layout template to an SVG fragment (layout coordinates).
fn template_svg(manifest: &Value, settings: &Value, page: Option<&PageInfo>) -> String {
    let themes = manifest.get("themes").and_then(|t| t.as_array()).cloned().unwrap_or_default();
    let want = settings.get("theme").and_then(|v| v.as_str()).unwrap_or("");
    let theme = themes.iter().find(|t| t.get("id").and_then(|v| v.as_str()) == Some(want)).or(themes.first()).cloned().unwrap_or(Value::Null);
    let elements = manifest.get("elements").and_then(|e| e.as_array()).cloned().unwrap_or_default();
    let mut items = manifest.pointer("/template/items").and_then(|i| i.as_array()).cloned().unwrap_or_default();
    items.sort_by_key(|i| i.get("zOrder").and_then(|z| z.as_i64()).unwrap_or(0));
    let mut s = String::new();
    let mut defs = String::new();
    for (n, it) in items.iter().enumerate() {
        let key = it.get("key").and_then(|k| k.as_str()).unwrap_or("");
        let rect: Vec<f32> =
            it.get("rect").and_then(|r| r.as_array()).map(|r| r.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect()).unwrap_or_default();
        if rect.len() != 4 {
            continue;
        }
        let (x, y, w, h) = (rect[0], rect[1], rect[2], rect[3]);
        let radius = it.get("border_radius").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let el = elements.iter().find(|e| e.get("key").and_then(|k| k.as_str()) == Some(key));
        let (text, img) = match el {
            Some(e) => element_text(e, settings, page),
            None => (it.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string(), None),
        };
        let is_text = it.get("type").and_then(|t| t.as_str()) == Some("text");
        // Elements without content (e.g. AM/PM in 24h mode) are hidden completely.
        let hidden = el.is_some() && is_text && text.is_empty();
        if let (Some(Value::String(bg)), false) = (resolve(it.get("background"), &theme), hidden) {
            let fill = svg_paint(&bg, &mut defs, &format!("lbg{n}"));
            s += &format!(r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{radius}" fill="{fill}"/>"#);
        }
        if let Some(img) = img {
            // to_href: usvg can't decode BMP/WebP (and animated GIF) itself.
            s += &format!(r#"<image x="{x}" y="{y}" width="{w}" height="{h}" preserveAspectRatio="xMidYMid meet" xlink:href="{}"/>"#, img.to_href());
        }
        if is_text && !text.is_empty() {
            let font = resolve(it.get("font"), &theme).unwrap_or(Value::Null);
            let family = font.get("family").and_then(|v| v.as_str()).unwrap_or("sans-serif");
            let size = font.get("size").and_then(|v| v.as_f64()).unwrap_or(16.0);
            let weight = font.get("weight").and_then(|v| v.as_i64()).unwrap_or(400);
            let italic = font.get("italic").and_then(|v| v.as_bool()).unwrap_or(false);
            let color = match resolve(it.get("color"), &theme) {
                Some(Value::String(c)) => c,
                _ => "#ffffff".into(),
            };
            let ow = it.get("outline_w").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let oc = match resolve(it.get("outline_c"), &theme) {
                Some(Value::String(c)) => c,
                _ => "#000000".into(),
            };
            let outline_only = it.get("outline_only").and_then(|v| v.as_bool()).unwrap_or(false);
            let color = svg_paint(&color, &mut defs, &format!("lfg{n}"));
            let oc = svg_paint(&oc, &mut defs, &format!("loc{n}"));
            let paint = if outline_only && ow > 0.0 {
                format!(r#"fill="none" stroke="{oc}" stroke-width="{ow}" stroke-linejoin="round""#)
            } else if ow > 0.0 {
                format!(r#"fill="{color}" stroke="{oc}" stroke-width="{ow}" paint-order="stroke""#)
            } else {
                format!(r#"fill="{color}""#)
            };
            let (anchor, tx) = match it.get("alignment").and_then(|a| a.as_str()) {
                Some("left") => ("start", x),
                Some("right") => ("end", x + w),
                _ => ("middle", x + w / 2.0),
            };
            s += &format!(
                r#"<text x="{tx}" y="{}" font-family="'{}', sans-serif" font-size="{size}" font-weight="{weight}" font-style="{}" {paint} text-anchor="{anchor}" dominant-baseline="central">{}</text>"#,
                y + h / 2.0,
                xml_escape(family),
                if italic { "italic" } else { "normal" },
                xml_escape(&text)
            );
        }
    }
    format!("<defs>{defs}</defs>{s}")
}

/// Render the info bar (Neo: 248x58 px).
pub fn render_info_bar(layouts_root: Option<&Path>, content: &InfoContent, w: u32, h: u32) -> RgbaImage {
    let (group, id, settings, page) = match content {
        InfoContent::Black => return RgbaImage::from_pixel(w, h, image::Rgba([0, 0, 0, 255])),
        InfoContent::Svg(svg) => return rasterize(svg, w, h),
        InfoContent::Clock { settings } => ("DigitalTime", settings.get("layout").and_then(|v| v.as_str()).unwrap_or("digital_time_01"), settings, None),
        InfoContent::Pagination { settings, page } => {
            ("Pagination", settings.get("layout").and_then(|v| v.as_str()).unwrap_or("pagination_01"), settings, Some(page))
        }
        InfoContent::Text(t) => return rasterize(&text_svg(t, w, h), w, h),
    };
    // A built-in id always means the built-in layout (its themes are the ones offered);
    // an imported layout of the same name is listed as `elgato:<id>`.
    let is_builtin = builtin_layouts(group).iter().any(|(i, _)| i == id);
    let manifest: Option<Value> = layouts_root
        .filter(|_| !is_builtin)
        .and_then(|r| layout_dir(r, group, id.strip_prefix(IMPORTED_PREFIX).unwrap_or(id)))
        .and_then(|d| std::fs::read(d.join("manifest.json")).ok())
        .and_then(|b| serde_json::from_slice(&b).ok());
    let manifest = manifest.unwrap_or_else(|| builtin_layout(group, id));
    // Center the 232x50 layout canvas on the physical screen, background fills everything.
    let bg = template_background(&manifest, settings);
    let ox = (w as f32 - LAYOUT_W) / 2.0;
    let oy = (h as f32 - LAYOUT_H) / 2.0;
    let mut defs = String::new();
    let bg = svg_paint(&bg, &mut defs, "screenbg");
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{w}" height="{h}"><defs>{defs}</defs><rect width="{w}" height="{h}" fill="{bg}"/><g transform="translate({ox} {oy})">{}</g></svg>"#,
        template_svg(&manifest, settings, page)
    );
    rasterize(&svg, w, h)
}

fn text_svg(t: &str, w: u32, h: u32) -> String {
    // Long titles: smaller font, then cut with an ellipsis (approximate glyph width).
    let n = t.chars().count().max(1) as f32;
    let avail = w as f32 - 16.0;
    let size = (avail / (n * 0.62)).clamp(11.0, 20.0);
    // Epsilon: f32 rounding would otherwise cut texts that exactly fit.
    let fit = (avail / (size * 0.62) + 1e-3) as usize;
    let text = if t.chars().count() > fit { format!("{}…", t.chars().take(fit.saturating_sub(1)).collect::<String>().trim_end()) } else { t.to_string() };
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}"><rect width="{w}" height="{h}" fill="#000"/><text x="{}" y="{}" font-family="sans-serif" font-size="{size:.1}" font-weight="bold" fill="#fff" text-anchor="middle" dominant-baseline="central">{}</text></svg>"##,
        w / 2,
        h / 2,
        xml_escape(&text)
    )
}

fn template_background(manifest: &Value, settings: &Value) -> String {
    let themes = manifest.get("themes").and_then(|t| t.as_array()).cloned().unwrap_or_default();
    let want = settings.get("theme").and_then(|v| v.as_str()).unwrap_or("");
    let theme = themes.iter().find(|t| t.get("id").and_then(|v| v.as_str()) == Some(want)).or(themes.first());
    theme.and_then(|t| t.pointer("/colors/backgroundColor")).and_then(|v| v.as_str()).unwrap_or("#000000").to_string()
}

/// Original info-bar layouts shipped with TuxDeck (no Elgato resources needed). Every
/// layout offers the same set of colour themes.
fn infobar_themes() -> Vec<Value> {
    let fonts = serde_json::json!({
        "font0": {"family": "sans-serif", "size": 34, "weight": 600},
        "font2": {"family": "sans-serif", "size": 12, "weight": 500},
        "textFont": {"family": "sans-serif", "size": 22, "weight": 700},
        "numberFont": {"family": "sans-serif", "size": 20, "weight": 600},
    });
    let mk = |id: &str, bg: &str, tx: &str, sub: &str| serde_json::json!({"id": id, "colors": {"backgroundColor": bg, "textColor": tx, "subtextColor": sub, "accentColor1": tx}, "fonts": fonts});
    vec![
        mk("black", "#000000", "#ffffff", "#9aa0aa"),
        mk("night", "#0b1220", "#7dd3fc", "#3a5572"),
        mk("amber", "#050505", "#fbbf24", "#8a5a12"),
        mk("mint", "#061410", "#34d399", "#1f6350"),
        mk("purple", "#120a1f", "#c6a3fc", "#5f3f8a"),
        mk("mono", "#141414", "#e5e7eb", "#6b7280"),
    ]
}

/// Built-in layouts for a group as (id, manifest).
fn builtin_layouts(group: &str) -> Vec<(String, Value)> {
    let lay = |elements: Value, items: Value| serde_json::json!({"themes": infobar_themes(), "elements": elements, "template": {"items": items}});
    if group == "DigitalTime" {
        vec![
            (
                "digital_time_01".to_string(),
                lay(
                    serde_json::json!([{"key":"h","type":"hours"},{"key":"m","type":"minutes"},{"key":"d","type":"day","dateFormat":"Long"},{"key":"dt","type":"date","dateFormat":"Short"}]),
                    serde_json::json!([
                        {"key":"d","type":"text","rect":[6,8,96,16],"font":"{{font2}}","color":"{{subtextColor}}","alignment":"left"},
                        {"key":"dt","type":"text","rect":[6,26,96,16],"font":"{{font2}}","color":"{{subtextColor}}","alignment":"left"},
                        {"key":"h","type":"text","rect":[100,0,56,50],"font":"{{font0}}","color":"{{textColor}}","alignment":"right"},
                        {"key":"c","type":"text","rect":[156,0,14,50],"value":":","font":"{{font0}}","color":"{{textColor}}","alignment":"center"},
                        {"key":"m","type":"text","rect":[170,0,56,50],"font":"{{font0}}","color":"{{textColor}}","alignment":"left"}
                    ]),
                ),
            ),
            (
                "digital_time_02".to_string(),
                lay(
                    serde_json::json!([{"key":"h","type":"hours"},{"key":"m","type":"minutes"},{"key":"d","type":"date","dateFormat":"Long"}]),
                    serde_json::json!([
                        {"key":"h","type":"text","rect":[26,-1,86,38],"font":"{{font0}}","color":"{{textColor}}","alignment":"right"},
                        {"key":"c","type":"text","rect":[112,-1,10,38],"value":":","font":"{{font0}}","color":"{{textColor}}","alignment":"center"},
                        {"key":"m","type":"text","rect":[122,-1,86,38],"font":"{{font0}}","color":"{{textColor}}","alignment":"left"},
                        {"key":"d","type":"text","rect":[0,38,232,14],"font":"{{font2}}","color":"{{subtextColor}}","alignment":"center"}
                    ]),
                ),
            ),
            (
                "digital_time_03".to_string(),
                lay(
                    serde_json::json!([{"key":"h","type":"hours"},{"key":"m","type":"minutes"},{"key":"d","type":"day","dateFormat":"Long"},{"key":"dt","type":"date","dateFormat":"Short"}]),
                    serde_json::json!([
                        {"key":"h","type":"text","rect":[4,0,54,50],"font":"{{font0}}","color":"{{textColor}}","alignment":"right"},
                        {"key":"c","type":"text","rect":[58,0,12,50],"value":":","font":"{{font0}}","color":"{{textColor}}","alignment":"center"},
                        {"key":"m","type":"text","rect":[70,0,54,50],"font":"{{font0}}","color":"{{textColor}}","alignment":"left"},
                        {"key":"d","type":"text","rect":[132,8,96,16],"font":"{{font2}}","color":"{{subtextColor}}","alignment":"right"},
                        {"key":"dt","type":"text","rect":[132,26,96,16],"font":"{{font2}}","color":"{{subtextColor}}","alignment":"right"}
                    ]),
                ),
            ),
            (
                "digital_time_04".to_string(),
                lay(
                    serde_json::json!([{"key":"h","type":"hours"},{"key":"m","type":"minutes"}]),
                    serde_json::json!([
                        {"key":"h","type":"text","rect":[24,0,86,50],"font":"{{font0}}","color":"{{textColor}}","alignment":"right"},
                        {"key":"c","type":"text","rect":[110,0,12,50],"value":":","font":"{{font0}}","color":"{{textColor}}","alignment":"center"},
                        {"key":"m","type":"text","rect":[122,0,86,50],"font":"{{font0}}","color":"{{textColor}}","alignment":"left"}
                    ]),
                ),
            ),
        ]
    } else {
        vec![
            (
                "pagination_01".to_string(),
                lay(
                    serde_json::json!([{"key":"icon","type":"carousel_page","display":"Icon"},{"key":"name","type":"carousel_page","display":"Name"},{"key":"num","type":"carousel_page","display":"Number"}]),
                    serde_json::json!([
                        {"key":"icon","type":"pixmap","rect":[11,8,36,36]},
                        {"key":"name","type":"text","rect":[51,9,134,32],"font":"{{textFont}}","color":"{{textColor}}","alignment":"left"},
                        {"key":"num","type":"text","rect":[189,10,30,30],"background":"{{accentColor1}}","border_radius":4,"font":"{{numberFont}}","color":"{{backgroundColor}}","alignment":"center"}
                    ]),
                ),
            ),
            (
                "pagination_02".to_string(),
                lay(
                    serde_json::json!([{"key":"name","type":"carousel_page","display":"Name"},{"key":"num","type":"carousel_page","display":"Number"}]),
                    serde_json::json!([
                        {"key":"name","type":"text","rect":[14,9,170,32],"font":"{{textFont}}","color":"{{textColor}}","alignment":"left"},
                        {"key":"num","type":"text","rect":[190,9,32,32],"background":"{{accentColor1}}","border_radius":16,"font":"{{numberFont}}","color":"{{backgroundColor}}","alignment":"center"}
                    ]),
                ),
            ),
        ]
    }
}

/// Manifest for a layout id (falls back to the first in the group).
fn builtin_layout(group: &str, id: &str) -> Value {
    let all = builtin_layouts(group);
    all.iter()
        .find(|(i, _)| i == id)
        .or_else(|| all.first())
        .map(|(_, m)| m.clone())
        .unwrap_or_else(|| serde_json::json!({"themes": [], "template": {"items": []}}))
}

/// All layout ids available for a group (built-ins plus any imported Elgato layouts).
/// Id prefix of an imported layout whose name is also a built-in one.
pub const IMPORTED_PREFIX: &str = "elgato:";

pub fn list_layouts(root: Option<&Path>, group: &str) -> Vec<(String, Vec<String>)> {
    let theme_ids = |m: &Value| -> Vec<String> {
        m.get("themes")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().filter_map(|t| t.get("id").and_then(|i| i.as_str()).map(str::to_string)).collect())
            .unwrap_or_default()
    };
    let mut out: Vec<(String, Vec<String>)> = builtin_layouts(group).iter().map(|(id, m)| (id.clone(), theme_ids(m))).collect();
    if let Some(root) = root {
        if let Ok(rd) = std::fs::read_dir(root.join(group)) {
            for e in rd.flatten() {
                let p = e.path();
                let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
                // Same rule as `layout_dir`: only directories with a manifest are layouts.
                if !p.join("manifest.json").is_file() {
                    continue;
                }
                let id = if builtin_layouts(group).iter().any(|(i, _)| i == stem) { format!("{IMPORTED_PREFIX}{stem}") } else { stem.to_string() };
                let themes = std::fs::read(p.join("manifest.json"))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                    .map(|m| theme_ids(&m))
                    .unwrap_or_default();
                out.push((id, themes));
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tuxdeck-layout-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_layout(root: &Path, group: &str, dir: &str, manifest: &Value) {
        let d = root.join(group).join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("manifest.json"), manifest.to_string()).unwrap();
    }

    fn page(name: &str, icon: Option<ImageData>) -> PageInfo {
        PageInfo { number: 3, count: 5, name: name.into(), icon }
    }

    #[test]
    fn layout_dirs() {
        let root = tmp_dir("dirs");
        assert!(layout_dir(&root, "DigitalTime", "a").is_none());
        write_layout(&root, "DigitalTime", "b.sdLayoutEx", &json!({}));
        write_layout(&root, "DigitalTime", "a.sdLayoutEx", &json!({}));
        std::fs::create_dir_all(root.join("DigitalTime/0-empty.sdLayoutEx")).unwrap();
        let first = root.join("DigitalTime/a.sdLayoutEx");
        assert_eq!(layout_dir(&root, "DigitalTime", "b"), Some(root.join("DigitalTime/b.sdLayoutEx")));
        assert_eq!(layout_dir(&root, "DigitalTime", "a"), Some(first.clone()));
        // Unknown or unsafe ids fall back to the first layout with a manifest.
        for id in ["missing", "", "..", "../DigitalTime/b", "x\\b", "0-empty"] {
            assert_eq!(layout_dir(&root, "DigitalTime", id), Some(first.clone()), "{id:?}");
        }
        assert!(layout_dir(&root, "Pagination", "a").is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn theme_substitution() {
        let theme = json!({"colors": {"textColor": "#123456"}, "fonts": {"font0": {"size": 30}}});
        assert_eq!(substitute("{{textColor}}", &theme), Some(json!("#123456")));
        assert_eq!(substitute("{{font0}}", &theme), Some(json!({"size": 30})));
        assert_eq!(substitute("{{nope}}", &theme), None);
        assert_eq!(substitute("{{textColor", &theme), None);
        assert_eq!(substitute("textColor", &theme), None);
        assert_eq!(resolve(Some(&json!("{{textColor}}")), &theme), Some(json!("#123456")));
        assert_eq!(resolve(Some(&json!("#fff")), &theme), Some(json!("#fff")));
        assert_eq!(resolve(Some(&json!({"size": 1})), &theme), Some(json!({"size": 1})));
        assert_eq!(resolve(Some(&json!("{{nope}}")), &Value::Null), None);
        assert_eq!(resolve(None, &theme), None);
    }

    fn text(el: Value, settings: Value) -> String {
        element_text(&el, &settings, None).0
    }

    #[test]
    fn clock_elements() {
        type Want = dyn Fn(chrono::DateTime<Local>) -> String;
        let around = |f: &dyn Fn() -> String, want: &Want| {
            let before = want(Local::now());
            let got = f();
            let after = want(Local::now());
            assert!(got == before || got == after, "{got} not {before} / {after}");
        };
        around(&|| text(json!({"type": "hours"}), json!({})), &|n| format!("{:02}", n.hour()));
        around(&|| text(json!({"type": "hours"}), json!({"24HourFormat": true})), &|n| format!("{:02}", n.hour()));
        around(&|| text(json!({"type": "hours"}), json!({"24HourFormat": false})), &|n| n.hour12().1.to_string());
        // `standardTime` is the inverse setting used by older layouts; 24HourFormat wins.
        around(&|| text(json!({"type": "hours"}), json!({"standardTime": true})), &|n| n.hour12().1.to_string());
        around(&|| text(json!({"type": "hours"}), json!({"standardTime": true, "24HourFormat": true})), &|n| format!("{:02}", n.hour()));
        around(&|| text(json!({"type": "minutes"}), json!({})), &|n| format!("{:02}", n.minute()));
        around(&|| text(json!({"type": "seconds"}), json!({})), &|n| format!("{:02}", n.second()));
        around(&|| text(json!({"type": "am/pm"}), json!({"24HourFormat": false})), &|n| if n.hour12().0 { "PM".into() } else { "AM".into() });
        assert_eq!(text(json!({"type": "am/pm"}), json!({})), "");
        assert_eq!(text(json!({"type": "colon"}), json!({})), ":");
        assert_eq!(text(json!({"type": "weather"}), json!({})), "");
        assert_eq!(text(json!({}), json!({})), "");
        // Day names: two letters unless long (the language may vary between runs).
        let short = text(json!({"type": "day"}), json!({}));
        let long = text(json!({"type": "day", "dateFormat": "Long"}), json!({}));
        assert_eq!(short.chars().count(), 2, "{short}");
        assert!(long.starts_with(&short) && long.chars().count() > 2, "{long}");
        let date = text(json!({"type": "date"}), json!({}));
        let b = date.as_bytes();
        assert!((b.len() == 5 && b[2] == b'/') || (b.len() == 6 && b[2] == b'.' && b[5] == b'.'), "{date}");
        let long_date = text(json!({"type": "date", "dateFormat": "Long"}), json!({}));
        assert!(long_date.contains(&Local::now().day().to_string()) && long_date.chars().any(char::is_alphabetic), "{long_date}");
    }

    #[test]
    fn pagination_elements() {
        let icon = ImageData::svg("<svg/>");
        let p = page("Music", Some(icon.clone()));
        let el = |display: &str| json!({"type": "carousel_page", "display": display});
        assert_eq!(element_text(&el("Number"), &json!({}), Some(&p)), ("3".into(), None));
        assert_eq!(element_text(&json!({"type": "carousel_page"}), &json!({}), Some(&p)).0, "3");
        assert_eq!(element_text(&el("Name"), &json!({}), Some(&p)).0, "Music");
        assert_eq!(element_text(&el("Icon"), &json!({}), Some(&p)), (String::new(), Some(icon)));
        assert_eq!(element_text(&el("Icon"), &json!({}), Some(&page("", None))), (String::new(), None));
        // Unnamed pages: "Page 3" in the current language.
        assert!(element_text(&el("Name"), &json!({}), Some(&page("", None))).0.ends_with('3'));
        assert_eq!(element_text(&el("Number"), &json!({}), None), (String::new(), None));
    }

    fn manifest(items: Value) -> Value {
        json!({
            "themes": [
                {"id": "dark", "colors": {"backgroundColor": "#000000", "textColor": "#ffffff"}, "fonts": {"big": {"family": "Big <Font>", "size": 30, "weight": 700, "italic": true}}},
                {"id": "light", "colors": {"backgroundColor": "#eeeeee", "textColor": "#111111"}, "fonts": {"big": {"size": 10}}}
            ],
            "elements": [{"key": "colon", "type": "colon"}, {"key": "ampm", "type": "am/pm"}],
            "template": {"items": items}
        })
    }

    #[test]
    fn template_markup() {
        let items = json!([
            {"key": "label", "type": "text", "rect": [0, 0, 100, 50], "value": "Hi & bye", "font": "{{big}}", "color": "{{textColor}}", "alignment": "left", "zOrder": 2},
            {"key": "colon", "type": "text", "rect": [100, 0, 10, 50], "alignment": "right", "background": "{{textColor}}", "border_radius": 3, "zOrder": 1},
            {"key": "ampm", "type": "text", "rect": [110, 0, 20, 50], "background": "#ff0000"},
            {"key": "bad", "type": "text", "rect": [1, 2], "value": "never"}
        ]);
        let m = manifest(items);
        let s = template_svg(&m, &json!({"theme": "dark"}), None);
        assert!(
            s.contains(
                r##"<text x="0" y="25" font-family="'Big &lt;Font&gt;', sans-serif" font-size="30" font-weight="700" font-style="italic" fill="#ffffff""##
            ),
            "{s}"
        );
        assert!(s.contains(">Hi &amp; bye</text>"));
        assert!(s.contains(r##"<rect x="100" y="0" width="10" height="50" rx="3" fill="#ffffff"/>"##), "{s}");
        assert!(s.contains(r#"text-anchor="end" dominant-baseline="central">:</text>"#), "{s}");
        // zOrder: the colon (1) is drawn before the label (2).
        assert!(s.find(">:<").unwrap() < s.find(">Hi &amp; bye<").unwrap());
        // AM/PM in 24 h mode is hidden including its background.
        assert!(!s.contains("#ff0000") && !s.contains("never"));
        let light = template_svg(&m, &json!({"theme": "light"}), None);
        assert!(light.contains(r##"font-family="'sans-serif', sans-serif" font-size="10" font-weight="400" font-style="normal" fill="#111111""##), "{light}");
        // Unknown theme: the first one.
        assert_eq!(template_svg(&m, &json!({"theme": "nope"}), None), s);
        assert_eq!(template_svg(&m, &json!({}), None), s);
        // 12 h mode shows AM/PM with its background.
        assert!(template_svg(&m, &json!({"24HourFormat": false}), None).contains("#ff0000"));
        assert_eq!(template_svg(&json!({}), &json!({}), None), "<defs></defs>");
    }

    #[test]
    fn template_outlines_and_gradients() {
        let item = |extra: Value| {
            let mut it = json!({"key": "t", "type": "text", "rect": [0, 0, 50, 50], "value": "X"});
            it.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            template_svg(&json!({"template": {"items": [it]}}), &json!({}), None)
        };
        let plain = item(json!({}));
        assert!(plain.contains(r##"font-size="16" font-weight="400" font-style="normal" fill="#ffffff" text-anchor="middle""##), "{plain}");
        assert!(plain.contains(r#"<text x="25" y="25""#));
        let outlined = item(json!({"outline_w": 2, "outline_c": "#00ff00", "color": "#0000ff"}));
        assert!(outlined.contains(r##"fill="#0000ff" stroke="#00ff00" stroke-width="2" paint-order="stroke""##), "{outlined}");
        let only = item(json!({"outline_w": 1.5, "outline_only": true}));
        assert!(only.contains(r##"fill="none" stroke="#000000" stroke-width="1.5" stroke-linejoin="round""##), "{only}");
        // outline_only without a width still fills.
        assert!(item(json!({"outline_only": true})).contains(r##"fill="#ffffff""##));
        let grad = item(json!({"color": "v$0:#000,1:#fff", "background": "0:#f00,1:#00f"}));
        assert!(
            grad.contains(r#"<linearGradient id="lbg0""#) && grad.contains(r#"<linearGradient id="lfg0""#) && grad.contains(r#"fill="url(#lfg0)""#),
            "{grad}"
        );
        // Non-string colours fall back to the defaults.
        assert!(item(json!({"color": 5})).contains(r##"fill="#ffffff""##));
        // Page icons are embedded as images.
        let icon = ImageData::new("image/png", vec![1, 2, 3]);
        let m = json!({"elements": [{"key": "i", "type": "carousel_page", "display": "Icon"}], "template": {"items": [{"key": "i", "type": "pixmap", "rect": [1, 2, 3, 4]}]}});
        let s = template_svg(&m, &json!({}), Some(&page("", Some(icon))));
        assert!(s.contains(r#"<image x="1" y="2" width="3" height="4" preserveAspectRatio="xMidYMid meet" xlink:href="data:image/png;base64,AQID"/>"#), "{s}");
    }

    #[test]
    fn backgrounds() {
        let m = manifest(json!([]));
        assert_eq!(template_background(&m, &json!({"theme": "light"})), "#eeeeee");
        assert_eq!(template_background(&m, &json!({"theme": "other"})), "#000000");
        assert_eq!(template_background(&json!({}), &json!({})), "#000000");
        assert_eq!(template_background(&json!({"themes": [{"id": "x", "colors": {"backgroundColor": "h$0:#000,1:#fff"}}]}), &json!({})), "h$0:#000,1:#fff");
    }

    #[test]
    fn info_text() {
        let s = text_svg("Hi", 248, 58);
        assert!(s.contains(r#"<text x="124" y="29" font-family="sans-serif" font-size="20.0""#) && s.contains(">Hi</text>"), "{s}");
        // Texts that fit at a reduced size are shown completely.
        for n in 1..=34 {
            let t = "W".repeat(n);
            let s = text_svg(&t, 248, 58);
            assert!(s.contains(&format!(">{t}</text>")), "{n}: {s}");
        }
        // Longer ones are cut at the minimum size of 11.
        let long = "abcdefghij".repeat(10);
        let s = text_svg(&long, 248, 58);
        assert!(s.contains(r#"font-size="11.0""#));
        assert!(s.contains(&format!(">{}…</text>", &long[..33])), "{s}");
        // No trailing space before the ellipsis, markup escaped.
        let s = text_svg(&format!("{} {}", "a".repeat(32), "b".repeat(20)), 248, 58);
        assert!(s.contains(&format!(">{}…</text>", "a".repeat(32))), "{s}");
        assert!(text_svg("<b>", 248, 58).contains(">&lt;b&gt;</text>"));
        assert!(text_svg("", 248, 58).contains("></text>"));
    }

    fn color_at(img: &RgbaImage, x: u32, y: u32) -> [u8; 3] {
        let p = img.get_pixel(x, y).0;
        [p[0], p[1], p[2]]
    }

    #[test]
    fn info_bar_rendering() {
        let black = render_info_bar(None, &InfoContent::Black, 248, 58);
        assert_eq!(black.dimensions(), (248, 58));
        assert!(black.pixels().all(|p| p.0 == [0, 0, 0, 255]));
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="#ff0000"/></svg>"##;
        assert_eq!(color_at(&render_info_bar(None, &InfoContent::Svg(svg.into()), 248, 58), 200, 50), [255, 0, 0]);
        let t = render_info_bar(None, &InfoContent::Text("Hello".into()), 248, 58);
        assert!(t.pixels().any(|p| p.0[0] > 200));
        assert_eq!(color_at(&t, 2, 2), [0, 0, 0]);

        // Built-in clock: theme background everywhere, text in the theme colour.
        for (theme, bg, fg) in [("night", [0x0b, 0x12, 0x20], [0x7d, 0xd3, 0xfc]), ("amber", [5, 5, 5], [0xfb, 0xbf, 0x24])] {
            let img = render_info_bar(None, &InfoContent::Clock { settings: json!({"theme": theme}) }, 248, 58);
            assert_eq!(color_at(&img, 0, 0), bg);
            assert_eq!(color_at(&img, 247, 57), bg);
            assert!(img.pixels().any(|p| p.0[..3] == fg), "{theme}");
        }
        for id in ["digital_time_01", "digital_time_02", "digital_time_03", "digital_time_04", "unknown"] {
            let img = render_info_bar(None, &InfoContent::Clock { settings: json!({"layout": id}) }, 248, 58);
            assert!(img.pixels().filter(|p| p.0[0] > 200).count() > 50, "{id}");
        }

        // Built-in pagination: number box in the accent colour, at layout offset (8, 4).
        let content = InfoContent::Pagination { settings: json!({}), page: page("Music", None) };
        let img = render_info_bar(None, &content, 248, 58);
        assert_eq!(color_at(&img, 8 + 192, 4 + 13), [255, 255, 255]);
        let p2 = InfoContent::Pagination { settings: json!({"layout": "pagination_02", "theme": "mint"}), page: page("", None) };
        let img = render_info_bar(None, &p2, 248, 58);
        assert_eq!(color_at(&img, 8 + 206, 4 + 11), [0x34, 0xd3, 0x99]);
    }

    #[test]
    fn page_icons() {
        // Icons of any supported format are drawn (BMP is not understood by usvg directly).
        let mut bmp = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([0, 255, 0])))
            .write_to(&mut std::io::Cursor::new(&mut bmp), image::ImageFormat::Bmp)
            .unwrap();
        let png = crate::render::encode_png(&RgbaImage::from_pixel(8, 8, image::Rgba([0, 255, 0, 255])));
        for icon in [ImageData::new("image/png", png), ImageData::new("image/bmp", bmp)] {
            let mime = icon.mime.clone();
            let content = InfoContent::Pagination { settings: json!({}), page: page("Music", Some(icon)) };
            let img = render_info_bar(None, &content, 248, 58);
            // Icon rect [11, 8, 36, 36] at offset (8, 4).
            assert_eq!(color_at(&img, 8 + 29, 4 + 26), [0, 255, 0], "{mime}");
        }
    }

    #[test]
    fn imported_layouts() {
        let root = tmp_dir("imported");
        let custom = json!({
            "themes": [{"id": "red", "colors": {"backgroundColor": "#ff0000"}}, {"id": "blue", "colors": {"backgroundColor": "#0000ff"}}],
            "template": {"items": []}
        });
        write_layout(&root, "DigitalTime", "custom.sdLayoutEx", &custom);
        let img = render_info_bar(Some(&root), &InfoContent::Clock { settings: json!({"layout": "custom", "theme": "blue"}) }, 248, 58);
        assert_eq!(color_at(&img, 120, 30), [0, 0, 255]);
        // Unknown ids use the first imported layout, built-in ids stay built-in.
        let img = render_info_bar(Some(&root), &InfoContent::Clock { settings: json!({"layout": "removed"}) }, 248, 58);
        assert_eq!(color_at(&img, 120, 30), [255, 0, 0]);
        write_layout(&root, "Pagination", "aaa.sdLayoutEx", &custom);
        let builtin = InfoContent::Pagination { settings: json!({"layout": "pagination_02", "theme": "night"}), page: page("Music", None) };
        assert_eq!(render_info_bar(Some(&root), &builtin, 248, 58).as_raw(), render_info_bar(None, &builtin, 248, 58).as_raw());
        let unknown = InfoContent::Pagination { settings: json!({"layout": "gone"}), page: page("Music", None) };
        assert_eq!(color_at(&render_info_bar(Some(&root), &unknown, 248, 58), 120, 30), [255, 0, 0]);
        // Broken manifests use the built-in layout.
        let bad = root.join("Pagination/broken.sdLayoutEx");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("manifest.json"), "{").unwrap();
        let content = InfoContent::Pagination { settings: json!({"layout": "broken"}), page: page("Music", None) };
        assert_eq!(render_info_bar(Some(&root), &content, 248, 58).as_raw(), render_info_bar(None, &content, 248, 58).as_raw());

        let builtin: Vec<String> = list_layouts(None, "DigitalTime").into_iter().map(|(id, _)| id).collect();
        assert_eq!(builtin, ["digital_time_01", "digital_time_02", "digital_time_03", "digital_time_04"]);
        let themes = ["black", "night", "amber", "mint", "purple", "mono"];
        assert!(list_layouts(None, "Pagination").iter().all(|(_, t)| t == &themes));
        assert_eq!(list_layouts(None, "Pagination").len(), 2);
        // Imported layouts are listed with their themes, sorted; built-in ids are not duplicated.
        write_layout(&root, "DigitalTime", "digital_time_01.sdLayoutEx", &custom);
        std::fs::write(root.join("DigitalTime/notes.txt"), "not a layout").unwrap();
        std::fs::create_dir_all(root.join("DigitalTime/empty.sdLayoutEx")).unwrap();
        let all = list_layouts(Some(&root), "DigitalTime");
        let ids: Vec<&str> = all.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["custom", "digital_time_01", "digital_time_02", "digital_time_03", "digital_time_04", "elgato:digital_time_01"]);
        assert_eq!(all[0].1, ["red", "blue"]);
        assert_eq!(all[1].1, themes);
        assert_eq!(all[5].1, ["red", "blue"]);
        // The built-in id renders the built-in layout, the imported one under its own id.
        let clock = |layout: &str| InfoContent::Clock { settings: json!({"layout": layout, "theme": "night"}) };
        assert_eq!(
            render_info_bar(Some(&root), &clock("digital_time_01"), 248, 58).as_raw(),
            render_info_bar(None, &clock("digital_time_01"), 248, 58).as_raw()
        );
        assert_eq!(
            render_info_bar(Some(&root), &clock("elgato:digital_time_01"), 248, 58).as_raw(),
            render_info_bar(Some(&root), &clock("custom"), 248, 58).as_raw()
        );
        assert_eq!(list_layouts(Some(&root.join("missing")), "DigitalTime").len(), 4);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn builtin_fallbacks() {
        let first = builtin_layout("DigitalTime", "digital_time_01");
        assert_eq!(builtin_layout("DigitalTime", "nope"), first);
        assert_ne!(builtin_layout("DigitalTime", "digital_time_02"), first);
        assert_eq!(builtin_layout("Pagination", "nope"), builtin_layout("Pagination", "pagination_01"));
        // Every built-in item references an element or carries a value, and all theme keys resolve.
        for group in ["DigitalTime", "Pagination"] {
            for (id, m) in builtin_layouts(group) {
                let elements = m["elements"].as_array().unwrap();
                for it in m.pointer("/template/items").unwrap().as_array().unwrap() {
                    let key = it["key"].as_str().unwrap();
                    assert!(elements.iter().any(|e| e["key"] == key) || it.get("value").is_some(), "{id}: {key}");
                    for t in m["themes"].as_array().unwrap() {
                        for f in ["font", "color", "background"] {
                            if let Some(Value::String(v)) = it.get(f) {
                                assert!(!v.starts_with("{{") || substitute(v, t).is_some(), "{id}: {v}");
                            }
                        }
                    }
                }
            }
        }
    }
}
