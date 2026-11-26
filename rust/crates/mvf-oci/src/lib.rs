//! # mvf-oci — offline OCI image support
//!
//! - [`store`] — a content-addressed store (`blobs/sha256/<hex>`): every blob is hashed while it is
//!   written and rejected on a digest or size mismatch; image records reference blobs by digest;
//!   identical content is stored once; unreferenced blobs are garbage-collected.
//! - [`import`] — import an **OCI image layout** (directory or tar) or a **Docker archive**
//!   (`docker save`, both the legacy `manifest.json` form and the Docker 25+ form that embeds an OCI
//!   layout) without a Docker daemon and without network access; selects the platform from an index.
//! - [`unpack`] — apply layers to a root filesystem safely: no absolute paths, no `..`, no writes
//!   through symlinks, hard links confined to the rootfs, OCI whiteouts, diff-id verification and
//!   size/entry limits.
//! - [`export`] — write a stored image back as an OCI layout tar or a Docker archive with
//!   byte-identical blobs, so digests round-trip.
//! - [`build`] — append a layer of files to a stored image (how an MVF node is packaged as an
//!   OCI-profile image without a Dockerfile engine).

pub mod build;
pub mod export;
pub mod import;
pub mod layers;
pub mod model;
pub mod store;
pub mod unpack;

pub use build::{FileEntry, LayerBlob, LayerBuilder, append_layer, create_image};
pub use export::{ArchiveFormat, export};
pub use import::{ImportSource, import, read_config};
pub use layers::{CacheReport, LayerCache};
pub use model::*;
pub use store::{ContentStore, GcReport, Limits};
pub use unpack::{DeferredLink, LayerOps, UnpackReport, Whiteout, unpack};

/// Errors from every OCI operation. Each variant is a refusal, never a silent fix-up.
#[derive(Debug, thiserror::Error)]
pub enum OciError {
    /// I/O failure.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// Cause.
        source: std::io::Error,
    },
    /// A blob's content does not hash to its declared digest.
    #[error("digest mismatch for {expected}: content hashes to {actual}")]
    DigestMismatch {
        /// Declared.
        expected: String,
        /// Computed.
        actual: String,
    },
    /// A blob's size differs from its descriptor.
    #[error("size mismatch for {digest}: descriptor says {expected} bytes, content has {actual}")]
    SizeMismatch {
        /// Blob.
        digest: String,
        /// Declared.
        expected: u64,
        /// Actual.
        actual: u64,
    },
    /// A configured limit was exceeded.
    #[error("limit exceeded: {0}")]
    Limit(String),
    /// Malformed archive or JSON document.
    #[error("malformed image: {0}")]
    Malformed(String),
    /// A referenced blob is not present.
    #[error("missing blob {0}")]
    MissingBlob(String),
    /// No manifest for the requested platform.
    #[error("no manifest for platform {0}")]
    Platform(String),
    /// A layer entry would escape the root filesystem or is otherwise unsafe.
    #[error("unsafe layer entry `{path}`: {reason}")]
    UnsafeEntry {
        /// Entry path.
        path: String,
        /// Why.
        reason: String,
    },
    /// The image uses a feature this implementation does not support (fails closed).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Unknown image name.
    #[error("no image named `{0}` in the store")]
    UnknownImage(String),
}

impl OciError {
    pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> OciError {
        let context = context.into();
        move |source| OciError::Io { context, source }
    }
}

/// Result alias.
pub type Result<T> = std::result::Result<T, OciError>;
