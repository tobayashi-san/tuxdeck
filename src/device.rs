//! Low level HID driver for Elgato Stream Deck hardware.
//!
//! Second generation protocol (JPEG images in 1024 byte reports): verified against
//! the official macOS app (`ESDDeviceNeo`, `ESDCommNeoKeypadPolling`) and on real
//! Stream Deck Neo hardware. First generation protocol (Stream Deck Mini and the
//! original Stream Deck, BMP images): implemented from the protocol as documented by
//! the python-elgato-streamdeck project, not verified on hardware.

use anyhow::{bail, Context, Result};
use hidapi::{DeviceInfo, HidApi, HidDevice};

pub const ELGATO_VID: u16 = 0x0fd9;

const IMAGE_REPORT_LEN: usize = 1024;
const IMAGE_HEADER_LEN: usize = 8;
const FEATURE_LEN: usize = 32;
const GEN1_FEATURE_LEN: usize = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Original,
    Mk2,
    Xl,
    Plus,
    Neo,
    Pedal,
    Mini,
}

/// HID protocol family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// MK.2, XL, +, Neo, Pedal, Original v2.
    Gen2,
    /// Stream Deck Mini (all revisions): 1024 byte image reports, page numbers from 0.
    Gen1Mini,
    /// Original Stream Deck (2017): 8191 byte image reports, pages from 1, keys mirrored.
    Gen1Original,
}

/// How images have to be encoded for the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    /// JPEG, rotated by 180°.
    Jpeg180,
    /// JPEG, as is.
    Jpeg,
    /// 24 bit BMP, rotated by 180°.
    Bmp180,
    /// 24 bit BMP, transposed (rotated 90° and mirrored), Stream Deck Mini.
    BmpTranspose,
}

#[derive(Debug)]
pub struct Model {
    pub pid: u16,
    pub kind: Kind,
    pub name: &'static str,
    /// Model identifier as written into Elgato profile manifests (`Device.Model`).
    pub model_id: &'static str,
    pub cols: u8,
    pub rows: u8,
    /// Size of a key image in pixels (square). 0 means the device has no displays.
    pub key_px: u32,
    /// Extra non-display buttons after the keypad (touch points on the Neo).
    pub touch_points: u8,
    /// Size of the auxiliary screen (Neo info bar / Plus touch strip).
    pub screen: Option<(u32, u32)>,
    pub encoders: u8,
    pub protocol: Protocol,
    pub key_format: ImageFormat,
    /// Format of the auxiliary screen (`screen`).
    pub screen_format: ImageFormat,
}

impl Model {
    pub fn key_count(&self) -> usize {
        self.cols as usize * self.rows as usize
    }

    /// Profiles of `other` can be shown on this model (same layout, e.g. Mini and
    /// Mini MK.2 or the different Stream Deck MK.2 revisions).
    pub fn accepts_profile_model(&self, other: &str) -> bool {
        if self.model_id == other {
            return true;
        }
        MODELS.iter().any(|m| m.model_id == other && m.kind == self.kind && m.cols == self.cols && m.rows == self.rows)
            || (matches!(self.kind, Kind::Original | Kind::Mk2) && MODELS.iter().any(|m| m.model_id == other && matches!(m.kind, Kind::Original | Kind::Mk2)))
    }
}

macro_rules! model {
    ($pid:expr, $kind:ident, $name:expr, $id:expr, $cols:expr, $rows:expr, $px:expr, $tp:expr, $screen:expr, $enc:expr, $proto:ident, $kf:ident, $sf:ident) => {
        Model {
            pid: $pid,
            kind: Kind::$kind,
            name: $name,
            model_id: $id,
            cols: $cols,
            rows: $rows,
            key_px: $px,
            touch_points: $tp,
            screen: $screen,
            encoders: $enc,
            protocol: Protocol::$proto,
            key_format: ImageFormat::$kf,
            screen_format: ImageFormat::$sf,
        }
    };
}

pub static MODELS: &[Model] = &[
    model!(0x009a, Neo, "Stream Deck Neo", "20GBJ9901", 4, 2, 96, 2, Some((248, 58)), 0, Gen2, Jpeg180, Jpeg180),
    model!(0x0060, Original, "Stream Deck (1st gen)", "20GAA9901", 5, 3, 72, 0, None, 0, Gen1Original, Bmp180, Bmp180),
    model!(0x006d, Original, "Stream Deck", "20GAA9902", 5, 3, 72, 0, None, 0, Gen2, Jpeg180, Jpeg180),
    model!(0x0080, Mk2, "Stream Deck MK.2", "20GBA9901", 5, 3, 72, 0, None, 0, Gen2, Jpeg180, Jpeg180),
    model!(0x00a5, Mk2, "Stream Deck MK.2", "20GBA9901", 5, 3, 72, 0, None, 0, Gen2, Jpeg180, Jpeg180),
    model!(0x006c, Xl, "Stream Deck XL", "20GAT9901", 8, 4, 96, 0, None, 0, Gen2, Jpeg180, Jpeg180),
    model!(0x008f, Xl, "Stream Deck XL", "20GAT9902", 8, 4, 96, 0, None, 0, Gen2, Jpeg180, Jpeg180),
    model!(0x0084, Plus, "Stream Deck +", "20GBD9901", 4, 2, 120, 0, Some((800, 100)), 4, Gen2, Jpeg, Jpeg),
    model!(0x0086, Pedal, "Stream Deck Pedal", "20GBF9901", 3, 1, 0, 0, None, 0, Gen2, Jpeg180, Jpeg180),
    model!(0x0063, Mini, "Stream Deck Mini", "20GAI9901", 3, 2, 80, 0, None, 0, Gen1Mini, BmpTranspose, BmpTranspose),
    model!(0x0090, Mini, "Stream Deck Mini MK.2", "20GAI9902", 3, 2, 80, 0, None, 0, Gen1Mini, BmpTranspose, BmpTranspose),
];

