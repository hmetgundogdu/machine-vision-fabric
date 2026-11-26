//! Deterministic layer and image construction — enough to package an MVF node as an OCI-profile
//! image (base image + one layer with the module, its `node.yaml` and an entrypoint) without a
//! Dockerfile engine. Timestamps are zero and entries sorted, so identical inputs give identical
//! digests.

use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;

use crate::import::read_config;
use crate::model::*;
use crate::store::ContentStore;
use crate::{OciError, Result};

/// One entry of a layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileEntry {
    /// A directory.
    Dir {
        /// Path inside the image.
        path: String,
        /// Permission bits.
        mode: u32,
    },
    /// A regular file.
    File {
        /// Path inside the image.
        path: String,
        /// Content.
        bytes: Vec<u8>,
        /// Permission bits.
        mode: u32,
    },
    /// A symbolic link.
    Symlink {
        /// Path inside the image.
        path: String,
        /// Link target (stored verbatim).
        target: String,
    },
    /// A hard link to an earlier path.
    HardLink {
        /// Path inside the image.
        path: String,
        /// Existing path.
        target: String,
    },
}

/// Builds a layer tar from entries (in the given order — tests use this to craft hostile layers).
#[derive(Debug, Default, Clone)]
pub struct LayerBuilder {
    entries: Vec<FileEntry>,
}

/// A finished layer.
#[derive(Debug, Clone)]
pub struct LayerBlob {
    /// Blob bytes as stored (gzip unless built uncompressed).
    pub bytes: Vec<u8>,
    /// Media type.
    pub media_type: String,
    /// Digest of the uncompressed tar.
    pub diff_id: Digest,
}

impl LayerBuilder {
    /// An empty layer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an entry.
    pub fn entry(mut self, e: FileEntry) -> Self {
        self.entries.push(e);
        self
    }

    /// Adds a regular file.
    pub fn file(self, path: &str, bytes: &[u8], mode: u32) -> Self {
        self.entry(FileEntry::File {
            path: path.into(),
            bytes: bytes.to_vec(),
            mode,
        })
    }

    /// Adds a directory.
    pub fn dir(self, path: &str) -> Self {
        self.entry(FileEntry::Dir {
            path: path.into(),
            mode: 0o755,
        })
    }

    /// Adds a symlink.
    pub fn symlink(self, path: &str, target: &str) -> Self {
        self.entry(FileEntry::Symlink {
            path: path.into(),
            target: target.into(),
        })
    }

    /// The uncompressed tar bytes. Paths are written verbatim (no normalization), so hostile names
    /// can be expressed for tests.
    pub fn tar_bytes(&self) -> Result<Vec<u8>> {
        let mut b = tar::Builder::new(Vec::new());
        for e in &self.entries {
            let mut h = tar::Header::new_gnu();
            h.set_mtime(0);
            h.set_uid(0);
            h.set_gid(0);
            let (path, data): (&str, &[u8]) = match e {
                FileEntry::Dir { path, mode } => {
                    h.set_entry_type(tar::EntryType::Directory);
                    h.set_mode(*mode);
                    h.set_size(0);
                    (path, &[])
                }
                FileEntry::File { path, bytes, mode } => {
                    h.set_entry_type(tar::EntryType::Regular);
                    h.set_mode(*mode);
                    h.set_size(bytes.len() as u64);
                    (path, bytes)
                }
                FileEntry::Symlink { path, target } | FileEntry::HardLink { path, target } => {
                    h.set_entry_type(if matches!(e, FileEntry::Symlink { .. }) {
                        tar::EntryType::Symlink
                    } else {
                        tar::EntryType::Link
                    });
                    h.set_mode(0o777);
                    h.set_size(0);
                    set_raw_link(&mut h, target)?;
                    (path, &[])
                }
            };
            set_raw_path(&mut h, path)?;
            h.set_cksum();
            b.append(&h, data).map_err(OciError::io("build layer"))?;
        }
        b.into_inner().map_err(OciError::io("finish layer"))
    }

    /// A gzip layer.
    pub fn gzip(&self) -> Result<LayerBlob> {
        let tar = self.tar_bytes()?;
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&tar).map_err(OciError::io("gzip layer"))?;
        Ok(LayerBlob {
            bytes: enc.finish().map_err(OciError::io("gzip layer"))?,
            media_type: media::OCI_LAYER_GZIP.into(),
            diff_id: Digest::of(&tar),
        })
    }

    /// An uncompressed layer.
    pub fn uncompressed(&self) -> Result<LayerBlob> {
        let tar = self.tar_bytes()?;
        Ok(LayerBlob {
            diff_id: Digest::of(&tar),
            bytes: tar,
            media_type: media::OCI_LAYER.into(),
        })
    }
}

fn set_raw_path(h: &mut tar::Header, path: &str) -> Result<()> {
    let name = &mut h.as_old_mut().name;
    if path.len() > name.len() {
        return Err(OciError::Limit(format!(
            "test path `{path}` longer than 100 bytes"
        )));
    }
    name.fill(0);
    name[..path.len()].copy_from_slice(path.as_bytes());
    Ok(())
}

fn set_raw_link(h: &mut tar::Header, target: &str) -> Result<()> {
    let link = &mut h.as_old_mut().linkname;
    if target.len() > link.len() {
        return Err(OciError::Limit(format!(
            "link target `{target}` longer than 100 bytes"
        )));
    }
    link.fill(0);
    link[..target.len()].copy_from_slice(target.as_bytes());
    Ok(())
}

