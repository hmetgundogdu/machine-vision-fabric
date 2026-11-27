//! Where management endpoints live and how clients find them.
//!
//! Each managed runtime registers itself in a per-user runtime directory: a small JSON file naming
//! its endpoint, pid and pipeline. On Unix the socket lives in the same directory. The directory
//! must be private to the user (mode 0700, owned by them) — a directory others can write to is
//! refused, because whoever controls it could plant a fake endpoint.
//!
//! - `MVF_RUNTIME_DIR` overrides the location.
//! - Unix: `$XDG_RUNTIME_DIR/mvf`, else `~/.mvf/run`.
//! - Windows: `%LOCALAPPDATA%\mvf\run` (the profile's ACL keeps it private).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A registered runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instance {
    /// Socket path (Unix) or pipe name (Windows).
    pub endpoint: String,
    /// Process id.
    pub pid: u32,
    /// Pipeline name.
    pub pipeline: String,
    /// Start time, ms since the Unix epoch.
    pub started_at_ms: u64,
    /// API version.
    pub api: u64,
    /// The registry file.
    #[serde(skip)]
    pub registry: PathBuf,
}

/// The per-user runtime directory, created private if absent.
pub fn runtime_dir() -> std::io::Result<PathBuf> {
    let dir = if let Some(d) = std::env::var_os("MVF_RUNTIME_DIR") {
        PathBuf::from(d)
    } else {
        default_dir()?
    };
    ensure_private(&dir)?;
    Ok(dir)
}

#[cfg(unix)]
fn default_dir() -> std::io::Result<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_RUNTIME_DIR").filter(|x| !x.is_empty()) {
        return Ok(PathBuf::from(x).join("mvf"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| std::io::Error::other("neither XDG_RUNTIME_DIR nor HOME is set"))?;
    Ok(PathBuf::from(home).join(".mvf/run"))
}

#[cfg(windows)]
fn default_dir() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .ok_or_else(|| std::io::Error::other("LOCALAPPDATA is not set"))?;
    Ok(PathBuf::from(base).join("mvf").join("run"))
}

/// Creates `dir` if needed and checks that only the current user can use it.
#[cfg(unix)]
pub fn ensure_private(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    if !dir.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(std::io::Error::other(format!(
            "{} is not a directory",
            dir.display()
        )));
    }
    // SAFETY: geteuid has no preconditions.
    let me = unsafe { libc::geteuid() };
    if meta.uid() != me {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "refusing runtime directory {}: owned by uid {}, not {me}",
                dir.display(),
                meta.uid()
            ),
        ));
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Creates `dir` if needed (it inherits the user profile's ACL).
#[cfg(windows)]
pub fn ensure_private(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Writes the registry file for an endpoint (atomically, private to the user).
pub(crate) fn register(dir: &Path, name: &str, instance: &Instance) -> std::io::Result<PathBuf> {
    let path = dir.join(format!("{name}.json"));
    let tmp = dir.join(format!(".{name}.json.tmp"));
    let body = serde_json::to_vec_pretty(instance).map_err(std::io::Error::other)?;
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut f = options.open(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// The runtimes registered in `dir` whose process still exists (Unix checks the pid; on Windows a
/// client finds out by connecting).
pub fn list(dir: &Path) -> Vec<Instance> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Instance> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let mut i: Instance = serde_json::from_slice(&std::fs::read(&p).ok()?).ok()?;
            i.registry = p;
            Some(i)
        })
        .filter(alive)
        .collect();
    out.sort_by_key(|i| (i.started_at_ms, i.pid));
    out
}

#[cfg(unix)]
fn alive(i: &Instance) -> bool {
    // SAFETY: signal 0 only probes existence. EPERM still means it exists.
    let r = unsafe { libc::kill(i.pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn alive(_: &Instance) -> bool {
    true
}
