//! Typed payload descriptor and its 192-byte little-endian header.
//!
//! Byte-for-byte compatible with `Mvf.Abstractions.PayloadDescriptor` (v1) and the Python/C++ SDKs:
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | u32 | magic `MVF1` (0x3146564D) |
//! | 4 | u16 | version (1) |
//! | 6 | u8 | flags (bit 0 = C-contiguous) |
//! | 8 | u32 | epoch |
//! | 12 | u16 | media type |
//! | 14 | u8 | element type |
//! | 15 | u8 | rank (≤ 8) |
//! | 16 | i64 | payload length |
//! | 24 | i64\[8\] | shape |
//! | 88 | i64\[8\] | strides (bytes, row-major) |
//!
//! The payload follows at offset 192 (a multiple of 64, so the payload stays 64-byte aligned).

use serde::Serialize;

/// Header size in bytes.
pub const HEADER_SIZE: usize = 192;
/// `MVF1` as a little-endian u32.
pub const MAGIC: u32 = 0x3146_564D;
/// Descriptor version.
pub const VERSION: u16 = 1;
/// Maximum rank.
pub const MAX_RANK: usize = 8;
const FLAG_CONTIGUOUS: u8 = 0x01;

/// Semantic tag of a payload (matches `PayloadMediaType`).
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

/// Element type (matches `PayloadElementType`, DLPack/numpy compatible).
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

/// Typed description of payload bytes (C-contiguous only, like descriptor v1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Descriptor {
    /// Semantic tag.
    pub media: MediaType,
    /// Element type.
    pub element: ElementType,
    /// Shape (row-major).
    pub shape: Vec<u64>,
    /// Epoch stamp (checkpoint epoch in the .NET engine; 0 when unused).
    pub epoch: u32,
}

/// Why a header was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DescriptorError {
    /// Fewer than [`HEADER_SIZE`] bytes.
    #[error("header truncated")]
    Truncated,
    /// Wrong magic or version.
    #[error("bad magic or version")]
    BadMagic,
    /// Rank above [`MAX_RANK`].
    #[error("rank {0} exceeds {MAX_RANK}")]
    Rank(u8),
    /// Unknown element type.
    #[error("unknown element type {0}")]
    ElementType(u8),
    /// Unknown media type.
    #[error("unknown media type {0}")]
    MediaType(u16),
    /// Negative dimension.
    #[error("negative dimension")]
    NegativeDimension,
    /// Not C-contiguous.
    #[error("non-contiguous layouts are not supported in v1")]
    NonContiguous,
    /// Size arithmetic overflowed.
    #[error("payload size overflows")]
    Overflow,
    /// Declared length disagrees with the shape.
    #[error("declared length {declared} disagrees with the shape ({computed} bytes)")]
    LengthMismatch {
        /// Declared.
        declared: i64,
        /// Computed from shape × element size.
        computed: u64,
    },
    /// Strides are not the row-major strides of the shape.
    #[error("strides are not row-major for the shape")]
    Strides,
}

impl Descriptor {
    /// A raw byte blob of `len` bytes.
    pub fn blob(len: u64) -> Self {
        Self {
            media: MediaType::Blob,
            element: ElementType::U8,
            shape: vec![len],
            epoch: 0,
        }
    }

