//! Opt-in image previews (#33 part 3).
//!
//! - **Opt-in per node.** Only nodes the operator named when starting the runtime
//!   (`mvf run --manage --preview cam`) are ever sampled. Nothing is previewed by default.
//! - **Capped.**
//!   - Sampling is at most `max_fps` per node, and the engine hands over a frame only when the
//!     single-slot queue is free.
//!   - Frames are downscaled to at most `max_pixels`.
//!   - Each client has a byte budget per second; a preview that does not fit is skipped, not
//!     queued.
//! - **Bounded ownership.** The engine passes a shared lease, not a copy. The preview thread holds
//!   it only while it downscales (one frame per node at most), then drops it, so the arena slot is
//!   freed.
//! - **Not retained.** Only the latest preview per node is kept in memory. Previews are never
//!   written to the feed's ring, snapshots, the audit log or disk. Who opened a preview is
//!   audited.
//! - **Format.** PNG (8-bit grey, RGB or RGBA; uncompressed deflate, so no codec dependency),
//!   base64 in the JSON line.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use mvf_engine::{ElementType, Value};
use serde_json::{Value as Json, json};

/// What may be previewed, and how much.
#[derive(Debug, Clone)]
pub struct PreviewConfig {
    /// Nodes whose output frames may be previewed (opt-in; empty = none).
    pub nodes: Vec<String>,
    /// Samples per second per node, at most (default 2).
    pub max_fps: f64,
    /// Pixels per preview, at most; larger frames are downscaled (default 160 × 120).
    pub max_pixels: u64,
    /// Preview bytes per second per client, at most (default 256 KiB).
    pub bytes_per_second: u64,
}

impl Default for PreviewConfig {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            max_fps: 2.0,
            max_pixels: 160 * 120,
            bytes_per_second: 256 * 1024,
        }
    }
}

/// One preview.
#[derive(Debug)]
pub struct Preview {
    /// Per-node sequence.
    pub seq: u64,
    /// Node id.
    pub node: String,
    /// Cycle the frame came from.
    pub cycle: u64,
    /// The `{"op":"preview",…}` line.
    pub line: Arc<str>,
    /// Its size on the wire.
    pub bytes: usize,
}

struct Sample {
    node: String,
    cycle: u64,
    frame: Value,
}

#[derive(Default)]
struct Latest {
    by_node: HashMap<String, Arc<Preview>>,
    seq: HashMap<String, u64>,
}

/// The latest preview of each opted-in node, and the thread that makes them.
pub struct PreviewHub {
    config: PreviewConfig,
    tx: SyncSender<Sample>,
    latest: Mutex<Latest>,
    changed: Condvar,
    /// Samples the engine offered while the preview thread was still busy (dropped, not queued).
    pub busy_drops: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for PreviewHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewHub")
            .field("nodes", &self.config.nodes)
            .finish_non_exhaustive()
    }
}

