//! Key / screen image composition. Everything is expressed as SVG and rasterized
//! with resvg, which gives us anti-aliased outlined titles like the official app.

use crate::device::ImageFormat;
use base64::Engine as _;
use image::{codecs::jpeg::JpegEncoder, imageops, DynamicImage, Rgba, RgbaImage};
use resvg::{tiny_skia, usvg};
use std::path::Path;
use std::sync::{Arc, OnceLock};

static FONTDB: parking_lot::RwLock<Option<Arc<usvg::fontdb::Database>>> = parking_lot::RwLock::new(None);
static EXTRA_FONT_DIRS: parking_lot::Mutex<Vec<std::path::PathBuf>> = parking_lot::Mutex::new(Vec::new());

/// Register additional font directories (fonts of info bar layouts). When they change
/// (layouts imported while the service runs), the fonts are loaded again.
/// Bumped when the font folders change: a database loaded before is not stored.
static FONT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn set_extra_font_dirs(dirs: Vec<std::path::PathBuf>) {
    let mut cur = EXTRA_FONT_DIRS.lock();
    if *cur != dirs {
        *cur = dirs;
        FONT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *FONTDB.write() = None;
    }
}

pub fn fontdb() -> Arc<usvg::fontdb::Database> {
    if let Some(db) = FONTDB.read().as_ref() {
        return db.clone();
    }
    let generation = FONT_GENERATION.load(std::sync::atomic::Ordering::SeqCst);
    let db = Arc::new(load_fonts());
    let mut slot = FONTDB.write();
    if FONT_GENERATION.load(std::sync::atomic::Ordering::SeqCst) == generation {
        *slot = Some(db.clone());
    }
    db
}

