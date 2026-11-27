//! MVF Storage Fabric (#31): volumes for nodes, with ordinary file I/O.
//!
//! MVF owns the declaration, binding, authorization, provisioning, lifecycle and accounting of a
//! node's filesystem; the operating system does every read and write. There is no file proxy and
//! no file API: a node opens `MVF_VOLUME_<NAME>` (a worker, a host path) or its mount target (an
//! OCI node, a kernel bind or tmpfs) with `open()`, `pathlib`, `std::fs` or OpenCV.
//!
//! ```text
//! <root>/volumes/<pipeline>/<volume>/data/        persistent and shared volumes (retained)
//! <root>/volumes/<pipeline>/<volume>/volume.json  type, writers, creation time
//! <root>/artifacts/blobs/sha256/<hex>             verified, read-only, deduplicated content
//! <root>/runs/run-<pid>-<n>/owner                 the engine that provisioned what follows
//! <root>/runs/run-<pid>-<n>/views/<node>-...       per-generation artifact views (pinned)
//! <root>/runs/run-<pid>-<n>/scratch/<node>-...     worker scratch (removed on stop, swept on crash)
//! ```
//!
//! Persistent data is never removed by a node stopping, being replaced or the engine exiting; only
//! an explicit [`delete_volume`] removes it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mvf_contract::{BoundMount, MountAccess, MountKind, VolumeKind};
use mvf_oci::{ContentStore, Digest};
use serde::Serialize;

/// Storage errors. Every one fails the node's start closed.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// I/O.
    #[error("storage {path}: {source}")]
    Io {
        /// Path.
        path: PathBuf,
        /// Cause.
        source: std::io::Error,
    },
    /// A host path or artifact path that is not allowed.
    #[error("storage: {0}")]
    Unsafe(String),
    /// An artifact that does not match its pinned digest.
    #[error("artifact `{volume}`: {message}")]
    Artifact {
        /// Volume.
        volume: String,
        /// What went wrong.
        message: String,
    },
    /// Not enough free space for a declared scratch size.
    #[error("storage: {0}")]
    NoSpace(String),
    /// A volume that exists with another type.
    #[error("storage: {0}")]
    Conflict(String),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> StorageError + '_ {
    move |source| StorageError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Where a provisioned mount comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Source {
    /// An MVF-managed persistent volume.
    Persistent,
    /// An MVF-managed shared volume.
    Shared,
    /// An operator-approved host directory.
    Bind,
    /// A digest-pinned artifact.
    Artifact {
        /// `sha256:...`.
        digest: String,
    },
    /// Scratch space.
    Scratch {
        /// Size limit in bytes.
        max_bytes: u64,
    },
}

/// One mount, ready for a backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provisioned {
    /// Logical mount name.
    pub name: String,
    /// Path inside a sandbox.
    pub target: String,
    /// Environment variable that carries the node's path to it.
    pub env: String,
    /// Host directory; `None` for scratch a sandbox provides itself (tmpfs).
    pub host_path: Option<PathBuf>,
    /// Read-only access.
    pub read_only: bool,
    /// Provenance.
    pub source: Source,
    /// Volume name (`None` for scratch).
    pub volume: Option<String>,
}

impl Provisioned {
    /// One line for the operator.
    pub fn describe(&self) -> String {
        let what = match &self.source {
            Source::Persistent => format!(
                "persistent volume `{}`",
                self.volume.as_deref().unwrap_or("")
            ),
            Source::Shared => format!("shared volume `{}`", self.volume.as_deref().unwrap_or("")),
            Source::Bind => format!(
                "host directory {}",
                self.host_path
                    .as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ),
            Source::Artifact { digest } => format!(
                "artifact `{}` {}",
                self.volume.as_deref().unwrap_or(""),
                &digest[..digest.len().min(19)]
            ),
            Source::Scratch { max_bytes } => format!("scratch {} MiB", max_bytes >> 20),
        };
        format!(
            "{} -> {what} ({})",
            self.name,
            if self.read_only {
                "read-only"
            } else {
                "read-write"
            }
        )
    }
}

/// How scratch space is provided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scratch {
    /// A host directory (workers): the size limit is advisory.
    HostDir,
    /// The sandbox mounts a size-limited tmpfs: the limit is hard.
    Tmpfs,
}

/// The mounts of one generation. Dropping it (or [`Grant::release`]) removes what was provisioned
/// for this generation only — scratch and artifact views — never volume data.
#[derive(Debug, Default)]
pub struct Grant {
    /// The mounts.
    pub mounts: Vec<Provisioned>,
    owned: Vec<PathBuf>,
    scratch: Vec<(String, PathBuf, u64)>,
}