fn put_json(store: &ContentStore, value: &serde_json::Value) -> Result<(Digest, u64)> {
    let bytes = serde_json::to_vec(value).map_err(|e| OciError::Malformed(e.to_string()))?;
    let digest = store.put_bytes(&bytes)?;
    Ok((digest, bytes.len() as u64))
}

/// Creates a new image from scratch with the given layers and execution config.
pub fn create_image(
    store: &ContentStore,
    name: &str,
    platform: &Platform,
    exec: &ExecConfig,
    layers: &[LayerBlob],
) -> Result<ImageRecord> {
    let mut config = serde_json::json!({
        "architecture": platform.architecture,
        "os": platform.os,
        "config": exec,
        "rootfs": { "type": "layers", "diff_ids": layers.iter().map(|l| l.diff_id.to_string()).collect::<Vec<_>>() },
        "history": layers.iter().map(|_| serde_json::json!({"created_by": "mvf-oci create_image"})).collect::<Vec<_>>(),
    });
    if let Some(v) = &platform.variant {
        config["variant"] = serde_json::json!(v);
    }
    finish(store, name, platform, config, None, layers)
}

/// Appends `layer` to `base`, optionally overriding execution settings, and stores the result as
/// `name`. Unknown config/manifest fields of the base are preserved.
pub fn append_layer(
    store: &ContentStore,
    base: &ImageRecord,
    name: &str,
    layer: &LayerBlob,
    exec: Option<&ExecConfig>,
) -> Result<ImageRecord> {
    let _ = read_config(store, base)?; // validates the base config
    let mut config: serde_json::Value =
        serde_json::from_slice(&store.read_json_blob(&base.config.digest)?)
            .map_err(|e| OciError::Malformed(e.to_string()))?;
    config["rootfs"]["diff_ids"]
        .as_array_mut()
        .ok_or_else(|| OciError::Malformed("base config without rootfs.diff_ids".into()))?
        .push(serde_json::json!(layer.diff_id.to_string()));
    if let Some(history) = config.get_mut("history").and_then(|h| h.as_array_mut()) {
        history.push(serde_json::json!({"created_by": "mvf-oci append_layer"}));
    }
    if let Some(exec) = exec {
        let target = config
            .as_object_mut()
            .ok_or_else(|| OciError::Malformed("config is not an object".into()))?
            .entry("config")
            .or_insert_with(|| serde_json::json!({}));
        let overrides =
            serde_json::to_value(exec).map_err(|e| OciError::Malformed(e.to_string()))?;
        if let (Some(t), Some(o)) = (target.as_object_mut(), overrides.as_object()) {
            for (k, v) in o {
                if k == "Labels" {
                    let labels = t.entry("Labels").or_insert_with(|| serde_json::json!({}));
                    if labels.is_null() {
                        *labels = serde_json::json!({});
                    }
                    if let (Some(dst), Some(src)) = (labels.as_object_mut(), v.as_object()) {
                        for (lk, lv) in src {
                            dst.insert(lk.clone(), lv.clone());
                        }
                    }
                } else {
                    t.insert(k.clone(), v.clone());
                }
            }
        }
    }
    let base_manifest: serde_json::Value =
        serde_json::from_slice(&store.read_json_blob(&base.manifest.digest)?)
            .map_err(|e| OciError::Malformed(e.to_string()))?;
    let layers = vec![layer.clone()];
    finish(
        store,
        name,
        &base.platform,
        config,
        Some((base, base_manifest)),
        &layers,
    )
}

fn finish(
    store: &ContentStore,
    name: &str,
    platform: &Platform,
    config: serde_json::Value,
    base: Option<(&ImageRecord, serde_json::Value)>,
    new_layers: &[LayerBlob],
) -> Result<ImageRecord> {
    let (config_digest, config_size) = put_json(store, &config)?;
    let mut layer_descriptors: Vec<Descriptor> = base
        .as_ref()
        .map(|(b, _)| b.layers.clone())
        .unwrap_or_default();
    for l in new_layers {
        let digest = store.put_bytes(&l.bytes)?;
        layer_descriptors.push(Descriptor {
            media_type: l.media_type.clone(),
            digest,
            size: l.bytes.len() as u64,
            platform: None,
            annotations: None,
        });
    }
    let config_descriptor = Descriptor {
        media_type: media::OCI_CONFIG.into(),
        digest: config_digest,
        size: config_size,
        platform: None,
        annotations: None,
    };
    let mut manifest = base.map(|(_, m)| m).unwrap_or_else(
        || serde_json::json!({ "schemaVersion": 2, "mediaType": media::OCI_MANIFEST }),
    );
    manifest["mediaType"] = serde_json::json!(media::OCI_MANIFEST);
    manifest["config"] =
        serde_json::to_value(&config_descriptor).map_err(|e| OciError::Malformed(e.to_string()))?;
    manifest["layers"] =
        serde_json::to_value(&layer_descriptors).map_err(|e| OciError::Malformed(e.to_string()))?;
    let (manifest_digest, manifest_size) = put_json(store, &manifest)?;
    let record = ImageRecord {
        name: name.to_string(),
        manifest: Descriptor {
            media_type: media::OCI_MANIFEST.into(),
            digest: manifest_digest,
            size: manifest_size,
            platform: None,
            annotations: None,
        },
        config: config_descriptor,
        layers: layer_descriptors,
        platform: platform.clone(),
        source: "built".into(),
    };
    store.save_record(&record)?;
    Ok(record)
}
