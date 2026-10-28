//! Key / screen image composition. Everything is expressed as SVG and rasterized
//! with resvg, which gives us anti-aliased outlined titles like the official app.

use crate::device::ImageFormat;
use base64::Engine as _;
use image::{codecs::jpeg::JpegEncoder, imageops, DynamicImage, Rgba, RgbaImage};
use resvg::{tiny_skia, usvg};
use std::path::Path;
use std::sync::{Arc, OnceLock};

static FONTDB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
static EXTRA_FONT_DIRS: OnceLock<Vec<std::path::PathBuf>> = OnceLock::new();

/// Register additional font directories (must be called before the first render).
pub fn set_extra_font_dirs(dirs: Vec<std::path::PathBuf>) {
    let _ = EXTRA_FONT_DIRS.set(dirs);
}

pub fn fontdb() -> Arc<usvg::fontdb::Database> {
    FONTDB
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            for d in EXTRA_FONT_DIRS.get().into_iter().flatten() {
                db.load_fonts_dir(d);
            }
            // Bundled fallback so titles render even on systems without (matching) fonts.
            db.load_font_data(include_bytes!("../assets/fonts/DejaVuSans.ttf").to_vec());
            db.load_font_data(include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf").to_vec());
            let families: Vec<String> = db.faces().flat_map(|f| f.families.iter().map(|(n, _)| n.clone())).collect();
            // A common UI font, else the desktop's sans-serif font (fontconfig), else DejaVu.
            let configured = std::process::Command::new("fc-match")
                .args(["-f", "%{family[0]}", "sans-serif"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|f| !f.is_empty());
            let prefs = ["Inter", "Noto Sans", "Adwaita Sans", "Cantarell", "Ubuntu", "Open Sans", "Roboto", "Liberation Sans", "Arimo", "Arial"];
            let pick = prefs.into_iter().chain(configured.as_deref()).find(|p| families.iter().any(|f| f == p)).unwrap_or("DejaVu Sans");
            db.set_sans_serif_family(pick);
            Arc::new(db)
        })
        .clone()
}

/// Encoded image ready to be embedded in SVG. Cheap to clone and to hash (key specs
/// are rebuilt and compared on every refresh): the content digest is computed once.
#[derive(Clone, Debug)]
pub struct ImageData {
    pub mime: String,
    pub bytes: Arc<[u8]>,
    digest: u64,
}

impl PartialEq for ImageData {
    fn eq(&self, o: &Self) -> bool {
        self.digest == o.digest && self.mime == o.mime && self.bytes.len() == o.bytes.len()
    }
}

impl std::hash::Hash for ImageData {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.mime.hash(h);
        self.digest.hash(h);
    }
}

/// Raster images are kept at most this large: keys are at most 144 px, previews use
/// up to 2x. Larger images would only cost memory and CPU on every refresh.
const MAX_EDGE: u32 = 288;

/// Decode a raster image with limits (decompression bombs).
fn decode_limited(bytes: &[u8]) -> Option<image::DynamicImage> {
    let mut r = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 << 20);
    r.limits(limits);
    r.decode().ok()
}

fn encode_png_dyn(img: &image::DynamicImage) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).ok()?;
    Some(out)
}

/// Decoded animated GIF: frames scaled to key size, with their display durations.
struct Animation {
    frames: Vec<(ImageData, std::time::Duration)>,
    total: std::time::Duration,
}

enum AnimState {
    Decoding,
    Static,
    Animated(Arc<Animation>),
}

fn decode_animation(bytes: &[u8]) -> Option<Animation> {
    use image::AnimationDecoder as _;
    use image::ImageDecoder as _;
    let mut dec = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(256 << 20);
    dec.set_limits(limits).ok()?;
    let mut frames = Vec::new();
    let mut total = std::time::Duration::ZERO;
    for f in dec.into_frames().take(500) {
        let f = f.ok()?;
        let (n, d) = f.delay().numer_denom_ms();
        let ms = n.checked_div(d).unwrap_or(100);
        // Like browsers: very short delays mean "as fast as possible" -> 100 ms.
        let delay = std::time::Duration::from_millis(if ms <= 10 { 100 } else { ms as u64 });
        let img = image::DynamicImage::ImageRgba8(f.into_buffer()).thumbnail(144, 144);
        frames.push((ImageData::new("image/png", encode_png_dyn(&img)?), delay));
        total += delay;
    }
    Some(Animation { frames, total })
}