impl Grant {
    /// Removes this generation's scratch and views. Returns operator notes (advisory quota
    /// overruns). Idempotent.
    pub fn release(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        for (name, dir, max) in self.scratch.drain(..) {
            let used = usage(&dir).0;
            if used > max {
                notes.push(format!(
                    "storage: scratch `{name}` used {} MiB, over its advisory {} MiB (not enforced for this backend)",
                    used >> 20,
                    max >> 20
                ));
            }
        }
        for dir in self.owned.drain(..) {
            make_writable(&dir);
            let _ = fs::remove_dir_all(&dir);
        }
        notes
    }

    /// Environment for a process that sees host paths (workers).
    pub fn host_env(&self) -> Vec<(String, std::ffi::OsString)> {
        self.mounts
            .iter()
            .filter_map(|m| {
                m.host_path
                    .as_ref()
                    .map(|p| (m.env.clone(), p.clone().into_os_string()))
            })
            .collect()
    }
}

impl Drop for Grant {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Debug, Default)]
struct State {
    run: Option<PathBuf>,
    next: u64,
}

/// Engine-scoped storage provisioning. Creates nothing until a node with mounts starts.
#[derive(Debug)]
pub struct StorageService {
    root: PathBuf,
    state: Mutex<State>,
}

impl Default for StorageService {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Drop for StorageService {
    fn drop(&mut self) {
        if let Ok(mut s) = self.state.lock()
            && let Some(run) = s.run.take()
        {
            make_writable(&run);
            let _ = fs::remove_dir_all(run);
        }
    }
}

/// The environment variable for a logical mount.
pub fn env_name(mount: &str) -> String {
    format!(
        "MVF_VOLUME_{}",
        mount.to_ascii_uppercase().replace('-', "_")
    )
}

impl StorageService {
    /// A service rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            state: Mutex::new(State::default()),
        }
    }

    /// `MVF_STORAGE_DIR`, or the per-user default.
    pub fn from_env() -> Self {
        Self::new(
            std::env::var_os("MVF_STORAGE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(default_root),
        )
    }

    /// The storage root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn run_dir(&self) -> Result<(PathBuf, u64), StorageError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| StorageError::Conflict("storage state poisoned".into()))?;
        let run = match &s.run {
            Some(r) => r.clone(),
            None => {
                let runs = self.root.join("runs");
                fs::create_dir_all(&runs).map_err(io(&runs))?;
                sweep(&self.root);
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos())
                    .unwrap_or(0);
                let r = runs.join(format!("run-{}-{nanos:09}", std::process::id()));
                fs::create_dir_all(&r).map_err(io(&r))?;
                fs::write(r.join("owner"), std::process::id().to_string()).map_err(io(&r))?;
                s.run = Some(r.clone());
                r
            }
        };
        s.next += 1;
        Ok((run, s.next))
    }

    /// Provisions `mounts` for one generation of `node`. `package_dir` resolves artifact files;
    /// `owner` is the uid a sandbox runs as (managed directories are given to it).
    pub fn provision(
        &self,
        node: &str,
        generation: u64,
        mounts: &[BoundMount],
        package_dir: Option<&Path>,
        scratch: Scratch,
        owner: Option<u32>,
    ) -> Result<Grant, StorageError> {
        let mut grant = Grant::default();
        for b in mounts {
            let m = &b.mount;
            let read_only = b.access == MountAccess::ReadOnly;
            let env = env_name(&m.name);
            let p = match (&b.volume, m.kind) {
                (None, _) | (_, MountKind::Scratch) => {
                    let max = m
                        .max_size
                        .as_deref()
                        .and_then(mvf_contract::parse_size)
                        .unwrap_or(0);
                    let host_path = match scratch {
                        Scratch::Tmpfs => None,
                        Scratch::HostDir => {
                            let (run, n) = self.run_dir()?;
                            let dir = run.join("scratch").join(format!(
                                "{}-g{generation}-{n}-{}",
                                file_stem(node),
                                m.name
                            ));
                            fs::create_dir_all(&dir).map_err(io(&dir))?;
                            let free = free_bytes(&dir).unwrap_or(u64::MAX);
                            if free < max {
                                let _ = fs::remove_dir_all(&dir);
                                return Err(StorageError::NoSpace(format!(
                                    "scratch `{}` needs {} MiB, {} MiB free under {}",
                                    m.name,
                                    max >> 20,
                                    free >> 20,
                                    self.root.display()
                                )));
                            }
                            grant.owned.push(dir.clone());
                            grant.scratch.push((m.name.clone(), dir.clone(), max));
                            Some(dir)
                        }
                    };
                    Provisioned {
                        name: m.name.clone(),
                        target: m.target.clone(),
                        env,
                        host_path,
                        read_only: false,
                        source: Source::Scratch { max_bytes: max },
                        volume: None,
                    }
                }
                (Some(v), _) => {
                    let (host_path, source) = match v.kind {
                        VolumeKind::Persistent | VolumeKind::Shared => (
                            self.managed(&b.scope, v, owner)?,
                            if v.kind == VolumeKind::Shared {
                                Source::Shared
                            } else {
                                Source::Persistent
                            },
                        ),
                        VolumeKind::Bind => (bind_path(v, read_only, owner)?, Source::Bind),
                        VolumeKind::Artifact => {
                            let (run, n) = self.run_dir()?;
                            let view = run.join("views").join(format!(
                                "{}-g{generation}-{n}-{}",
                                file_stem(node),
                                m.name
                            ));
                            let digest = self.artifact(v, package_dir, &view)?;
                            grant.owned.push(view.clone());
                            (view, Source::Artifact { digest })
                        }
                    };
                    Provisioned {
                        name: m.name.clone(),
                        target: m.target.clone(),
                        env,
                        host_path: Some(host_path),
                        read_only,
                        source,
                        volume: Some(v.name.clone()),
                    }
                }
            };
            grant.mounts.push(p);
        }
        Ok(grant)
    }

    /// The data directory of a persistent or shared volume, created on first use and retained.
    fn managed(
        &self,
        scope: &str,
        v: &mvf_contract::VolumeDecl,
        owner: Option<u32>,
    ) -> Result<PathBuf, StorageError> {
        let dir = self
            .root
            .join("volumes")
            .join(file_stem(scope))
            .join(&v.name);
        let data = dir.join("data");
        let meta = dir.join("volume.json");
        if let Ok(existing) = fs::read(&meta) {
            let info: serde_json::Value = serde_json::from_slice(&existing).unwrap_or_default();
            if info["type"] != v.kind.as_str() {
                return Err(StorageError::Conflict(format!(
                    "volume `{scope}/{}` exists as `{}`, the pipeline declares `{}`; delete it explicitly to change its type",
                    v.name,
                    info["type"].as_str().unwrap_or("?"),
                    v.kind.as_str()
                )));
            }
        } else {
            fs::create_dir_all(&data).map_err(io(&data))?;
            let info = serde_json::json!({
                "scope": scope,
                "name": v.name,
                "type": v.kind.as_str(),
                "writers": v.writers,
                "createdMs": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
            });
            fs::write(&meta, serde_json::to_vec_pretty(&info).unwrap_or_default())
                .map_err(io(&meta))?;
        }
        fs::create_dir_all(&data).map_err(io(&data))?;
        #[cfg(unix)]
        if let Some(uid) = owner {
            std::os::unix::fs::chown(&data, Some(uid), Some(uid)).map_err(io(&data))?;
        }
        #[cfg(not(unix))]
        let _ = owner;
        Ok(data)
    }

    /// Imports (once, verified) and pins an artifact into a per-generation read-only view.
    fn artifact(
        &self,
        v: &mvf_contract::VolumeDecl,
        package_dir: Option<&Path>,
        view: &Path,
    ) -> Result<String, StorageError> {
        let fail = |message: String| StorageError::Artifact {
            volume: v.name.clone(),
            message,
        };
        let (Some(file), Some(digest)) = (&v.file, &v.digest) else {
            return Err(fail("needs `file` and `digest`".into()));
        };
        let digest = Digest::parse(digest).map_err(|e| fail(e.to_string()))?;
        let store_dir = self.root.join("artifacts");
        let store = ContentStore::open(&store_dir).map_err(|e| fail(e.to_string()))?;
        if !store.has(&digest) {
            let package = package_dir.ok_or_else(|| {
                fail("artifact files resolve against the pipeline package; none is known".into())
            })?;
            if !mvf_contract::is_safe_relative_path(file) {
                return Err(fail(format!(
                    "`{file}` is not a relative path inside the package"
                )));
            }
            let src = package.join(file);
            let meta = fs::symlink_metadata(&src).map_err(io(&src))?;
            if !meta.file_type().is_file() {
                return Err(fail(format!(
                    "`{}` is not a regular file (symlinks are not followed)",
                    src.display()
                )));
            }
            let canonical_pkg = fs::canonicalize(package).map_err(io(package))?;
            let canonical = fs::canonicalize(&src).map_err(io(&src))?;
            if !canonical.starts_with(&canonical_pkg) {
                return Err(fail(format!("`{file}` resolves outside the package")));
            }
            let reader = fs::File::open(&src).map_err(io(&src))?;
            store
                .put_verified(&digest, Some(meta.len()), reader)
                .map_err(|e| fail(e.to_string()))?;
        } else if std::env::var_os("MVF_STORAGE_VERIFY").is_some_and(|v| v == "always") {
            // Verified once on import (the blob is read-only and content-addressed); re-hashing on
            // every start is opt-in because it costs a full read of the model.
            store
                .verify_blob(&digest)
                .map_err(|e| fail(e.to_string()))?;
        }
        let blob = store.blob_path(&digest);
        set_read_only(&blob);
        let name = Path::new(file)
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "artifact".into());
        fs::create_dir_all(view).map_err(io(view))?;
        let link = view.join(&name);
        if fs::hard_link(&blob, &link).is_err() {
            fs::copy(&blob, &link).map_err(io(&link))?;
            set_read_only(&link);
        }
        set_read_only(view);
        Ok(digest.to_string())
    }
}

