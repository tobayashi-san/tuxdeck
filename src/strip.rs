//! Stream Deck + touch strip: one 200x100 px segment per dial, described by an
//! Elgato touch strip layout (built-in `$X1`, `$A0`, `$A1`, `$B1`, `$B2`, `$C1` or a
//! custom JSON file of the plugin) and filled with the values a plugin sends via
//! `setFeedback`.

use crate::render::{xml_escape, ImageData, Title};
use serde_json::{json, Map, Value};
use std::path::Path;

pub const SEG_W: u32 = 200;
pub const SEG_H: u32 = 100;

/// Items of a built-in layout (SDK reference "Touch strip layout").
pub fn builtin_layout(id: &str) -> Option<Value> {
    let title = json!({"key": "title", "type": "text", "rect": [16, 10, 136, 24], "font": {"size": 16, "weight": 600}, "alignment": "left"});
    let items = match id {
        "$X1" => json!([title, {"key": "icon", "type": "pixmap", "rect": [76, 40, 48, 48]}]),
        "$A0" => json!([{"key": "full-canvas", "type": "pixmap", "rect": [0, 0, 200, 100], "zOrder": 0},
                        {"key": "title", "type": "text", "rect": [16, 10, 136, 24], "font": {"size": 16, "weight": 600}, "alignment": "left", "zOrder": 1}]),
        "$A1" => json!([title, {"key": "icon", "type": "pixmap", "rect": [16, 40, 48, 48]},
                        {"key": "value", "type": "text", "rect": [76, 40, 108, 32], "font": {"size": 24, "weight": 600}, "alignment": "right"}]),
        "$B1" => json!([title, {"key": "icon", "type": "pixmap", "rect": [16, 40, 48, 48]},
                        {"key": "value", "type": "text", "rect": [76, 40, 108, 32], "font": {"size": 24, "weight": 600}, "alignment": "right"},
                        {"key": "indicator", "type": "bar", "rect": [76, 74, 108, 20], "value": 0, "subtype": 4}]),
        "$B2" => json!([title, {"key": "icon", "type": "pixmap", "rect": [16, 40, 48, 48]},
                        {"key": "value", "type": "text", "rect": [76, 40, 108, 32], "font": {"size": 24, "weight": 600}, "alignment": "right"},
                        {"key": "indicator", "type": "gbar", "rect": [76, 74, 108, 20], "value": 0, "subtype": 4, "bar_h": 12,
                         "bar_fill_c": "0:#ff0000,0.33:#a6d4ec,0.66:#f4b675,1:#00ff00"}]),
        "$C1" => json!([title,
                        {"key": "icon1", "type": "pixmap", "rect": [16, 40, 24, 24]},
                        {"key": "indicator1", "type": "bar", "rect": [48, 42, 136, 20], "value": 0, "subtype": 4},
                        {"key": "icon2", "type": "pixmap", "rect": [16, 70, 24, 24]},
                        {"key": "indicator2", "type": "bar", "rect": [48, 72, 136, 20], "value": 0, "subtype": 4}]),
        _ => return None,
    };
    Some(json!({"id": id, "items": items}))
}

/// Resolve a layout reference: built-in id or a JSON file relative to the plugin.
pub fn load_layout(reference: &str, plugin_dir: Option<&Path>) -> Option<Value> {
    if let Some(l) = builtin_layout(reference) {
        return Some(l);
    }
    let dir = plugin_dir?;
    let rel = Path::new(reference.trim_start_matches("./"));
    if rel.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir)) {
        return None;
    }
    let raw = std::fs::read(dir.join(rel)).ok()?;
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw);
    let v: Value = serde_json::from_slice(raw).ok()?;
    v.get("items").and_then(|i| i.as_array())?;
    Some(v)
}

/// Everything needed to draw one segment. `values` are the merged feedback values,
/// `defaults` are used for keys the plugin did not set (title, icon).
pub struct Segment<'a> {
    pub layout: &'a Value,
    pub values: &'a Map<String, Value>,
    pub defaults: &'a Map<String, Value>,
    /// User title style; applies to the `title` item like in the official app.
    pub title_style: Option<&'a Title>,
    pub show_title: bool,
    pub background: Option<&'a ImageData>,
    pub plugin_dir: Option<&'a Path>,
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().trim_end_matches('%').trim().parse().ok(),
        _ => None,
    }
}

/// Merge an item definition with a feedback value (primitive = `value`, object = overrides).
fn merged_item(item: &Value, fb: Option<&Value>) -> Value {
    let mut it = item.clone();
    if !it.is_object() {
        return it;
    }
    match fb {
        Some(Value::Object(o)) => {
            for (k, v) in o {
                if !matches!(k.as_str(), "key" | "type" | "rect") {
                    it[k] = v.clone();
                }
            }
        }
        Some(v) => it["value"] = v.clone(),
        None => {}
    }
    it
}

