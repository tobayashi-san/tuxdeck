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
            s += &format!(
                r#"<image x="{x}" y="{y}" width="{w}" height="{h}" preserveAspectRatio="xMidYMid meet" xlink:href="data:{};base64,{}"/>"#,
                img.mime,
                base64_encode(&img.bytes)
            );
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

fn base64_encode(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(b)
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
        InfoContent::Text(t) => {
            // Long titles: smaller font, then cut with an ellipsis (approximate glyph width).
            let n = t.chars().count().max(1) as f32;
            let avail = w as f32 - 16.0;
            let size = (avail / (n * 0.62)).clamp(11.0, 20.0);
            let fit = (avail / (size * 0.62)) as usize;
            let text = if t.chars().count() > fit { format!("{}…", t.chars().take(fit.saturating_sub(1)).collect::<String>().trim_end()) } else { t.clone() };
            let svg = format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}"><rect width="{w}" height="{h}" fill="#000"/><text x="{}" y="{}" font-family="sans-serif" font-size="{size:.1}" font-weight="bold" fill="#fff" text-anchor="middle" dominant-baseline="central">{}</text></svg>"##,
                w / 2,
                h / 2,
                xml_escape(&text)
            );
            return rasterize(&svg, w, h);
        }
    };
    let manifest: Option<Value> = layouts_root
        .and_then(|r| layout_dir(r, group, id))
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
                if out.iter().any(|(i, _)| i == stem) {
                    continue;
                }
                let themes = std::fs::read(p.join("manifest.json"))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                    .map(|m| theme_ids(&m))
                    .unwrap_or_default();
                out.push((stem.to_string(), themes));
            }
        }
    }
    out.sort();
    out
}
