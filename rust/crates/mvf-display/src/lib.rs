//! MVF Display Fabric core (#30).
//!
//! MVF manages presentation **access and lifecycle**; it never draws a node's UI. This crate holds
//! everything that needs no windowing library, so node applications and the engine never link one:
//!
//! - [`session`]: is this process in an interactive user session (native windows possible) or a
//!   service / headless one?
//! - [`surface`]: the language-neutral shared-memory surface (`protocol/display-surface.md`). A node
//!   writes frames into one of three buffers and never blocks; a display host copies out the latest
//!   frame. Integrity does not depend on the producer's memory model: every frame and every input
//!   event carries a CRC-32 the reader verifies, so a producer without atomics (Python) is safe.
//! - [`registry`]: the engine-side allocation of surfaces per pipeline node, their descriptors in a
//!   per-user rendezvous directory, and cleanup of runs whose engine died.
//!
//! The display host (`mvf-display-host`) is a separate process in the user's session. It may be
//! absent, slow, crash or restart: the graph never waits for it.

pub mod alive;
pub mod registry;
pub mod session;
pub mod surface;

pub use registry::{
    Descriptor, Discovered, DisplayRegistry, Grants, SurfaceRequest, discover, sweep,
};
pub use session::{Session, SessionKind};
pub use surface::Geometry;
pub use surface::{
    Frame, FrameInfo, HostStatus, InputEvent, InputKind, PixelFormat, Presented, SurfaceError,
    SurfaceReader, SurfaceWriter,
};
