//! Layer-level rootfs cache. Each layer is unpacked **once**, keyed by its uncompressed digest
//! (`diff_id`, verified while unpacking), into its own directory, and its whiteouts are recorded
//! instead of applied. An image's rootfs is then *composed* by replaying the cached layers in order
//! onto an empty directory: whiteouts first, then entries. Regular files are **hard-linked** (no
//! bytes copied); directories and symlinks are recreated.
//!
//! The replay uses the same securejoin resolution as [`crate::unpack`]: parent symlinks are
//! followed inside the rootfs, so an upper layer's `lib/x.so` over a lower `lib -> usr/lib`
//! symlink lands in `usr/lib`, exactly as a full unpack would place it. The composed tree is
//! therefore identical to `unpack()`, which the tests check entry by entry.
//!
//! Hard links share inodes with the cache, so a composed rootfs must be mounted **read-only**, as
//! the OCI backend does. When the cache and the destination are on different filesystems, files are
//! copied instead.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::import::read_config;
use crate::model::ImageRecord;
use crate::store::ContentStore;
use crate::unpack::{
    LayerOps, UnpackReport, Whiteouts, ensure_dir, remove_any, secure_join, symlink,
    unpack_layer_into,
};
use crate::{OciError, Result};

static TMP: AtomicU64 = AtomicU64::new(0);

/// What materializing an image from the cache did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheReport {
    /// Layers unpacked into the cache by this call.
    pub layers_unpacked: usize,
    /// Layers already in the cache.
    pub layers_reused: usize,
    /// Unpack counters of the newly cached layers.
    pub unpack: UnpackReport,
    /// Files hard-linked into the rootfs.
    pub linked: u64,
    /// Files copied (cross-filesystem fallback).
    pub copied: u64,
    /// Directories created.
    pub dirs: u64,
    /// Symlinks created.
    pub symlinks: u64,
    /// Whiteouts applied during composition.
    pub whiteouts: u64,
}

/// A directory of unpacked layers.
#[derive(Debug, Clone)]
pub struct LayerCache {
    root: PathBuf,
}

impl LayerCache {
    /// A cache rooted at `root` (created on first use).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The cache directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn layer_dir(&self, diff_id: &crate::model::Digest) -> PathBuf {
        self.root.join(diff_id.hex())
    }

    /// Ensures every layer of `record` is cached, then composes its rootfs into `dest` (which must
    /// not exist). `prepare_layer` runs on a freshly unpacked layer before it is published (for
    /// example, to give it the ownership a user namespace needs); `owner` is applied to the
    /// directories and symlinks composition creates.
    pub fn materialize(
        &self,
        store: &ContentStore,
        record: &ImageRecord,
        dest: &Path,
        prepare_layer: &dyn Fn(&Path) -> io::Result<()>,
        owner: Option<u32>,
    ) -> Result<CacheReport> {
        if !cfg!(unix) {
            return Err(OciError::Unsupported(
                "a Linux rootfs needs a Unix host (symlinks, permissions)".into(),
            ));
        }
        if dest.exists() {
            return Err(OciError::Malformed(format!(
                "{} already exists",
                dest.display()
            )));
        }
        fs::create_dir_all(&self.root)
            .map_err(OciError::io(format!("create {}", self.root.display())))?;
        let config = read_config(store, record)?;
        let mut report = CacheReport::default();
        let mut layers = Vec::with_capacity(record.layers.len());
        for (i, layer) in record.layers.iter().enumerate() {
            let diff_id = config
                .rootfs
                .diff_ids
                .get(i)
                .ok_or_else(|| OciError::Malformed("fewer diff_ids than layers".into()))?;
            let dir = self.layer_dir(diff_id);
            if dir.join("layer.json").is_file() {
                report.layers_reused += 1;
            } else {
                self.unpack_layer(
                    store,
                    layer,
                    diff_id,
                    &dir,
                    prepare_layer,
                    &mut report.unpack,
                )?;
                report.layers_unpacked += 1;
            }
            layers.push(dir);
        }
        fs::create_dir_all(dest).map_err(OciError::io(format!("create {}", dest.display())))?;
        let composed = layers.iter().try_for_each(|dir| {
            let ops: LayerOps = serde_json::from_slice(
                &fs::read(dir.join("layer.json"))
                    .map_err(OciError::io(format!("read {}", dir.display())))?,
            )
            .map_err(|e| OciError::Malformed(format!("{}: {e}", dir.display())))?;
            compose_layer(&dir.join("rootfs"), &ops, dest, owner, &mut report)
        });
        if let Err(e) = composed {
            let _ = fs::remove_dir_all(dest);
            return Err(e);
        }
        Ok(report)
    }

    /// Unpacks one layer alone into `dir/rootfs`, records its whiteouts, publishes atomically.
    fn unpack_layer(
        &self,
        store: &ContentStore,
        layer: &crate::model::Descriptor,
        diff_id: &crate::model::Digest,
        dir: &Path,
        prepare_layer: &dyn Fn(&Path) -> io::Result<()>,
        report: &mut UnpackReport,
    ) -> Result<()> {
        let tmp = self.root.join(format!(
            "{}.tmp-{}-{}",
            diff_id.hex(),
            std::process::id(),
            TMP.fetch_add(1, Ordering::Relaxed)
        ));
        let rootfs = tmp.join("rootfs");
        fs::create_dir_all(&rootfs)
            .map_err(OciError::io(format!("create {}", rootfs.display())))?;
        let result = (|| {
            let mut recorded = LayerOps::default();
            unpack_layer_into(
                store,
                layer,
                diff_id,
                &rootfs,
                report,
                &mut Whiteouts::Record(&mut recorded),
            )?;
            let json =
                serde_json::to_vec(&recorded).map_err(|e| OciError::Malformed(e.to_string()))?;
            fs::write(tmp.join("layer.json"), json).map_err(OciError::io("write layer record"))?;
            prepare_layer(&rootfs).map_err(OciError::io("prepare layer"))
        })();
        if let Err(e) = result {
            let _ = fs::remove_dir_all(&tmp);
            return Err(e);
        }
        if fs::rename(&tmp, dir).is_err() {
            // Another preparation published the same layer first; its content is identical.
            let _ = fs::remove_dir_all(&tmp);
        }
        Ok(())
    }
}