fn load_fonts() -> usvg::fontdb::Database {
    let mut db = usvg::fontdb::Database::new();
    db.load_system_fonts();
    for d in EXTRA_FONT_DIRS.lock().iter() {
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
    db
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
        // The content wins over the file name (a WebP saved as .png).
        let sniffed = sniff(&bytes);
        let mime = if mime == "application/octet-stream" || (sniffed != "image/svg+xml" && mime.starts_with("image/") && sniffed != mime) {
            sniffed.to_string()
        } else {
            mime
        };
        ImageData::new(mime, bytes).normalized()
    }

    /// Parse `data:` URLs as sent by plugins (`setImage`), base64 or percent/plain encoded.
    /// Lenient like browsers: line-wrapped or unpadded base64, stray trailing `;`
    /// (World Clock plugin), MIME aliases.
    pub fn from_data_url(url: &str) -> Option<ImageData> {
        // Remembered: plugins send the same picture repeatedly, and big ones are decoded
        // and scaled ahead, outside the engine lock (see `sdk.rs`).
        type Decoded = parking_lot::Mutex<std::collections::HashMap<u64, Option<ImageData>>>;
        static DECODED: OnceLock<Decoded> = OnceLock::new();
        if url.len() < 4096 {
            return Self::parse_data_url(url);
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        url.hash(&mut h);
        let key = h.finish();
        let cache = DECODED.get_or_init(Default::default);
        if let Some(d) = cache.lock().get(&key) {
            return d.clone();
        }
        let d = Self::parse_data_url(url);
        let mut c = cache.lock();
        if c.len() > 64 {
            c.clear();
        }
        c.insert(key, d.clone());
        d
    }

    fn parse_data_url(url: &str) -> Option<ImageData> {
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

    /// The image as a data URL usvg can show inside another SVG (remembered per image:
    /// keys and the touch strip are rebuilt often).
    pub fn to_href(&self) -> String {
        type Hrefs = parking_lot::Mutex<std::collections::HashMap<u64, Arc<str>>>;
        static HREFS: OnceLock<Hrefs> = OnceLock::new();
        let cache = HREFS.get_or_init(Default::default);
        if let Some(h) = cache.lock().get(&self.digest) {
            return h.to_string();
        }
        let href: Arc<str> = self.make_href().into();
        let mut c = cache.lock();
        if c.len() > 256 {
            c.clear();
        }
        c.insert(self.digest, href.clone());
        href.to_string()
    }

    fn make_href(&self) -> String {
        let mut mime = self.mime.as_str();
        let png;
        let bytes: &[u8] = match mime {
            // usvg cannot decode GIF frames, BMP or WebP itself: the first frame as PNG.
            "image/gif" | "image/bmp" | "image/webp" => match decode_limited(&self.bytes).and_then(|img| encode_png_dyn(&img)) {
                Some(out) => {
                    png = out;
                    mime = "image/png";
                    &png
                }
                None => &self.bytes,
            },
            // An SVG inside an SVG loses its embedded raster images in usvg: render it here.
            "image/svg+xml" => match std::str::from_utf8(&self.bytes).ok().and_then(svg_to_png) {
                Some(out) => {
                    png = out;
                    mime = "image/png";
                    &png
                }
                None => &self.bytes,
            },
            _ => &self.bytes,
        };
        format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    /// For storing an uploaded image: formats usvg can't show (WebP, BMP) become PNG.
    pub fn storable(self) -> ImageData {
        if !matches!(self.mime.as_str(), "image/webp" | "image/bmp") {
            return self;
        }
        match decode_limited(&self.bytes).and_then(|img| encode_png_dyn(&img)) {
            Some(png) => ImageData::new("image/png", png),
            None => self,
        }
    }
}

/// Rasterize an SVG image at its own aspect ratio (longer side 288 px, enough for every
/// key and the touch strip).
fn svg_to_png(svg: &str) -> Option<Vec<u8>> {
    let tree = usvg::Tree::from_str(svg, &usvg_options()).ok()?;
    let (w, h) = (tree.size().width(), tree.size().height());
    let scale = MAX_EDGE as f32 / w.max(h);
    let (pw, ph) = (((w * scale).round() as u32).max(1), ((h * scale).round() as u32).max(1));
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph)?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    pixmap.encode_png().ok()
}

/// SVG options: our fonts, and images only from `data:` URLs (an SVG of a plugin must
/// not read files of the computer).
fn usvg_options() -> usvg::Options<'static> {
    let db = fontdb();
    // Used when a font-family value can't be parsed (e.g. "--apple-system" from the
    // weather plugin); usvg's default would be Times New Roman.
    let font_family = db.family_name(&usvg::fontdb::Family::SansSerif).to_string();
    let font_resolver = usvg::FontResolver { select_font: Box::new(select_font), select_fallback: usvg::FontResolver::default_fallback_selector() };
    let image_href_resolver = usvg::ImageHrefResolver { resolve_data: usvg::ImageHrefResolver::default_data_resolver(), resolve_string: Box::new(|_, _| None) };
    usvg::Options { fontdb: db, font_family, font_resolver, image_href_resolver, ..Default::default() }
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

/// SVG paint for a colour or a CSS gradient: `linear-gradient(135deg, #f00 0%, #00f 100%)`
/// (an angle or `to <side>`) and `radial-gradient(circle, …)`; stops may use any CSS
/// colour, positions in %. Anything else goes to [`svg_paint`].
pub fn css_paint(c: &str, defs: &mut String, id: &str) -> String {
    match parse_css_gradient(c) {
        Some(g) => {
            let stops: String = g.stops.iter().map(|(col, at)| format!(r#"<stop offset="{at:.4}" stop-color="{}"/>"#, xml_escape(col))).collect();
            *defs += &match g.kind {
                GradientKind::Linear(deg) => {
                    // CSS: 0deg points up, 90deg right; the line spans the whole square.
                    let (sin, cos) = deg.to_radians().sin_cos();
                    let half = (sin.abs() + cos.abs()) / 2.0;
                    // (Rounded, and without a "-0".)
                    let r = |v: f64| (v * 10000.0).round() / 10000.0 + 0.0;
                    let (x1, y1, x2, y2) = (r(0.5 - sin * half), r(0.5 + cos * half), r(0.5 + sin * half), r(0.5 - cos * half));
                    format!(r#"<linearGradient id="{id}" x1="{x1:.4}" y1="{y1:.4}" x2="{x2:.4}" y2="{y2:.4}">{stops}</linearGradient>"#)
                }
                // Circle out to the corners (CSS default: farthest-corner).
                GradientKind::Radial => format!(r#"<radialGradient id="{id}" cx="0.5" cy="0.5" r="0.7071">{stops}</radialGradient>"#),
            };
            format!("url(#{id})")
        }
        None => svg_paint(c, defs, id),
    }
}

enum GradientKind {
    /// Angle in degrees (CSS convention).
    Linear(f64),
    Radial,
}

struct CssGradient {
    kind: GradientKind,
    /// Colour and position 0–1.
    stops: Vec<(String, f64)>,
}

/// Split at top-level commas (not inside `rgba(…)`).
fn split_top(s: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0i32, 0);
    for (i, ch) in s.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

fn parse_css_gradient(c: &str) -> Option<CssGradient> {
    let c = c.trim().trim_end_matches(';');
    let c = c.strip_prefix("background:").or_else(|| c.strip_prefix("background-image:")).map(str::trim).unwrap_or(c);
    let (kind, inner) = if let Some(r) = c.strip_prefix("linear-gradient(") {
        (GradientKind::Linear(180.0), r)
    } else if let Some(r) = c.strip_prefix("radial-gradient(") {
        (GradientKind::Radial, r)
    } else {
        return None;
    };
    let inner = inner.strip_suffix(')')?;
    let mut parts = split_top(inner);
    let mut kind = kind;
    // First part: direction or shape, if it isn't a colour stop.
    let first = parts.first()?.to_ascii_lowercase();
    if let GradientKind::Linear(_) = kind {
        let side = |s: &str| -> Option<f64> {
            Some(match s.trim() {
                "to top" => 0.0,
                "to right" => 90.0,
                "to bottom" => 180.0,
                "to left" => 270.0,
                "to top right" | "to right top" => 45.0,
                "to bottom right" | "to right bottom" => 135.0,
                "to bottom left" | "to left bottom" => 225.0,
                "to top left" | "to left top" => 315.0,
                _ => return None,
            })
        };
        let angle = first.strip_suffix("deg").and_then(|a| a.trim().parse::<f64>().ok()).or_else(|| side(&first));
        if let Some(a) = angle {
            kind = GradientKind::Linear(a);
            parts.remove(0);
        }
    } else if first.starts_with("circle")
        || first.starts_with("ellipse")
        || first.starts_with("closest")
        || first.starts_with("farthest")
        || first.starts_with("at ")
    {
        parts.remove(0);
    }
    if parts.len() < 2 || parts.len() > 16 {
        return None;
    }
    let n = parts.len();
    let mut stops = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        // "<colour> <pos>%" – the position is optional (spread evenly then).
        let (col, at) = match p.rsplit_once(char::is_whitespace) {
            Some((col, pos)) if pos.ends_with('%') => (col.trim(), pos.trim_end_matches('%').trim().parse::<f64>().ok()? / 100.0),
            _ => (p.trim(), i as f64 / (n - 1) as f64),
        };
        if col.is_empty() || col.len() > 40 || col.contains(['<', '>', '"', '\'', '&']) {
            return None;
        }
        stops.push((col.to_string(), at.clamp(0.0, 1.0)));
    }
    Some(CssGradient { kind, stops })
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
    // A colour or a CSS gradient (as cssgradient.io and the UI's gradient editor write it).
    let mut defs = String::new();
    let bg = css_paint(spec.background.as_deref().unwrap_or("#000000"), &mut defs, "keybg");
    if !defs.is_empty() {
        s += &format!("<defs>{defs}</defs>");
    }
    s += &format!(r#"<rect width="{c}" height="{c}" fill="{bg}"/>"#, c = CANVAS);
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
    let opt = usvg_options();
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
    fn css_gradients() {
        let mut defs = String::new();
        // As cssgradient.io writes it.
        let p = css_paint("background: linear-gradient(90deg, rgba(2,0,36,1) 0%, rgba(9,121,105,1) 35%, rgba(0,212,255,1) 100%);", &mut defs, "k");
        assert_eq!(p, "url(#k)");
        assert!(defs.contains(r#"x1="0.0000" y1="0.5000" x2="1.0000" y2="0.5000""#), "{defs}");
        assert!(defs.contains(r#"<stop offset="0.3500" stop-color="rgba(9,121,105,1)"/>"#), "{defs}");
        let mut defs = String::new();
        css_paint("linear-gradient(to bottom, #fff, #000)", &mut defs, "k");
        assert!(defs.contains(r#"x1="0.5000" y1="0.0000" x2="0.5000" y2="1.0000""#), "{defs}");
        assert!(defs.contains(r#"offset="1.0000""#));
        let mut defs = String::new();
        css_paint("radial-gradient(circle, #ff0000 0%, #0000ff 100%)", &mut defs, "k");
        assert!(defs.contains("<radialGradient"), "{defs}");
        // Rendered: left red, right blue.
        let key = render_key(&KeySpec { background: Some("linear-gradient(90deg, #ff0000 0%, #0000ff 100%)".into()), ..Default::default() }, 72);
        let (l, r) = (key.get_pixel(2, 36).0, key.get_pixel(69, 36).0);
        assert!(l[0] > 200 && l[2] < 60 && r[2] > 200 && r[0] < 60, "{l:?} {r:?}");
        // Not a gradient, or nonsense: a colour (escaped) as before.
        assert_eq!(css_paint("#123456", &mut String::new(), "k"), "#123456");
        assert_eq!(css_paint("linear-gradient(90deg, \"/><script>, #000)", &mut String::new(), "k"), "linear-gradient(90deg, &quot;/&gt;&lt;script&gt;, #000)");
        assert_eq!(css_paint("linear-gradient(90deg, #fff)", &mut String::new(), "k"), "linear-gradient(90deg, #fff)");
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

    fn title(text: &str, align: &str, size: u32) -> Title {
        Title {
            text: text.into(),
            align: align.into(),
            size,
            color: "#ffffff".into(),
            family: String::new(),
            bold: false,
            italic: false,
            underline: false,
            outline: 0,
        }
    }

    fn png(img: &RgbaImage) -> ImageData {
        ImageData::new("image/png", encode_png(img))
    }

    /// Bounding rows (min, max) and mean row of bright pixels.
    fn bright_rows(img: &RgbaImage) -> (u32, u32, f32) {
        let ys: Vec<u32> = img.enumerate_pixels().filter(|(_, _, p)| p.0[0] > 128 && p.0[1] > 128 && p.0[2] > 128).map(|(_, y, _)| y).collect();
        assert!(!ys.is_empty(), "no bright pixels");
        (*ys.iter().min().unwrap(), *ys.iter().max().unwrap(), ys.iter().sum::<u32>() as f32 / ys.len() as f32)
    }

    #[test]
    fn key_backgrounds() {
        let black = render_key(&KeySpec::default(), 72);
        assert_eq!(black.dimensions(), (72, 72));
        assert!(black.pixels().all(|p| p.0 == [0, 0, 0, 255]));
        let red = render_key(&KeySpec { background: Some("#ff0000".into()), ..Default::default() }, 72);
        assert!(red.pixels().all(|p| p.0 == [255, 0, 0, 255]));
        let named = render_key(&KeySpec { background: Some("blue".into()), ..Default::default() }, 96);
        assert_eq!(named.get_pixel(48, 48).0, [0, 0, 255, 255]);
        // Markup in the colour can't break out of the attribute.
        let svg = key_svg(&KeySpec { background: Some(r#"red"/><script/>"#.into()), ..Default::default() });
        assert!(svg.contains("fill=\"red&quot;/&gt;&lt;script/&gt;\"") && !svg.contains("<script"));
    }

    #[test]
    fn key_images() {
        let green = png(&RgbaImage::from_pixel(10, 10, Rgba([0, 255, 0, 255])));
        let img = render_key(&KeySpec { image: Some(green), ..Default::default() }, 72);
        assert_eq!(img.get_pixel(1, 1).0, [0, 255, 0, 255]);
        assert_eq!(img.get_pixel(70, 70).0, [0, 255, 0, 255]);
        // Wide images are letterboxed (meet) over the background.
        let wide = png(&RgbaImage::from_pixel(20, 10, Rgba([0, 0, 255, 255])));
        let img = render_key(&KeySpec { background: Some("#ff0000".into()), image: Some(wide), ..Default::default() }, 72);
        assert_eq!(img.get_pixel(36, 2).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(36, 36).0, [0, 0, 255, 255]);
        assert_eq!(img.get_pixel(36, 69).0, [255, 0, 0, 255]);
        // Transparent images show the background through.
        let clear = png(&RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 0])));
        let img = render_key(&KeySpec { background: Some("#ff0000".into()), image: Some(clear), ..Default::default() }, 72);
        assert_eq!(img.get_pixel(36, 36).0, [255, 0, 0, 255]);
        // Broken image data falls back to the plain background.
        let plain = KeySpec { background: Some("#336699".into()), ..Default::default() };
        let broken = KeySpec { image: Some(ImageData::new("image/png", b"not a png".to_vec())), ..plain.clone() };
        assert_eq!(render_key(&broken, 72).as_raw(), render_key(&plain, 72).as_raw());
        let broken_svg = KeySpec { image: Some(ImageData::svg("<svg")), ..plain.clone() };
        assert_eq!(render_key(&broken_svg, 72).as_raw(), render_key(&plain, 72).as_raw());
    }

    #[test]
    fn title_alignment() {
        let render = |align: &str| render_key(&KeySpec { title: Some(title("Hg", align, 14)), ..Default::default() }, 144);
        let (top, middle, bottom) = (bright_rows(&render("top")), bright_rows(&render("middle")), bright_rows(&render("bottom")));
        assert!(top.2 < 48.0, "top {top:?}");
        assert!((56.0..88.0).contains(&middle.2), "middle {middle:?}");
        assert!(bottom.2 > 96.0, "bottom {bottom:?}");
        assert!(top.0 < 20 && bottom.1 > 124);
        // Unknown alignments use the bottom position (Elgato default).
        assert_eq!(render("").as_raw(), render("bottom").as_raw());
        assert_eq!(render("sideways").as_raw(), render("bottom").as_raw());
    }

    #[test]
    fn title_size_and_lines() {
        let height = |t: Title| {
            let (a, b, _) = bright_rows(&render_key(&KeySpec { title: Some(t), ..Default::default() }, 144));
            b - a
        };
        let small = height(title("H", "middle", 10));
        let large = height(title("H", "middle", 20));
        assert!(large > small * 3 / 2, "{small} vs {large}");
        let two = height(title("H\nH", "middle", 10));
        assert!(two > small * 2, "{small} vs {two}");
        // Sizes below 4 pt are raised to 4 pt.
        assert_eq!(title_svg(&title("x", "top", 0)), title_svg(&title("x", "top", 4)));
        assert!(title_svg(&title("x", "top", 0)).contains(r#"font-size="8""#));
        assert_eq!(title_svg(&title("", "top", 12)), "");
        let svg = title_svg(&title("a\nb\nc", "middle", 12));
        assert_eq!(svg.matches("<tspan").count(), 3);
    }

    #[test]
    fn title_markup() {
        let mut t = title("A & <B>", "top", 12);
        t.family = r#"Fira "Code""#.into();
        t.bold = true;
        t.italic = true;
        t.underline = true;
        t.outline = 2;
        t.color = "#123456".into();
        let svg = title_svg(&t);
        assert!(svg.contains(r#"font-family="'Fira &quot;Code&quot;', sans-serif""#), "{svg}");
        assert!(svg.contains(r##"font-weight="bold" font-style="italic" text-decoration="underline" fill="#123456""##), "{svg}");
        assert!(svg.contains(r#"stroke-width="4.8""#), "{svg}");
        assert!(svg.contains("A &amp; &lt;B&gt;"));
        let plain = title_svg(&title("x", "top", 12));
        assert!(plain.contains(r#"font-family="sans-serif""#) && plain.contains(r#"font-weight="normal" font-style="normal" text-decoration="none""#));
        assert!(!plain.contains("stroke"));
        // Outlined titles get dark pixels around the glyphs on a light background.
        let mut o = title("O", "middle", 24);
        o.outline = 3;
        let img = render_key(&KeySpec { background: Some("#808080".into()), title: Some(o), ..Default::default() }, 144);
        assert!(img.pixels().any(|p| p.0[0] < 40));
    }

    #[test]
    fn overlays_and_big_text() {
        let base = KeySpec { background: Some("#ff0000".into()), ..Default::default() };
        let ok = render_key(&KeySpec { overlay: Some(Overlay::Ok), ..base.clone() }, 144);
        // Dimmed background, green circle.
        let corner = ok.get_pixel(2, 2).0;
        assert!(corner[0] > 90 && corner[0] < 140 && corner[1] == 0, "{corner:?}");
        let c = ok.get_pixel(72, 46).0;
        assert!(c[1] > 180 && c[0] < 80, "{c:?}");
        let alert = render_key(&KeySpec { overlay: Some(Overlay::Alert), ..base.clone() }, 144);
        let y = alert.get_pixel(50, 100).0;
        assert!(y[0] > 200 && y[1] > 160 && y[2] < 60, "{y:?}");
        assert_eq!(alert.get_pixel(72, 70).0[..3], [0, 0, 0]);

        let big = KeySpec { big_text: Some("3<".into()), ..base.clone() };
        assert!(key_svg(&big).contains(">3&lt;</text>"));
        let img = render_key(&big, 144);
        let (top, bottom, mean) = bright_rows(&img);
        assert!(top > 20 && bottom < 124 && (60.0..84.0).contains(&mean), "{top} {bottom} {mean}");
    }

    #[test]
    fn render_cache() {
        let spec = KeySpec { big_text: Some(format!("cache-{}", std::process::id())), ..Default::default() };
        let first = cached(&spec, 72, "test-kind", |img| {
            assert_eq!(img.dimensions(), (72, 72));
            vec![1, 2, 3]
        });
        assert_eq!(first, [1, 2, 3]);
        assert_eq!(cached(&spec, 72, "test-kind", |_| panic!("must be cached")), [1, 2, 3]);
        // Size and kind are part of the key.
        assert_eq!(cached(&spec, 96, "test-kind", |img| vec![img.width() as u8]), [96]);
        assert_eq!(cached(&spec, 72, "other-kind", |_| vec![9]), [9]);
    }

    #[test]
    fn device_encoding_details() {
        let tags: std::collections::HashSet<_> =
            [ImageFormat::Jpeg180, ImageFormat::Jpeg, ImageFormat::Bmp180, ImageFormat::BmpTranspose].into_iter().map(format_tag).collect();
        assert_eq!(tags.len(), 4);
        let mut src = RgbaImage::from_pixel(30, 20, Rgba([0, 0, 0, 255]));
        src.put_pixel(29, 0, Rgba([10, 20, 30, 255]));
        for (f, dims) in
            [(ImageFormat::Jpeg180, (30, 20)), (ImageFormat::Jpeg, (30, 20)), (ImageFormat::Bmp180, (30, 20)), (ImageFormat::BmpTranspose, (20, 30))]
        {
            let out = encode_device(&src, f);
            assert_eq!(image::load_from_memory(&out).unwrap().to_rgba8().dimensions(), dims, "{f:?}");
        }
        assert_eq!(encode_device_jpeg(&src), encode_device(&src, ImageFormat::Jpeg180));
        // 24 bit bottom-up BMP with a 54 byte header, BGR pixels: the first stored pixel
        // is the bottom left one of the device image = the top right one of the source.
        for f in [ImageFormat::Bmp180, ImageFormat::BmpTranspose] {
            let bmp = encode_device(&src, f);
            let le32 = |o: usize| u32::from_le_bytes(bmp[o..o + 4].try_into().unwrap());
            let (w, h): (u32, u32) = if f == ImageFormat::Bmp180 { (30, 20) } else { (20, 30) };
            assert_eq!(&bmp[..2], b"BM");
            assert_eq!(le32(2) as usize, bmp.len());
            assert_eq!((le32(10), le32(14)), (54, 40));
            assert_eq!((le32(18), le32(22) as i32), (w, h as i32), "{f:?}");
            assert_eq!(u16::from_le_bytes([bmp[28], bmp[29]]), 24);
            assert_eq!(le32(30), 0, "uncompressed");
            assert_eq!(bmp.len(), 54 + (w * 3).next_multiple_of(4) as usize * h as usize);
            assert_eq!(&bmp[54..57], &[30, 20, 10], "{f:?}");
        }
        // Rows are padded to 4 bytes.
        assert_eq!(encode_device(&RgbaImage::new(5, 3), ImageFormat::Bmp180).len(), 54 + 16 * 3);
        // Alpha is dropped, not blended.
        let semi = RgbaImage::from_pixel(16, 16, Rgba([200, 100, 50, 0]));
        let back = image::load_from_memory(&encode_device(&semi, ImageFormat::Bmp180)).unwrap().to_rgb8();
        assert_eq!(back.get_pixel(3, 3).0, [200, 100, 50]);
    }

    #[test]
    fn png_round_trip() {
        let img = RgbaImage::from_fn(7, 5, |x, y| Rgba([x as u8 * 30, y as u8 * 40, 7, (x * y * 9) as u8]));
        let out = encode_png(&img);
        assert_eq!(&out[..4], b"\x89PNG");
        assert_eq!(image::load_from_memory(&out).unwrap().to_rgba8(), img);
    }

    #[test]
    fn rasterize_edge_cases() {
        let bad = rasterize("not an svg", 10, 5);
        assert_eq!(bad.dimensions(), (10, 5));
        assert!(bad.pixels().all(|p| p.0[3] == 0));
        // Non-uniform scaling to the requested size.
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="#00ff00"/></svg>"##;
        let img = rasterize(svg, 40, 20);
        assert_eq!(img.dimensions(), (40, 20));
        assert!(img.pixels().all(|p| p.0 == [0, 255, 0, 255]));
        // Semi transparent colours come back un-premultiplied.
        let semi = r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4" fill="#ff0000" fill-opacity="0.5"/></svg>"##;
        let p = rasterize(semi, 4, 4).get_pixel(1, 1).0;
        assert!(p[0] > 250 && (120..136).contains(&p[3]), "{p:?}");
    }

    #[test]
    fn paints_and_escaping() {
        let mut defs = String::new();
        assert_eq!(svg_paint(" v$#abcdef ", &mut defs, "x"), "#abcdef");
        assert_eq!(svg_paint("<b>", &mut defs, "x"), "&lt;b&gt;");
        assert!(defs.is_empty());
        assert_eq!(svg_paint("v$-1:red, 0.5 : #00f ,9:lime,x:#fff", &mut defs, "v"), "url(#v)");
        assert!(defs.starts_with(r#"<linearGradient id="v" x1="0" y1="0" x2="0" y2="1">"#), "{defs}");
        assert!(defs.contains(r##"<stop offset="0" stop-color="red"/><stop offset="0.5" stop-color="#00f"/><stop offset="1" stop-color="lime"/><stop offset="0" stop-color="#fff"/>"##), "{defs}");
        let mut defs = String::new();
        svg_paint("0:#000,1:#fff", &mut defs, "h");
        assert!(defs.contains(r#"x2="1" y2="0""#));

        assert_eq!(xml_escape("a&b<c>\"d\"'"), "a&amp;b&lt;c&gt;&quot;d&quot;'");
        assert_eq!(xml_escape("x\u{0}y\u{7}z\n\tw\u{9b}"), "xyz\n\tw");
        assert_eq!(percent_decode("%41%zz%4"), "A%zz%4");
        assert_eq!(percent_decode("caf%C3%A9%20%"), "café %");
        assert_eq!(percent_decode("%ff"), "\u{fffd}");
    }

    #[test]
    fn sniffing_and_data_url_errors() {
        assert_eq!(sniff(b"\x89PNG\r\n"), "image/png");
        assert_eq!(sniff(b"\xff\xd8\xff"), "image/jpeg");
        assert_eq!(sniff(b"GIF89a"), "image/gif");
        assert_eq!(sniff(b"RIFF....WEBP"), "image/webp");
        assert_eq!(sniff(b"BM"), "image/bmp");
        assert_eq!(sniff(b"<svg/>"), "image/svg+xml");
        assert_eq!(sniff(b""), "image/svg+xml");

        assert!(ImageData::from_data_url("image/png;base64,iVBORw0K").is_none());
        assert!(ImageData::from_data_url("data:image/png;base64").is_none());
        assert!(ImageData::from_data_url("data:image/png;base64,!!!").is_none());
        let jpg = ImageData::from_data_url("data:image/jpg;base64,/9j/").unwrap();
        assert_eq!((jpg.mime.as_str(), &jpg.bytes[..]), ("image/jpeg", &[0xff, 0xd8, 0xff][..]));
        assert_eq!(ImageData::from_data_url("data:image/pjpeg;BASE64,/9j/").unwrap().mime, "image/jpeg");
        assert_eq!(ImageData::from_data_url("data:IMAGE/PNG;base64,iVBORw0K").unwrap().mime, "image/png");
        assert_eq!(ImageData::from_data_url("data:;base64,R0lGODlh").unwrap().mime, "image/gif");
        assert_eq!(ImageData::from_data_url("data:application/octet-stream;base64,Qk0=").unwrap().mime, "image/bmp");
        assert_eq!(ImageData::from_data_url("data:,<svg/>").unwrap().mime, "image/svg+xml");
        // Percent encoded base64 (`+` and `/` escaped).
        assert_eq!(&ImageData::from_data_url("data:image/jpeg;base64,%2F9j%2F").unwrap().bytes[..], &[0xff, 0xd8, 0xff]);
    }

    #[test]
    fn image_identity() {
        let a = ImageData::new("image/png", vec![1, 2, 3]);
        assert_eq!(a, ImageData::new("image/png", vec![1, 2, 3]));
        assert_ne!(a, ImageData::new("image/jpeg", vec![1, 2, 3]));
        assert_ne!(a, ImageData::new("image/png", vec![1, 2, 4]));
        let hash = |i: &ImageData| {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            i.hash(&mut h);
            h.finish()
        };
        assert_eq!(hash(&a), hash(&a.clone()));
        assert_ne!(hash(&a), hash(&ImageData::new("image/png", vec![3, 2, 1])));
        // Spec hashes change with any visible property.
        let spec = KeySpec { image: Some(a.clone()), title: Some(title("t", "top", 10)), ..Default::default() };
        let mut other = spec.clone();
        other.title.as_mut().unwrap().bold = true;
        assert_ne!(spec, other);
        assert_eq!(spec, spec.clone());
    }

    #[test]
    fn hrefs() {
        let p = png(&RgbaImage::from_pixel(2, 2, Rgba([1, 2, 3, 255])));
        assert_eq!(p.to_href(), format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&p.bytes)));
        // BMP, GIF and WebP are converted to PNG for usvg.
        let mut bmp = Vec::new();
        DynamicImage::ImageRgb8(image::RgbImage::from_pixel(3, 3, image::Rgb([9, 8, 7])))
            .write_to(&mut std::io::Cursor::new(&mut bmp), image::ImageFormat::Bmp)
            .unwrap();
        let href = ImageData::new("image/bmp", bmp).to_href();
        let b64 = href.strip_prefix("data:image/png;base64,").expect("png href");
        let back = image::load_from_memory(&base64::engine::general_purpose::STANDARD.decode(b64).unwrap()).unwrap().to_rgb8();
        assert_eq!(back.get_pixel(1, 1).0, [9, 8, 7]);
        // Undecodable data is passed through unchanged.
        assert_eq!(ImageData::new("image/gif", b"GIF8broken".to_vec()).to_href(), "data:image/gif;base64,R0lGOGJyb2tlbg==");
        // SVGs are rendered here (usvg drops raster images of an SVG inside an SVG);
        // what isn't an SVG stays as it is.
        assert!(ImageData::svg("<svg/>").to_href().starts_with("data:image/png;base64,"));
        assert_eq!(ImageData::svg("<nope").to_href(), "data:image/svg+xml;base64,PG5vcGU=");
    }

    #[test]
    fn svg_images_keep_their_raster_content() {
        let mut red = Vec::new();
        DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 4, image::Rgb([255, 0, 0])))
            .write_to(&mut std::io::Cursor::new(&mut red), image::ImageFormat::Png)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&red);
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="72" height="72"><image width="72" height="72" xlink:href="data:image/png;base64,{b64}"/></svg>"#
        );
        let key = render_key(&KeySpec { image: Some(ImageData::svg(svg)), ..Default::default() }, 72);
        assert_eq!(key.get_pixel(36, 36).0, [255, 0, 0, 255]);
        // Files are not read.
        let file = r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="72" height="72"><image width="72" height="72" xlink:href="/usr/share/pixmaps/x.png"/></svg>"#;
        let key = render_key(&KeySpec { image: Some(ImageData::svg(file)), ..Default::default() }, 72);
        assert_eq!(key.get_pixel(36, 36).0, [0, 0, 0, 255]);
    }

    #[test]
    fn image_files() {
        let dir = std::env::temp_dir().join(format!("tuxdeck-render-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("icon.png");
        std::fs::write(&p, encode_png(&RgbaImage::from_pixel(4, 4, Rgba([1, 2, 3, 255])))).unwrap();
        let a = ImageData::from_file(&p).unwrap();
        assert_eq!(a.mime, "image/png");
        // Cached while unchanged, re-read when the file changes.
        assert_eq!(ImageData::from_file(&p).unwrap(), a);
        std::fs::write(&p, encode_png(&RgbaImage::from_pixel(9, 9, Rgba([3, 2, 1, 255])))).unwrap();
        let b = ImageData::from_file(&p).unwrap();
        assert_ne!(a, b);
        assert_eq!(image::load_from_memory(&b.bytes).unwrap().width(), 9);
        // No extension: the content decides.
        let raw = dir.join("noext");
        std::fs::write(&raw, b"GIF89a....").unwrap();
        assert_eq!(ImageData::from_file(&raw).unwrap().mime, "image/gif");
        let svg = dir.join("icon.svg");
        std::fs::write(&svg, "<svg/>").unwrap();
        assert_eq!(ImageData::from_file(&svg).unwrap().mime, "image/svg+xml");
        assert!(ImageData::from_file(&dir).is_none());
        assert!(ImageData::from_file(&dir.join("missing.png")).is_none());
        // Large files that can't be decoded are dropped.
        let junk = dir.join("junk.png");
        std::fs::write(&junk, vec![0u8; 70 * 1024]).unwrap();
        assert!(ImageData::from_file(&junk).is_none());
        // Large GIFs are kept as they are (animation).
        let gif_path = dir.join("big.gif");
        std::fs::write(&gif_path, [&b"GIF89a"[..], &vec![0u8; 70 * 1024]].concat()).unwrap();
        assert_eq!(ImageData::from_file(&gif_path).unwrap().bytes.len(), 6 + 70 * 1024);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mid_sized_images_are_kept() {
        // Over 64 KiB but small dimensions: the original bytes stay.
        let noise = RgbaImage::from_fn(200, 200, |x, y| {
            let v = (x * 7919 + y * 104729).wrapping_mul(2654435761);
            Rgba([(v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8, 255])
        });
        let mut bytes = Vec::new();
        DynamicImage::ImageRgba8(noise).write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        assert!(bytes.len() > 64 * 1024, "{}", bytes.len());
        let img = ImageData::new("image/png", bytes.clone()).normalized().unwrap();
        assert_eq!(&img.bytes[..], &bytes[..]);
    }

    fn gif_with_delays(delays_ms: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
            for (i, d) in delays_ms.iter().enumerate() {
                let c = if i % 2 == 0 { [255, 0, 0, 255] } else { [0, 0, 255, 255] };
                let frame = image::Frame::from_parts(RgbaImage::from_pixel(300, 200, Rgba(c)), 0, 0, image::Delay::from_numer_denom_ms(*d, 1));
                enc.encode_frame(frame).unwrap();
            }
        }
        out
    }

    #[test]
    fn animation_decoding() {
        let a = decode_animation(&gif_with_delays(&[0, 250, 10])).unwrap();
        let delays: Vec<u64> = a.frames.iter().map(|f| f.1.as_millis() as u64).collect();
        // Very short delays are shown for 100 ms like in browsers.
        assert_eq!(delays, [100, 250, 100]);
        assert_eq!(a.total.as_millis(), 450);
        // Frames are scaled to fit 144 px.
        let f = image::load_from_memory(&a.frames[0].0.bytes).unwrap();
        assert_eq!((f.width(), f.height()), (144, 96));
        assert!(decode_animation(b"notagif").is_none());
        assert_eq!(animation_frame(&ImageData::new("image/png", vec![1])), (None, false));
    }

    #[test]
    fn fallback_icons() {
        let svg = fallback_icon_svg("<&>", "#ff0000");
        assert!(svg.contains(">&lt;&amp;&gt;</text>"));
        let img = rasterize(&fallback_icon_svg("X", ""), 72, 72);
        assert!(img.get_pixel(0, 0).0[3] == 0, "transparent background");
        assert!(img.pixels().any(|p| p.0[3] > 200 && p.0[0] > 200));
    }
}