fn image_href(value: &str, plugin_dir: Option<&Path>) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    let data = if v.starts_with("data:") {
        ImageData::from_data_url(v)?
    } else if v.starts_with("<svg") || v.starts_with("<?xml") {
        ImageData::svg(v)
    } else {
        let p = crate::plugins::resolve_image(plugin_dir?, v)?;
        ImageData::from_file(&p)?
    };
    Some(data.to_href())
}

fn paint(c: &str, defs: &mut String, id: &str, _vertical: bool) -> String {
    crate::render::svg_paint(c, defs, id)
}

fn text_width_estimate(text: &str, size: f64) -> f64 {
    text.chars().count() as f64 * size * 0.56
}

/// Build the SVG of one segment (200x100).
pub fn segment_svg(seg: &Segment) -> String {
    feedback_svg(seg, SEG_W, SEG_H)
}

/// Render a feedback layout onto a `w`x`h` canvas (Plus segment, Neo info bar).
pub fn feedback_svg(seg: &Segment, w: u32, h: u32) -> String {
    let mut items: Vec<Value> = seg.layout.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default();
    items.retain(Value::is_object);
    items.sort_by_key(|i| i.get("zOrder").and_then(|z| z.as_i64()).unwrap_or(0));
    let mut defs = String::new();
    let mut body = String::new();
    if let Some(bg) = seg.background {
        body += &format!(r#"<image width="{w}" height="{h}" preserveAspectRatio="xMidYMid slice" xlink:href="{}"/>"#, bg.to_href());
    }
    for (n, item) in items.iter().enumerate() {
        let key = item.get("key").and_then(|k| k.as_str()).unwrap_or("");
        let fb = seg.values.get(key).or_else(|| seg.defaults.get(key));
        let it = merged_item(item, fb);
        if it.get("enabled").and_then(|v| v.as_bool()) == Some(false) {
            continue;
        }
        let rect: Vec<f64> = it.get("rect").and_then(|r| r.as_array()).map(|r| r.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect()).unwrap_or_default();
        if rect.len() != 4 {
            continue;
        }
        let (x, y, w, h) = (rect[0], rect[1], rect[2], rect[3]);
        let opacity = it.get("opacity").and_then(|v| v.as_f64()).unwrap_or(1.0).clamp(0.0, 1.0);
        let clip = format!("c{n}");
        defs += &format!(r#"<clipPath id="{clip}"><rect x="{x}" y="{y}" width="{w}" height="{h}"/></clipPath>"#);
        let mut g = String::new();
        if let Some(bg) = it.get("background").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            let p = paint(bg, &mut defs, &format!("bg{n}"), false);
            g += &format!(r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{p}"/>"#);
        }
        match it.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "text" => {
                if key == "title" && !seg.show_title {
                    continue;
                }
                let text = match it.get("value") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Number(n)) => n.to_string(),
                    Some(Value::Bool(b)) => b.to_string(),
                    _ => String::new(),
                };
                if text.is_empty() {
                    continue;
                }
                let mut size = it.pointer("/font/size").and_then(|v| v.as_f64()).unwrap_or(16.0);
                let mut weight = it.pointer("/font/weight").and_then(|v| v.as_i64()).unwrap_or(400);
                let mut color = it.get("color").and_then(|v| v.as_str()).unwrap_or("white").to_string();
                let mut italic = false;
                let mut family = "sans-serif".to_string();
                if key == "title" {
                    if let Some(t) = seg.title_style {
                        // Elgato title sizes are points on a 72 px key.
                        size = (t.size as f64 * 4.0 / 3.0).clamp(8.0, 40.0);
                        weight = if t.bold { 700 } else { 400 };
                        italic = t.italic;
                        color = t.color.clone();
                        if !t.family.is_empty() {
                            family = format!("'{}', sans-serif", xml_escape(&t.family));
                        }
                    }
                }
                let align = it.get("alignment").and_then(|a| a.as_str()).unwrap_or("center");
                let overflow = it.get("text-overflow").and_then(|a| a.as_str()).unwrap_or("clip");
                let mut text = text.replace('\n', " ");
                if overflow == "ellipsis" && text_width_estimate(&text, size) > w {
                    while !text.is_empty() && text_width_estimate(&format!("{text}…"), size) > w {
                        text.pop();
                    }
                    text.push('…');
                }
                let (anchor, tx) = match align {
                    "left" => ("start", x),
                    "right" => ("end", x + w),
                    _ => ("middle", x + w / 2.0),
                };
                let cp = paint(&color, &mut defs, &format!("tc{n}"), false);
                g += &format!(
                    r#"<text x="{tx}" y="{}" font-family="{family}" font-size="{size}" font-weight="{weight}" font-style="{}" fill="{cp}" text-anchor="{anchor}" dominant-baseline="central">{}</text>"#,
                    y + h / 2.0,
                    if italic { "italic" } else { "normal" },
                    xml_escape(&text)
                );
            }
            "pixmap" => {
                if let Some(href) = it.get("value").and_then(|v| v.as_str()).and_then(|v| image_href(v, seg.plugin_dir)) {
                    g += &format!(r#"<image x="{x}" y="{y}" width="{w}" height="{h}" preserveAspectRatio="xMidYMid meet" xlink:href="{href}"/>"#);
                }
            }
            ty @ ("bar" | "gbar") => {
                let min = it.pointer("/range/min").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let max = it.pointer("/range/max").and_then(|v| v.as_f64()).unwrap_or(100.0);
                let val = num(it.get("value")).unwrap_or(min);
                let frac = if max > min { ((val - min) / (max - min)).clamp(0.0, 1.0) } else { 0.0 };
                let subtype = it.get("subtype").and_then(|v| v.as_i64()).unwrap_or(4);
                let bw = it.get("border_w").and_then(|v| v.as_f64()).unwrap_or(2.0);
                let bg_c = it.get("bar_bg_c").and_then(|v| v.as_str()).unwrap_or("darkgray").to_string();
                let fill_c = it.get("bar_fill_c").and_then(|v| v.as_str()).unwrap_or("white").to_string();
                let border_c = it.get("bar_border_c").and_then(|v| v.as_str()).unwrap_or("white").to_string();
                let bh = if ty == "gbar" { it.get("bar_h").and_then(|v| v.as_f64()).unwrap_or(10.0).min(h) } else { h };
                let r = if subtype == 4 || subtype == 3 { bh / 2.0 } else { 0.0 };
                let bgp = paint(&bg_c, &mut defs, &format!("bb{n}"), false);
                let fp = paint(&fill_c, &mut defs, &format!("bf{n}"), false);
                let bp = paint(&border_c, &mut defs, &format!("bo{n}"), false);
                if ty == "gbar" {
                    // Full gradient bar plus a triangle marking the value below it.
                    g += &format!(r#"<rect x="{x}" y="{y}" width="{w}" height="{bh}" rx="{r}" fill="{fp}"/>"#);
                    let tx = x + w * frac;
                    let ty0 = y + bh + 1.0;
                    let th = (h - bh - 1.0).max(4.0);
                    g += &format!(r#"<path d="M{tx} {ty0} L{} {} L{} {} Z" fill="{bp}"/>"#, tx - th * 0.6, ty0 + th, tx + th * 0.6, ty0 + th);
                } else {
                    let inset = if bw > 0.0 { bw } else { 0.0 };
                    g += &format!(r#"<rect x="{x}" y="{y}" width="{w}" height="{bh}" rx="{r}" fill="{bgp}"/>"#);
                    let fw = ((w - 2.0 * inset) * frac).max(0.0);
                    if fw > 0.0 {
                        g += &format!(
                            r#"<rect x="{}" y="{}" width="{fw}" height="{}" rx="{}" fill="{fp}"/>"#,
                            x + inset,
                            y + inset,
                            (bh - 2.0 * inset).max(1.0),
                            (r - inset).max(0.0)
                        );
                    }
                    if bw > 0.0 {
                        g += &format!(
                            r#"<rect x="{}" y="{}" width="{}" height="{}" rx="{r}" fill="none" stroke="{bp}" stroke-width="{bw}"/>"#,
                            x + bw / 2.0,
                            y + bw / 2.0,
                            w - bw,
                            bh - bw
                        );
                    }
                }
            }
            _ => {}
        }
        if !g.is_empty() {
            body += &format!(r#"<g clip-path="url(#{clip})" opacity="{opacity}">{g}</g>"#);
        }
    }
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{w}" height="{h}" viewBox="0 0 {w} {h}"><defs>{defs}</defs><rect width="{w}" height="{h}" fill="#000"/>{body}</svg>"##
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_layouts_render() {
        for id in ["$X1", "$A0", "$A1", "$B1", "$B2", "$C1"] {
            let l = builtin_layout(id).unwrap();
            let mut values = Map::new();
            values.insert("title".into(), json!("Vol"));
            values.insert("value".into(), json!("42 %"));
            values.insert("indicator".into(), json!({"value": 42, "bar_fill_c": "#ff0000"}));
            let seg = Segment { layout: &l, values: &values, defaults: &Map::new(), title_style: None, show_title: true, background: None, plugin_dir: None };
            let svg = segment_svg(&seg);
            assert!(svg.starts_with("<svg"), "{id}");
            let img = crate::render::rasterize(&svg, SEG_W, SEG_H);
            assert_eq!(img.dimensions(), (SEG_W, SEG_H));
            if id == "$B1" {
                // Indicator filled to ~42 %: red pixels left, none at the far right.
                assert!(img.get_pixel(100, 84).0[0] > 200 && img.get_pixel(100, 84).0[1] < 80);
                assert!(img.get_pixel(175, 84).0[0] < 200 || img.get_pixel(175, 84).0[1] > 80);
            }
        }
    }

    #[test]
    fn feedback_merging() {
        let it = json!({"key": "value", "type": "text", "rect": [0, 0, 10, 10], "color": "white"});
        assert_eq!(merged_item(&it, Some(&json!(5)))["value"], 5);
        let m = merged_item(&it, Some(&json!({"value": "x", "color": "red", "rect": [1, 1, 1, 1]})));
        assert_eq!(m["color"], "red");
        assert_eq!(m["rect"][0], 0);
    }
}