pub fn model_for_pid(pid: u16) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.pid == pid)
}

/// Raw input decoded from a HID input report.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// Key and touch point states (keys first, then touch points).
    Buttons(Vec<bool>),
    /// Encoder push states (Stream Deck +).
    EncoderPress(Vec<bool>),
    /// Encoder rotation ticks (Stream Deck +).
    EncoderTurn(Vec<i8>),
    /// Touch strip interaction (Stream Deck +): short/long tap or swipe.
    Touch { kind: u8, x: u16, y: u16, x2: u16, y2: u16 },
}

pub struct Deck {
    dev: HidDevice,
    pub model: &'static Model,
    pub serial: String,
    pub firmware: String,
    pub path: String,
}

/// All supported decks currently attached.
pub fn enumerate(api: &HidApi) -> Vec<&DeviceInfo> {
    api.device_list().filter(|d| is_deck_interface(d.vendor_id(), d.product_id(), d.interface_number())).collect()
}

fn is_deck_interface(vid: u16, pid: u16, interface: i32) -> bool {
    // Only the first HID interface carries the Stream Deck protocol.
    vid == ELGATO_VID && model_for_pid(pid).is_some() && interface <= 0
}

fn extract_string(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

impl Deck {
    pub fn open(api: &HidApi, info: &DeviceInfo) -> Result<Self> {
        let model = model_for_pid(info.product_id()).context("unsupported device")?;
        let dev =
            info.open_device(api).with_context(|| format!("cannot open {} ({}); check permissions (udev rule)", model.name, info.path().to_string_lossy()))?;
        let mut deck = Deck { dev, model, serial: String::new(), firmware: String::new(), path: info.path().to_string_lossy().into_owned() };
        let ((sr, so), (fr, fo)) = info_reports(model.protocol);
        deck.serial = deck.read_feature_string(sr, so).unwrap_or_default();
        if deck.serial.is_empty() {
            deck.serial = info.serial_number().unwrap_or_default().to_string();
        }
        deck.firmware = deck.read_feature_string(fr, fo).unwrap_or_default();
        // An image upload interrupted earlier (e.g. by a crash) would corrupt the next
        // one: send an empty image report first, like python-elgato-streamdeck does.
        let _ = deck.reset_key_stream();
        Ok(deck)
    }

    fn reset_key_stream(&self) -> Result<()> {
        self.dev.write(&key_stream_reset_report(self.model.protocol))?;
        Ok(())
    }

    fn read_feature_string(&self, report: u8, offset: usize) -> Result<String> {
        let mut buf = vec![0u8; feature_len(self.model.protocol)];
        buf[0] = report;
        let n = self.dev.get_feature_report(&mut buf)?;
        Ok(extract_string(&buf[offset.min(n)..n]))
    }

    fn write_feature(&self, buf: &[u8]) -> Result<()> {
        self.dev.send_feature_report(buf)?;
        Ok(())
    }

    /// Restore the device to its boot state (shows the Elgato logo).
    pub fn reset(&self) -> Result<()> {
        self.write_feature(&reset_report(self.model.protocol))
    }

    pub fn set_brightness(&self, percent: u8) -> Result<()> {
        self.write_feature(&brightness_report(self.model.protocol, percent))
    }

    /// Set the LED color of a Neo touch point (0 = left, 1 = right).
    pub fn set_touch_point_color(&self, index: u8, rgb: [u8; 3]) -> Result<()> {
        self.write_feature(&touch_point_color_report(self.model, index, rgb)?)
    }

    /// Upload an already device-encoded key image (see `Model::key_format`).
    pub fn set_key_image(&self, key: u8, data: &[u8]) -> Result<()> {
        if !has_key_display(self.model, key) {
            bail!("key {key} has no display");
        }
        for r in key_image_reports(self.model, key, data) {
            self.dev.write(&r)?;
        }
        Ok(())
    }

    /// Upload an already device-encoded (JPEG, flipped) image to the info bar (Neo).
    pub fn set_screen_image(&self, jpeg: &[u8]) -> Result<()> {
        for r in screen_image_reports(self.model, jpeg)? {
            self.dev.write(&r)?;
        }
        Ok(())
    }

    /// Stream Deck + touch strip partial update (JPEG, not rotated).
    pub fn set_lcd_region(&self, x: u16, y: u16, w: u16, h: u16, jpeg: &[u8]) -> Result<()> {
        for r in lcd_reports(x, y, w, h, jpeg) {
            self.dev.write(&r)?;
        }
        Ok(())
    }

    /// Wait up to `timeout_ms` for an input report. `Ok(None)` on timeout.
    pub fn read_input(&self, timeout_ms: i32) -> Result<Option<Input>> {
        let mut buf = [0u8; 512];
        let n = self.dev.read_timeout(&mut buf, timeout_ms)?;
        Ok(parse_input(self.model, &buf[..n]))
    }
}

fn feature_len(protocol: Protocol) -> usize {
    if protocol == Protocol::Gen2 {
        FEATURE_LEN
    } else {
        GEN1_FEATURE_LEN
    }
}

/// Zero padded feature report starting with `data`.
fn feature_report(protocol: Protocol, data: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; feature_len(protocol)];
    buf[..data.len()].copy_from_slice(data);
    buf
}

/// Feature reports holding the serial number and the firmware version as
/// `(report id, string offset)`.
fn info_reports(protocol: Protocol) -> ((u8, usize), (u8, usize)) {
    match protocol {
        Protocol::Gen2 => ((0x06, 2), (0x05, 6)),
        Protocol::Gen1Mini | Protocol::Gen1Original => ((0x03, 5), (0x04, 5)),
    }
}

