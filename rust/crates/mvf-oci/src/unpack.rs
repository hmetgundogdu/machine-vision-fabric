//! Safe application of image layers to a root filesystem.
//!
//! Every entry path is resolved **inside** the rootfs: `..` is clamped at the root and symlinks in
//! parent directories are followed as if the rootfs were `/` (securejoin semantics), so no entry can
//! create, overwrite or link anything outside it. The final component is never followed: an entry
//! replaces whatever is at its path (including a symlink) instead of writing through it. Absolute
//! entry paths are rejected. OCI whiteouts (`.wh.<name>`, `.wh..wh..opq`) are applied; device nodes
//! and FIFOs are skipped (unprivileged extraction cannot create them, and they are reported).
//! File ownership is not preserved (rootless); setuid/setgid/sticky bits are dropped. Each layer's
//! uncompressed stream is hashed and must equal the config's `diff_id`. On any error the
//! destination is removed.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use sha2::{Digest as _, Sha256};

use crate::import::read_config;
use crate::model::{Digest, ImageRecord, media};
use crate::store::ContentStore;
use crate::{OciError, Result};

/// What an unpack did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnpackReport {
    /// Layers applied.
    pub layers: usize,
    /// Entries processed.
    pub entries: u64,
    /// Bytes of regular file content written.
    pub bytes: u64,
    /// Whiteouts applied.
    pub whiteouts: u64,
    /// Device nodes / FIFOs skipped.
    pub skipped_special: u64,
}

struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

/// Unpacks a stored image into `dest` (which must not exist or be empty).
pub fn unpack(store: &ContentStore, record: &ImageRecord, dest: &Path) -> Result<UnpackReport> {
    if !cfg!(unix) {
        return Err(OciError::Unsupported(
            "unpacking a Linux rootfs requires a Unix host (symlinks, permissions)".into(),
        ));
    }
    if dest.exists()
        && fs::read_dir(dest)
            .map_err(OciError::io(format!("read {}", dest.display())))?
            .next()
            .is_some()
    {
        return Err(OciError::Malformed(format!(
            "{} is not empty",
            dest.display()
        )));
    }
    fs::create_dir_all(dest).map_err(OciError::io(format!("create {}", dest.display())))?;
    let result = unpack_into(store, record, dest);
    if result.is_err() {
        let _ = fs::remove_dir_all(dest);
    }
    result
}

fn unpack_into(store: &ContentStore, record: &ImageRecord, root: &Path) -> Result<UnpackReport> {
    let config = read_config(store, record)?;
    let mut report = UnpackReport::default();
    for (i, layer) in record.layers.iter().enumerate() {
        let expected = config
            .rootfs
            .diff_ids
            .get(i)
            .ok_or_else(|| OciError::Malformed("fewer diff_ids than layers".into()))?;
        unpack_layer_into(
            store,
            layer,
            expected,
            root,
            &mut report,
            &mut Whiteouts::Apply,
        )?;
    }
    Ok(report)
}

/// A whiteout seen in a layer: `name: None` is an opaque directory marker for `parent`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Whiteout {
    /// Directory components (relative to the rootfs).
    pub parent: Vec<String>,
    /// Hidden entry, or `None` for `.wh..wh..opq`.
    pub name: Option<String>,
}

/// A hard link whose target is in a lower layer (resolved when layers are composed).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeferredLink {
    /// The link's path components.
    pub path: Vec<String>,
    /// The target's path components.
    pub target: Vec<String>,
}

/// What a layer unpacked alone could not apply: whiteouts and hard links refer to lower layers.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LayerOps {
    /// Whiteouts, in layer order.
    pub whiteouts: Vec<Whiteout>,
    /// Hard links into lower layers.
    pub links: Vec<DeferredLink>,
}

/// Applied while unpacking a whole image; recorded when a layer is unpacked alone.
pub(crate) enum Whiteouts<'a> {
    Apply,
    Record(&'a mut LayerOps),
}

