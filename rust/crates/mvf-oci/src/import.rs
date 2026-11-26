//! Offline import of OCI image layouts (directory or tar) and Docker archives (`docker save`).
//! No daemon, no network: everything comes from the given path and is verified by digest.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Component, Path};

use flate2::read::GzDecoder;

use crate::model::*;
use crate::store::ContentStore;
use crate::{OciError, Result};

/// What was imported from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSource {
    /// OCI image layout (`oci-layout` + `index.json` + `blobs/`).
    OciLayout,
    /// Docker 25+ `docker save` archive (embeds an OCI layout and a `manifest.json`).
    DockerArchive,
    /// Legacy `docker save` archive (`manifest.json`, `<id>.json`, `<id>/layer.tar`).
    DockerArchiveLegacy,
}

impl ImportSource {
    fn as_str(self) -> &'static str {
        match self {
            ImportSource::OciLayout => "oci-layout",
            ImportSource::DockerArchive => "docker-archive",
            ImportSource::DockerArchiveLegacy => "docker-archive-legacy",
        }
    }
}

/// Imports an image from a directory (OCI layout) or a tar/tar.gz (OCI layout or Docker archive),
/// selecting `platform`. The record is saved under `name`, or the name found in the archive.
pub fn import(
    store: &ContentStore,
    path: &Path,
    platform: &Platform,
    name: Option<&str>,
) -> Result<ImageRecord> {
    if path.is_dir() {
        import_layout_dir(store, path, platform, name)
    } else {
        import_tar(store, path, platform, name)
    }
}

fn read_small(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let meta = std::fs::metadata(path).map_err(OciError::io(format!("stat {}", path.display())))?;
    if meta.len() > limit {
        return Err(OciError::Limit(format!(
            "{} exceeds {limit} bytes",
            path.display()
        )));
    }
    std::fs::read(path).map_err(OciError::io(format!("read {}", path.display())))
}

fn check_layout_marker(bytes: &[u8]) -> Result<()> {
    let v: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| OciError::Malformed(format!("oci-layout: {e}")))?;
    match v.get("imageLayoutVersion").and_then(|v| v.as_str()) {
        Some("1.0.0") => Ok(()),
        other => Err(OciError::Unsupported(format!(
            "imageLayoutVersion {other:?}"
        ))),
    }
}

fn import_layout_dir(
    store: &ContentStore,
    dir: &Path,
    platform: &Platform,
    name: Option<&str>,
) -> Result<ImageRecord> {
    let limit = store.limits.max_json_bytes;
    check_layout_marker(&read_small(&dir.join("oci-layout"), limit)?)?;
    let index = read_small(&dir.join("index.json"), limit)?;
    let fetch = |d: &Descriptor| -> Result<()> {
        if store.has(&d.digest) {
            return Ok(());
        }
        let path = dir.join("blobs/sha256").join(d.digest.hex());
        let file = File::open(&path).map_err(|_| OciError::MissingBlob(d.digest.to_string()))?;
        store
            .put_verified(&d.digest, Some(d.size), file)
            .map(|_| ())
    };
    resolve(
        store,
        &index,
        platform,
        name,
        ImportSource::OciLayout,
        &fetch,
    )
}

/// Entry name inside an archive, normalized; rejects traversal.
fn archive_name(raw: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for c in raw.components() {
        match c {
            Component::Normal(p) => parts.push(p.to_string_lossy().to_string()),
            Component::CurDir => {}
            _ => {
                return Err(OciError::UnsafeEntry {
                    path: raw.display().to_string(),
                    reason: "archive entries must be relative paths without `..`".into(),
                });
            }
        }
    }
    Ok(parts.join("/"))
}

