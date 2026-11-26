//! Export a stored image as an OCI image layout tar or a Docker archive. Blobs are copied
//! byte-for-byte (and re-verified while copying), so manifest, config and layer digests round-trip.
//!
//! Formats:
//! - **OCI layout** (`oci-layout`, `index.json`, `blobs/sha256/…`): the OCI Image Layout spec; read
//!   by `skopeo`, `crane`, `podman load`, `docker load` (Docker 25+) and by `mvf-rs image import`.
//! - **Docker archive**: the same OCI layout **plus** a `manifest.json` (`Config`, `RepoTags`,
//!   `Layers` pointing into `blobs/`), which is what `docker save` writes since Docker 25 and what
//!   `docker load` reads on every version that understands `blobs/` paths.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest as _, Sha256};

use crate::model::{Descriptor, Digest, ImageRecord, media};
use crate::store::ContentStore;
use crate::{OciError, Result};

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    /// OCI image layout tar.
    OciLayout,
    /// Docker archive (OCI layout + `manifest.json`).
    Docker,
}

fn header(size: u64) -> tar::Header {
    let mut h = tar::Header::new_ustar();
    h.set_size(size);
    h.set_mode(0o644);
    h.set_uid(0);
    h.set_gid(0);
    h.set_mtime(0);
    h.set_entry_type(tar::EntryType::Regular);
    h
}

fn add_bytes<W: Write>(b: &mut tar::Builder<W>, path: &str, bytes: &[u8]) -> Result<()> {
    let mut h = header(bytes.len() as u64);
    b.append_data(&mut h, path, bytes)
        .map_err(OciError::io(format!("write {path}")))
}

struct Verifying<R> {
    inner: R,
    hasher: Sha256,
}

impl<R: Read> Read for Verifying<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

fn add_blob<W: Write>(store: &ContentStore, b: &mut tar::Builder<W>, d: &Descriptor) -> Result<()> {
    let file = store.open_blob(&d.digest)?;
    let size = file
        .metadata()
        .map_err(OciError::io(format!("stat {}", d.digest)))?
        .len();
    if size != d.size {
        return Err(OciError::SizeMismatch {
            digest: d.digest.to_string(),
            expected: d.size,
            actual: size,
        });
    }
    let mut reader = Verifying {
        inner: file,
        hasher: Sha256::new(),
    };
    let mut h = header(size);
    b.append_data(
        &mut h,
        format!("blobs/sha256/{}", d.digest.hex()),
        &mut reader,
    )
    .map_err(OciError::io(format!("write blob {}", d.digest)))?;
    let actual = Digest::from_hasher(reader.hasher);
    if actual != d.digest {
        return Err(OciError::DigestMismatch {
            expected: d.digest.to_string(),
            actual: actual.to_string(),
        });
    }
    Ok(())
}

/// Writes `record` to `out` in `format`.
pub fn export(
    store: &ContentStore,
    record: &ImageRecord,
    out: &Path,
    format: ArchiveFormat,
) -> Result<()> {
    let file = File::create(out).map_err(OciError::io(format!("create {}", out.display())))?;
    let mut b = tar::Builder::new(file);
    add_bytes(&mut b, "oci-layout", br#"{"imageLayoutVersion":"1.0.0"}"#)?;

    let tag = record
        .name
        .rsplit_once(':')
        .filter(|(_, t)| !t.contains('/'))
        .map(|(_, t)| t.to_string())
        .unwrap_or_else(|| "latest".into());
    let index = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": media::OCI_INDEX,
        "manifests": [{
            "mediaType": record.manifest.media_type,
            "digest": record.manifest.digest,
            "size": record.manifest.size,
            "annotations": {
                "io.containerd.image.name": record.name,
                "org.opencontainers.image.ref.name": tag,
            },
        }],
    });
    add_bytes(&mut b, "index.json", index.to_string().as_bytes())?;

    for d in std::iter::once(&record.manifest)
        .chain(std::iter::once(&record.config))
        .chain(record.layers.iter())
    {
        add_blob(store, &mut b, d)?;
    }

    if format == ArchiveFormat::Docker {
        let repo_tags: Vec<&str> = if record.name.starts_with("sha256:") {
            vec![]
        } else {
            vec![record.name.as_str()]
        };
        let manifest = serde_json::json!([{
            "Config": format!("blobs/sha256/{}", record.config.digest.hex()),
            "RepoTags": repo_tags,
            "Layers": record.layers.iter().map(|l| format!("blobs/sha256/{}", l.digest.hex())).collect::<Vec<_>>(),
        }]);
        add_bytes(&mut b, "manifest.json", manifest.to_string().as_bytes())?;
    }
    b.into_inner()
        .and_then(|mut f| f.flush())
        .map_err(OciError::io(format!("finish {}", out.display())))
}