/// Applies one layer onto `root`, verifying its uncompressed digest against `expected`.
pub(crate) fn unpack_layer_into(
    store: &ContentStore,
    layer: &crate::model::Descriptor,
    expected: &Digest,
    root: &Path,
    report: &mut UnpackReport,
    whiteouts: &mut Whiteouts<'_>,
) -> Result<()> {
    {
        let blob = store.open_blob(&layer.digest)?;
        let decompressed: Box<dyn Read> = match layer.media_type.as_str() {
            media::OCI_LAYER_GZIP | media::DOCKER_LAYER_GZIP => Box::new(GzDecoder::new(blob)),
            media::OCI_LAYER | media::DOCKER_LAYER => Box::new(blob),
            other => return Err(OciError::Unsupported(format!("layer media type `{other}`"))),
        };
        let mut hashing = HashingReader {
            inner: decompressed,
            hasher: Sha256::new(),
        };
        {
            let mut archive = tar::Archive::new(&mut hashing);
            for entry in archive
                .entries()
                .map_err(OciError::io(format!("read layer {}", layer.digest)))?
            {
                let entry = entry.map_err(OciError::io(format!("read layer {}", layer.digest)))?;
                apply(store, root, entry, report, whiteouts)?;
            }
        }
        // Hash any trailing padding so the digest covers the whole uncompressed stream.
        io::copy(&mut hashing, &mut io::sink()).map_err(OciError::io("drain layer"))?;
        let diff_id = Digest::from_hasher(hashing.hasher);
        if &diff_id != expected {
            return Err(OciError::DigestMismatch {
                expected: expected.to_string(),
                actual: diff_id.to_string(),
            });
        }
        report.layers += 1;
    }
    Ok(())
}

/// Normalizes an entry path into relative components; rejects absolute paths and NULs.
fn entry_components(raw: &Path) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    for c in raw.components() {
        match c {
            Component::Normal(p) => {
                let s = p.to_string_lossy().to_string();
                if s.contains('\0') {
                    return Err(unsafe_entry(raw, "NUL in path"));
                }
                parts.push(s);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(unsafe_entry(
                    raw,
                    "`..` is not allowed in layer entry paths",
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(unsafe_entry(
                    raw,
                    "absolute paths are not allowed in layers",
                ));
            }
        }
    }
    Ok(parts)
}

fn unsafe_entry(path: &Path, reason: &str) -> OciError {
    OciError::UnsafeEntry {
        path: path.display().to_string(),
        reason: reason.into(),
    }
}

/// Resolves `components` inside `root`, following symlinks in every component (absolute link
/// targets restart at `root`, `..` is clamped at `root`). Returns a path inside `root`.
pub(crate) fn secure_join(root: &Path, components: &[String]) -> Result<PathBuf> {
    let mut pending: VecDeque<String> = components.iter().cloned().collect();
    let mut resolved: Vec<String> = Vec::new();
    let mut links = 0;
    while let Some(c) = pending.pop_front() {
        match c.as_str() {
            "" | "." => {}
            ".." => {
                resolved.pop();
            }
            name => {
                let candidate: PathBuf = root.join(resolved.join("/")).join(name);
                match fs::symlink_metadata(&candidate) {
                    Ok(m) if m.file_type().is_symlink() => {
                        links += 1;
                        if links > 40 {
                            return Err(OciError::UnsafeEntry {
                                path: components.join("/"),
                                reason: "too many levels of symbolic links".into(),
                            });
                        }
                        let target = fs::read_link(&candidate)
                            .map_err(OciError::io(format!("readlink {}", candidate.display())))?;
                        if target.is_absolute() {
                            resolved.clear();
                        }
                        let mut prefix: Vec<String> = Vec::new();
                        for t in target.components() {
                            match t {
                                Component::Normal(p) => {
                                    prefix.push(p.to_string_lossy().to_string())
                                }
                                Component::ParentDir => prefix.push("..".into()),
                                _ => {}
                            }
                        }
                        for p in prefix.into_iter().rev() {
                            pending.push_front(p);
                        }
                    }
                    _ => resolved.push(name.to_string()),
                }
            }
        }
    }
    Ok(root.join(resolved.join("/")))
}

/// Creates every directory of `components` inside `root` (following in-root symlinks) and returns
/// the resolved directory. Every existing component of the resolved path is a real directory
/// (`secure_join` followed the symlinks), so creating the missing tail cannot follow a link.
pub(crate) fn ensure_dir(root: &Path, components: &[String]) -> Result<PathBuf> {
    let dir = secure_join(root, components)?;
    match fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            // A lower layer had a file here; an upper layer turns it into a directory.
            fs::remove_file(&dir).map_err(OciError::io(format!("replace {}", dir.display())))?;
            fs::create_dir(&dir).map_err(OciError::io(format!("mkdir {}", dir.display())))?;
        }
        Err(_) => {
            fs::create_dir_all(&dir).map_err(OciError::io(format!("mkdir {}", dir.display())))?;
        }
    }
    Ok(dir)
}

pub(crate) fn remove_any(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => {
            fs::remove_dir_all(path).map_err(OciError::io(format!("remove {}", path.display())))
        }
        Ok(_) => fs::remove_file(path).map_err(OciError::io(format!("remove {}", path.display()))),
        Err(_) => Ok(()),
    }
}

#[cfg(unix)]
pub(crate) fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    // Drop setuid/setgid/sticky: an unprivileged unpack must not create privileged binaries.
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777));
}

#[cfg(not(unix))]
pub(crate) fn set_mode(_: &Path, _: u32) {}