fn import_tar(
    store: &ContentStore,
    path: &Path,
    platform: &Platform,
    name: Option<&str>,
) -> Result<ImageRecord> {
    let file = File::open(path).map_err(OciError::io(format!("open {}", path.display())))?;
    let mut buffered = BufReader::new(file);
    let gz = buffered
        .fill_buf()
        .map(|b| b.starts_with(&[0x1f, 0x8b]))
        .map_err(OciError::io("read archive"))?;
    let reader: Box<dyn Read> = if gz {
        Box::new(GzDecoder::new(buffered))
    } else {
        Box::new(buffered)
    };
    let mut archive = tar::Archive::new(reader);
    let limit = store.limits.max_json_bytes;

    let mut small: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut legacy_layers: BTreeMap<String, (Digest, u64)> = BTreeMap::new();
    let mut aliases: BTreeMap<String, String> = BTreeMap::new();
    let mut entries = 0u64;
    for entry in archive.entries().map_err(OciError::io("read archive"))? {
        let mut entry = entry.map_err(OciError::io("read archive entry"))?;
        entries += 1;
        if entries > store.limits.max_entries {
            return Err(OciError::Limit("too many archive entries".into()));
        }
        let raw = entry
            .path()
            .map_err(OciError::io("entry path"))?
            .to_path_buf();
        let name_in_archive = archive_name(&raw)?;
        let kind = entry.header().entry_type();
        if kind.is_symlink() {
            // Legacy `docker save` de-duplicates layers with `<id>/layer.tar -> ../<other>/layer.tar`.
            if let Some(target) = entry.link_name().map_err(OciError::io("link name"))? {
                let target = target.to_string_lossy().to_string();
                if name_in_archive.ends_with("/layer.tar")
                    && let Some(rest) = target.strip_prefix("../")
                    && !rest.contains("..")
                {
                    aliases.insert(name_in_archive, rest.to_string());
                }
            }
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        if let Some(hex) = name_in_archive.strip_prefix("blobs/sha256/") {
            let digest = Digest::parse(&format!("sha256:{hex}"))?;
            store.put_verified(&digest, Some(entry.size()), &mut entry)?;
        } else if name_in_archive.ends_with("/layer.tar") {
            let computed = store.put_computed(&mut entry)?;
            legacy_layers.insert(name_in_archive, computed);
        } else if matches!(
            name_in_archive.as_str(),
            "oci-layout" | "index.json" | "manifest.json"
        ) || (name_in_archive.ends_with(".json") && !name_in_archive.contains('/'))
        {
            if entry.size() > limit {
                return Err(OciError::Limit(format!(
                    "{name_in_archive} exceeds {limit} bytes"
                )));
            }
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry
                .read_to_end(&mut bytes)
                .map_err(OciError::io(format!("read {name_in_archive}")))?;
            small.insert(name_in_archive, bytes);
        }
    }

    let already = |d: &Descriptor| -> Result<()> {
        if store.has(&d.digest) {
            Ok(())
        } else {
            Err(OciError::MissingBlob(d.digest.to_string()))
        }
    };
    match (small.get("oci-layout"), small.get("index.json")) {
        (Some(layout), Some(index)) => {
            check_layout_marker(layout)?;
            let source = if small.contains_key("manifest.json") {
                ImportSource::DockerArchive
            } else {
                ImportSource::OciLayout
            };
            resolve(store, index, platform, name, source, &already)
        }
        _ => match small.get("manifest.json") {
            Some(manifest) => {
                import_legacy(store, manifest, &small, &legacy_layers, &aliases, platform, name)
            }
            None => Err(OciError::Malformed(
                "neither an OCI layout (oci-layout + index.json) nor a Docker archive (manifest.json)"
                    .into(),
            )),
        },
    }
}

fn parse_json<T: serde::de::DeserializeOwned>(what: &str, bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|e| OciError::Malformed(format!("{what}: {e}")))
}

fn is_index(media: &str) -> bool {
    media == media::OCI_INDEX || media == media::DOCKER_LIST
}

fn is_manifest(media: &str) -> bool {
    media == media::OCI_MANIFEST || media == media::DOCKER_MANIFEST
}

fn supported_layer(media: &str) -> Result<()> {
    match media {
        media::OCI_LAYER
        | media::OCI_LAYER_GZIP
        | media::DOCKER_LAYER_GZIP
        | media::DOCKER_LAYER => Ok(()),
        media::OCI_LAYER_ZSTD => Err(OciError::Unsupported(
            "zstd-compressed layers (use gzip or uncompressed layers)".into(),
        )),
        other => Err(OciError::Unsupported(format!("layer media type `{other}`"))),
    }
}

