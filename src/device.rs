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
    api.device_list()
        .filter(|d| d.vendor_id() == ELGATO_VID && model_for_pid(d.product_id()).is_some())
        // Only the first HID interface carries the Stream Deck protocol.
        .filter(|d| d.interface_number() <= 0)
        .collect()
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
        let ((sr, so), (fr, fo)) = match model.protocol {
            Protocol::Gen2 => ((0x06, 2), (0x05, 6)),
            Protocol::Gen1Mini | Protocol::Gen1Original => ((0x03, 5), (0x04, 5)),
        };
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
        let len = match self.model.protocol {
            Protocol::Gen2 => IMAGE_REPORT_LEN,
            Protocol::Gen1Mini => 1024,
            Protocol::Gen1Original => 8191,
        };
        let mut buf = vec![0u8; len];
        buf[0] = 0x02;
        self.dev.write(&buf)?;
        Ok(())
    }

    fn feature_len(&self) -> usize {
        if self.model.protocol == Protocol::Gen2 {
            FEATURE_LEN
        } else {
            GEN1_FEATURE_LEN
        }
    }

    fn read_feature_string(&self, report: u8, offset: usize) -> Result<String> {
        let mut buf = vec![0u8; self.feature_len()];
        buf[0] = report;
        let n = self.dev.get_feature_report(&mut buf)?;
        Ok(extract_string(&buf[offset.min(n)..n]))
    }

    fn write_feature(&self, data: &[u8]) -> Result<()> {
        let mut buf = vec![0u8; self.feature_len()];
        buf[..data.len()].copy_from_slice(data);
        self.dev.send_feature_report(&buf)?;
        Ok(())
    }

    /// Restore the device to its boot state (shows the Elgato logo).
    pub fn reset(&self) -> Result<()> {
        match self.model.protocol {
            Protocol::Gen2 => self.write_feature(&[0x03, 0x02]),
            _ => self.write_feature(&[0x0b, 0x63]),
        }
    }

    pub fn set_brightness(&self, percent: u8) -> Result<()> {
        match self.model.protocol {
            Protocol::Gen2 => self.write_feature(&[0x03, 0x08, percent.min(100)]),
            _ => self.write_feature(&[0x05, 0x55, 0xaa, 0xd1, 0x01, percent.min(100)]),
        }
    }

    /// Set the LED color of a Neo touch point (0 = left, 1 = right).
    pub fn set_touch_point_color(&self, index: u8, rgb: [u8; 3]) -> Result<()> {
        if index >= self.model.touch_points {
            bail!("touch point {index} out of range");
        }
        let key = self.model.key_count() as u8 + index;
        self.write_feature(&[0x03, 0x06, key, rgb[0], rgb[1], rgb[2]])
    }

    /// Upload an already device-encoded key image (see `Model::key_format`).
    pub fn set_key_image(&self, key: u8, data: &[u8]) -> Result<()> {
        if key as usize >= self.model.key_count() || self.model.key_px == 0 {
            bail!("key {key} has no display");
        }
        for r in key_image_reports(self.model, key, data) {
            self.dev.write(&r)?;
        }
        Ok(())
    }

    /// Upload an already device-encoded (JPEG, flipped) image to the info bar (Neo).
    pub fn set_screen_image(&self, jpeg: &[u8]) -> Result<()> {
        match self.model.kind {
            Kind::Neo => self.write_chunked(|page, last, len| [0x02, 0x0b, 0x00, last, len as u8, (len >> 8) as u8, page as u8, (page >> 8) as u8], jpeg),
            Kind::Plus => {
                let (w, h) = self.model.screen.unwrap();
                self.set_lcd_region(0, 0, w as u16, h as u16, jpeg)
            }
            _ => bail!("device has no screen"),
        }
    }

    /// Stream Deck + touch strip partial update (JPEG, not rotated).
    pub fn set_lcd_region(&self, x: u16, y: u16, w: u16, h: u16, jpeg: &[u8]) -> Result<()> {
        for r in lcd_reports(x, y, w, h, jpeg) {
            self.dev.write(&r)?;
        }
        Ok(())
    }

    fn write_chunked(&self, header: impl Fn(usize, u8, usize) -> [u8; IMAGE_HEADER_LEN], data: &[u8]) -> Result<()> {
        let payload = IMAGE_REPORT_LEN - IMAGE_HEADER_LEN;
        let mut page = 0usize;
        let mut rest = data;
        loop {
            let chunk = &rest[..rest.len().min(payload)];
            rest = &rest[chunk.len()..];
            let last = rest.is_empty() as u8;
            let mut buf = [0u8; IMAGE_REPORT_LEN];
            buf[..IMAGE_HEADER_LEN].copy_from_slice(&header(page, last, chunk.len()));
            buf[IMAGE_HEADER_LEN..IMAGE_HEADER_LEN + chunk.len()].copy_from_slice(chunk);
            self.dev.write(&buf)?;
            page += 1;
            if last == 1 {
                return Ok(());
            }
        }
    }

    /// Wait up to `timeout_ms` for an input report. `Ok(None)` on timeout.
    pub fn read_input(&self, timeout_ms: i32) -> Result<Option<Input>> {
        let mut buf = [0u8; 512];
        let n = self.dev.read_timeout(&mut buf, timeout_ms)?;
        let min = if self.model.protocol == Protocol::Gen2 { 4 } else { 1 + self.model.key_count() };
        if n < min || buf[0] != 0x01 {
            return Ok(None);
        }
        let data = &buf[..n];
        if self.model.protocol != Protocol::Gen2 {
            return Ok(Some(Input::Buttons(gen1_buttons(self.model, data))));
        }
        let input = match data[1] {
            0x00 => {
                let count = self.model.key_count() + self.model.touch_points as usize;
                Input::Buttons(data[4..(4 + count).min(n)].iter().map(|&b| b != 0).collect())
            }
            0x02 if n >= 15 => {
                let rd = |o: usize| u16::from_le_bytes([data[o], data[o + 1]]);
                Input::Touch { kind: data[4], x: rd(6), y: rd(8), x2: rd(10), y2: rd(12) }
            }
            0x03 if n >= 5 + self.model.encoders as usize => {
                let enc = &data[5..5 + self.model.encoders as usize];
                if data[4] == 0x00 {
                    Input::EncoderPress(enc.iter().map(|&b| b != 0).collect())
                } else {
                    Input::EncoderTurn(enc.iter().map(|&b| b as i8).collect())
                }
            }
            _ => return Ok(None),
        };
        Ok(Some(input))
    }
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
}
