//! v2 framing: `[header 24 bytes][body length bytes]`, little-endian.
//!
//! ```text
//! 0  magic       "MV"
//! 2  version     u8 = 2
//! 3  kind        u8   (see [`Kind`])
//! 4  flags       u16  (kind-specific)
//! 6  reserved    u16  (0)
//! 8  id          u64  correlation id (0 = unsolicited)
//! 16 generation  u32  the node generation the frame belongs to
//! 20 length      u32  body bytes (<= the negotiated maximum)
//! ```

use std::io::{Read, Write};

/// Frame magic.
pub const MAGIC: [u8; 2] = *b"MV";
/// Protocol version carried by every frame.
pub const VERSION: u8 = 2;
/// Header size.
pub const HEADER: usize = 24;
/// Default (and largest negotiable) body size: control messages are small; payloads never travel
/// here.
pub const MAX_FRAME: u32 = 1 << 20;

/// Frame kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Kind {
    /// Child -> engine handshake (JSON: protocol, moduleId, capability, ready, features, session).
    Hello = 1,
    /// Child -> engine: warmup finished.
    Ready = 2,
    /// Engine -> child: run one cycle (binary [`crate::Execute`]).
    Execute = 3,
    /// Child -> engine: the cycle's outputs (binary [`crate::ResultMsg`] + optional JSON tail).
    Result = 4,
    /// Child -> engine: a request failed (JSON: message, code).
    Error = 5,
    /// Engine -> child: apply config (JSON).
    Configure = 6,
    /// Child -> engine: config applied.
    Configured = 7,
    /// Engine -> child: write state into a slot (binary: offset u64, capacity u64).
    Checkpoint = 8,
    /// Child -> engine: state written (flags bit 0 = empty).
    State = 9,
    /// Engine -> child: restore from a slot (binary: offset u64).
    Restore = 10,
    /// Child -> engine: restored.
    Restored = 11,
    /// Engine -> child: liveness probe.
    Ping = 12,
    /// Child -> engine: liveness answer.
    Pong = 13,
    /// Engine -> child: abandon request `id` (best effort; the deadline still applies).
    Cancel = 14,
    /// Engine -> child: exit.
    Shutdown = 15,
}

impl Kind {
    /// From the wire byte.
    pub fn from_u8(b: u8) -> Option<Self> {
        use Kind::*;
        Some(match b {
            1 => Hello,
            2 => Ready,
            3 => Execute,
            4 => Result,
            5 => Error,
            6 => Configure,
            7 => Configured,
            8 => Checkpoint,
            9 => State,
            10 => Restore,
            11 => Restored,
            12 => Ping,
            13 => Pong,
            14 => Cancel,
            15 => Shutdown,
            _ => return None,
        })
    }
}

/// A frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Kind.
    pub kind: Kind,
    /// Kind-specific flags.
    pub flags: u16,
    /// Correlation id.
    pub id: u64,
    /// Node generation.
    pub generation: u32,
    /// Body length.
    pub length: u32,
}

impl Header {
    /// A header for `kind` with no flags.
    pub fn new(kind: Kind, id: u64, generation: u32) -> Self {
        Self {
            kind,
            flags: 0,
            id,
            generation,
            length: 0,
        }
    }

    /// Encodes the header.
    pub fn encode(&self) -> [u8; HEADER] {
        let mut b = [0u8; HEADER];
        b[0..2].copy_from_slice(&MAGIC);
        b[2] = VERSION;
        b[3] = self.kind as u8;
        b[4..6].copy_from_slice(&self.flags.to_le_bytes());
        b[8..16].copy_from_slice(&self.id.to_le_bytes());
        b[16..20].copy_from_slice(&self.generation.to_le_bytes());
        b[20..24].copy_from_slice(&self.length.to_le_bytes());
        b
    }

    /// Decodes and validates a header.
    pub fn decode(b: &[u8; HEADER], max: u32) -> Result<Self, FrameError> {
        if b[0..2] != MAGIC {
            return Err(FrameError::BadMagic([b[0], b[1]]));
        }
        if b[2] != VERSION {
            return Err(FrameError::Version(b[2]));
        }
        let kind = Kind::from_u8(b[3]).ok_or(FrameError::UnknownKind(b[3]))?;
        let length = u32::from_le_bytes([b[20], b[21], b[22], b[23]]);
        if length > max {
            return Err(FrameError::Oversized { length, max });
        }
        Ok(Self {
            kind,
            flags: u16::from_le_bytes([b[4], b[5]]),
            id: u64::from_le_bytes(b[8..16].try_into().unwrap_or([0; 8])),
            generation: u32::from_le_bytes([b[16], b[17], b[18], b[19]]),
            length,
        })
    }
}

/// Why a frame could not be read. Every variant except [`FrameError::Eof`] is a protocol
/// violation: the peer is not trusted to resynchronise, the channel is closed.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// Clean end of stream at a frame boundary.
    #[error("channel closed")]
    Eof,
    /// The stream ended inside a frame.
    #[error("channel closed inside a frame ({got} of {want} bytes)")]
    Truncated {
        /// Bytes read.
        got: usize,
        /// Bytes expected.
        want: usize,
    },
    /// Not a v2 frame.
    #[error("bad frame magic {0:02x?}")]
    BadMagic([u8; 2]),
    /// Another protocol version.
    #[error("unsupported frame version {0}")]
    Version(u8),
    /// Unknown kind.
    #[error("unknown frame kind {0}")]
    UnknownKind(u8),
    /// Body larger than allowed.
    #[error("frame body of {length} bytes exceeds the limit of {max}")]
    Oversized {
        /// Declared length.
        length: u32,
        /// Limit.
        max: u32,
    },
    /// A body that does not parse for its kind.
    #[error("malformed {kind:?} body: {reason}")]
    Malformed {
        /// Kind.
        kind: Kind,
        /// Reason.
        reason: String,
    },
    /// I/O.
    #[error("channel: {0}")]
    Io(#[from] std::io::Error),
}

fn fill(r: &mut impl Read, buf: &mut [u8], at_boundary: bool) -> Result<(), FrameError> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) if got == 0 && at_boundary => return Err(FrameError::Eof),
            Ok(0) => {
                return Err(FrameError::Truncated {
                    got,
                    want: buf.len(),
                });
            }
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Reads one frame. Partial reads are completed; a short stream is [`FrameError::Truncated`].
pub fn read_frame(r: &mut impl Read, max: u32, body: &mut Vec<u8>) -> Result<Header, FrameError> {
    let mut h = [0u8; HEADER];
    fill(r, &mut h, true)?;
    let header = Header::decode(&h, max)?;
    body.resize(header.length as usize, 0);
    fill(r, body, false)?;
    Ok(header)
}

/// Writes one frame (header and body in a single write when possible).
pub fn write_frame(w: &mut impl Write, mut header: Header, body: &[u8]) -> std::io::Result<()> {
    header.length =
        u32::try_from(body.len()).map_err(|_| std::io::Error::other("frame body too large"))?;
    let mut buf = Vec::with_capacity(HEADER + body.len());
    buf.extend_from_slice(&header.encode());
    buf.extend_from_slice(body);
    w.write_all(&buf)?;
    w.flush()
}
