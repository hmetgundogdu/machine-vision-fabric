//! Content-addressed, verified blob store plus image records.
//!
//! Layout:
//! ```text
//! <root>/blobs/sha256/<hex>     immutable content, verified on write
//! <root>/images/<sha256(name)>.json   ImageRecord
//! <root>/tmp/                   in-flight writes (renamed into place only after verification)
//! ```

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest as _, Sha256};

use crate::model::{Digest, ImageRecord};
use crate::{OciError, Result};

/// Resource limits for imports and unpacking.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Largest single blob.
    pub max_blob_bytes: u64,
    /// Largest JSON document (index, manifest, config).
    pub max_json_bytes: u64,
    /// Most layers per image.
    pub max_layers: usize,
    /// Deepest nested index.
    pub max_index_depth: usize,
    /// Largest total uncompressed rootfs.
    pub max_unpacked_bytes: u64,
    /// Most entries across all layers.
    pub max_entries: u64,
    /// Longest entry path.
    pub max_path_len: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_blob_bytes: 8 << 30,
            max_json_bytes: 4 << 20,
            max_layers: 128,
            max_index_depth: 4,
            max_unpacked_bytes: 16 << 30,
            max_entries: 2_000_000,
            max_path_len: 4096,
        }
    }
}

/// The store.
#[derive(Debug, Clone)]
pub struct ContentStore {
    root: PathBuf,
    /// Limits applied to every operation on this store.
    pub limits: Limits,
}

