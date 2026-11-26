//! OCI data model: digests, descriptors, platforms, manifests, configs and image records.
//! Image JSON may carry extension fields, so these types are lenient on input (unknown fields
//! are ignored) and strict about what matters: digests, sizes and media types.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{OciError, Result};

/// OCI media types this implementation understands.
pub mod media {
    /// OCI image index.
    pub const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
    /// OCI image manifest.
    pub const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
    /// OCI image config.
    pub const OCI_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
    /// OCI layer, uncompressed.
    pub const OCI_LAYER: &str = "application/vnd.oci.image.layer.v1.tar";
    /// OCI layer, gzip.
    pub const OCI_LAYER_GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
    /// OCI layer, zstd (recognised, not supported).
    pub const OCI_LAYER_ZSTD: &str = "application/vnd.oci.image.layer.v1.tar+zstd";
    /// Docker manifest list.
    pub const DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
    /// Docker manifest v2.
    pub const DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
    /// Docker config.
    pub const DOCKER_CONFIG: &str = "application/vnd.docker.container.image.v1+json";
    /// Docker layer, gzip.
    pub const DOCKER_LAYER_GZIP: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";
    /// Docker layer, uncompressed (legacy `docker save`).
    pub const DOCKER_LAYER: &str = "application/vnd.docker.image.rootfs.diff.tar";
}

/// A validated `sha256:<64 hex>` digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);

impl Digest {
    /// Parses and validates a digest string.
    pub fn parse(s: &str) -> Result<Self> {
        let hex = s.strip_prefix("sha256:").ok_or_else(|| {
            OciError::Malformed(format!("unsupported digest `{s}` (sha256 only)"))
        })?;
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(OciError::Malformed(format!("invalid sha256 digest `{s}`")));
        }
        Ok(Self(s.to_string()))
    }

    /// Digest of `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(bytes);
        Self::from_hasher(h)
    }

    pub(crate) fn from_hasher(h: Sha256) -> Self {
        let out = h.finalize();
        let mut hex = String::with_capacity(64);
        for b in out.iter() {
            hex.push_str(&format!("{b:02x}"));
        }
        Self(format!("sha256:{hex}"))
    }

    /// The 64 hex characters.
    pub fn hex(&self) -> &str {
        &self.0[7..]
    }

    /// The full `sha256:<hex>` string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Digest {
    type Error = OciError;
    fn try_from(s: String) -> Result<Self> {
        Self::parse(&s)
    }
}

impl From<Digest> for String {
    fn from(d: Digest) -> String {
        d.0
    }
}

/// An OCI content descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    /// Media type.
    pub media_type: String,
    /// Content digest.
    pub digest: Digest,
    /// Content size in bytes.
    pub size: u64,
    /// Platform (index entries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    /// Annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<std::collections::BTreeMap<String, String>>,
}

/// OS/architecture/variant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Platform {
    /// `linux`, `windows`, …
    pub os: String,
    /// `amd64`, `arm64`, …
    pub architecture: String,
    /// `v8`, `v7`, … (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

impl Platform {
    /// `linux/<host arch>` — OCI images are Linux images on every MVF host.
    pub fn host_linux() -> Self {
        let architecture = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            "x86_64" => "amd64",
            "arm" => "arm",
            other => other,
        };
        Self {
            os: "linux".into(),
            architecture: architecture.into(),
            variant: None,
        }
    }

    /// Parses `os/arch[/variant]`.
    pub fn parse(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split('/').collect();
        match parts.as_slice() {
            [os, arch] => Ok(Self {
                os: (*os).into(),
                architecture: (*arch).into(),
                variant: None,
            }),
            [os, arch, variant] => Ok(Self {
                os: (*os).into(),
                architecture: (*arch).into(),
                variant: Some((*variant).into()),
            }),
            _ => Err(OciError::Malformed(format!(
                "platform `{s}` is not os/arch[/variant]"
            ))),
        }
    }

    /// Whether an image for `other` runs on this platform request (variant only checked when the
    /// request names one).
    pub fn accepts(&self, other: &Platform) -> bool {
        self.os == other.os
            && self.architecture == other.architecture
            && (self.variant.is_none() || self.variant == other.variant)
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.os, self.architecture)?;
        if let Some(v) = &self.variant {
            write!(f, "/{v}")?;
        }
        Ok(())
    }
}

/// OCI image index (also Docker manifest list).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Index {
    /// Must be 2.
    pub schema_version: u32,
    /// Media type, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Entries.
    pub manifests: Vec<Descriptor>,
}

/// OCI image manifest (also Docker manifest v2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Must be 2.
    pub schema_version: u32,
    /// Media type, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Config blob.
    pub config: Descriptor,
    /// Layers, base first.
    pub layers: Vec<Descriptor>,
    /// Annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<std::collections::BTreeMap<String, String>>,
}

/// The parts of an image config MVF uses. Everything else is preserved verbatim in the blob.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImageConfig {
    /// OS.
    #[serde(default)]
    pub os: String,
    /// Architecture.
    #[serde(default)]
    pub architecture: String,
    /// Variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// Execution defaults.
    #[serde(default)]
    pub config: Option<ExecConfig>,
    /// Layer diff ids (uncompressed digests).
    pub rootfs: RootFs,
}

/// Execution defaults from the image config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ExecConfig {
    /// `user[:group]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// `KEY=value` entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<String>>,
    /// Entrypoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    /// Default arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cmd: Option<Vec<String>>,
    /// Working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// Labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::BTreeMap<String, String>>,
}

/// `rootfs` of an image config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RootFs {
    /// Must be `layers`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Uncompressed layer digests, base first.
    pub diff_ids: Vec<Digest>,
}

/// Label naming the MVF node contract inside the image filesystem (metadata convention).
pub const NODE_CONTRACT_LABEL: &str = "dev.mvf.node.contract";

/// A stored image: what the store keeps per name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRecord {
    /// Name (`repo:tag`), unique in the store.
    pub name: String,
    /// Manifest descriptor (its digest identifies the image).
    pub manifest: Descriptor,
    /// Config descriptor.
    pub config: Descriptor,
    /// Layer descriptors, base first.
    pub layers: Vec<Descriptor>,
    /// Platform of the image.
    pub platform: Platform,
    /// Where it came from: `oci-layout`, `docker-archive`, `docker-archive-legacy`, `built`.
    pub source: String,
}
