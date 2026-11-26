//! Engine-side surface allocation and the rendezvous directory display hosts watch.
//!
//! Layout (per user, never world-accessible):
//!
//! ```text
//! <root>/run-<pid>-<nanos>/owner                 engine pid
//! <root>/run-<pid>-<nanos>/<node>.<k>.surface    shared memory (+ .writer.lock, .host.lock)
//! <root>/run-<pid>-<nanos>/<node>.<k>.json       descriptor: title, size, input grants
//! ```
//!
//! Surfaces belong to the **pipeline node**, not to a generation: a restarted or replacing
//! generation gets the same files, so its window stays open. The engine removes its run directory
//! when it stops; a run whose engine died is swept by the next engine.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::alive::process_alive;
use crate::surface::{self, SurfaceError};

/// Input routed to the node's windows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Grants {
    /// Keyboard events.
    pub keyboard: bool,
    /// Pointer and wheel events.
    pub mouse: bool,
}

/// What a node's surfaces should look like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceRequest {
    /// Window title.
    pub title: String,
    /// Initial content width (logical pixels).
    pub width: u32,
    /// Initial content height.
    pub height: u32,
    /// Largest frame width.
    pub max_width: u32,
    /// Largest frame height.
    pub max_height: u32,
    /// Windows (surfaces) to allocate.
    pub windows: u32,
    /// Routed input.
    pub grants: Grants,
}

/// A surface descriptor, written by the engine. A host trusts only these fields and the surface
/// header it validates itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    /// Descriptor version (1).
    pub version: u32,
    /// Pipeline node id.
    pub node: String,
    /// Window index within the node.
    pub index: u32,
    /// Window title.
    pub title: String,
    /// Initial content width.
    pub width: u32,
    /// Initial content height.
    pub height: u32,
    /// Surface file name, relative to the run directory.
    pub surface: String,
    /// Routed input.
    pub grants: Grants,
    /// Engine process id.
    pub engine_pid: u32,
}

/// A surface a host found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// Surface file.
    pub surface: PathBuf,
    /// Its descriptor.
    pub descriptor: Descriptor,
}

#[derive(Debug, Default)]
struct Run {
    dir: Option<PathBuf>,
    nodes: HashMap<String, Vec<PathBuf>>,
}

/// Engine-scoped surface allocation. Creates nothing until a node asks for a surface.
#[derive(Debug)]
pub struct DisplayRegistry {
    root: PathBuf,
    run: Mutex<Run>,
}

impl DisplayRegistry {
    /// A registry rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            run: Mutex::new(Run::default()),
        }
    }

    /// `MVF_DISPLAY_DIR`, or the per-user default.
    pub fn from_env() -> Self {
        Self::new(
            std::env::var_os("MVF_DISPLAY_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(default_root),
        )
    }

    /// The rendezvous directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// This engine's run directory, once a surface exists.
    pub fn run_dir(&self) -> Option<PathBuf> {
        self.run.lock().ok().and_then(|r| r.dir.clone())
    }

    /// The surfaces of `node`, created on first request (later generations get the same files).
    pub fn surfaces(
        &self,
        node: &str,
        request: &SurfaceRequest,
    ) -> Result<Vec<PathBuf>, SurfaceError> {
        let mut run = self
            .run
            .lock()
            .map_err(|_| SurfaceError::Frame("display registry poisoned".into()))?;
        if let Some(existing) = run.nodes.get(node) {
            return Ok(existing.clone());
        }
        let dir = match &run.dir {
            Some(d) => d.clone(),
            None => {
                let d = self.create_run()?;
                run.dir = Some(d.clone());
                d
            }
        };
        let stem = file_stem(node);
        let mut paths = Vec::new();
        for index in 0..request.windows.max(1) {
            let name = format!("{stem}.{index}.surface");
            let path = dir.join(&name);
            surface::create(&path, request.max_width, request.max_height, index)?;
            let descriptor = Descriptor {
                version: 1,
                node: node.to_string(),
                index,
                title: if request.windows > 1 {
                    format!("{} ({})", request.title, index + 1)
                } else {
                    request.title.clone()
                },
                width: request.width,
                height: request.height,
                surface: name,
                grants: request.grants,
                engine_pid: std::process::id(),
            };
            write_atomic(
                &dir.join(format!("{stem}.{index}.json")),
                &serde_json::to_vec_pretty(&descriptor).unwrap_or_default(),
            )?;
            paths.push(path);
        }
        run.nodes.insert(node.to_string(), paths.clone());
        Ok(paths)
    }

    fn create_run(&self) -> Result<PathBuf, SurfaceError> {
        ensure_private_dir(&self.root)?;
        sweep(&self.root);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir = self
            .root
            .join(format!("run-{}-{nanos:09}", std::process::id()));
        ensure_private_dir(&dir)?;
        write_atomic(
            &dir.join("owner"),
            std::process::id().to_string().as_bytes(),
        )?;
        Ok(dir)
    }

    /// Removes this engine's surfaces and descriptors. Idempotent.
    pub fn release(&self) {
        if let Ok(mut run) = self.run.lock()
            && let Some(dir) = run.dir.take()
        {
            run.nodes.clear();
            remove_run(&dir);
        }
    }
}

