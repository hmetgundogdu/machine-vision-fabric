//! Typed runtime values. Every value carried on an edge has exactly one [`PortType`]; the executor
//! re-checks it against the producer's declared port at runtime (UI/graph validation is not trusted
//! alone).

use std::sync::Arc;

use mvf_contract::PortType;
pub use mvf_memory::{Descriptor, ElementType, MAX_RANK, MediaType};
use serde::Serialize;

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