impl PreviewHub {
    /// Starts the preview thread.
    pub fn start(config: PreviewConfig) -> Arc<Self> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Sample>(1);
        let hub = Arc::new(Self {
            config,
            tx,
            latest: Mutex::new(Latest::default()),
            changed: Condvar::new(),
            busy_drops: std::sync::atomic::AtomicU64::new(0),
        });
        let weak = Arc::downgrade(&hub);
        let _ = std::thread::Builder::new()
            .name("mvf-preview".into())
            .spawn(move || worker(rx, weak));
        hub
    }

    /// The configuration.
    pub fn config(&self) -> &PreviewConfig {
        &self.config
    }

    /// Whether `node` was opted in.
    pub fn enabled(&self, node: &str) -> bool {
        self.config.nodes.iter().any(|n| n == node)
    }

    /// Offers a node's frame from the engine thread: taken if the preview thread is free,
    /// dropped otherwise. Never waits.
    pub fn offer(&self, node: &str, cycle: u64, frame: &Value) {
        let sample = Sample {
            node: node.to_string(),
            cycle,
            frame: frame.clone(),
        };
        if let Err(TrySendError::Full(_)) = self.tx.try_send(sample) {
            self.busy_drops
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The first preview of `node` newer than `after`, waiting up to `wait`.
    pub fn wait_newer(&self, node: &str, after: u64, wait: Duration) -> Option<Arc<Preview>> {
        let latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        let newer = |l: &Latest| l.by_node.get(node).is_some_and(|p| p.seq > after);
        let (latest, _) = self
            .changed
            .wait_timeout_while(latest, wait, |l| !newer(l))
            .unwrap_or_else(|e| e.into_inner());
        latest.by_node.get(node).filter(|p| p.seq > after).cloned()
    }

    fn publish(&self, node: String, cycle: u64, mut body: Json) {
        let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        let seq = {
            let s = latest.seq.entry(node.clone()).or_insert(0);
            *s += 1;
            *s
        };
        body["op"] = json!("preview");
        body["node"] = json!(node);
        body["seq"] = json!(seq);
        body["cycle"] = json!(cycle);
        let line: Arc<str> = Arc::from(body.to_string());
        let bytes = line.len() + 1;
        latest.by_node.insert(
            node.clone(),
            Arc::new(Preview {
                seq,
                node,
                cycle,
                line,
                bytes,
            }),
        );
        drop(latest);
        self.changed.notify_all();
    }
}

fn worker(rx: Receiver<Sample>, hub: std::sync::Weak<PreviewHub>) {
    for sample in rx {
        let Some(hub) = hub.upgrade() else { return };
        let body = render(&sample.frame, hub.config.max_pixels);
        // The lease goes here, right after downscaling: the arena slot is free again.
        drop(sample.frame);
        hub.publish(sample.node, sample.cycle, body);
    }
}

/// Downscales and encodes one frame, or explains why it cannot be previewed.
fn render(frame: &Value, max_pixels: u64) -> Json {
    let Value::Frame(frame) = frame else {
        return json!({"format": "none", "reason": "not a frame"});
    };
    let d = &frame.payload.descriptor;
    let (h, w, c) = match d.shape.as_slice() {
        [h, w] => (*h, *w, 1),
        [h, w, c] => (*h, *w, *c),
        _ if d.element == ElementType::U8 => return encoded(frame, max_pixels),
        _ => return json!({"format": "none", "reason": "not an image shape"}),
    };
    if d.element != ElementType::U8 || !matches!(c, 1 | 3 | 4) || h == 0 || w == 0 {
        return json!({"format": "none", "reason": "not an 8-bit grey, RGB or RGBA image"});
    }
    let Ok(bytes) = frame.payload.try_bytes() else {
        return json!({"format": "none", "reason": "the payload is not readable on the host"});
    };
    let t0 = Instant::now();
    let scale = ((h * w) as f64 / max_pixels.max(1) as f64).sqrt().max(1.0);
    let (oh, ow) = (
        ((h as f64 / scale).floor() as u64).max(1),
        ((w as f64 / scale).floor() as u64).max(1),
    );
    let (h, w, c, oh, ow) = (h as usize, w as usize, c as usize, oh as usize, ow as usize);
    let mut pixels = Vec::with_capacity(oh * ow * c);
    for y in 0..oh {
        let sy = y * h / oh;
        for x in 0..ow {
            let sx = x * w / ow;
            let at = (sy * w + sx) * c;
            pixels.extend_from_slice(bytes.get(at..at + c).unwrap_or(&[0, 0, 0, 0][..c]));
        }
    }
    let png = png(ow as u32, oh as u32, c as u8, &pixels);
    json!({
        "format": "png",
        "width": ow,
        "height": oh,
        "sourceWidth": w,
        "sourceHeight": h,
        "renderUs": t0.elapsed().as_micros() as u64,
        "data": base64(&png),
    })
}

/// A byte blob: previewed only when its content says it is an image this host decodes (BMP).
/// Other blobs are never shown as raw pixels.
fn encoded(frame: &mvf_engine::Frame, max_pixels: u64) -> Json {
    let Ok(bytes) = frame.payload.try_bytes() else {
        return json!({"format": "none", "reason": "the payload is not readable on the host"});
    };
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return json!({"format": "none", "reason": "a PNG-encoded blob: compressed PNG is not decoded for previews (BMP is)"});
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return json!({"format": "none", "reason": "a JPEG-encoded blob: JPEG is not decoded for previews (BMP is)"});
    }
    if !bytes.starts_with(b"BM") {
        return json!({"format": "none", "reason": "a blob without a known image signature (only BMP-encoded blobs are decoded)"});
    }
    let t0 = Instant::now();
    match bmp(bytes, max_pixels) {
        Ok((w, h, ow, oh, pixels)) => json!({
            "format": "png",
            "encoding": "bmp",
            "width": ow,
            "height": oh,
            "sourceWidth": w,
            "sourceHeight": h,
            "renderUs": t0.elapsed().as_micros() as u64,
            "data": base64(&png(ow as u32, oh as u32, 3, &pixels)),
        }),
        Err(why) => {
            json!({"format": "none", "reason": format!("a BMP-encoded blob that cannot be decoded: {why}")})
        }
    }
}