impl Default for DisplayRegistry {
    /// [`DisplayRegistry::from_env`]: only computes the directory; nothing is created until a node
    /// asks for a surface.
    fn default() -> Self {
        Self::from_env()
    }
}

impl Drop for DisplayRegistry {
    fn drop(&mut self) {
        self.release();
    }
}

/// The per-user default rendezvous directory.
pub fn default_root() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("mvf").join("display"))
            .unwrap_or_else(|| std::env::temp_dir().join("mvf-display"))
    }
    #[cfg(unix)]
    {
        let uid = {
            // SAFETY: getuid cannot fail.
            unsafe { libc::getuid() }
        };
        if cfg!(target_os = "linux")
            && let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())
        {
            return PathBuf::from(runtime).join("mvf-display");
        }
        // macOS: TMPDIR is per user. Elsewhere /tmp with the uid in the name.
        if cfg!(target_os = "macos")
            && let Some(tmp) = std::env::var_os("TMPDIR").filter(|v| !v.is_empty())
        {
            return PathBuf::from(tmp).join("mvf-display");
        }
        PathBuf::from(format!("/tmp/mvf-display-{uid}"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::env::temp_dir().join("mvf-display")
    }
}

fn file_stem(node: &str) -> String {
    node.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> SurfaceError + '_ {
    move |source| SurfaceError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Creates `dir` (and parents) readable only by this user; refuses a directory another user owns
/// or others may write.
fn ensure_private_dir(dir: &Path) -> Result<(), SurfaceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        if !dir.exists() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(io(dir))?;
        }
        let meta = fs::metadata(dir).map_err(io(dir))?;
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        if meta.uid() != uid || meta.permissions().mode() & 0o022 != 0 {
            return Err(SurfaceError::Invalid {
                path: dir.to_path_buf(),
                reason: "display directory must be owned by this user and not writable by others"
                    .into(),
            });
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir).map_err(io(dir))
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), SurfaceError> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(io(&tmp))?;
    fs::rename(&tmp, path).map_err(io(path))
}

fn owner_of(run: &Path) -> Option<u32> {
    fs::read_to_string(run.join("owner"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn remove_run(dir: &Path) {
    // Descriptors first, so hosts stop presenting before the surfaces disappear.
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "json") {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    // A host on Windows may still map a surface for a moment; the next sweep finishes the job.
    let _ = fs::remove_dir_all(dir);
}

/// Removes run directories whose engine is gone. Returns how many were removed.
pub fn sweep(root: &Path) -> usize {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() || !e.file_name().to_string_lossy().starts_with("run-") {
            continue;
        }
        let dead = match owner_of(&path) {
            Some(pid) => pid != std::process::id() && !process_alive(pid),
            None => true,
        };
        if dead {
            remove_run(&path);
            removed += 1;
        }
    }
    removed
}

/// Every surface of every live run under `root` (what a display host presents).
pub fn discover(root: &Path) -> Vec<Discovered> {
    let mut found = Vec::new();
    let Ok(runs) = fs::read_dir(root) else {
        return found;
    };
    for run in runs.flatten() {
        let dir = run.path();
        if !dir.is_dir() || owner_of(&dir).is_none_or(|pid| !process_alive(pid)) {
            continue;
        }
        let Ok(files) = fs::read_dir(&dir) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let Some(descriptor) = fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Descriptor>(&b).ok())
            else {
                continue;
            };
            // The surface must be a plain file inside this run directory.
            let name = Path::new(&descriptor.surface);
            if descriptor.version != 1
                || name.components().count() != 1
                || name.file_name().is_none()
            {
                continue;
            }
            found.push(Discovered {
                surface: dir.join(name),
                descriptor,
            });
        }
    }
    found.sort_by(|a, b| a.surface.cmp(&b.surface));
    found
}