/// Validates an operator-approved host directory (exists, no symlink on the way, not a system or
/// home directory) and, for a read-write sandbox mount, that the sandbox uid can write it.
fn bind_path(
    v: &mvf_contract::VolumeDecl,
    read_only: bool,
    owner: Option<u32>,
) -> Result<PathBuf, StorageError> {
    let raw = v
        .host_path
        .as_deref()
        .ok_or_else(|| StorageError::Unsafe(format!("bind volume `{}` has no hostPath", v.name)))?;
    if let Some(why) = mvf_contract::host_path_issue(raw) {
        return Err(StorageError::Unsafe(why));
    }
    let path = PathBuf::from(raw);
    let canonical = fs::canonicalize(&path).map_err(io(&path))?;
    if !canonical.is_dir() {
        return Err(StorageError::Unsafe(format!("`{raw}` is not a directory")));
    }
    if strip_verbatim(&canonical) != strip_verbatim(&path) {
        return Err(StorageError::Unsafe(format!(
            "`{raw}` resolves through a symlink to `{}`; bind the real directory",
            canonical.display()
        )));
    }
    if let Some(why) = mvf_contract::host_path_issue(&strip_verbatim(&canonical).to_string_lossy())
    {
        return Err(StorageError::Unsafe(why));
    }
    for home in ["HOME", "USERPROFILE"] {
        if let Some(h) = std::env::var_os(home).filter(|h| !h.is_empty())
            && fs::canonicalize(&h).is_ok_and(|h| h == canonical)
        {
            return Err(StorageError::Unsafe(format!(
                "`{raw}` is a home directory; bind a directory inside it instead"
            )));
        }
    }
    #[cfg(unix)]
    if !read_only && let Some(uid) = owner {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(&canonical).map_err(io(&canonical))?;
        let writable = meta.uid() == uid || meta.mode() & 0o002 != 0;
        if !writable {
            return Err(StorageError::Unsafe(format!(
                "the sandbox runs as uid {uid} and cannot write `{raw}` (owner uid {}); give it the directory or bind it read-only",
                meta.uid()
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = (read_only, owner);
    Ok(canonical)
}

fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_string())
}

fn file_stem(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn set_read_only(p: &Path) {
    if let Ok(meta) = fs::metadata(p) {
        let mut perm = meta.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perm.set_mode(if meta.is_dir() { 0o555 } else { 0o444 });
        }
        #[cfg(not(unix))]
        perm.set_readonly(true);
        let _ = fs::set_permissions(p, perm);
    }
}

/// Makes a tree writable again so it can be removed (read-only views).
fn make_writable(p: &Path) {
    let Ok(meta) = fs::symlink_metadata(p) else {
        return;
    };
    if meta.file_type().is_symlink() {
        return;
    }
    if meta.is_dir() {
        let mut perm = meta.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perm.set_mode(0o755);
        }
        #[cfg(not(unix))]
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        let _ = fs::set_permissions(p, perm);
        if let Ok(entries) = fs::read_dir(p) {
            for e in entries.flatten() {
                make_writable(&e.path());
            }
        }
    } else if cfg!(windows) {
        let mut perm = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perm.set_readonly(false);
        let _ = fs::set_permissions(p, perm);
    }
}

/// Bytes and files under `dir` (symlinks are not followed).
pub fn usage(dir: &Path) -> (u64, u64) {
    let mut total = (0u64, 0u64);
    let Ok(entries) = fs::read_dir(dir) else {
        return total;
    };
    for e in entries.flatten() {
        let Ok(meta) = fs::symlink_metadata(e.path()) else {
            continue;
        };
        if meta.is_dir() {
            let (b, f) = usage(&e.path());
            total.0 += b;
            total.1 += f;
        } else {
            total.0 += meta.len();
            total.1 += 1;
        }
    }
    total
}

/// Free bytes on the filesystem holding `path`.
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid C string and out-pointer.
    (unsafe { libc::statvfs(c.as_ptr(), &mut s) } == 0)
        .then(|| s.f_bavail as u64 * s.f_frsize as u64)
}

/// Free bytes on the volume holding `path`.
#[cfg(windows)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    // SAFETY: valid wide string; the other out-pointers may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

/// Free bytes (unknown platform).
#[cfg(not(any(unix, windows)))]
pub fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks existence.
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: plain Win32 calls; the handle is closed.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// Removes per-run directories (scratch, views) whose engine died. Volume data is untouched.
pub fn sweep(root: &Path) -> usize {
    let Ok(runs) = fs::read_dir(root.join("runs")) else {
        return 0;
    };
    let mut removed = 0;
    for r in runs.flatten() {
        let dir = r.path();
        let owner = fs::read_to_string(dir.join("owner"))
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok());
        if owner.is_none_or(|pid| pid != std::process::id() && !process_alive(pid)) {
            make_writable(&dir);
            if fs::remove_dir_all(&dir).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// The per-user default storage root.
pub fn default_root() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("mvf").join("storage"))
            .unwrap_or_else(|| std::env::temp_dir().join("mvf-storage"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Application Support/mvf/storage"))
            .unwrap_or_else(|| std::env::temp_dir().join("mvf-storage"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // SAFETY: getuid cannot fail.
        if unsafe { libc::getuid() } == 0 {
            return PathBuf::from("/var/lib/mvf/storage");
        }
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .map(|d| d.join("mvf/storage"))
            .unwrap_or_else(|| std::env::temp_dir().join("mvf-storage"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::env::temp_dir().join("mvf-storage")
    }
}

/// A retained volume, for `mvf volume list|inspect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeInfo {
    /// Pipeline scope.
    pub scope: String,
    /// Volume name.
    pub name: String,
    /// `persistent` or `shared`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Data directory.
    pub path: PathBuf,
    /// Bytes used.
    pub bytes: u64,
    /// Files.
    pub files: u64,
    /// Shared volume writers.
    pub writers: Vec<String>,
    /// Creation time (Unix ms).
    pub created_ms: u64,
}

/// Every retained volume under `root`.
pub fn list_volumes(root: &Path) -> Vec<VolumeInfo> {
    let mut out = Vec::new();
    let Ok(scopes) = fs::read_dir(root.join("volumes")) else {
        return out;
    };
    for scope in scopes.flatten() {
        let Ok(volumes) = fs::read_dir(scope.path()) else {
            continue;
        };
        for v in volumes.flatten() {
            let dir = v.path();
            let Ok(meta) = fs::read(dir.join("volume.json")) else {
                continue;
            };
            let info: serde_json::Value = serde_json::from_slice(&meta).unwrap_or_default();
            let (bytes, files) = usage(&dir.join("data"));
            out.push(VolumeInfo {
                scope: info["scope"].as_str().unwrap_or_default().to_string(),
                name: info["name"].as_str().unwrap_or_default().to_string(),
                kind: info["type"].as_str().unwrap_or_default().to_string(),
                path: dir.join("data"),
                bytes,
                files,
                writers: info["writers"]
                    .as_array()
                    .map(|w| {
                        w.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
                created_ms: info["createdMs"].as_u64().unwrap_or(0),
            });
        }
    }
    out.sort_by(|a, b| (&a.scope, &a.name).cmp(&(&b.scope, &b.name)));
    out
}

/// Deletes a retained volume and its data (the only way data is ever removed). Returns the bytes
/// freed.
pub fn delete_volume(root: &Path, scope: &str, name: &str) -> Result<u64, StorageError> {
    let info = list_volumes(root)
        .into_iter()
        .find(|v| v.scope == scope && v.name == name)
        .ok_or_else(|| StorageError::Conflict(format!("no volume `{scope}/{name}`")))?;
    let dir = info
        .path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| info.path.clone());
    make_writable(&dir);
    fs::remove_dir_all(&dir).map_err(io(&dir))?;
    Ok(info.bytes)
}

/// Pinned artifacts in the store: `(digest, bytes)`.
pub fn list_artifacts(root: &Path) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = fs::read_dir(root.join("artifacts/blobs/sha256"))
        .map(|d| {
            d.flatten()
                .filter_map(|e| {
                    let len = e.metadata().ok()?.len();
                    Some((format!("sha256:{}", e.file_name().to_string_lossy()), len))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}
