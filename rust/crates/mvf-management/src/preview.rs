//! Opt-in image previews (#33 part 3).
//!
//! - **Opt-in per node.** Only nodes the operator named when starting the runtime
//!   (`mvf-rs run --manage --preview cam`) are ever sampled. Nothing is previewed by default.
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

    #[test]
    fn a_non_image_frame_is_described_not_rendered() {
        let v = Value::Json(json!({"a": 1}));
        assert_eq!(render(&v, 100)["format"], "none");
    }
}