/// For an animated GIF, the frame to show now (`None` for other images). The second
/// value tells whether the image needs periodic refreshes (animating or still decoding).
pub fn animation_frame(img: &ImageData) -> (Option<ImageData>, bool) {
    use std::collections::HashMap;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    static CACHE: OnceLock<parking_lot::Mutex<HashMap<u64, AnimState>>> = OnceLock::new();
    if img.mime != "image/gif" {
        return (None, false);
    }
    let cache = CACHE.get_or_init(Default::default);
    let mut c = cache.lock();
    let anim = match c.get(&img.digest) {
        Some(AnimState::Animated(a)) => a.clone(),
        Some(AnimState::Static) => return (None, false),
        Some(AnimState::Decoding) => return (None, true),
        None => {
            if c.len() > 64 {
                c.retain(|_, s| matches!(s, AnimState::Decoding));
            }
            c.insert(img.digest, AnimState::Decoding);
            // Decoding big GIFs takes a while: not on the caller's (engine) thread.
            let (digest, bytes) = (img.digest, img.bytes.clone());
            std::thread::spawn(move || {
                let state = match decode_animation(&bytes) {
                    Some(a) if a.frames.len() > 1 => AnimState::Animated(Arc::new(a)),
                    _ => AnimState::Static,
                };
                CACHE.get_or_init(Default::default).lock().insert(digest, state);
            });
            return (None, true);
        }
    };
    drop(c);
    let start = START.get_or_init(std::time::Instant::now);
    let mut t = std::time::Duration::from_nanos((start.elapsed().as_nanos() % anim.total.as_nanos().max(1)) as u64);
    for (frame, delay) in &anim.frames {
        if t < *delay {
            return (Some(frame.clone()), true);
        }
        t -= *delay;
    }
    (anim.frames.last().map(|f| f.0.clone()), true)
}

impl ImageData {
    pub fn new(mime: impl Into<String>, bytes: impl Into<Arc<[u8]>>) -> ImageData {
        use std::hash::{Hash, Hasher};
        let bytes = bytes.into();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        ImageData { mime: mime.into(), bytes, digest: h.finish() }
    }

    pub fn svg(svg: impl Into<String>) -> ImageData {
        ImageData::new("image/svg+xml", svg.into().into_bytes())
    }

    /// Scale large raster images down once (GIFs stay as they are for animation).
    /// Images that can't be decoded within the limits are dropped.
    fn normalized(self) -> Option<ImageData> {
        if !matches!(self.mime.as_str(), "image/png" | "image/jpeg" | "image/webp" | "image/bmp") || self.bytes.len() <= 64 * 1024 {
            return Some(self);
        }
        let img = decode_limited(&self.bytes)?;
        if img.width() <= MAX_EDGE && img.height() <= MAX_EDGE && self.bytes.len() <= 512 * 1024 {
            return Some(self);
        }
        let small = img.resize(MAX_EDGE.min(img.width()), MAX_EDGE.min(img.height()), imageops::FilterType::Lanczos3);
        Some(ImageData::new("image/png", encode_png_dyn(&small)?))
    }

    /// Read an image file; cached by path, size and modification time because
    /// key specs are rebuilt on every refresh.
    pub fn from_file(path: &Path) -> Option<ImageData> {
        use std::collections::HashMap;
        type FileCache = parking_lot::Mutex<HashMap<std::path::PathBuf, (std::time::SystemTime, u64, ImageData)>>;
        static FILES: OnceLock<FileCache> = OnceLock::new();
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let mtime = meta.modified().ok()?;
        let cache = FILES.get_or_init(Default::default);
        if let Some((t, len, d)) = cache.lock().get(path) {
            if *t == mtime && *len == meta.len() {
                return Some(d.clone());
            }
        }
        let d = Self::read_file(path)?;
        let mut c = cache.lock();
        if c.len() > 2000 {
            c.clear();
        }
        c.insert(path.to_path_buf(), (mtime, meta.len(), d.clone()));
        Some(d)
    }