/// Empty image report that aborts an interrupted image upload.
fn key_stream_reset_report(protocol: Protocol) -> Vec<u8> {
    let len = match protocol {
        Protocol::Gen2 => IMAGE_REPORT_LEN,
        Protocol::Gen1Mini => 1024,
        Protocol::Gen1Original => 8191,
    };
    let mut buf = vec![0u8; len];
    buf[0] = 0x02;
    buf
}

fn reset_report(protocol: Protocol) -> Vec<u8> {
    match protocol {
        Protocol::Gen2 => feature_report(protocol, &[0x03, 0x02]),
        _ => feature_report(protocol, &[0x0b, 0x63]),
    }
}

fn brightness_report(protocol: Protocol, percent: u8) -> Vec<u8> {
    match protocol {
        Protocol::Gen2 => feature_report(protocol, &[0x03, 0x08, percent.min(100)]),
        _ => feature_report(protocol, &[0x05, 0x55, 0xaa, 0xd1, 0x01, percent.min(100)]),
    }
}

fn touch_point_color_report(model: &Model, index: u8, rgb: [u8; 3]) -> Result<Vec<u8>> {
    if index >= model.touch_points {
        bail!("touch point {index} out of range");
    }
    let key = model.key_count() as u8 + index;
    Ok(feature_report(model.protocol, &[0x03, 0x06, key, rgb[0], rgb[1], rgb[2]]))
}

fn has_key_display(model: &Model, key: u8) -> bool {
    (key as usize) < model.key_count() && model.key_px != 0
}

/// Output reports that upload an image to the auxiliary screen (Neo info bar, Plus touch strip).
fn screen_image_reports(model: &Model, jpeg: &[u8]) -> Result<Vec<Vec<u8>>> {
    match model.kind {
        Kind::Neo => Ok(chunked_reports(|page, last, len| [0x02, 0x0b, 0x00, last, len as u8, (len >> 8) as u8, page as u8, (page >> 8) as u8], jpeg)),
        Kind::Plus => {
            let (w, h) = model.screen.unwrap();
            Ok(lcd_reports(0, 0, w as u16, h as u16, jpeg))
        }
        _ => bail!("device has no screen"),
    }
}

fn chunked_reports(header: impl Fn(usize, u8, usize) -> [u8; IMAGE_HEADER_LEN], data: &[u8]) -> Vec<Vec<u8>> {
    let payload = IMAGE_REPORT_LEN - IMAGE_HEADER_LEN;
    let mut out = Vec::new();
    let mut page = 0usize;
    let mut rest = data;
    loop {
        let chunk = &rest[..rest.len().min(payload)];
        rest = &rest[chunk.len()..];
        let last = rest.is_empty() as u8;
        let mut buf = vec![0u8; IMAGE_REPORT_LEN];
        buf[..IMAGE_HEADER_LEN].copy_from_slice(&header(page, last, chunk.len()));
        buf[IMAGE_HEADER_LEN..IMAGE_HEADER_LEN + chunk.len()].copy_from_slice(chunk);
        out.push(buf);
        page += 1;
        if last == 1 {
            return out;
        }
    }
}

/// Decode an input report (`data`: the bytes read, starting with the report id).
fn parse_input(model: &Model, data: &[u8]) -> Option<Input> {
    let n = data.len();
    let min = if model.protocol == Protocol::Gen2 { 4 } else { 1 + model.key_count() };
    if n < min || data[0] != 0x01 {
        return None;
    }
    if model.protocol != Protocol::Gen2 {
        return Some(Input::Buttons(gen1_buttons(model, data)));
    }
    let input = match data[1] {
        0x00 => {
            let count = model.key_count() + model.touch_points as usize;
            Input::Buttons(data[4..(4 + count).min(n)].iter().map(|&b| b != 0).collect())
        }
        0x02 if n >= 15 => {
            let rd = |o: usize| u16::from_le_bytes([data[o], data[o + 1]]);
            Input::Touch { kind: data[4], x: rd(6), y: rd(8), x2: rd(10), y2: rd(12) }
        }
        0x03 if n >= 5 + model.encoders as usize => {
            let enc = &data[5..5 + model.encoders as usize];
            if data[4] == 0x00 {
                Input::EncoderPress(enc.iter().map(|&b| b != 0).collect())
            } else {
                Input::EncoderTurn(enc.iter().map(|&b| b as i8).collect())
            }
        }
        _ => return None,
    };
    Some(input)
}

/// Physical key index of the original Stream Deck: its keys are numbered from the
/// right within each row.
fn original_key_index(model: &Model, key: usize) -> usize {
    let cols = model.cols as usize;
    let col = key % cols;
    key - col + (cols - 1 - col)
}

/// Key states of a first generation input report (`0x01, state…`).
fn gen1_buttons(model: &Model, data: &[u8]) -> Vec<bool> {
    let raw: Vec<bool> = data[1..].iter().take(model.key_count()).map(|&b| b != 0).collect();
    if model.protocol == Protocol::Gen1Original {
        (0..raw.len()).map(|k| raw.get(original_key_index(model, k)).copied().unwrap_or(false)).collect()
    } else {
        raw
    }
}

