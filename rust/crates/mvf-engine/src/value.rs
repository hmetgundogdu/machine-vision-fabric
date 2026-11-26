//! Typed runtime values. Every value carried on an edge has exactly one [`PortType`]; the executor
//! re-checks it against the producer's declared port at runtime (UI/graph validation is not trusted
//! alone).

use std::sync::Arc;

use mvf_contract::PortType;
use serde::Serialize;

/// Semantic tag of a payload (matches `Mvf.Abstractions.PayloadMediaType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
#[repr(u16)]
pub enum MediaType {
    /// Opaque bytes.
    Blob = 0,
    /// Numeric tensor.
    Tensor = 1,
    /// Image (a tensor with this tag).
    Image = 2,
    /// UTF-8 JSON text.
    Json = 3,
}

impl MediaType {
    /// From the wire value.
    pub fn from_u16(v: u16) -> Option<Self> {
        Some(match v {
            0 => MediaType::Blob,
            1 => MediaType::Tensor,
            2 => MediaType::Image,
            3 => MediaType::Json,
            _ => return None,
        })
    }
}

/// Element type (matches `Mvf.Abstractions.PayloadElementType`, DLPack/numpy compatible).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
#[repr(u8)]
pub enum ElementType {
    /// u8
    U8 = 0,
    /// i8
    I8 = 1,
    /// u16
    U16 = 2,
    /// i16
    I16 = 3,
    /// u32
    U32 = 4,
    /// i32
    I32 = 5,
    /// u64
    U64 = 6,
    /// i64
    I64 = 7,
    /// f16
    F16 = 8,
    /// bf16
    BF16 = 9,
    /// f32
    F32 = 10,
    /// f64
    F64 = 11,
}

impl ElementType {
    /// From the wire value.
    pub fn from_u8(v: u8) -> Option<Self> {
        use ElementType::*;
        Some(match v {
            0 => U8,
            1 => I8,
            2 => U16,
            3 => I16,
            4 => U32,
            5 => I32,
            6 => U64,
            7 => I64,
            8 => F16,
            9 => BF16,
            10 => F32,
            11 => F64,
            _ => return None,
        })
    }

    /// Size of one element in bytes.
    pub fn size(self) -> u64 {
        use ElementType::*;
        match self {
            U8 | I8 => 1,
            U16 | I16 | F16 | BF16 => 2,
            U32 | I32 | F32 => 4,
            U64 | I64 | F64 => 8,
        }
    }
}

/// Maximum tensor rank (matches the 192-byte descriptor header).
pub const MAX_RANK: usize = 8;

/// Typed description of payload bytes (C-contiguous only, like descriptor v1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Descriptor {
    /// Semantic tag.
    pub media: MediaType,
    /// Element type.
    pub element: ElementType,
    /// Shape (row-major).
    pub shape: Vec<u64>,
}

impl Descriptor {
    /// A raw byte blob of `len` bytes.
    pub fn blob(len: u64) -> Self {
        Self {
            media: MediaType::Blob,
            element: ElementType::U8,
            shape: vec![len],
        }
    }

    /// An 8-bit image `height × width × channels`.
    pub fn image_u8(height: u64, width: u64, channels: u64) -> Self {
        Self {
            media: MediaType::Image,
            element: ElementType::U8,
            shape: vec![height, width, channels],
        }
    }

    /// Total bytes implied by the shape, or `None` on overflow / rank above [`MAX_RANK`].
    pub fn byte_len(&self) -> Option<u64> {
        if self.shape.len() > MAX_RANK {
            return None;
        }
        self.shape
            .iter()
            .try_fold(self.element.size(), |acc, d| acc.checked_mul(*d))
    }
}

/// Where payload bytes live.
#[derive(Debug, Clone)]
pub enum PayloadData {
    /// Reference-counted heap bytes (in-process hand-off is an `Arc` clone, never a byte copy).
    Heap(Arc<[u8]>),
}

/// Typed payload: descriptor + bytes. Cloning shares the bytes.
#[derive(Debug, Clone)]
pub struct Payload {
    /// Descriptor validated against the byte length at construction.
    pub descriptor: Descriptor,
    data: PayloadData,
}

impl Payload {
    /// Wraps heap bytes; fails if the descriptor does not describe exactly these bytes.
    pub fn heap(descriptor: Descriptor, bytes: impl Into<Arc<[u8]>>) -> Result<Self, String> {
        let bytes = bytes.into();
        match descriptor.byte_len() {
            Some(n) if n == bytes.len() as u64 => Ok(Self {
                descriptor,
                data: PayloadData::Heap(bytes),
            }),
            Some(n) => Err(format!(
                "descriptor implies {n} bytes but {} were given",
                bytes.len()
            )),
            None => Err("descriptor size overflows or exceeds the maximum rank".into()),
        }
    }

    /// Read-only view of the bytes (zero copy for both heap and arena payloads).
    pub fn bytes(&self) -> &[u8] {
        match &self.data {
            PayloadData::Heap(b) => b,
        }
    }

    /// Where the bytes live.
    pub fn data(&self) -> &PayloadData {
        &self.data
    }

    /// Byte length.
    pub fn len(&self) -> usize {
        self.bytes().len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.bytes().is_empty()
    }

    /// FNV-1a 64-bit checksum of the bytes (deterministic test/record fingerprint).
    pub fn checksum(&self) -> u64 {
        fnv1a64(self.bytes())
    }
}

/// FNV-1a 64-bit hash.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Frame identity carried with the payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrameMeta {
    /// Producing source (camera) id.
    pub source: String,
    /// Monotonic frame sequence assigned by the source.
    pub sequence: u64,
}

/// A frame: identity + typed payload.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Identity.
    pub meta: FrameMeta,
    /// Pixels.
    pub payload: Payload,
}

/// A classification result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Classification {
    /// Label.
    pub label: String,
    /// Optional measurement.
    pub measurement: Option<f64>,
    /// Unit of the measurement.
    pub unit: Option<String>,
    /// Free-form details.
    pub details: Option<String>,
}

/// A value on an edge.
#[derive(Debug, Clone)]
pub enum Value {
    /// `frame`
    Frame(Frame),
    /// `tensor`
    Tensor(Payload),
    /// `classification`
    Classification(Classification),
    /// `json`
    Json(serde_json::Value),
    /// `value` — a typed scalar (number, string, bool).
    Scalar(serde_json::Value),
    /// `signal` — the only control value.
    Signal(bool),
}

impl Value {
    /// The port type of this value.
    pub fn port_type(&self) -> PortType {
        match self {
            Value::Frame(_) => PortType::Frame,
            Value::Tensor(_) => PortType::Tensor,
            Value::Classification(_) => PortType::Classification,
            Value::Json(_) => PortType::Json,
            Value::Scalar(_) => PortType::Value,
            Value::Signal(_) => PortType::Signal,
        }
    }

    /// The payload, for frame and tensor values.
    pub fn payload(&self) -> Option<&Payload> {
        match self {
            Value::Frame(f) => Some(&f.payload),
            Value::Tensor(p) => Some(p),
            _ => None,
        }
    }
}
