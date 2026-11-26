//! # mvf-memory — CPU data plane
//!
//! - [`descriptor`] — the typed payload descriptor and its 192-byte header, byte-compatible with the
//!   .NET engine and the Python/C++ SDKs;
//! - [`arena`] — the engine-owned, file-backed shared-memory arena with RAII reservations and
//!   generation-checked, reference-counted leases.
//!
//! Local only: the arena is a file mapped by processes on the same machine. No network.

pub mod arena;
pub mod descriptor;

pub use arena::{
    Arena, ArenaConfig, ArenaError, ArenaMetrics, Handle, Lease, MetricsSnapshot, Reservation,
};
pub use descriptor::{Descriptor, DescriptorError, ElementType, HEADER_SIZE, MAX_RANK, MediaType};