/// Output reports that upload one key image.
pub fn key_image_reports(model: &Model, key: u8, data: &[u8]) -> Vec<Vec<u8>> {
    let (len, hdr) = match model.protocol {
        Protocol::Gen2 => (IMAGE_REPORT_LEN, IMAGE_HEADER_LEN),
        Protocol::Gen1Mini => (1024, 16),
        Protocol::Gen1Original => (8191, 16),
    };
    let payload = len - hdr;
    let chunks: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(payload).collect() };
    let n = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(page, chunk)| {
            let last = (page + 1 == n) as u8;
            let mut buf = vec![0u8; len];
            match model.protocol {
                Protocol::Gen2 => {
                    buf[..8].copy_from_slice(&[0x02, 0x07, key, last, chunk.len() as u8, (chunk.len() >> 8) as u8, page as u8, (page >> 8) as u8])
                }
                Protocol::Gen1Mini => buf[..6].copy_from_slice(&[0x02, 0x01, page as u8, 0, last, key + 1]),
                Protocol::Gen1Original => {
                    let k = original_key_index(model, key as usize) as u8;
                    buf[..6].copy_from_slice(&[0x02, 0x01, page as u8 + 1, 0, last, k + 1])
                }
            }
            buf[hdr..hdr + chunk.len()].copy_from_slice(chunk);
            buf
        })
        .collect()
}

