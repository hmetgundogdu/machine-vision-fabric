//! v2 message bodies. Hot-path bodies are fixed little-endian layouts; anything open-ended
//! (classification, structured values, config) is a JSON tail.

use crate::frame::{FrameError, Kind};

/// "No offset" marker (a source has no input; a classifier no output slot).
pub const NONE: u64 = u64::MAX;

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap_or([0; 8]))
}

fn malformed(kind: Kind, reason: impl Into<String>) -> FrameError {
    FrameError::Malformed {
        kind,
        reason: reason.into(),
    }
}

/// `execute` body.
///
/// ```text
/// 0  input     u64  arena offset of the input slot (NONE for a source)
/// 8  sequence  u64
/// 16 out       u64  output slot offset (NONE = none)
/// 24 capacity  u64  output slot capacity
/// 32 deadline  u32  budget in microseconds (0 = none)
/// 36 camera    u16  length, then the bytes
/// 38 content   u16  length, then the bytes
/// 40 camera bytes, content-type bytes
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Execute {
    /// Input slot offset.
    pub input: Option<u64>,
    /// Frame sequence.
    pub sequence: u64,
    /// Output slot `(offset, capacity)`.
    pub out: Option<(u64, u64)>,
    /// Budget in microseconds (0 = none).
    pub deadline_us: u32,
    /// Camera id.
    pub camera: String,
    /// Content type.
    pub content_type: String,
}

impl Execute {
    /// Encodes the body.
    pub fn encode(&self) -> Vec<u8> {
        let cam = self.camera.as_bytes();
        let ct = self.content_type.as_bytes();
        let (cam, ct) = (
            &cam[..cam.len().min(u16::MAX as usize)],
            &ct[..ct.len().min(u16::MAX as usize)],
        );
        let mut b = Vec::with_capacity(40 + cam.len() + ct.len());
        b.extend_from_slice(&self.input.unwrap_or(NONE).to_le_bytes());
        b.extend_from_slice(&self.sequence.to_le_bytes());
        let (o, c) = self.out.unwrap_or((NONE, 0));
        b.extend_from_slice(&o.to_le_bytes());
        b.extend_from_slice(&c.to_le_bytes());
        b.extend_from_slice(&self.deadline_us.to_le_bytes());
        b.extend_from_slice(&(cam.len() as u16).to_le_bytes());
        b.extend_from_slice(&(ct.len() as u16).to_le_bytes());
        b.extend_from_slice(cam);
        b.extend_from_slice(ct);
        b
    }

    /// Decodes the body.
    pub fn decode(b: &[u8]) -> Result<Self, FrameError> {
        if b.len() < 40 {
            return Err(malformed(Kind::Execute, "shorter than 40 bytes"));
        }
        let cam = u16::from_le_bytes([b[36], b[37]]) as usize;
        let ct = u16::from_le_bytes([b[38], b[39]]) as usize;
        if b.len() != 40 + cam + ct {
            return Err(malformed(
                Kind::Execute,
                "string lengths disagree with the body",
            ));
        }
        let text = |s: &[u8]| {
            std::str::from_utf8(s)
                .map(str::to_string)
                .map_err(|_| malformed(Kind::Execute, "not UTF-8"))
        };
        let input = u64_at(b, 0);
        let out = u64_at(b, 16);
        Ok(Self {
            input: (input != NONE).then_some(input),
            sequence: u64_at(b, 8),
            out: (out != NONE).then_some((out, u64_at(b, 24))),
            deadline_us: u32::from_le_bytes([b[32], b[33], b[34], b[35]]),
            camera: text(&b[40..40 + cam])?,
            content_type: text(&b[40 + cam..])?,
        })
    }
}

/// `result` flag bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResultFlags;

impl ResultFlags {
    /// A frame was written to the output slot.
    pub const FRAME: u32 = 1;
    /// The node dropped the frame (`frame: null`).
    pub const DROP: u32 = 2;
    /// A source's stream ended.
    pub const EXHAUSTED: u32 = 4;
    /// A JSON tail follows (classification, value, cameraId).
    pub const JSON: u32 = 8;
}

/// `result` body.
///
/// ```text
/// 0  flags     u32  ResultFlags
/// 4  reserved  u32
/// 8  out       u64  offset of the written frame
/// 16 sequence  u64  a source's frame sequence (0 = default)
/// 24 JSON tail (when flags & JSON): {"classification":…, "value":…, "cameraId":…}
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResultMsg {
    /// Flags.
    pub flags: u32,
    /// Output offset.
    pub out: u64,
    /// Source sequence.
    pub sequence: u64,
    /// JSON tail.
    pub tail: Option<serde_json::Value>,
}

impl ResultMsg {
    /// Encodes the body.
    pub fn encode(&self) -> Vec<u8> {
        let mut flags = self.flags & !ResultFlags::JSON;
        let tail = self.tail.as_ref().map(|t| t.to_string().into_bytes());
        if tail.is_some() {
            flags |= ResultFlags::JSON;
        }
        let mut b = Vec::with_capacity(24 + tail.as_ref().map_or(0, Vec::len));
        b.extend_from_slice(&flags.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&self.out.to_le_bytes());
        b.extend_from_slice(&self.sequence.to_le_bytes());
        if let Some(t) = tail {
            b.extend_from_slice(&t);
        }
        b
    }

    /// Decodes the body.
    pub fn decode(b: &[u8]) -> Result<Self, FrameError> {
        if b.len() < 24 {
            return Err(malformed(Kind::Result, "shorter than 24 bytes"));
        }
        let flags = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let tail = if flags & ResultFlags::JSON != 0 {
            Some(
                serde_json::from_slice(&b[24..])
                    .map_err(|e| malformed(Kind::Result, format!("JSON tail: {e}")))?,
            )
        } else if b.len() > 24 {
            return Err(malformed(
                Kind::Result,
                "bytes after the body without the JSON flag",
            ));
        } else {
            None
        };
        Ok(Self {
            flags,
            out: u64_at(b, 8),
            sequence: u64_at(b, 16),
            tail,
        })
    }
}

/// `checkpoint` body: `offset u64, capacity u64`.
pub fn slot(offset: u64, capacity: u64) -> Vec<u8> {
    let mut b = offset.to_le_bytes().to_vec();
    b.extend_from_slice(&capacity.to_le_bytes());
    b
}

/// Decodes a `checkpoint`/`restore` slot body (`restore` carries only the offset).
pub fn decode_slot(kind: Kind, b: &[u8]) -> Result<(u64, u64), FrameError> {
    match b.len() {
        8 => Ok((u64_at(b, 0), 0)),
        16 => Ok((u64_at(b, 0), u64_at(b, 8))),
        _ => Err(malformed(kind, "a slot is 8 or 16 bytes")),
    }
}