fn check_size(store: &ContentStore, d: &Descriptor) -> Result<()> {
    let actual = std::fs::metadata(store.blob_path(&d.digest))
        .map_err(|_| OciError::MissingBlob(d.digest.to_string()))?
        .len();
    if actual != d.size {
        return Err(OciError::SizeMismatch {
            digest: d.digest.to_string(),
            expected: d.size,
            actual,
        });
    }
    Ok(())
}

fn ref_name(d: &Descriptor) -> Option<String> {
    let a = d.annotations.as_ref()?;
    a.get("io.containerd.image.name")
        .or_else(|| a.get("org.opencontainers.image.ref.name"))
        .cloned()
}

/// Walks an index to the manifest for `platform`, fetching and verifying every blob it needs.
fn resolve(
    store: &ContentStore,
    index_bytes: &[u8],
    platform: &Platform,
    name: Option<&str>,
    source: ImportSource,
    fetch: &dyn Fn(&Descriptor) -> Result<()>,
) -> Result<ImageRecord> {
    let mut queue: Vec<(Vec<u8>, usize)> = vec![(index_bytes.to_vec(), 0)];
    let mut seen_platforms = Vec::new();
    while let Some((bytes, depth)) = queue.pop() {
        if depth > store.limits.max_index_depth {
            return Err(OciError::Limit("index nesting too deep".into()));
        }
        let index: Index = parse_json("index", &bytes)?;
        if index.schema_version != 2 {
            return Err(OciError::Unsupported(format!(
                "index schemaVersion {}",
                index.schema_version
            )));
        }
        for d in &index.manifests {
            if let Some(p) = &d.platform
                && !platform.accepts(p)
            {
                seen_platforms.push(p.to_string());
                continue;
            }
            if is_index(&d.media_type) {
                fetch(d)?;
                check_size(store, d)?;
                queue.push((store.read_json_blob(&d.digest)?, depth + 1));
                continue;
            }
            if !is_manifest(&d.media_type) {
                continue; // attestations and other artifacts
            }
            fetch(d)?;
            check_size(store, d)?;
            let manifest: Manifest = parse_json("manifest", &store.read_json_blob(&d.digest)?)?;
            if manifest.schema_version != 2 {
                return Err(OciError::Unsupported("manifest schemaVersion".into()));
            }
            if manifest.layers.len() > store.limits.max_layers {
                return Err(OciError::Limit(format!("{} layers", manifest.layers.len())));
            }
            fetch(&manifest.config)?;
            check_size(store, &manifest.config)?;
            let config: ImageConfig =
                parse_json("config", &store.read_json_blob(&manifest.config.digest)?)?;
            let image_platform = Platform {
                os: config.os.clone(),
                architecture: config.architecture.clone(),
                variant: config.variant.clone(),
            };
            if !platform.accepts(&image_platform) {
                seen_platforms.push(image_platform.to_string());
                continue;
            }
            if config.rootfs.diff_ids.len() != manifest.layers.len() {
                return Err(OciError::Malformed(format!(
                    "config lists {} diff_ids for {} layers",
                    config.rootfs.diff_ids.len(),
                    manifest.layers.len()
                )));
            }
            for layer in &manifest.layers {
                supported_layer(&layer.media_type)?;
                fetch(layer)?;
                check_size(store, layer)?;
            }
            let record = ImageRecord {
                name: name
                    .map(str::to_string)
                    .or_else(|| ref_name(d))
                    .unwrap_or_else(|| d.digest.to_string()),
                manifest: Descriptor {
                    media_type: d.media_type.clone(),
                    digest: d.digest.clone(),
                    size: d.size,
                    platform: None,
                    annotations: None,
                },
                config: manifest.config.clone(),
                layers: manifest.layers.clone(),
                platform: image_platform,
                source: source.as_str().into(),
            };
            store.save_record(&record)?;
            return Ok(record);
        }
    }
    Err(OciError::Platform(format!(
        "{platform} (archive offers: {})",
        if seen_platforms.is_empty() {
            "none".to_string()
        } else {
            seen_platforms.join(", ")
        }
    )))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct LegacyEntry {
    config: String,
    #[serde(default)]
    repo_tags: Option<Vec<String>>,
    layers: Vec<String>,
}

fn import_legacy(
    store: &ContentStore,
    manifest_json: &[u8],
    small: &BTreeMap<String, Vec<u8>>,
    layers: &BTreeMap<String, (Digest, u64)>,
    aliases: &BTreeMap<String, String>,
    platform: &Platform,
    name: Option<&str>,
) -> Result<ImageRecord> {
    let entries: Vec<LegacyEntry> = parse_json("manifest.json", manifest_json)?;
    let entry = match name {
        Some(n) => entries
            .iter()
            .find(|e| {
                e.repo_tags
                    .as_ref()
                    .is_some_and(|t| t.iter().any(|t| t == n))
            })
            .or(entries.first()),
        None => entries.first(),
    }
    .ok_or_else(|| OciError::Malformed("manifest.json lists no image".into()))?;
    let config_bytes = small
        .get(entry.config.as_str())
        .ok_or_else(|| OciError::MissingBlob(entry.config.clone()))?;
    // Legacy config files are named after their digest; verify when they are.
    if let Some(hex) = entry.config.strip_suffix(".json")
        && hex.len() == 64
    {
        let expected = Digest::parse(&format!("sha256:{hex}"))?;
        let actual = Digest::of(config_bytes);
        if actual != expected {
            return Err(OciError::DigestMismatch {
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
    }
    let config: ImageConfig = parse_json("config", config_bytes)?;
    let image_platform = Platform {
        os: config.os.clone(),
        architecture: config.architecture.clone(),
        variant: config.variant.clone(),
    };
    if !platform.accepts(&image_platform) {
        return Err(OciError::Platform(format!(
            "{platform} (archive offers {image_platform})"
        )));
    }
    if config.rootfs.diff_ids.len() != entry.layers.len() {
        return Err(OciError::Malformed(
            "diff_ids do not match the layer list".into(),
        ));
    }
    let config_digest = store.put_bytes(config_bytes)?;
    let mut descriptors = Vec::new();
    for (i, path) in entry.layers.iter().enumerate() {
        let resolved = aliases.get(path).unwrap_or(path);
        let (digest, size) = layers
            .get(resolved)
            .ok_or_else(|| OciError::MissingBlob(path.clone()))?;
        // An uncompressed layer's digest is its diff id; it must match the config.
        if digest != &config.rootfs.diff_ids[i] {
            return Err(OciError::DigestMismatch {
                expected: config.rootfs.diff_ids[i].to_string(),
                actual: digest.to_string(),
            });
        }
        descriptors.push(Descriptor {
            media_type: media::OCI_LAYER.into(),
            digest: digest.clone(),
            size: *size,
            platform: None,
            annotations: None,
        });
    }
    let config_descriptor = Descriptor {
        media_type: media::OCI_CONFIG.into(),
        digest: config_digest,
        size: config_bytes.len() as u64,
        platform: None,
        annotations: None,
    };
    let manifest = Manifest {
        schema_version: 2,
        media_type: Some(media::OCI_MANIFEST.into()),
        config: config_descriptor.clone(),
        layers: descriptors.clone(),
        annotations: None,
    };
    let manifest_bytes =
        serde_json::to_vec(&manifest).map_err(|e| OciError::Malformed(e.to_string()))?;
    let manifest_digest = store.put_bytes(&manifest_bytes)?;
    let record = ImageRecord {
        name: name
            .map(str::to_string)
            .or_else(|| entry.repo_tags.as_ref().and_then(|t| t.first().cloned()))
            .unwrap_or_else(|| manifest_digest.to_string()),
        manifest: Descriptor {
            media_type: media::OCI_MANIFEST.into(),
            digest: manifest_digest,
            size: manifest_bytes.len() as u64,
            platform: None,
            annotations: None,
        },
        config: config_descriptor,
        layers: descriptors,
        platform: image_platform,
        source: ImportSource::DockerArchiveLegacy.as_str().into(),
    };
    store.save_record(&record)?;
    Ok(record)
}

/// Reads and parses the image config of a stored image.
pub fn read_config(store: &ContentStore, record: &ImageRecord) -> Result<ImageConfig> {
    parse_json("config", &store.read_json_blob(&record.config.digest)?)
}
