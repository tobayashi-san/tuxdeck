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
        // `key` and `type` can't be overridden either; missing feedback changes nothing.
        let m = merged_item(&it, Some(&json!({"key": "other", "type": "bar"})));
        assert_eq!((m["key"].as_str(), m["type"].as_str()), (Some("value"), Some("text")));
        assert_eq!(merged_item(&it, None), it);
        assert_eq!(merged_item(&json!("x"), Some(&json!(1))), json!("x"));
    }

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("tuxdeck-strip-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn svg_for(layout: Value, values: Value, defaults: Value, title_style: Option<&Title>, show_title: bool) -> String {
        let (Value::Object(values), Value::Object(defaults)) = (values, defaults) else { panic!() };
        let seg = Segment { layout: &layout, values: &values, defaults: &defaults, title_style, show_title, background: None, plugin_dir: None };
        segment_svg(&seg)
    }

    fn svg_items(items: Value, values: Value) -> String {
        svg_for(json!({ "items": items }), values, json!({}), None, true)
    }

    #[test]
    fn layout_loading() {
        assert_eq!(load_layout("$B2", None).unwrap()["id"], "$B2");
        assert!(builtin_layout("$Z9").is_none());
        assert!(load_layout("layouts/custom.json", None).is_none());
        let dir = tmp_dir("load");
        std::fs::create_dir_all(dir.join("layouts")).unwrap();
        std::fs::write(dir.join("layouts/custom.json"), br#"{"id": "custom", "items": [{"key": "a", "type": "text", "rect": [0, 0, 10, 10]}]}"#).unwrap();
        std::fs::write(dir.join("bom.json"), [&b"\xef\xbb\xbf"[..], br#"{"items": []}"#].concat()).unwrap();
        std::fs::write(dir.join("noitems.json"), br#"{"id": "x"}"#).unwrap();
        std::fs::write(dir.join("bad.json"), b"{").unwrap();
        std::fs::write(dir.join("objitems.json"), br#"{"items": {"a": 1}}"#).unwrap();
        assert_eq!(load_layout("layouts/custom.json", Some(&dir)).unwrap()["id"], "custom");
        assert_eq!(load_layout("./layouts/custom.json", Some(&dir)).unwrap()["id"], "custom");
        assert!(load_layout("bom.json", Some(&dir)).is_some(), "UTF-8 BOM");
        for bad in ["noitems.json", "bad.json", "objitems.json", "missing.json", "../load/bom.json", "layouts/../bom.json"] {
            assert!(load_layout(bad, Some(&dir)).is_none(), "{bad}");
        }
        let abs = dir.join("bom.json");
        assert!(load_layout(abs.to_str().unwrap(), Some(&dir)).is_none(), "absolute paths");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn numbers() {
        assert_eq!(num(Some(&json!(42))), Some(42.0));
        assert_eq!(num(Some(&json!(-1.5))), Some(-1.5));
        assert_eq!(num(Some(&json!(" 42 % "))), Some(42.0));
        assert_eq!(num(Some(&json!("7.5%"))), Some(7.5));
        assert_eq!(num(Some(&json!("loud"))), None);
        assert_eq!(num(Some(&json!(true))), None);
        assert_eq!(num(None), None);
    }

    #[test]
    fn image_references() {
        assert!(image_href("  ", None).is_none());
        assert!(image_href("data:image/svg+xml,<svg/>", None).unwrap().starts_with("data:image/svg+xml;base64,"));
        assert!(image_href("<svg xmlns='http://www.w3.org/2000/svg'/>", None).unwrap().starts_with("data:image/svg+xml;base64,"));
        assert!(image_href("<?xml version='1.0'?><svg/>", None).unwrap().starts_with("data:image/svg+xml;base64,"));
        assert!(image_href("icons/knob", None).is_none());
        let dir = tmp_dir("img");
        std::fs::create_dir_all(dir.join("icons")).unwrap();
        std::fs::write(dir.join("icons/knob.png"), crate::render::encode_png(&image::RgbaImage::new(2, 2))).unwrap();
        assert!(image_href("icons/knob", Some(&dir)).unwrap().starts_with("data:image/png;base64,"));
        assert!(image_href("icons/knob.png", Some(&dir)).is_some());
        assert!(image_href("icons/missing", Some(&dir)).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn item_order_and_filtering() {
        let svg = svg_items(
            json!([
                {"key": "b", "type": "text", "rect": [0, 0, 10, 10], "value": "second", "zOrder": 2},
                "not an item",
                {"key": "a", "type": "text", "rect": [0, 0, 10, 10], "value": "first", "zOrder": 1},
                {"key": "c", "type": "text", "rect": [0, 0, 10], "value": "bad rect"},
                {"key": "d", "type": "text", "rect": [0, 0, 10, 10], "value": "disabled", "enabled": false},
                {"key": "e", "type": "unknown", "rect": [0, 0, 10, 10], "value": "x"},
                {"key": "f", "type": "text", "rect": [0, 0, 10, 10]}
            ]),
            json!({"f": {"enabled": false}}),
        );
        let (a, b) = (svg.find(">first<").unwrap(), svg.find(">second<").unwrap());
        assert!(a < b, "zOrder");
        assert!(!svg.contains("bad rect") && !svg.contains("disabled"));
        assert_eq!(svg.matches("<g ").count(), 2);
        // A layout without items is just the black canvas.
        let empty = svg_for(json!({}), json!({}), json!({}), None, true);
        assert!(empty.ends_with(r##"<defs></defs><rect width="200" height="100" fill="#000"/></svg>"##), "{empty}");
    }

    #[test]
    fn text_items() {
        let item = |align: &str, value: Value| json!([{"key": "v", "type": "text", "rect": [10, 20, 100, 30], "alignment": align, "value": value}]);
        assert!(svg_items(item("left", json!("L")), json!({})).contains(r#"<text x="10" y="35""#));
        assert!(svg_items(item("right", json!("R")), json!({})).contains(r#"x="110" y="35""#));
        assert!(svg_items(item("center", json!("C")), json!({})).contains(r#"x="60" y="35""#));
        let s = svg_items(item("right", json!(3.5)), json!({}));
        assert!(s.contains(r#"text-anchor="end""#) && s.contains(">3.5</text>"));
        assert!(svg_items(item("left", json!(true)), json!({})).contains(">true</text>"));
        assert!(!svg_items(item("left", json!("")), json!({})).contains("<text"));
        assert!(!svg_items(item("left", json!(null)), json!({})).contains("<text"));
        assert!(svg_items(item("left", json!("a\nb <&>")), json!({})).contains(">a b &lt;&amp;&gt;</text>"));
        // Defaults: white, 16 px, weight 400.
        assert!(svg_items(item("left", json!("x")), json!({}))
            .contains(r#"font-family="sans-serif" font-size="16" font-weight="400" font-style="normal" fill="white""#));
        let styled =
            json!([{"key": "v", "type": "text", "rect": [0, 0, 100, 30], "font": {"size": 24, "weight": 700}, "color": "0:#f00,1:#00f", "value": "x"}]);
        let s = svg_items(styled, json!({}));
        assert!(s.contains(r#"font-size="24" font-weight="700""#) && s.contains(r#"fill="url(#tc0)""#) && s.contains(r#"<linearGradient id="tc0""#));
    }

    #[test]
    fn text_overflow() {
        let item =
            |overflow: &str| json!([{"key": "v", "type": "text", "rect": [0, 0, 136, 24], "text-overflow": overflow, "value": "ABCDEFGHIJKLMNOPQRSTUVWXYZ"}]);
        assert!(svg_items(item("ellipsis"), json!({})).contains(">ABCDEFGHIJKLMN…</text>"));
        assert!(svg_items(item("clip"), json!({})).contains(">ABCDEFGHIJKLMNOPQRSTUVWXYZ</text>"));
        let short = json!([{"key": "v", "type": "text", "rect": [0, 0, 136, 24], "text-overflow": "ellipsis", "value": "Vol"}]);
        assert!(svg_items(short, json!({})).contains(">Vol</text>"));
        // Even a single character doesn't fit: only the ellipsis remains.
        let tiny = json!([{"key": "v", "type": "text", "rect": [0, 0, 4, 24], "text-overflow": "ellipsis", "value": "Volume"}]);
        assert!(svg_items(tiny, json!({})).contains(">…</text>"));
    }

    #[test]
    fn titles() {
        let l = builtin_layout("$X1").unwrap();
        let mut style = Title {
            text: String::new(),
            align: "top".into(),
            size: 16,
            color: "#00ff00".into(),
            family: "My <Font>".into(),
            bold: true,
            italic: true,
            underline: false,
            outline: 0,
        };
        let s = svg_for(l.clone(), json!({"title": "Vol"}), json!({}), Some(&style), true);
        assert!(s.contains(r#"font-family="'My &lt;Font&gt;', sans-serif" font-size="21.33"#), "{s}");
        assert!(s.contains(r##"font-weight="700" font-style="italic" fill="#00ff00""##), "{s}");
        style.size = 60;
        style.bold = false;
        style.family = String::new();
        let s = svg_for(l.clone(), json!({"title": "Vol"}), json!({}), Some(&style), true);
        assert!(s.contains(r#"font-family="sans-serif" font-size="40" font-weight="400""#), "{s}");
        style.size = 1;
        assert!(svg_for(l.clone(), json!({"title": "Vol"}), json!({}), Some(&style), true).contains(r#"font-size="8""#));
        // Hidden titles, and defaults used when the plugin sent no title.
        assert!(!svg_for(l.clone(), json!({"title": "Vol"}), json!({}), None, false).contains(">Vol<"));
        assert!(svg_for(l.clone(), json!({}), json!({"title": "Default"}), None, true).contains(">Default</text>"));
        assert!(svg_for(l.clone(), json!({"title": "Set"}), json!({"title": "Default"}), None, true).contains(">Set</text>"));
        // The user's title style only applies to the title item.
        let a1 = builtin_layout("$A1").unwrap();
        let s = svg_for(a1, json!({"value": "42"}), json!({}), Some(&style), true);
        assert!(s.contains(r#"font-size="24" font-weight="600" font-style="normal" fill="white""#), "{s}");
    }

    #[test]
    fn pixmaps_backgrounds_and_opacity() {
        let items = json!([
            {"key": "icon", "type": "pixmap", "rect": [16, 40, 48, 48], "background": "#ff0000", "opacity": 3},
            {"key": "none", "type": "pixmap", "rect": [0, 0, 10, 10], "opacity": -1}
        ]);
        let s = svg_items(items.clone(), json!({"icon": "data:image/svg+xml,<svg/>"}));
        assert!(s.contains(r#"<image x="16" y="40" width="48" height="48" preserveAspectRatio="xMidYMid meet" xlink:href="data:image/svg+xml;base64,"#));
        assert!(s.contains(r##"<rect x="16" y="40" width="48" height="48" fill="#ff0000"/>"##));
        assert!(s.contains(r#"<g clip-path="url(#c0)" opacity="1">"#), "opacity clamped");
        assert!(s.contains(r#"<clipPath id="c1">"#) && !s.contains("url(#c1)"), "empty items are not drawn");
        let s = svg_items(json!([{"key": "x", "type": "pixmap", "rect": [0, 0, 10, 10], "background": "v$0:#000,1:#fff", "opacity": -2}]), json!({}));
        assert!(s.contains(r#"fill="url(#bg0)""#) && s.contains(r#"opacity="0""#));
        // Segment background image: covers the whole canvas.
        let bg = ImageData::svg("<svg xmlns='http://www.w3.org/2000/svg'/>");
        let l = json!({"items": []});
        let seg =
            Segment { layout: &l, values: &Map::new(), defaults: &Map::new(), title_style: None, show_title: true, background: Some(&bg), plugin_dir: None };
        let s = feedback_svg(&seg, 248, 58);
        assert!(s.contains(r#"width="248" height="58" viewBox="0 0 248 58""#));
        assert!(s.contains(r#"<image width="248" height="58" preserveAspectRatio="xMidYMid slice""#));
    }

    #[test]
    fn bars() {
        let bar = |extra: Value| {
            let mut it = json!({"key": "b", "type": "bar", "rect": [0, 0, 100, 20]});
            it.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            svg_items(json!([it]), json!({}))
        };
        // Range and percent strings: 50 of 0..200 = 25 % of the inner width (100 - 2 * 2).
        let s = bar(json!({"value": "50%", "range": {"min": 0, "max": 200}}));
        assert!(s.contains(r#"<rect x="2" y="2" width="24" height="16" rx="8""#), "{s}");
        assert!(s.contains(r#"fill="darkgray""#) && s.contains(r#"stroke="white" stroke-width="2""#));
        // Clamped to the range; at the minimum there is no fill rect.
        assert!(bar(json!({"value": 500})).contains(r#"width="96""#));
        let empty = bar(json!({"value": -5}));
        assert_eq!(empty.matches("<rect").count(), 4, "{empty}");
        assert!(bar(json!({"value": 50, "range": {"min": 10, "max": 10}})).matches("<rect").count() == 4, "empty range");
        // Square bars, no border, custom colours.
        let s = bar(json!({"value": 50, "subtype": 0, "border_w": 0, "bar_bg_c": "#111111", "bar_fill_c": "#222222"}));
        assert!(s.contains(r##"<rect x="0" y="0" width="100" height="20" rx="0" fill="#111111"/>"##), "{s}");
        assert!(s.contains(r##"<rect x="0" y="0" width="50" height="20" rx="0" fill="#222222"/>"##), "{s}");
        assert!(!s.contains("stroke="));

        let g = svg_items(json!([{"key": "g", "type": "gbar", "rect": [0, 0, 100, 20], "bar_h": 12, "value": 50, "bar_fill_c": "0:#f00,1:#0f0"}]), json!({}));
        assert!(g.contains(r#"<rect x="0" y="0" width="100" height="12" rx="6" fill="url(#bf0)"/>"#), "{g}");
        assert!(g.contains(r#"<path d="M50 13 L"#), "{g}");
        // bar_h can't exceed the item height.
        let g = svg_items(json!([{"key": "g", "type": "gbar", "rect": [0, 0, 100, 20], "bar_h": 50}]), json!({}));
        assert!(g.contains(r#"height="20" rx="10""#) && g.contains(r#"<path d="M0 21 L"#), "{g}");
    }

    #[test]
    fn bar_rendering() {
        let l = builtin_layout("$B1").unwrap();
        let (Value::Object(values), defaults) = (json!({"indicator": {"value": 75, "bar_fill_c": "#0000ff"}}), Map::new()) else { panic!() };
        let seg = Segment { layout: &l, values: &values, defaults: &defaults, title_style: None, show_title: true, background: None, plugin_dir: None };
        let img = crate::render::rasterize(&segment_svg(&seg), SEG_W, SEG_H);
        // Indicator rect [76, 74, 108, 20]: fill ends at 76 + 2 + 104 * 0.75 = 156.
        assert_eq!(img.get_pixel(150, 84).0, [0, 0, 255, 255]);
        let p = img.get_pixel(165, 84).0;
        assert!(p[0] > 150 && p[2] < 200, "{p:?}");
        // Nothing drawn outside the items: black canvas.
        assert_eq!(img.get_pixel(5, 95).0, [0, 0, 0, 255]);
    }
}