    /// An 8-bit image `height × width × channels`.
    pub fn image_u8(height: u64, width: u64, channels: u64) -> Self {
        Self {
            media: MediaType::Image,
            element: ElementType::U8,
            shape: vec![height, width, channels],
            epoch: 0,
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

    /// Row-major strides in bytes.
    pub fn strides(&self) -> Option<Vec<u64>> {
        let mut strides = vec![0u64; self.shape.len()];
        let mut running = self.element.size();
        for i in (0..self.shape.len()).rev() {
            strides[i] = running;
            running = running.checked_mul(self.shape[i])?;
        }
        Some(strides)
    }

    /// Writes the header into `out[..HEADER_SIZE]`.
    pub fn write_header(&self, out: &mut [u8]) -> Result<(), DescriptorError> {
        if out.len() < HEADER_SIZE {
            return Err(DescriptorError::Truncated);
        }
        let rank = self.shape.len();
        if rank > MAX_RANK {
            return Err(DescriptorError::Rank(rank as u8));
        }
        let length = self.byte_len().ok_or(DescriptorError::Overflow)?;
        let length = i64::try_from(length).map_err(|_| DescriptorError::Overflow)?;
        let strides = self.strides().ok_or(DescriptorError::Overflow)?;
        let h = &mut out[..HEADER_SIZE];
        h.fill(0);
        h[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        h[4..6].copy_from_slice(&VERSION.to_le_bytes());
        h[6] = FLAG_CONTIGUOUS;
        h[8..12].copy_from_slice(&self.epoch.to_le_bytes());
        h[12..14].copy_from_slice(&(self.media as u16).to_le_bytes());
        h[14] = self.element as u8;
        h[15] = rank as u8;
        h[16..24].copy_from_slice(&length.to_le_bytes());
        for i in 0..rank {
            let dim = i64::try_from(self.shape[i]).map_err(|_| DescriptorError::Overflow)?;
            let stride = i64::try_from(strides[i]).map_err(|_| DescriptorError::Overflow)?;
            h[24 + i * 8..32 + i * 8].copy_from_slice(&dim.to_le_bytes());
            h[88 + i * 8..96 + i * 8].copy_from_slice(&stride.to_le_bytes());
        }
        Ok(())
    }

    /// Reads and validates a header. Never trusts the writer: magic, version, rank, element and
    /// media types, contiguity, overflow and the declared length are all checked. Strides written by
    /// a writer that omits them (all zero) are accepted; non-zero strides must be row-major.
    pub fn read_header(h: &[u8]) -> Result<Self, DescriptorError> {
        if h.len() < HEADER_SIZE {
            return Err(DescriptorError::Truncated);
        }
        let u16_at = |o: usize| u16::from_le_bytes([h[o], h[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]]);
        let i64_at = |o: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&h[o..o + 8]);
            i64::from_le_bytes(b)
        };
        if u32_at(0) != MAGIC || u16_at(4) != VERSION {
            return Err(DescriptorError::BadMagic);
        }
        if h[6] & FLAG_CONTIGUOUS == 0 {
            return Err(DescriptorError::NonContiguous);
        }
        let rank = h[15];
        if usize::from(rank) > MAX_RANK {
            return Err(DescriptorError::Rank(rank));
        }
        let element = ElementType::from_u8(h[14]).ok_or(DescriptorError::ElementType(h[14]))?;
        let media =
            MediaType::from_u16(u16_at(12)).ok_or(DescriptorError::MediaType(u16_at(12)))?;
        let mut shape = Vec::with_capacity(rank.into());
        for i in 0..usize::from(rank) {
            let d = i64_at(24 + i * 8);
            shape.push(u64::try_from(d).map_err(|_| DescriptorError::NegativeDimension)?);
        }
        let descriptor = Self {
            media,
            element,
            shape,
            epoch: u32_at(8),
        };
        let computed = descriptor.byte_len().ok_or(DescriptorError::Overflow)?;
        let declared = i64_at(16);
        if u64::try_from(declared).ok() != Some(computed) {
            return Err(DescriptorError::LengthMismatch { declared, computed });
        }
        let strides = descriptor.strides().ok_or(DescriptorError::Overflow)?;
        let written: Vec<i64> = (0..usize::from(rank)).map(|i| i64_at(88 + i * 8)).collect();
        if written.iter().any(|s| *s != 0)
            && written
                .iter()
                .zip(&strides)
                .any(|(w, s)| u64::try_from(*w).ok() != Some(*s))
        {
            return Err(DescriptorError::Strides);
        }
        Ok(descriptor)
    }
}