#[cfg(unix)]
pub(crate) fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
pub(crate) fn symlink(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::other("symlinks need a Unix host"))
}

fn apply<R: Read>(
    store: &ContentStore,
    root: &Path,
    mut entry: tar::Entry<'_, R>,
    report: &mut UnpackReport,
    whiteouts: &mut Whiteouts<'_>,
) -> Result<()> {
    let limits = store.limits;
    report.entries += 1;
    if report.entries > limits.max_entries {
        return Err(OciError::Limit(format!(
            "more than {} entries",
            limits.max_entries
        )));
    }
    let raw = entry
        .path()
        .map_err(OciError::io("entry path"))?
        .to_path_buf();
    if raw.as_os_str().len() > limits.max_path_len {
        return Err(OciError::Limit(format!(
            "entry path longer than {}",
            limits.max_path_len
        )));
    }
    let components = entry_components(&raw)?;
    let Some((name, parent)) = components.split_last() else {
        return Ok(()); // the root itself
    };
    let kind = entry.header().entry_type();
    let mode = entry.header().mode().unwrap_or(0o644);

    // Whiteouts: applied to the layers below, or recorded when the layer is unpacked alone.
    if let Whiteouts::Record(ops) = whiteouts
        && (name == ".wh..wh..opq" || name.starts_with(".wh."))
    {
        ops.whiteouts.push(Whiteout {
            parent: parent.to_vec(),
            name: name
                .strip_prefix(".wh.")
                .filter(|n| *n != ".wh..opq")
                .map(str::to_string),
        });
        report.whiteouts += 1;
        return Ok(());
    }
    if name == ".wh..wh..opq" {
        let dir = secure_join(root, parent)?;
        if dir.is_dir() {
            for child in fs::read_dir(&dir)
                .map_err(OciError::io("read opaque dir"))?
                .flatten()
            {
                remove_any(&child.path())?;
            }
        }
        report.whiteouts += 1;
        return Ok(());
    }
    if let Some(hidden) = name.strip_prefix(".wh.") {
        let dir = secure_join(root, parent)?;
        remove_any(&dir.join(hidden))?;
        report.whiteouts += 1;
        return Ok(());
    }

    if kind.is_dir() {
        let dir = ensure_dir(root, &components)?;
        set_mode(&dir, mode);
        return Ok(());
    }

    let parent_dir = ensure_dir(root, parent)?;
    let target = parent_dir.join(name);
    if kind.is_file() || kind == tar::EntryType::Continuous {
        report.bytes += entry.size();
        if report.bytes > limits.max_unpacked_bytes {
            return Err(OciError::Limit(format!(
                "rootfs exceeds {} bytes",
                limits.max_unpacked_bytes
            )));
        }
        remove_any(&target)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(OciError::io(format!("create {}", target.display())))?;
        io::copy(&mut entry, &mut file)
            .map_err(OciError::io(format!("write {}", target.display())))?;
        set_mode(&target, mode);
    } else if kind.is_symlink() {
        let link = entry
            .link_name()
            .map_err(OciError::io("symlink target"))?
            .ok_or_else(|| unsafe_entry(&raw, "symlink without target"))?
            .to_path_buf();
        remove_any(&target)?;
        // Stored verbatim; it is only ever resolved by `secure_join`, which keeps it inside root.
        symlink(&link, &target).map_err(OciError::io(format!("symlink {}", target.display())))?;
    } else if kind.is_hard_link() {
        let link = entry
            .link_name()
            .map_err(OciError::io("hardlink target"))?
            .ok_or_else(|| unsafe_entry(&raw, "hard link without target"))?
            .to_path_buf();
        let link_components = entry_components(&link)?;
        let Some((link_name, link_parent)) = link_components.split_last() else {
            return Err(unsafe_entry(&raw, "hard link to the root"));
        };
        let source = secure_join(root, link_parent)?.join(link_name);
        match fs::symlink_metadata(&source) {
            Ok(m) if m.is_file() => {}
            _ if matches!(whiteouts, Whiteouts::Record(_)) => {
                // Unpacked alone: the target may be in a lower layer; resolved at composition.
                if let Whiteouts::Record(ops) = whiteouts {
                    ops.links.push(DeferredLink {
                        path: components.clone(),
                        target: link_components.clone(),
                    });
                }
                return Ok(());
            }
            _ => {
                return Err(unsafe_entry(
                    &raw,
                    "hard link target is not a regular file in the rootfs",
                ));
            }
        }
        remove_any(&target)?;
        fs::hard_link(&source, &target)
            .map_err(OciError::io(format!("link {}", target.display())))?;
    } else if matches!(
        kind,
        tar::EntryType::Char | tar::EntryType::Block | tar::EntryType::Fifo
    ) {
        report.skipped_special += 1;
    }
    Ok(())
}