/// What garbage collection removed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Blobs removed.
    pub removed: usize,
    /// Bytes freed.
    pub bytes: u64,
    /// Blobs kept (referenced by at least one image).
    pub kept: usize,
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl ContentStore {
    /// Opens (creating if needed) a store at `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        for dir in ["blobs/sha256", "images", "tmp"] {
            fs::create_dir_all(root.join(dir))
                .map_err(OciError::io(format!("create {}", root.join(dir).display())))?;
        }
        Ok(Self {
            root,
            limits: Limits::default(),
        })
    }

    /// Store root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of a blob (whether or not it exists).
    pub fn blob_path(&self, digest: &Digest) -> PathBuf {
        self.root.join("blobs/sha256").join(digest.hex())
    }

    /// Whether a blob is present.
    pub fn has(&self, digest: &Digest) -> bool {
        self.blob_path(digest).is_file()
    }

    /// Streams `reader` into the store, hashing as it goes. The blob only becomes visible after its
    /// digest (and size, when given) matched. Existing identical content is reused (deduplicated).
    pub fn put_verified(
        &self,
        expected: &Digest,
        expected_size: Option<u64>,
        reader: impl Read,
    ) -> Result<u64> {
        if let Some(size) = expected_size
            && size > self.limits.max_blob_bytes
        {
            return Err(OciError::Limit(format!(
                "blob {expected} declares {size} bytes (limit {})",
                self.limits.max_blob_bytes
            )));
        }
        let (actual, size, tmp) = self.write_tmp(reader)?;
        if &actual != expected {
            let _ = fs::remove_file(&tmp);
            return Err(OciError::DigestMismatch {
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
        if let Some(want) = expected_size
            && want != size
        {
            let _ = fs::remove_file(&tmp);
            return Err(OciError::SizeMismatch {
                digest: expected.to_string(),
                expected: want,
                actual: size,
            });
        }
        self.commit_tmp(&tmp, &actual)?;
        Ok(size)
    }

    /// Streams `reader` into the store and returns its digest (content whose digest is not known in
    /// advance, e.g. legacy Docker layer tars; the caller verifies it against the image config).
    pub fn put_computed(&self, reader: impl Read) -> Result<(Digest, u64)> {
        let (digest, size, tmp) = self.write_tmp(reader)?;
        self.commit_tmp(&tmp, &digest)?;
        Ok((digest, size))
    }

    /// Stores small in-memory content.
    pub fn put_bytes(&self, bytes: &[u8]) -> Result<Digest> {
        let digest = Digest::of(bytes);
        self.put_verified(&digest, Some(bytes.len() as u64), bytes)?;
        Ok(digest)
    }

    fn write_tmp(&self, mut reader: impl Read) -> Result<(Digest, u64, PathBuf)> {
        let tmp = self.root.join("tmp").join(format!(
            "{}-{}",
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file =
            File::create(&tmp).map_err(OciError::io(format!("create {}", tmp.display())))?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let _ = fs::remove_file(&tmp);
                    return Err(OciError::Io {
                        context: "read blob".into(),
                        source: e,
                    });
                }
            };
            size += n as u64;
            if size > self.limits.max_blob_bytes {
                let _ = fs::remove_file(&tmp);
                return Err(OciError::Limit(format!(
                    "blob exceeds {} bytes",
                    self.limits.max_blob_bytes
                )));
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])
                .map_err(OciError::io(format!("write {}", tmp.display())))?;
        }
        file.sync_all()
            .map_err(OciError::io(format!("sync {}", tmp.display())))?;
        Ok((Digest::from_hasher(hasher), size, tmp))
    }

    fn commit_tmp(&self, tmp: &Path, digest: &Digest) -> Result<()> {
        let dest = self.blob_path(digest);
        if dest.is_file() {
            // Content-addressed: identical bytes are already present.
            let _ = fs::remove_file(tmp);
            return Ok(());
        }
        fs::rename(tmp, &dest).map_err(OciError::io(format!("commit {}", dest.display())))
    }

    /// Opens a blob for reading.
    pub fn open_blob(&self, digest: &Digest) -> Result<File> {
        File::open(self.blob_path(digest)).map_err(|_| OciError::MissingBlob(digest.to_string()))
    }

    /// Reads a small blob fully, re-verifying its digest and the JSON size limit.
    pub fn read_json_blob(&self, digest: &Digest) -> Result<Vec<u8>> {
        let path = self.blob_path(digest);
        let meta = fs::metadata(&path).map_err(|_| OciError::MissingBlob(digest.to_string()))?;
        if meta.len() > self.limits.max_json_bytes {
            return Err(OciError::Limit(format!(
                "JSON blob {digest} is {} bytes (limit {})",
                meta.len(),
                self.limits.max_json_bytes
            )));
        }
        let bytes = fs::read(&path).map_err(OciError::io(format!("read {}", path.display())))?;
        let actual = Digest::of(&bytes);
        if &actual != digest {
            return Err(OciError::DigestMismatch {
                expected: digest.to_string(),
                actual: actual.to_string(),
            });
        }
        Ok(bytes)
    }

    /// Re-hashes a stored blob (detects on-disk corruption or tampering).
    pub fn verify_blob(&self, digest: &Digest) -> Result<()> {
        let mut file = self.open_blob(digest)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = file
                .read(&mut buf)
                .map_err(OciError::io(format!("read {digest}")))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let actual = Digest::from_hasher(hasher);
        if &actual != digest {
            return Err(OciError::DigestMismatch {
                expected: digest.to_string(),
                actual: actual.to_string(),
            });
        }
        Ok(())
    }

    fn record_path(&self, name: &str) -> PathBuf {
        self.root
            .join("images")
            .join(format!("{}.json", Digest::of(name.as_bytes()).hex()))
    }

    /// Saves (or replaces) an image record.
    pub fn save_record(&self, record: &ImageRecord) -> Result<()> {
        let path = self.record_path(&record.name);
        let json =
            serde_json::to_vec_pretty(record).map_err(|e| OciError::Malformed(e.to_string()))?;
        fs::write(&path, json).map_err(OciError::io(format!("write {}", path.display())))
    }

    /// Loads an image record by name.
    pub fn record(&self, name: &str) -> Result<ImageRecord> {
        let path = self.record_path(name);
        let bytes = fs::read(&path).map_err(|_| OciError::UnknownImage(name.to_string()))?;
        serde_json::from_slice(&bytes).map_err(|e| OciError::Malformed(e.to_string()))
    }

    /// Every image record, sorted by name.
    pub fn records(&self) -> Result<Vec<ImageRecord>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.root.join("images"))
            .map_err(OciError::io("list images"))?
            .flatten()
        {
            let bytes = fs::read(entry.path()).map_err(OciError::io("read image record"))?;
            out.push(
                serde_json::from_slice::<ImageRecord>(&bytes)
                    .map_err(|e| OciError::Malformed(e.to_string()))?,
            );
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Removes an image record (its blobs stay until [`ContentStore::gc`]).
    pub fn remove(&self, name: &str) -> Result<()> {
        fs::remove_file(self.record_path(name))
            .map_err(|_| OciError::UnknownImage(name.to_string()))
    }

    /// Removes every blob not referenced by an image record and stale temporary files.
    pub fn gc(&self) -> Result<GcReport> {
        let mut live = BTreeSet::new();
        for r in self.records()? {
            live.insert(r.manifest.digest.hex().to_string());
            live.insert(r.config.digest.hex().to_string());
            for l in &r.layers {
                live.insert(l.digest.hex().to_string());
            }
        }
        let mut report = GcReport::default();
        for entry in fs::read_dir(self.root.join("blobs/sha256"))
            .map_err(OciError::io("list blobs"))?
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().to_string();
            if live.contains(&name) {
                report.kept += 1;
                continue;
            }
            report.bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            fs::remove_file(entry.path()).map_err(OciError::io("remove blob"))?;
            report.removed += 1;
        }
        for entry in fs::read_dir(self.root.join("tmp"))
            .map_err(OciError::io("list tmp"))?
            .flatten()
        {
            let _ = fs::remove_file(entry.path());
        }
        Ok(report)
    }
}