/// The largest BMP edge decoded for a preview.
const BMP_MAX_EDGE: u64 = 16_384;

/// Decodes an uncompressed BMP (8-bit palette, 24-bit, or 32-bit), sampling only the pixels the
/// downscaled preview needs. Every offset is checked against the buffer. Returns (source width,
/// source height, preview width, preview height, RGB pixels).
fn bmp(b: &[u8], max_pixels: u64) -> Result<(usize, usize, usize, usize, Vec<u8>), String> {
    let u16_at = |o: usize| -> Result<u16, String> {
        b.get(o..o + 2)
            .map(|s| u16::from_le_bytes([s[0], s[1]]))
            .ok_or_else(|| "truncated header".to_string())
    };
    let u32_at = |o: usize| -> Result<u32, String> {
        b.get(o..o + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
            .ok_or_else(|| "truncated header".to_string())
    };
    let data_offset = u32_at(10)? as usize;
    let dib = u32_at(14)? as usize;
    if dib < 40 {
        return Err(format!(
            "{dib}-byte DIB header (only BITMAPINFOHEADER and later)"
        ));
    }
    let width = u32_at(18)? as i32;
    let height = u32_at(22)? as i32;
    let bpp = u16_at(28)?;
    let compression = u32_at(30)?;
    let top_down = height < 0;
    let (w, h) = (width.unsigned_abs() as u64, height.unsigned_abs() as u64);
    if w == 0 || h == 0 || w > BMP_MAX_EDGE || h > BMP_MAX_EDGE || width < 0 {
        return Err(format!("{width}x{height} is outside 1..={BMP_MAX_EDGE}"));
    }
    match (bpp, compression) {
        (8 | 24 | 32, 0) | (32, 3) => {}
        _ => {
            return Err(format!(
                "{bpp}-bit, compression {compression} (uncompressed 8, 24 and 32-bit only)"
            ));
        }
    }
    // Bitfields: only the usual BGRX/BGRA masks (red 0x00FF0000, green 0x0000FF00, blue 0xFF).
    if compression == 3
        && (u32_at(54)?, u32_at(58)?, u32_at(62)?) != (0x00FF_0000, 0x0000_FF00, 0x0000_00FF)
    {
        return Err("32-bit bitfields with non-standard colour masks".into());
    }
    let stride = (u64::from(bpp) * w).div_ceil(32) * 4;
    let end = (data_offset as u64)
        .checked_add(stride * h)
        .ok_or("pixel data size overflows")?;
    if end > b.len() as u64 {
        return Err(format!(
            "pixel data needs {end} bytes, the blob has {}",
            b.len()
        ));
    }
    let palette: Vec<[u8; 3]> = if bpp == 8 {
        let used = match u32_at(46)? {
            0 => 256,
            n => n.min(256),
        } as usize;
        let at = 14 + dib;
        let table = b.get(at..at + used * 4).ok_or("truncated palette")?;
        table.chunks_exact(4).map(|e| [e[2], e[1], e[0]]).collect()
    } else {
        Vec::new()
    };
    let scale = ((w * h) as f64 / max_pixels.max(1) as f64).sqrt().max(1.0);
    let (oh, ow) = (
        ((h as f64 / scale).floor() as usize).max(1),
        ((w as f64 / scale).floor() as usize).max(1),
    );
    let (w, h) = (w as usize, h as usize);
    let (stride, bytes_pp) = (stride as usize, usize::from(bpp / 8));
    let mut pixels = Vec::with_capacity(ow * oh * 3);
    for y in 0..oh {
        let sy = y * h / oh;
        let row = if top_down { sy } else { h - 1 - sy };
        for x in 0..ow {
            let sx = x * w / ow;
            let at = data_offset + row * stride + sx * bytes_pp;
            match bpp {
                8 => {
                    let index = usize::from(b[at]);
                    pixels
                        .extend_from_slice(palette.get(index).ok_or("palette index out of range")?);
                }
                _ => pixels.extend_from_slice(&[b[at + 2], b[at + 1], b[at]]),
            }
        }
    }
    Ok((w, h, ow, oh, pixels))
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

/// An 8-bit PNG with stored (uncompressed) deflate blocks.
pub fn png(width: u32, height: u32, channels: u8, pixels: &[u8]) -> Vec<u8> {
    let color = match channels {
        1 => 0,
        3 => 2,
        _ => 6,
    };
    let mut raw = Vec::with_capacity(pixels.len() + height as usize);
    let row = width as usize * channels as usize;
    for r in pixels.chunks(row.max(1)) {
        raw.push(0); // filter: none
        raw.extend_from_slice(r);
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = if raw.is_empty() {
        vec![&[][..]]
    } else {
        raw.chunks(65535).collect()
    };
    let last = blocks.len() - 1;
    for (i, block) in blocks.iter().enumerate() {
        z.push(u8::from(i == last));
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, color, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Decodes standard base64 (padding optional); `None` on any other character.
pub fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes().filter(|&c| c != b'=') {
        let v = B64.iter().position(|&x| x == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The engine-side sampler, kept by the [`crate::Tap`].
#[derive(Debug, Default)]
pub(crate) struct Sampler {
    next: HashMap<String, Instant>,
}

impl Sampler {
    pub(crate) fn wants(&mut self, hub: &PreviewHub, node: &str) -> bool {
        if !hub.enabled(node) {
            return false;
        }
        let now = Instant::now();
        match self.next.get(node) {
            Some(t) if now < *t => false,
            _ => {
                let gap = Duration::from_secs_f64(1.0 / hub.config.max_fps.max(0.01));
                self.next.insert(node.to_string(), now + gap);
                true
            }
        }
    }
}

/// A preview viewer's byte budget.
#[derive(Debug)]
pub(crate) struct Budget {
    tokens: f64,
    rate: f64,
    at: Instant,
}

impl Budget {
    pub(crate) fn new(bytes_per_second: u64) -> Self {
        Self {
            tokens: bytes_per_second as f64,
            rate: bytes_per_second as f64,
            at: Instant::now(),
        }
    }

    /// Spends `n` bytes if the budget allows.
    pub(crate) fn spend(&mut self, n: usize) -> bool {
        let now = Instant::now();
        self.tokens =
            (self.tokens + now.duration_since(self.at).as_secs_f64() * self.rate).min(self.rate);
        self.at = now;
        if self.tokens >= n as f64 {
            self.tokens -= n as f64;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for n in 0..70u8 {
            let data: Vec<u8> = (0..n).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(unbase64(&base64(&data)).as_deref(), Some(&data[..]));
        }
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert!(unbase64("not*base64").is_none());
    }

    #[test]
    fn png_has_its_signature_header_and_crc() {
        let png = png(2, 1, 3, &[255, 0, 0, 0, 255, 0]);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..24], &[0, 0, 0, 2, 0, 0, 0, 1]);
        // CRC-32 of "IEND" is the well-known AE 42 60 82.
        assert_eq!(&png[png.len() - 4..], &[0xAE, 0x42, 0x60, 0x82]);
    }

    fn blob(bytes: Vec<u8>) -> Value {
        let payload =
            mvf_engine::Payload::heap(mvf_engine::Descriptor::blob(bytes.len() as u64), bytes)
                .expect("payload");
        Value::Frame(mvf_engine::Frame {
            meta: mvf_engine::FrameMeta {
                source: "t".into(),
                sequence: 1,
            },
            payload,
        })
    }

    /// A BMP: `bpp` 8 (grey palette), 24 or 32 (bitfields), `w` x `h`, pixel (x, y) = colour(x, y).
    fn bmp_file(bpp: u16, w: i32, h: i32, colour: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let (aw, ah) = (w.unsigned_abs() as usize, h.unsigned_abs() as usize);
        let stride = (usize::from(bpp) * aw).div_ceil(32) * 4;
        let palette = if bpp == 8 { 256 * 4 } else { 0 };
        let masks = if bpp == 32 { 12 } else { 0 };
        let offset = 14 + 40 + masks + palette;
        let mut b = Vec::new();
        b.extend_from_slice(b"BM");
        b.extend_from_slice(&((offset + stride * ah) as u32).to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&(offset as u32).to_le_bytes());
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&bpp.to_le_bytes());
        b.extend_from_slice(&(if bpp == 32 { 3u32 } else { 0 }).to_le_bytes());
        b.extend_from_slice(&[0; 12]);
        b.extend_from_slice(&0u32.to_le_bytes()); // colours used: 0 = 256
        b.extend_from_slice(&[0; 4]);
        if bpp == 32 {
            for m in [0x00FF_0000u32, 0x0000_FF00, 0x0000_00FF] {
                b.extend_from_slice(&m.to_le_bytes());
            }
        }
        if bpp == 8 {
            for i in 0..=255u8 {
                b.extend_from_slice(&[i, i, i, 0]);
            }
        }
        for row in 0..ah {
            let y = if h < 0 { row } else { ah - 1 - row };
            let mut line = Vec::new();
            for x in 0..aw {
                let [r, g, bl] = colour(x, y);
                match bpp {
                    8 => line.push(r),
                    24 => line.extend_from_slice(&[bl, g, r]),
                    _ => line.extend_from_slice(&[bl, g, r, 255]),
                }
            }
            line.resize(stride, 0);
            b.extend_from_slice(&line);
        }
        b
    }

    fn decoded(v: &Json) -> (u64, u64, Vec<u8>) {
        assert_eq!(v["format"], "png", "{v}");
        let png = unbase64(v["data"].as_str().expect("data")).expect("base64");
        // Stored deflate, filter 0, RGB: strip the zlib and row framing.
        let idat = png.windows(4).position(|w| w == b"IDAT").expect("IDAT") + 4;
        let len = u32::from_be_bytes(png[idat - 8..idat - 4].try_into().expect("len")) as usize;
        let z = &png[idat..idat + len];
        let (mut raw, mut i) = (Vec::new(), 2);
        loop {
            let n = u16::from_le_bytes([z[i + 1], z[i + 2]]) as usize;
            raw.extend_from_slice(&z[i + 5..i + 5 + n]);
            let last = z[i] & 1 == 1;
            i += 5 + n;
            if last {
                break;
            }
        }
        let (w, h) = (
            v["width"].as_u64().expect("w"),
            v["height"].as_u64().expect("h"),
        );
        let px = raw
            .chunks(w as usize * 3 + 1)
            .flat_map(|r| r[1..].to_vec())
            .collect();
        (w, h, px)
    }

    #[test]
    fn bmp_blobs_are_decoded_bottom_up_top_down_palette_and_bitfields() {
        let colour = |x: usize, y: usize| [(x * 40) as u8, (y * 60) as u8, 7];
        for (bpp, h) in [(24, 3), (24, -3), (32, 3), (32, -3)] {
            let v = render(&blob(bmp_file(bpp, 5, h, colour)), 10_000);
            assert_eq!(v["encoding"], "bmp");
            assert_eq!(
                (v["sourceWidth"].as_u64(), v["sourceHeight"].as_u64()),
                (Some(5), Some(3))
            );
            let (w, hh, px) = decoded(&v);
            assert_eq!((w, hh), (5, 3));
            for (x, y) in [(0, 0), (4, 0), (2, 1), (4, 2)] {
                assert_eq!(
                    &px[(y * 5 + x) * 3..(y * 5 + x) * 3 + 3],
                    &colour(x, y),
                    "{bpp}-bit h={h} ({x},{y})"
                );
            }
        }
        let grey = |x: usize, _: usize| [(x * 50) as u8; 3];
        let (_, _, px) = decoded(&render(&blob(bmp_file(8, 4, 2, grey)), 10_000));
        assert_eq!(&px[9..12], &[150, 150, 150], "palette lookup");
        // Downscaled to the preview cap, sampled from the file.
        let big = render(&blob(bmp_file(24, 640, 480, colour)), 160 * 120);
        assert_eq!(
            (big["width"].as_u64(), big["height"].as_u64()),
            (Some(160), Some(120))
        );
    }

    #[test]
    fn other_blobs_say_what_they_are_and_bad_bmps_are_refused() {
        let reason = |b: Vec<u8>| {
            render(&blob(b), 100)["reason"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        assert!(reason(b"\x89PNG\r\n\x1a\nxxxx".to_vec()).contains("PNG-encoded"));
        assert!(reason(vec![0xFF, 0xD8, 0xFF, 0xE0, 0]).contains("JPEG-encoded"));
        assert!(reason(b"plain bytes".to_vec()).contains("without a known image signature"));
        let mut truncated = bmp_file(24, 8, 8, |_, _| [1, 2, 3]);
        truncated.truncate(truncated.len() - 10);
        assert!(reason(truncated).contains("pixel data needs"));
        let mut huge = bmp_file(24, 2, 2, |_, _| [1, 2, 3]);
        huge[18..22].copy_from_slice(&100_000i32.to_le_bytes());
        assert!(reason(huge).contains("outside"));
        let mut rle = bmp_file(24, 2, 2, |_, _| [1, 2, 3]);
        rle[30..34].copy_from_slice(&1u32.to_le_bytes());
        assert!(reason(rle).contains("compression 1"));
        let mut masks = bmp_file(32, 2, 2, |_, _| [1, 2, 3]);
        masks[54..58].copy_from_slice(&0xFFu32.to_le_bytes());
        assert!(reason(masks).contains("non-standard colour masks"));
        assert!(reason(b"BM".to_vec()).contains("truncated header"));
    }

    #[test]
    fn a_non_image_frame_is_described_not_rendered() {
        let v = Value::Json(json!({"a": 1}));
        assert_eq!(render(&v, 100)["format"], "none");
    }
}