    fn read_file(path: &Path) -> Option<ImageData> {
        let bytes = std::fs::read(path).ok()?;
        let mime = mime_guess::from_path(path).first_or_octet_stream().essence_str().to_string();
        let mime = if mime == "application/octet-stream" { sniff(&bytes).to_string() } else { mime };
        ImageData::new(mime, bytes).normalized()
    }

    /// Parse `data:` URLs as sent by plugins (`setImage`), base64 or percent/plain encoded.
    /// Lenient like browsers: line-wrapped or unpadded base64, stray trailing `;`
    /// (World Clock plugin), MIME aliases.
    pub fn from_data_url(url: &str) -> Option<ImageData> {
        let rest = url.trim().strip_prefix("data:")?;
        let (meta, data) = rest.split_once(',')?;
        let mime = meta.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        let bytes = if meta.split(';').any(|p| p.trim().eq_ignore_ascii_case("base64")) {
            use base64::engine::{general_purpose::GeneralPurpose, DecodePaddingMode, GeneralPurposeConfig};
            const LENIENT: GeneralPurpose =
                GeneralPurpose::new(&base64::alphabet::STANDARD, GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent));
            let clean: String = percent_decode(data).chars().filter(|c| !c.is_whitespace() && *c != ';').collect();
            LENIENT.decode(clean).ok()?
        } else {
            let text = percent_decode(data);
            let text = text.trim_end();
            text.strip_suffix(';').unwrap_or(text).as_bytes().to_vec()
        };
        let mime = match mime.as_str() {
            "image/pjpeg" | "image/jpg" => "image/jpeg".to_string(),
            "image/x-png" => "image/png".to_string(),
            "" | "application/octet-stream" => sniff(&bytes).to_string(),
            _ => mime,
        };
        ImageData::new(mime, bytes).normalized()
    }

    pub fn to_href(&self) -> String {
        let mut mime = self.mime.as_str();
        // usvg cannot decode animated GIF frames itself; re-encode the first frame.
        let png;
        let bytes: &[u8] = if mime == "image/gif" || mime == "image/bmp" || mime == "image/webp" {
            match decode_limited(&self.bytes).and_then(|img| encode_png_dyn(&img)) {
                Some(out) => {
                    png = out;
                    mime = "image/png";
                    &png
                }
                None => &self.bytes,
            }
        } else {
            &self.bytes
        };
        format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
    }
}

fn sniff(b: &[u8]) -> &'static str {
    if b.starts_with(b"\x89PNG") {
        "image/png"
    } else if b.starts_with(b"\xff\xd8") {
        "image/jpeg"
    } else if b.starts_with(b"GIF8") {
        "image/gif"
    } else if b.starts_with(b"RIFF") {
        "image/webp"
    } else if b.starts_with(b"BM") {
        "image/bmp"
    } else {
        "image/svg+xml"
    }
}

pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// SVG paint for a color or a gradient. Gradients use the Elgato notation
/// `"0:#ff0000,1:#00ff00"`, optionally prefixed with `h$` (horizontal, default) or
/// `v$` (vertical) as in the official info bar layouts. Gradient definitions are
/// appended to `defs` and referenced by `id`.
pub fn svg_paint(c: &str, defs: &mut String, id: &str) -> String {
    let c = c.trim();
    let (vertical, body) = match c.get(..2) {
        Some("v$") => (true, &c[2..]),
        Some("h$") => (false, &c[2..]),
        _ => (false, c),
    };
    if !body.contains(':') {
        return xml_escape(body);
    }
    let mut stops = String::new();
    for part in body.split(',') {
        if let Some((o, col)) = part.split_once(':') {
            let o: f64 = o.trim().parse().unwrap_or(0.0);
            stops += &format!(r#"<stop offset="{}" stop-color="{}"/>"#, o.clamp(0.0, 1.0), xml_escape(col.trim()));
        }
    }
    let (x2, y2) = if vertical { (0, 1) } else { (1, 0) };
    *defs += &format!(r#"<linearGradient id="{id}" x1="0" y1="0" x2="{x2}" y2="{y2}">{stops}</linearGradient>"#);
    format!("url(#{id})")
}

pub fn xml_escape(s: &str) -> String {
    // Control characters are not allowed in XML: one would make the whole SVG invalid.
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[derive(Clone, Debug, PartialEq, Hash)]
pub struct Title {
    pub text: String,
    /// "top" | "middle" | "bottom"
    pub align: String,
    /// Font size in Elgato points (relative to a 72 px key).
    pub size: u32,
    pub color: String,
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub outline: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Hash)]
pub enum Overlay {
    Ok,
    Alert,
}

#[derive(Clone, Debug, Default, PartialEq, Hash)]
pub struct KeySpec {
    pub background: Option<String>,
    pub image: Option<ImageData>,
    pub title: Option<Title>,
    pub overlay: Option<Overlay>,
    /// Large centered text (page indicator, timer) drawn when no image is set.
    pub big_text: Option<String>,
}

/// Canvas the SVG is authored in (Elgato keys are 72pt; we use 2x).
const CANVAS: f32 = 144.0;

pub fn key_svg(spec: &KeySpec) -> String {
    let mut s = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{c}" height="{c}" viewBox="0 0 {c} {c}">"#,
        c = CANVAS
    );
    s += &format!(r#"<rect width="{c}" height="{c}" fill="{}"/>"#, xml_escape(spec.background.as_deref().unwrap_or("#000000")), c = CANVAS);
    if let Some(img) = &spec.image {
        s += &format!(r#"<image width="{c}" height="{c}" preserveAspectRatio="xMidYMid meet" xlink:href="{}"/>"#, img.to_href(), c = CANVAS);
    }
    if let Some(t) = &spec.big_text {
        s += &format!(
            r##"<text x="72" y="72" font-family="sans-serif" font-weight="bold" font-size="56" fill="#ffffff" text-anchor="middle" dominant-baseline="central">{}</text>"##,
            xml_escape(t)
        );
    }
    if let Some(t) = &spec.title {
        s += &title_svg(t);
    }
    match spec.overlay {
        Some(Overlay::Ok) => {
            s += r##"<rect width="144" height="144" fill="#000000" fill-opacity="0.55"/><circle cx="72" cy="72" r="34" fill="#2ecc71"/><path d="M55 73 L67 85 L90 60" stroke="#ffffff" stroke-width="9" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"##
        }
        Some(Overlay::Alert) => {
            s += r##"<rect width="144" height="144" fill="#000000" fill-opacity="0.55"/><path d="M72 36 L110 104 L34 104 Z" fill="#f1c40f" stroke="#f1c40f" stroke-width="8" stroke-linejoin="round"/><rect x="68" y="56" width="8" height="28" rx="3" fill="#000"/><circle cx="72" cy="94" r="5" fill="#000"/>"##
        }
        None => {}
    }
    s += "</svg>";
    s
}

fn title_svg(t: &Title) -> String {
    if t.text.is_empty() {
        return String::new();
    }
    let scale = CANVAS / 72.0;
    let size = t.size.max(4) as f32 * scale;
    let lines: Vec<&str> = t.text.split('\n').collect();
    let lh = size * 1.15;
    let total = lh * lines.len() as f32;
    let margin = 3.0 * scale;
    let first_baseline = match t.align.as_str() {
        "top" => margin + size * 0.9,
        "middle" => (CANVAS - total) / 2.0 + size * 0.9,
        _ => CANVAS - margin - total + size * 0.9,
    };
    let family = if t.family.is_empty() { "sans-serif".to_string() } else { format!("'{}', sans-serif", xml_escape(&t.family)) };
    let mut s = format!(
        r#"<text xml:space="preserve" font-family="{family}" font-size="{size}" font-weight="{}" font-style="{}" text-decoration="{}" fill="{}" text-anchor="middle""#,
        if t.bold { "bold" } else { "normal" },
        if t.italic { "italic" } else { "normal" },
        if t.underline { "underline" } else { "none" },
        xml_escape(&t.color),
    );
    if t.outline > 0 {
        s += &format!(r##" stroke="#000000" stroke-width="{}" stroke-linejoin="round" paint-order="stroke""##, t.outline as f32 * scale * 1.2);
    }
    s += ">";
    for (i, l) in lines.iter().enumerate() {
        s += &format!(r#"<tspan x="72" y="{}">{}</tspan>"#, first_baseline + i as f32 * lh, xml_escape(l));
    }
    s += "</text>";
    s
}

/// usvg's default font selection, but with sans-serif instead of serif as the last
/// resort. Plugin images are nested SVGs, which don't get `Options::font_family`
/// (usvg 0.45) but do use this resolver.
fn select_font(font: &usvg::Font, db: &mut Arc<usvg::fontdb::Database>) -> Option<usvg::fontdb::ID> {
    use usvg::fontdb::{Family, Query, Stretch, Style, Weight};
    let mut families: Vec<Family> = font
        .families()
        .iter()
        .map(|f| match f {
            usvg::FontFamily::Serif => Family::Serif,
            usvg::FontFamily::SansSerif => Family::SansSerif,
            usvg::FontFamily::Cursive => Family::Cursive,
            usvg::FontFamily::Fantasy => Family::Fantasy,
            usvg::FontFamily::Monospace => Family::Monospace,
            usvg::FontFamily::Named(s) => Family::Name(s),
        })
        .collect();
    families.push(Family::SansSerif);
    let stretch = match font.stretch() {
        usvg::FontStretch::UltraCondensed => Stretch::UltraCondensed,
        usvg::FontStretch::ExtraCondensed => Stretch::ExtraCondensed,
        usvg::FontStretch::Condensed => Stretch::Condensed,
        usvg::FontStretch::SemiCondensed => Stretch::SemiCondensed,
        usvg::FontStretch::Normal => Stretch::Normal,
        usvg::FontStretch::SemiExpanded => Stretch::SemiExpanded,
        usvg::FontStretch::Expanded => Stretch::Expanded,
        usvg::FontStretch::ExtraExpanded => Stretch::ExtraExpanded,
        usvg::FontStretch::UltraExpanded => Stretch::UltraExpanded,
    };
    let style = match font.style() {
        usvg::FontStyle::Normal => Style::Normal,
        usvg::FontStyle::Italic => Style::Italic,
        usvg::FontStyle::Oblique => Style::Oblique,
    };
    db.query(&Query { families: &families, weight: Weight(font.weight()), stretch, style })
}

/// Rasterize an SVG document to `w`x`h` pixels.
pub fn rasterize(svg: &str, w: u32, h: u32) -> RgbaImage {
    let db = fontdb();
    // Used when a font-family value can't be parsed (e.g. "--apple-system" from the
    // weather plugin); usvg's default would be Times New Roman.
    let font_family = db.family_name(&usvg::fontdb::Family::SansSerif).to_string();
    let font_resolver = usvg::FontResolver { select_font: Box::new(select_font), select_fallback: usvg::FontResolver::default_fallback_selector() };
    let opt = usvg::Options { fontdb: db, font_family, font_resolver, ..Default::default() };
    let mut pixmap = tiny_skia::Pixmap::new(w.max(1), h.max(1)).unwrap();
    match usvg::Tree::from_str(svg, &opt) {
        Ok(tree) => {
            let sz = tree.size();
            let tf = tiny_skia::Transform::from_scale(w as f32 / sz.width(), h as f32 / sz.height());
            resvg::render(&tree, tf, &mut pixmap.as_mut());
        }
        Err(e) => tracing::warn!("svg render failed: {e}"),
    }
    // tiny-skia uses premultiplied alpha.
    let mut img = RgbaImage::new(w, h);
    for (p, c) in img.pixels_mut().zip(pixmap.pixels()) {
        let c = c.demultiply();
        *p = Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    img
}

/// Render + encode with a small cache keyed by the spec hash (previews and device
/// images are requested repeatedly for unchanged keys).
pub fn cached(spec: &KeySpec, px: u32, kind: &str, encode: impl FnOnce(&RgbaImage) -> Vec<u8>) -> Vec<u8> {
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};
    type Cache = parking_lot::Mutex<HashMap<(u64, u32, String), Vec<u8>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    spec.hash(&mut h);
    let key = (h.finish(), px, kind.to_string());
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().get(&key) {
        return v.clone();
    }
    let out = encode(&render_key(spec, px));
    let mut c = cache.lock();
    if c.len() > 600 {
        c.clear();
    }
    c.insert(key, out.clone());
    out
}

pub fn render_key(spec: &KeySpec, px: u32) -> RgbaImage {
    rasterize(&key_svg(spec), px, px)
}

/// Device encoding: JPEG, rotated by 180° (most second generation models).
pub fn encode_device_jpeg(img: &RgbaImage) -> Vec<u8> {
    encode_device(img, ImageFormat::Jpeg180)
}

/// Encode an image in the native format of a device.
pub fn encode_device(img: &RgbaImage, format: ImageFormat) -> Vec<u8> {
    let img = match format {
        ImageFormat::Jpeg180 | ImageFormat::Bmp180 => imageops::rotate180(img),
        ImageFormat::Jpeg => img.clone(),
        // Rotate 90° counter-clockwise, then mirror vertically = transpose.
        ImageFormat::BmpTranspose => RgbaImage::from_fn(img.height(), img.width(), |x, y| *img.get_pixel(y, x)),
    };
    let rgb = DynamicImage::ImageRgba8(img).to_rgb8();
    let mut out = Vec::new();
    match format {
        ImageFormat::Jpeg180 | ImageFormat::Jpeg => JpegEncoder::new_with_quality(&mut out, 94).encode_image(&rgb).expect("jpeg encode"),
        ImageFormat::Bmp180 | ImageFormat::BmpTranspose => {
            image::codecs::bmp::BmpEncoder::new(&mut out).encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8).expect("bmp encode")
        }
    }
    out
}

/// Cache key component for `cached` (one entry per device format).
pub fn format_tag(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg180 => "device-jpeg",
        ImageFormat::Jpeg => "device-jpeg-upright",
        ImageFormat::Bmp180 => "device-bmp",
        ImageFormat::BmpTranspose => "device-bmp-transposed",
    }
}

pub fn encode_png(img: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    DynamicImage::ImageRgba8(img.clone()).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).expect("png encode");
    out
}

/// Simple icon used for built-in actions when the original Elgato resources
/// have not been imported.
/// Fallback icon for an action without its own image: a simple light glyph on a
/// transparent background, matching the flat line style of the bundled icons. `_color`
/// is kept for call-site compatibility but no longer drawn.
pub fn fallback_icon_svg(glyph: &str, _color: &str) -> String {
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144"><text x="72" y="78" font-family="sans-serif" font-weight="600" font-size="62" fill="#e6e9ef" text-anchor="middle" dominant-baseline="central">{}</text></svg>"##,
        xml_escape(glyph)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_url_variants() {
        let svg = ImageData::from_data_url("data:image/svg+xml;charset=utf8,<svg xmlns=%22http://www.w3.org/2000/svg%22/>").unwrap();
        assert_eq!(svg.mime, "image/svg+xml");
        assert!(String::from_utf8(svg.bytes.to_vec()).unwrap().contains("\"http://www.w3.org/2000/svg\""));
        let png = ImageData::from_data_url("data:image/png;base64,iVBORw0K").unwrap();
        assert_eq!(png.mime, "image/png");
    }

    fn gif(colors: &[[u8; 4]]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
            for c in colors {
                let frame = image::Frame::from_parts(RgbaImage::from_pixel(20, 20, Rgba(*c)), 0, 0, image::Delay::from_numer_denom_ms(100, 1));
                enc.encode_frame(frame).unwrap();
            }
        }
        out
    }

    #[test]
    fn animated_gifs() {
        let wait = |img: &ImageData| {
            for _ in 0..200 {
                let (frame, ticking) = animation_frame(img);
                if frame.is_some() || !ticking {
                    return (frame, ticking);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("animation not decoded");
        };
        let anim = ImageData::new("image/gif", gif(&[[255, 0, 0, 255], [0, 0, 255, 255]]));
        let (frame, ticking) = wait(&anim);
        assert!(ticking);
        assert_eq!(&frame.unwrap().bytes[..4], b"\x89PNG");
        // A single frame GIF is static.
        let still = ImageData::new("image/gif", gif(&[[0, 255, 0, 255]]));
        assert_eq!(wait(&still), (None, false));
    }

    #[test]
    fn large_images_are_scaled_down() {
        let mut big = Vec::new();
        let noise = RgbaImage::from_fn(1200, 800, |x, y| Rgba([(x * 7 % 256) as u8, (y * 13 % 256) as u8, ((x ^ y) % 256) as u8, 255]));
        DynamicImage::ImageRgba8(noise).write_to(&mut std::io::Cursor::new(&mut big), image::ImageFormat::Png).unwrap();
        let url = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&big));
        let img = ImageData::from_data_url(&url).unwrap();
        let decoded = image::load_from_memory(&img.bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (288, 192));
    }

    #[test]
    fn lenient_data_urls() {
        // World Clock appends a ';' after the SVG.
        let svg = ImageData::from_data_url("data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg'/>;").unwrap();
        assert!(String::from_utf8(svg.bytes.to_vec()).unwrap().ends_with("/>"));
        let png = ImageData::from_data_url("data:image/x-png;base64,iVBO\nRw0K").unwrap();
        assert_eq!((png.mime.as_str(), &png.bytes[..4]), ("image/png", &b"\x89PNG"[..]));
        let unpadded = ImageData::from_data_url("data:image/png;base64,iVBORw0KGgo").unwrap();
        assert_eq!(&unpadded.bytes[..4], b"\x89PNG");
    }

    #[test]
    fn control_characters_in_titles() {
        let spec = |t: &str| KeySpec {
            title: Some(Title {
                text: t.into(),
                align: "middle".into(),
                size: 16,
                color: "#ffffff".into(),
                family: String::new(),
                bold: false,
                italic: false,
                underline: false,
                outline: 0,
            }),
            ..Default::default()
        };
        assert_eq!(render_key(&spec("Track\x1b[0m"), 72).as_raw(), render_key(&spec("Track[0m"), 72).as_raw());
    }

    #[test]
    fn gradients() {
        let mut defs = String::new();
        assert_eq!(svg_paint("#ff0000", &mut defs, "a"), "#ff0000");
        assert!(defs.is_empty());
        assert_eq!(svg_paint("h$0:#2DB19A,1:#64C835", &mut defs, "g1"), "url(#g1)");
        assert!(defs.contains(r##"<stop offset="1" stop-color="#64C835"/>"##));
    }

    #[test]
    fn device_formats() {
        let mut img = RgbaImage::from_pixel(80, 80, Rgba([0, 0, 0, 255]));
        img.put_pixel(10, 0, Rgba([255, 0, 0, 255]));
        let bmp = encode_device(&img, ImageFormat::BmpTranspose);
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(bmp.len(), 54 + 80 * 80 * 3);
        let back = image::load_from_memory(&bmp).unwrap().to_rgb8();
        assert_eq!(back.get_pixel(0, 10).0, [255, 0, 0]);
        let jpg = encode_device(&img, ImageFormat::Jpeg);
        assert_eq!(&jpg[..2], b"\xff\xd8");
    }

    #[test]
    fn css_system_font_stack_falls_back_to_sans() {
        // The weather plugin sends "--apple-system" when it thinks it runs on macOS; usvg
        // can't parse that and drops the whole list. The fallback must be sans-serif.
        let svg = |family: &str| {
            format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96"><text x="10" y="60" font-size="40" font-family="{family}" fill="#fff">Ag</text></svg>"##
            )
        };
        let stack = rasterize(
            &svg("--apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, Cantarell, 'Open Sans', 'Helvetica Neue', sans-serif"),
            96,
            96,
        );
        let weather = rasterize(&svg("--apple-system"), 96, 96);
        let sans = rasterize(&svg("sans-serif"), 96, 96);
        assert_eq!(stack.as_raw(), sans.as_raw());
        assert_eq!(weather.as_raw(), sans.as_raw());
        // Plugin images arrive as nested SVGs (data URLs in <image>).
        let nested = |family: &str| {
            let inner = base64::engine::general_purpose::STANDARD.encode(svg(family));
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96"><image href="data:image/svg+xml;base64,{inner}" width="96" height="96"/></svg>"#
            )
        };
        assert_eq!(rasterize(&nested("--apple-system"), 96, 96).as_raw(), rasterize(&nested("sans-serif"), 96, 96).as_raw());
    }

    #[test]
    fn renders_title() {
        // Must work without system fonts too (bundled DejaVu Sans).
        let spec = KeySpec {
            title: Some(Title {
                text: "Hi".into(),
                align: "middle".into(),
                size: 16,
                color: "#ffffff".into(),
                family: String::new(),
                bold: true,
                italic: false,
                underline: false,
                outline: 2,
            }),
            ..Default::default()
        };
        let img = render_key(&spec, 96);
        assert!(img.pixels().any(|p| p.0[0] > 200), "title pixels expected");
    }
}