/// Replays one cached layer onto `dest`: its whiteouts and its hard links into lower layers
/// (both refer to the layers below), then its entries in pre-order, resolving every parent inside
/// `dest` like a full unpack does.
fn compose_layer(
    layer: &Path,
    ops: &LayerOps,
    dest: &Path,
    owner: Option<u32>,
    report: &mut CacheReport,
) -> Result<()> {
    for link in &ops.links {
        let (Some((name, parent)), Some((target_name, target_parent))) =
            (link.path.split_last(), link.target.split_last())
        else {
            continue;
        };
        let source = secure_join(dest, target_parent)?.join(target_name);
        match fs::symlink_metadata(&source) {
            Ok(m) if m.is_file() => {}
            _ => {
                return Err(OciError::UnsafeEntry {
                    path: link.path.join("/"),
                    reason: "hard link target is not a regular file in the rootfs".into(),
                });
            }
        }
        let target = ensure_dir(dest, parent)?.join(name);
        remove_any(&target)?;
        fs::hard_link(&source, &target)
            .map_err(OciError::io(format!("link {}", target.display())))?;
        report.linked += 1;
    }
    for w in &ops.whiteouts {
        let dir = secure_join(dest, &w.parent)?;
        match &w.name {
            Some(hidden) => remove_any(&dir.join(hidden))?,
            None if dir.is_dir() => {
                for child in fs::read_dir(&dir)
                    .map_err(OciError::io("read opaque dir"))?
                    .flatten()
                {
                    remove_any(&child.path())?;
                }
            }
            None => {}
        }
        report.whiteouts += 1;
    }
    replay(layer, &mut Vec::new(), dest, owner, report)
}

fn replay(
    dir: &Path,
    components: &mut Vec<String>,
    dest: &Path,
    owner: Option<u32>,
    report: &mut CacheReport,
) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(OciError::io(format!("read {}", dir.display())))?
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let source = entry.path();
        let meta = fs::symlink_metadata(&source)
            .map_err(OciError::io(format!("stat {}", source.display())))?;
        components.push(name.clone());
        if meta.is_dir() {
            let made = !secure_join(dest, components)?.is_dir();
            let target = ensure_dir(dest, components)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                crate::unpack::set_mode(&target, meta.permissions().mode());
            }
            if made {
                report.dirs += 1;
                chown(&target, owner);
            }
            replay(&source, components, dest, owner, report)?;
        } else {
            let parent = ensure_dir(dest, &components[..components.len() - 1])?;
            let target = parent.join(&name);
            remove_any(&target)?;
            if meta.file_type().is_symlink() {
                let link = fs::read_link(&source)
                    .map_err(OciError::io(format!("readlink {}", source.display())))?;
                symlink(&link, &target)
                    .map_err(OciError::io(format!("symlink {}", target.display())))?;
                chown(&target, owner);
                report.symlinks += 1;
            } else if fs::hard_link(&source, &target).is_ok() {
                report.linked += 1;
            } else {
                fs::copy(&source, &target)
                    .map_err(OciError::io(format!("copy {}", target.display())))?;
                chown(&target, owner);
                report.copied += 1;
            }
        }
        components.pop();
    }
    Ok(())
}

#[cfg(unix)]
fn chown(path: &Path, owner: Option<u32>) {
    if let Some(id) = owner {
        let _ = std::os::unix::fs::lchown(path, Some(id), Some(id));
    }
}

#[cfg(not(unix))]
fn chown(_: &Path, _: Option<u32>) {}