/// Output reports for a Stream Deck + touch strip region.
pub fn lcd_reports(x: u16, y: u16, w: u16, h: u16, jpeg: &[u8]) -> Vec<Vec<u8>> {
    let payload = IMAGE_REPORT_LEN - 16;
    let chunks: Vec<&[u8]> = if jpeg.is_empty() { vec![&[][..]] } else { jpeg.chunks(payload).collect() };
    let n = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(page, chunk)| {
            let last = (page + 1 == n) as u8;
            let mut buf = vec![0u8; IMAGE_REPORT_LEN];
            buf[..16].copy_from_slice(&[
                0x02,
                0x0c,
                x as u8,
                (x >> 8) as u8,
                y as u8,
                (y >> 8) as u8,
                w as u8,
                (w >> 8) as u8,
                h as u8,
                (h >> 8) as u8,
                last,
                page as u8,
                (page >> 8) as u8,
                chunk.len() as u8,
                (chunk.len() >> 8) as u8,
                0,
            ]);
            buf[16..16 + chunk.len()].copy_from_slice(chunk);
            buf
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(pid: u16) -> &'static Model {
        model_for_pid(pid).unwrap()
    }

    #[test]
    fn gen2_key_reports() {
        let data = vec![7u8; 2000];
        let r = key_image_reports(model(0x009a), 3, &data);
        assert_eq!(r.len(), 2);
        assert_eq!(&r[0][..8], &[0x02, 0x07, 3, 0, 0xf8, 0x03, 0, 0]);
        assert_eq!(&r[1][..8], &[0x02, 0x07, 3, 1, 0xd8, 0x03, 1, 0]);
        assert!(r.iter().all(|x| x.len() == 1024));
    }

    #[test]
    fn mini_key_reports() {
        // 80x80 BMP = 54 + 19200 bytes → 20 reports of 1008 bytes payload.
        let data = vec![1u8; 19254];
        let r = key_image_reports(model(0x0063), 5, &data);
        assert_eq!(r.len(), 20);
        assert_eq!(&r[0][..6], &[0x02, 0x01, 0, 0, 0, 6]);
        assert_eq!(&r[19][..6], &[0x02, 0x01, 19, 0, 1, 6]);
        assert_eq!(r[0][16], 1);
    }

    #[test]
    fn original_key_reports_and_mirroring() {
        let m = model(0x0060);
        let data = vec![1u8; 15606];
        let r = key_image_reports(m, 0, &data);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].len(), 8191);
        // Key 0 (top left) is physical key 4 (numbered from the right).
        assert_eq!(&r[0][..6], &[0x02, 0x01, 1, 0, 0, 5]);
        assert_eq!(&r[1][..6], &[0x02, 0x01, 2, 0, 1, 5]);
        let mut report = vec![0x01];
        report.extend((0..15).map(|i| (i == 4) as u8));
        let b = gen1_buttons(m, &report);
        assert!(b[0] && b.iter().filter(|x| **x).count() == 1);
    }

    #[test]
    fn profile_compatibility() {
        assert!(model(0x0090).accepts_profile_model("20GAI9901"));
        assert!(model(0x0080).accepts_profile_model("20GAA9902"));
        assert!(!model(0x009a).accepts_profile_model("20GBA9901"));
    }

    #[test]
    fn profile_compatibility_matrix() {
        // Same model id, other revisions of the same layout, and the 15 key family.
        for (pid, accepted, rejected) in [
            (0x009a, &["20GBJ9901"][..], &["20GBA9901", "20GBD9901", "20GAI9901"][..]),
            (0x0060, &["20GAA9901", "20GAA9902", "20GBA9901"], &["20GAT9901", "20GBJ9901"]),
            (0x006d, &["20GAA9901", "20GAA9902", "20GBA9901"], &["20GAT9902"]),
            (0x00a5, &["20GBA9901", "20GAA9901", "20GAA9902"], &["20GAI9902", "20GBD9901"]),
            (0x006c, &["20GAT9901", "20GAT9902"], &["20GAA9902", "20GBA9901", "20GBD9901"]),
            (0x008f, &["20GAT9901", "20GAT9902"], &["20GBJ9901"]),
            (0x0084, &["20GBD9901"], &["20GBJ9901", "20GAT9901"]),
            (0x0086, &["20GBF9901"], &["20GAI9901", "20GBA9901"]),
            (0x0063, &["20GAI9901", "20GAI9902"], &["20GBF9901", "20GAA9901"]),
        ] {
            let m = model(pid);
            for id in accepted {
                assert!(m.accepts_profile_model(id), "{pid:#06x} should accept {id}");
            }
            for id in rejected {
                assert!(!m.accepts_profile_model(id), "{pid:#06x} should reject {id}");
            }
        }
        assert!(MODELS.iter().all(|m| !m.accepts_profile_model("") && !m.accepts_profile_model("20XXX0000")));
    }

    #[test]
    fn model_table() {
        use ImageFormat::*;
        use Protocol::*;
        type Row = (u16, Kind, &'static str, u8, u8, u32, u8, Option<(u32, u32)>, u8, Protocol, ImageFormat);
        #[rustfmt::skip]
        let want: &[Row] = &[
            (0x009a, Kind::Neo, "20GBJ9901", 4, 2, 96, 2, Some((248, 58)), 0, Gen2, Jpeg180),
            (0x0060, Kind::Original, "20GAA9901", 5, 3, 72, 0, None, 0, Gen1Original, Bmp180),
            (0x006d, Kind::Original, "20GAA9902", 5, 3, 72, 0, None, 0, Gen2, Jpeg180),
            (0x0080, Kind::Mk2, "20GBA9901", 5, 3, 72, 0, None, 0, Gen2, Jpeg180),
            (0x00a5, Kind::Mk2, "20GBA9901", 5, 3, 72, 0, None, 0, Gen2, Jpeg180),
            (0x006c, Kind::Xl, "20GAT9901", 8, 4, 96, 0, None, 0, Gen2, Jpeg180),
            (0x008f, Kind::Xl, "20GAT9902", 8, 4, 96, 0, None, 0, Gen2, Jpeg180),
            (0x0084, Kind::Plus, "20GBD9901", 4, 2, 120, 0, Some((800, 100)), 4, Gen2, Jpeg),
            (0x0086, Kind::Pedal, "20GBF9901", 3, 1, 0, 0, None, 0, Gen2, Jpeg180),
            (0x0063, Kind::Mini, "20GAI9901", 3, 2, 80, 0, None, 0, Gen1Mini, BmpTranspose),
            (0x0090, Kind::Mini, "20GAI9902", 3, 2, 80, 0, None, 0, Gen1Mini, BmpTranspose),
        ];
        assert_eq!(MODELS.len(), want.len());
        for &(pid, kind, id, cols, rows, px, tp, screen, enc, proto, fmt) in want {
            let m = model(pid);
            assert_eq!(m.pid, pid);
            assert_eq!((m.kind, m.model_id, m.cols, m.rows, m.key_px), (kind, id, cols, rows, px), "{}", m.name);
            assert_eq!((m.touch_points, m.screen, m.encoders, m.protocol), (tp, screen, enc, proto), "{}", m.name);
            assert_eq!((m.key_format, m.screen_format), (fmt, fmt), "{}", m.name);
            assert_eq!(m.key_count(), cols as usize * rows as usize);
            assert!(!m.name.is_empty());
        }
        let mut pids: Vec<u16> = MODELS.iter().map(|m| m.pid).collect();
        pids.sort();
        pids.dedup();
        assert_eq!(pids.len(), MODELS.len(), "duplicate pid");
        assert!(model_for_pid(0x0000).is_none() && model_for_pid(0xffff).is_none());
    }

    #[test]
    fn deck_interfaces() {
        assert!(is_deck_interface(ELGATO_VID, 0x009a, 0));
        // hidapi reports -1 when the interface number is unknown.
        assert!(is_deck_interface(ELGATO_VID, 0x0063, -1));
        assert!(!is_deck_interface(ELGATO_VID, 0x009a, 1));
        assert!(!is_deck_interface(ELGATO_VID, 0x1234, 0));
        assert!(!is_deck_interface(0x046d, 0x009a, 0));
    }

    /// Payloads of Gen2 key / Neo screen reports, joined using the length fields.
    fn join_gen2(reports: &[Vec<u8>]) -> Vec<u8> {
        reports.iter().flat_map(|r| r[8..8 + u16::from_le_bytes([r[4], r[5]]) as usize].to_vec()).collect()
    }

    #[test]
    fn gen2_chunking_boundaries() {
        let m = model(0x0080);
        let empty = key_image_reports(m, 14, &[]);
        assert_eq!(empty.len(), 1);
        assert_eq!(&empty[0][..8], &[0x02, 0x07, 14, 1, 0, 0, 0, 0]);
        assert!(empty[0][8..].iter().all(|&b| b == 0));

        let exact = key_image_reports(m, 0, &[9u8; 1016]);
        assert_eq!(exact.len(), 1);
        assert_eq!(&exact[0][..8], &[0x02, 0x07, 0, 1, 0xf8, 0x03, 0, 0]);
        assert!(exact[0][8..].iter().all(|&b| b == 9));

        let data: Vec<u8> = (0..1017u32).map(|i| (i % 251) as u8).collect();
        let r = key_image_reports(m, 1, &data);
        assert_eq!(r.len(), 2);
        assert_eq!(&r[0][..8], &[0x02, 0x07, 1, 0, 0xf8, 0x03, 0, 0]);
        assert_eq!(&r[1][..8], &[0x02, 0x07, 1, 1, 1, 0, 1, 0]);
        assert!(r[1][9..].iter().all(|&b| b == 0), "padding");
        assert_eq!(join_gen2(&r), data);

        // Page numbers are 16 bit little endian.
        let big = vec![1u8; 1016 * 300];
        let r = key_image_reports(m, 2, &big);
        assert_eq!(r.len(), 300);
        assert_eq!(&r[299][..8], &[0x02, 0x07, 2, 1, 0xf8, 0x03, 0x2b, 0x01]);
        assert_eq!(r.iter().filter(|x| x[3] == 1).count(), 1);
        assert!(r.iter().enumerate().all(|(i, x)| u16::from_le_bytes([x[6], x[7]]) as usize == i));
    }

    #[test]
    fn gen1_chunking_boundaries() {
        let mini = model(0x0090);
        let r = key_image_reports(mini, 0, &[3u8; 1008]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].len(), 1024);
        assert_eq!(&r[0][..16], &[0x02, 0x01, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let r = key_image_reports(mini, 2, &[3u8; 1009]);
        assert_eq!(r.len(), 2);
        assert_eq!(&r[1][..6], &[0x02, 0x01, 1, 0, 1, 3]);
        assert_eq!((r[1][16], r[1][17]), (3, 0));
        let r = key_image_reports(mini, 1, &[]);
        assert_eq!((r.len(), &r[0][..6]), (1, &[0x02, 0x01, 0, 0, 1, 2][..]));

        let orig = model(0x0060);
        let r = key_image_reports(orig, 7, &[5u8; 8175]);
        assert_eq!(r.len(), 1);
        assert_eq!(&r[0][..16], &[0x02, 0x01, 1, 0, 1, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(r[0][16..].iter().all(|&b| b == 5));
        assert_eq!(key_image_reports(orig, 7, &[5u8; 8176]).len(), 2);
    }

    #[test]
    fn original_key_numbering() {
        let m = model(0x0060);
        // Keys are numbered from the right within each row (5 columns).
        let want = [4, 3, 2, 1, 0, 9, 8, 7, 6, 5, 14, 13, 12, 11, 10];
        for (k, &phys) in want.iter().enumerate() {
            assert_eq!(original_key_index(m, k), phys);
            assert_eq!(original_key_index(m, phys), k);
            assert_eq!(key_image_reports(m, k as u8, &[1])[0][5], phys as u8 + 1);
        }
        // Only the original is mirrored: Mini keys go straight through.
        let mini = model(0x0063);
        assert_eq!(key_image_reports(mini, 0, &[1])[0][5], 1);
        assert_eq!(key_image_reports(mini, 5, &[1])[0][5], 6);
    }

    #[test]
    fn lcd_report_layout() {
        let data: Vec<u8> = (0..2100u32).map(|i| (i % 253) as u8).collect();
        let r = lcd_reports(200, 0, 200, 100, &data);
        assert_eq!(r.len(), 3);
        assert!(r.iter().all(|x| x.len() == 1024));
        assert_eq!(&r[0][..16], &[0x02, 0x0c, 200, 0, 0, 0, 200, 0, 100, 0, 0, 0, 0, 0xf0, 0x03, 0]);
        assert_eq!(&r[1][..16], &[0x02, 0x0c, 200, 0, 0, 0, 200, 0, 100, 0, 0, 1, 0, 0xf0, 0x03, 0]);
        assert_eq!(&r[2][..16], &[0x02, 0x0c, 200, 0, 0, 0, 200, 0, 100, 0, 1, 2, 0, 84, 0, 0]);
        let joined: Vec<u8> = r.iter().flat_map(|x| x[16..16 + u16::from_le_bytes([x[13], x[14]]) as usize].to_vec()).collect();
        assert_eq!(joined, data);
        // 16 bit coordinates.
        let r = lcd_reports(0x0123, 0x0201, 800, 0x0304, &[]);
        assert_eq!(&r[0][..16], &[0x02, 0x0c, 0x23, 0x01, 0x01, 0x02, 0x20, 0x03, 0x04, 0x03, 1, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn screen_reports() {
        let data = vec![0xabu8; 1016 + 10];
        let neo = screen_image_reports(model(0x009a), &data).unwrap();
        assert_eq!(neo.len(), 2);
        assert_eq!(&neo[0][..8], &[0x02, 0x0b, 0x00, 0, 0xf8, 0x03, 0, 0]);
        assert_eq!(&neo[1][..8], &[0x02, 0x0b, 0x00, 1, 10, 0, 1, 0]);
        assert_eq!(join_gen2(&neo), data);
        let empty = screen_image_reports(model(0x009a), &[]).unwrap();
        assert_eq!((empty.len(), &empty[0][..8]), (1, &[0x02, 0x0b, 0, 1, 0, 0, 0, 0][..]));
        // The Plus touch strip gets one full size region update.
        assert_eq!(screen_image_reports(model(0x0084), &data).unwrap(), lcd_reports(0, 0, 800, 100, &data));
        for pid in [0x0080, 0x006c, 0x0086, 0x0063, 0x0060] {
            assert!(screen_image_reports(model(pid), &data).is_err(), "{pid:#06x}");
        }
    }

    #[test]
    fn feature_reports() {
        assert_eq!(reset_report(Protocol::Gen2), [&[0x03, 0x02][..], &[0; 30]].concat());
        assert_eq!(reset_report(Protocol::Gen1Mini), [&[0x0b, 0x63][..], &[0; 15]].concat());
        assert_eq!(reset_report(Protocol::Gen1Original), reset_report(Protocol::Gen1Mini));
        assert_eq!(brightness_report(Protocol::Gen2, 42), [&[0x03, 0x08, 42][..], &[0; 29]].concat());
        assert_eq!(brightness_report(Protocol::Gen2, 250)[2], 100);
        assert_eq!(brightness_report(Protocol::Gen1Original, 0), [&[0x05, 0x55, 0xaa, 0xd1, 0x01, 0][..], &[0; 11]].concat());
        assert_eq!(brightness_report(Protocol::Gen1Mini, 101)[5], 100);
        assert_eq!(info_reports(Protocol::Gen2), ((0x06, 2), (0x05, 6)));
        assert_eq!(info_reports(Protocol::Gen1Mini), ((0x03, 5), (0x04, 5)));
        assert_eq!(info_reports(Protocol::Gen1Original), ((0x03, 5), (0x04, 5)));
        for (p, len) in [(Protocol::Gen2, 1024), (Protocol::Gen1Mini, 1024), (Protocol::Gen1Original, 8191)] {
            let r = key_stream_reset_report(p);
            assert_eq!(r.len(), len);
            assert_eq!(r[0], 0x02);
            assert!(r[1..].iter().all(|&b| b == 0));
        }
    }

    #[test]
    fn touch_point_colors() {
        let neo = model(0x009a);
        assert_eq!(touch_point_color_report(neo, 0, [1, 2, 3]).unwrap(), [&[0x03, 0x06, 8, 1, 2, 3][..], &[0; 26]].concat());
        assert_eq!(touch_point_color_report(neo, 1, [0xff, 0, 0x80]).unwrap()[..6], [0x03, 0x06, 9, 0xff, 0, 0x80]);
        assert!(touch_point_color_report(neo, 2, [0; 3]).is_err());
        assert!(touch_point_color_report(model(0x0080), 0, [0; 3]).is_err());
    }

    #[test]
    fn key_displays() {
        assert!(has_key_display(model(0x009a), 0) && has_key_display(model(0x009a), 7));
        // Neo touch points have no display.
        assert!(!has_key_display(model(0x009a), 8));
        assert!(has_key_display(model(0x006c), 31) && !has_key_display(model(0x006c), 32));
        assert!(!has_key_display(model(0x0086), 0));
    }

    #[test]
    fn feature_strings() {
        assert_eq!(extract_string(b"AL12K1A01234\0\0garbage"), "AL12K1A01234");
        assert_eq!(extract_string(b"  1.01.000 \0"), "1.01.000");
        assert_eq!(extract_string(b"no terminator"), "no terminator");
        assert_eq!(extract_string(b"\0abc"), "");
        assert_eq!(extract_string(b""), "");
        assert_eq!(extract_string(b"A\xffB\0"), "A\u{fffd}B");
        // Gen2 serial at offset 2 of report 0x06, firmware at offset 6 of report 0x05.
        let mut serial = vec![0x06, 0x0c];
        serial.extend(b"A00WA1234567\0");
        let ((_, so), (_, fo)) = info_reports(Protocol::Gen2);
        assert_eq!(extract_string(&serial[so..]), "A00WA1234567");
        let fw = [&[0x05, 0x0c, 0xfe, 0x92, 0x4c, 0x03][..], b"1.00.012", &[0; 18]].concat();
        assert_eq!(extract_string(&fw[fo..]), "1.00.012");
    }

    fn report(head: &[u8], len: usize) -> Vec<u8> {
        let mut r = head.to_vec();
        r.resize(len.max(head.len()), 0);
        r
    }

    #[test]
    fn gen2_input_parsing() {
        let neo = model(0x009a);
        // 8 keys followed by the 2 touch points.
        let r = report(&[0x01, 0x00, 0x0a, 0x00, 1, 0, 0, 0, 0, 0, 0, 1, 0, 1], 512);
        let want: Vec<bool> = [1, 0, 0, 0, 0, 0, 0, 1, 0, 1].iter().map(|&b| b == 1).collect();
        assert_eq!(parse_input(neo, &r), Some(Input::Buttons(want)));
        // Truncated reports give fewer states rather than panicking.
        assert_eq!(parse_input(neo, &[0x01, 0x00, 0x0a, 0x00, 0xff]), Some(Input::Buttons(vec![true])));
        assert_eq!(parse_input(neo, &[0x01, 0x00, 0x0a, 0x00]), Some(Input::Buttons(vec![])));
        assert_eq!(parse_input(neo, &[0x01, 0x00, 0x0a]), None);
        assert_eq!(parse_input(neo, &[]), None);
        assert_eq!(parse_input(neo, &report(&[0x02, 0x00, 0x0a, 0x00, 1], 512)), None);
        assert_eq!(parse_input(neo, &report(&[0x01, 0x07], 512)), None);

        let xl = model(0x006c);
        let mut r = report(&[0x01, 0x00, 32, 0], 512);
        r[4 + 31] = 1;
        let Some(Input::Buttons(b)) = parse_input(xl, &r) else { panic!() };
        assert_eq!(b.len(), 32);
        assert_eq!(b.iter().position(|&x| x), Some(31));

        let pedal = model(0x0086);
        assert_eq!(parse_input(pedal, &report(&[0x01, 0x00, 0x03, 0x00, 0, 1, 0], 512)), Some(Input::Buttons(vec![false, true, false])));
    }

    #[test]
    fn plus_input_parsing() {
        let plus = model(0x0084);
        assert_eq!(parse_input(plus, &report(&[0x01, 0x00, 0x08, 0x00, 0, 0, 0, 0, 0, 0, 1, 0], 512)), Some(Input::Buttons((0..8).map(|i| i == 6).collect())));
        // Encoder push: data[4] = 0, then one state per dial.
        assert_eq!(parse_input(plus, &report(&[0x01, 0x03, 0x05, 0x00, 0x00, 1, 0, 0, 1], 512)), Some(Input::EncoderPress(vec![true, false, false, true])));
        // Encoder turn: data[4] = 1, signed ticks per dial.
        assert_eq!(parse_input(plus, &report(&[0x01, 0x03, 0x05, 0x00, 0x01, 1, 0xff, 0, 0xfd], 512)), Some(Input::EncoderTurn(vec![1, -1, 0, -3])));
        assert_eq!(parse_input(plus, &[0x01, 0x03, 0x05, 0x00, 0x01, 1, 2, 3]), None);
        assert_eq!(parse_input(plus, &[0x01, 0x03, 0x05, 0x00, 0x01, 1, 2, 3, 4]), Some(Input::EncoderTurn(vec![1, 2, 3, 4])));
        // Touch strip: kind at 4, then x, y, x2, y2 as 16 bit little endian.
        let tap = report(&[0x01, 0x02, 0x0e, 0x00, 0x01, 0x00, 0x2c, 0x01, 0x32, 0x00], 512);
        assert_eq!(parse_input(plus, &tap), Some(Input::Touch { kind: 1, x: 300, y: 50, x2: 0, y2: 0 }));
        let long = report(&[0x01, 0x02, 0x0e, 0x00, 0x02, 0x00, 0x1f, 0x03, 0x63, 0x00], 512);
        assert_eq!(parse_input(plus, &long), Some(Input::Touch { kind: 2, x: 799, y: 99, x2: 0, y2: 0 }));
        let swipe = report(&[0x01, 0x02, 0x0e, 0x00, 0x03, 0x00, 0x0a, 0x00, 0x14, 0x00, 0x58, 0x02, 0x1e, 0x00], 15);
        assert_eq!(parse_input(plus, &swipe), Some(Input::Touch { kind: 3, x: 10, y: 20, x2: 600, y2: 30 }));
        assert_eq!(parse_input(plus, &swipe[..14]), None);
    }

    #[test]
    fn gen1_input_parsing() {
        let mini = model(0x0063);
        assert_eq!(parse_input(mini, &[0x01, 0, 1, 0, 0, 0, 1]), Some(Input::Buttons(vec![false, true, false, false, false, true])));
        // Extra bytes are ignored, short reports are dropped.
        assert_eq!(parse_input(mini, &report(&[0x01, 1], 64)), Some(Input::Buttons(vec![true, false, false, false, false, false])));
        assert_eq!(parse_input(mini, &[0x01, 0, 1, 0, 0, 0]), None);
        assert_eq!(parse_input(mini, &[0x02, 0, 1, 0, 0, 0, 1]), None);

        let orig = model(0x0060);
        for phys in 0..15 {
            let r: Vec<u8> = std::iter::once(0x01).chain((0..15).map(|i| (i == phys) as u8)).collect();
            let Some(Input::Buttons(b)) = parse_input(orig, &r) else { panic!() };
            assert_eq!(b.len(), 15);
            assert_eq!(b.iter().position(|&x| x), Some(original_key_index(orig, phys)));
        }
        assert_eq!(parse_input(orig, &[0x01; 15]), None);
    }

    /// Red bar in the top right quarter of an otherwise black image.
    fn marker(w: u32, h: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(w, h, |x, y| if x >= w / 2 && y < h / 4 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 0, 0, 255]) })
    }

    /// Checks that `device` shows `src` transformed as required by `format`.
    fn assert_orientation(src: &image::RgbaImage, device: &image::RgbImage, format: ImageFormat) {
        let (w, h) = src.dimensions();
        let want = if format == ImageFormat::BmpTranspose { (h, w) } else { (w, h) };
        assert_eq!(device.dimensions(), want, "{format:?}");
        for i in 0..4 {
            for j in 0..4 {
                let (sx, sy) = ((2 * i + 1) * w / 8, (2 * j + 1) * h / 8);
                let (dx, dy) = match format {
                    ImageFormat::Jpeg => (sx, sy),
                    ImageFormat::Jpeg180 | ImageFormat::Bmp180 => (w - 1 - sx, h - 1 - sy),
                    ImageFormat::BmpTranspose => (sy, sx),
                };
                let red = |p: [u8; 3]| p[0] > 180 && p[1] < 80 && p[2] < 80;
                assert_eq!(red(device.get_pixel(dx, dy).0), red(src.get_pixel(sx, sy).0[..3].try_into().unwrap()), "{format:?} at {sx},{sy}");
            }
        }
    }

    #[test]
    fn key_images_end_to_end() {
        // Encode, packetize, reassemble and decode like the device would, for every model.
        for m in MODELS.iter().filter(|m| m.key_px > 0) {
            let src = marker(m.key_px, m.key_px);
            let data = crate::render::encode_device(&src, m.key_format);
            let reports = key_image_reports(m, 0, &data);
            let joined = match m.protocol {
                Protocol::Gen2 => join_gen2(&reports),
                Protocol::Gen1Mini | Protocol::Gen1Original => reports.iter().flat_map(|r| r[16..].to_vec()).collect(),
            };
            assert_eq!(&joined[..data.len()], &data[..], "{}", m.name);
            assert!(joined[data.len()..].iter().all(|&b| b == 0));
            let decoded = image::load_from_memory(&joined).unwrap().to_rgb8();
            assert_orientation(&src, &decoded, m.key_format);
            match m.key_format {
                ImageFormat::Bmp180 | ImageFormat::BmpTranspose => {
                    assert_eq!(&data[..2], b"BM");
                    assert_eq!(data.len(), 54 + (m.key_px * m.key_px * 3) as usize);
                }
                ImageFormat::Jpeg | ImageFormat::Jpeg180 => assert_eq!((&data[..2], &data[data.len() - 2..]), (&b"\xff\xd8"[..], &b"\xff\xd9"[..])),
            }
        }
    }

    #[test]
    fn screen_images_end_to_end() {
        for m in MODELS.iter().filter(|m| m.screen.is_some()) {
            let (w, h) = m.screen.unwrap();
            let src = marker(w, h);
            let data = crate::render::encode_device(&src, m.screen_format);
            let reports = screen_image_reports(m, &data).unwrap();
            let joined: Vec<u8> = match m.kind {
                Kind::Plus => reports.iter().flat_map(|r| r[16..16 + u16::from_le_bytes([r[13], r[14]]) as usize].to_vec()).collect(),
                _ => join_gen2(&reports),
            };
            assert_eq!(joined, data, "{}", m.name);
            assert_orientation(&src, &image::load_from_memory(&joined).unwrap().to_rgb8(), m.screen_format);
        }
    }
}
