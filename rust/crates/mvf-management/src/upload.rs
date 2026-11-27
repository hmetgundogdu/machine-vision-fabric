//! Uploading a prepared node package (#33 part 3): chunked, resumable, digest-checked, then staged
//! in one atomic step. Activation is the ordinary `replace` command naming the staged contract,
//! so it goes through the engine's validation, cut-over and rollback like any replacement.
//!
//! ```text
//! <staging>/.partial/<package>/<path>.part        bytes received so far
//! <staging>/.partial/<package>/<path>.part.json   its declared size and digest
//! <staging>/.partial/<package>/<path>             a file whose digest matched
//! <staging>/<package>/                            a staged package (immutable), with manifest.json
//! ```
//!
//! - **Integrity.** Every file declares its SHA-256 up front, and it is checked when the last
//!   byte arrives and again at staging. A mismatch discards the file.
//! - **Resumable.** An interrupted transfer continues from `upload-status`'s `received`, also
//!   after a reconnect or a restart. A chunk the runtime already has is acknowledged as a
//!   duplicate and not written twice.
//! - **Bounded.** Package and path names are restricted (no `..`, no absolute paths, nothing
//!   hidden), and file size, package size and file count are capped.
//! - **Immutable once staged.** Staging the same package again with the same manifest is a no-op;
//!   a different manifest under the same name is refused. A new version gets a new name.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Where uploads go, and how big they may be.
#[derive(Debug, Clone)]
pub struct UploadOptions {
    /// The staging directory (the CLI uses `<pipeline dir>/.mvf/staged`).
    pub staging: PathBuf,
    /// How `replace` names a staged package, relative to the pipeline (`.mvf/staged`).
    pub relative: String,
    /// Largest file (default 256 MiB).
    pub max_file: u64,
    /// Largest package (default 1 GiB).
    pub max_package: u64,
    /// Most files per package (default 64).
    pub max_files: usize,
}

impl UploadOptions {
    /// Defaults for a staging directory.
    pub fn new(staging: impl Into<PathBuf>, relative: impl Into<String>) -> Self {
        Self {
            staging: staging.into(),
            relative: relative.into(),
            max_file: 256 << 20,
            max_package: 1 << 30,
            max_files: 64,
        }
    }
}

/// Uploads and staging for one runtime.
#[derive(Debug)]
pub struct Uploads {
    options: UploadOptions,
    lock: Mutex<()>,
}

/// A refusal: a stable code and a message.
pub type Refused = (&'static str, String);

fn name_ok(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// A package-relative path: up to 4 segments of safe names.
fn path_ok(p: &str) -> bool {
    let parts: Vec<&str> = p.split('/').collect();
    (1..=4).contains(&parts.len()) && parts.iter().all(|s| name_ok(s))
}

/// `sha256:<hex>` of a file.
pub fn file_digest(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("sha256:{hex}"))
}

/// `sha256:<hex>` of bytes.
pub fn digest(bytes: &[u8]) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn io(e: std::io::Error) -> Refused {
    ("io", e.to_string())
}

impl Uploads {
    /// Uploads into `options.staging`, created if needed.
    pub fn new(options: UploadOptions) -> std::io::Result<Self> {
        std::fs::create_dir_all(options.staging.join(".partial"))?;
        Ok(Self {
            options,
            lock: Mutex::new(()),
        })
    }

    fn partial(&self, package: &str) -> PathBuf {
        self.options.staging.join(".partial").join(package)
    }

    fn names(&self, msg: &Value) -> Result<(String, String), Refused> {
        let package = msg["package"].as_str().unwrap_or_default();
        let path = msg["path"].as_str().unwrap_or_default();
        if !name_ok(package) {
            return Err((
                "invalid",
                format!(
                    "package {package:?}: 1-64 characters [A-Za-z0-9-_.], not starting with a dot"
                ),
            ));
        }
        if !path_ok(path) {
            return Err((
                "invalid",
                format!("path {path:?}: up to 4 segments of safe names, no `..`, nothing hidden"),
            ));
        }
        Ok((package.to_string(), path.to_string()))
    }

    /// Bytes already received for a file, and whether it is complete.
    pub fn status(&self, msg: &Value) -> Value {
        let (package, path) = match self.names(msg) {
            Ok(x) => x,
            Err((code, m)) => {
                return json!({"op": "upload-status", "status": "refused", "code": code, "message": m});
            }
        };
        // A file of a staged package is complete: re-sending the package is a no-op.
        let staged = self.options.staging.join(&package).join(&path);
        if staged.is_file() {
            let size = std::fs::metadata(&staged).map(|m| m.len()).unwrap_or(0);
            return json!({"op": "upload-status", "package": package, "path": path, "received": size, "complete": true, "staged": true});
        }
        let dir = self.partial(&package);
        let done = dir.join(&path);
        let part = dir.join(format!("{path}.part"));
        let received = if done.is_file() {
            std::fs::metadata(&done).map(|m| m.len()).unwrap_or(0)
        } else {
            std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0)
        };
        json!({"op": "upload-status", "package": package, "path": path, "received": received, "complete": done.is_file()})
    }

    /// Takes one chunk: `{package, path, size, digest, offset, data (base64)}`.
    pub fn upload(&self, msg: &Value) -> Value {
        let reply = |status: &str| json!({"op": "upload", "package": msg["package"], "path": msg["path"], "status": status});
        let refuse = |(code, message): Refused| {
            let mut r = reply("refused");
            r["code"] = json!(code);
            r["message"] = json!(message);
            r
        };
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let (package, path) = match self.names(msg) {
            Ok(x) => x,
            Err(e) => return refuse(e),
        };
        let (Some(size), Some(declared), Some(offset), Some(data)) = (
            msg["size"].as_u64(),
            msg["digest"]
                .as_str()
                .filter(|d| d.starts_with("sha256:") && d.len() == 71),
            msg["offset"].as_u64(),
            msg["data"].as_str(),
        ) else {
            return refuse((
                "invalid",
                "an upload chunk needs size, digest (sha256:…), offset and data".into(),
            ));
        };
        if size > self.options.max_file {
            return refuse((
                "too-large",
                format!("a file is at most {} bytes", self.options.max_file),
            ));
        }
        let Some(bytes) = crate::preview::unbase64(data) else {
            return refuse(("invalid", "data is not base64".into()));
        };
        let dir = self.partial(&package);
        let done = dir.join(&path);
        let part = dir.join(format!("{path}.part"));
        let meta = dir.join(format!("{path}.part.json"));
        if done.is_file() || self.options.staging.join(&package).join(&path).is_file() {
            let mut r = reply("complete");
            r["duplicate"] = json!(true);
            r["received"] = json!(size);
            return r;
        }
        let declaration = json!({"size": size, "digest": declared});
        let known: Option<Value> = std::fs::read(&meta)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        if known.as_ref().is_some_and(|k| *k != declaration) {
            if offset != 0 {
                return refuse((
                    "conflict",
                    "this file was declared with another size or digest; restart it at offset 0"
                        .into(),
                ));
            }
            let _ = std::fs::remove_file(&part);
        }
        if known.is_none() || known.as_ref() != Some(&declaration) {
            if let Err(e) = self.admit(&package, &path, size) {
                return refuse(e);
            }
            if let Some(d) = part.parent()
                && let Err(e) = std::fs::create_dir_all(d)
            {
                return refuse(io(e));
            }
            if let Err(e) = std::fs::write(&meta, declaration.to_string()) {
                return refuse(io(e));
            }
        }
        let received = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let end = offset + bytes.len() as u64;
        if offset < received && end <= received {
            let mut r = reply("partial");
            r["duplicate"] = json!(true);
            r["received"] = json!(received);
            return r;
        }
        if offset != received {
            let mut r = refuse(("offset-mismatch", format!("expected offset {received}")));
            r["received"] = json!(received);
            return r;
        }
        if end > size {
            return refuse((
                "too-large",
                format!("the chunk ends at {end}, past the declared size {size}"),
            ));
        }
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part)
            .and_then(|mut f| {
                f.write_all(&bytes)?;
                if end == size {
                    f.sync_all()?;
                }
                Ok(())
            });
        if let Err(e) = written {
            return refuse(io(e));
        }
        if end < size {
            let mut r = reply("partial");
            r["received"] = json!(end);
            return r;
        }
        // The last byte: the file must be exactly what was declared.
        let actual = match file_digest(&part) {
            Ok(d) => d,
            Err(e) => return refuse(io(e)),
        };
        let _ = std::fs::remove_file(&meta);
        if actual != declared {
            let _ = std::fs::remove_file(&part);
            let mut r = reply("rejected");
            r["code"] = json!("digest-mismatch");
            r["message"] = json!(format!(
                "declared {declared}, received {actual}; the file was discarded"
            ));
            return r;
        }
        if let Err(e) = std::fs::rename(&part, &done) {
            return refuse(io(e));
        }
        let mut r = reply("complete");
        r["received"] = json!(size);
        r["digest"] = json!(actual);
        r
    }

    /// Checks the package's limits before a new file is accepted.
    fn admit(&self, package: &str, path: &str, size: u64) -> Result<(), Refused> {
        let mut files = 0usize;
        let mut bytes = 0u64;
        for entry in walk(&self.partial(package)) {
            let name = entry.to_string_lossy();
            if name.ends_with(".part.json") {
                files += 1;
                bytes += std::fs::read(&entry)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                    .and_then(|v| v["size"].as_u64())
                    .unwrap_or(0);
            } else if !name.ends_with(".part") && !entry.ends_with(path) {
                files += 1;
                bytes += std::fs::metadata(&entry).map(|m| m.len()).unwrap_or(0);
            }
        }
        if files + 1 > self.options.max_files {
            return Err((
                "too-large",
                format!("a package has at most {} files", self.options.max_files),
            ));
        }
        if bytes + size > self.options.max_package {
            return Err((
                "too-large",
                format!("a package is at most {} bytes", self.options.max_package),
            ));
        }
        Ok(())
    }

    /// Stages a complete package: `{package, files: {path: digest}}`. Returns what `replace`
    /// needs.
    pub fn stage(&self, args: &Value) -> Result<Value, Refused> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let package = args["package"].as_str().unwrap_or_default();
        if !name_ok(package) {
            return Err(("invalid", format!("package {package:?}")));
        }
        let Some(files) = args["files"].as_object().filter(|f| !f.is_empty()) else {
            return Err(("invalid", "stage needs files: {path: digest}".into()));
        };
        let staged = self.options.staging.join(package);
        let manifest = json!({"package": package, "files": files});
        if staged.is_dir() {
            let existing: Option<Value> = std::fs::read(staged.join("manifest.json"))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            return if existing.as_ref() == Some(&manifest) {
                Ok(self.staged(package, &staged, true))
            } else {
                Err((
                    "exists",
                    format!(
                        "{package} is already staged with other contents; stage a new version under a new name"
                    ),
                ))
            };
        }
        let dir = self.partial(package);
        let mut present: Vec<String> = walk(&dir)
            .into_iter()
            .filter_map(|p| {
                p.strip_prefix(&dir)
                    .ok()
                    .map(|r| r.to_string_lossy().replace('\\', "/"))
            })
            .collect();
        present.sort();
        if let Some(p) = present
            .iter()
            .find(|p| p.ends_with(".part") || p.ends_with(".part.json"))
        {
            return Err(("incomplete", format!("{p} has not been fully received")));
        }
        let mut listed: Vec<String> = files.keys().cloned().collect();
        listed.sort();
        if present != listed {
            return Err((
                "mismatch",
                format!("received {present:?}, the manifest lists {listed:?}"),
            ));
        }
        for (path, want) in files {
            if !path_ok(path) {
                return Err(("invalid", format!("path {path:?}")));
            }
            let got = file_digest(&dir.join(path)).map_err(io)?;
            if Some(got.as_str()) != want.as_str() {
                return Err((
                    "digest-mismatch",
                    format!("{path}: manifest {want}, file {got}"),
                ));
            }
        }
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
        )
        .map_err(io)?;
        std::fs::rename(&dir, &staged).map_err(io)?;
        Ok(self.staged(package, &staged, false))
    }

    fn staged(&self, package: &str, dir: &Path, already: bool) -> Value {
        let contract = dir
            .join("node.yaml")
            .is_file()
            .then(|| format!("{}/{package}/node.yaml", self.options.relative));
        json!({"package": package, "contract": contract, "alreadyStaged": already})
    }
}

/// Every file below `dir`, recursively.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// Uploads every file of `dir` as `package` through `request`, resuming each file from what the
/// runtime already has, then stages it. Returns the stage reply.
pub fn upload_dir(
    request: &mut dyn FnMut(&Value) -> std::io::Result<Value>,
    package: &str,
    dir: &Path,
    chunk: usize,
) -> std::io::Result<Value> {
    let mut files = serde_json::Map::new();
    for path in walk(dir) {
        let rel = path
            .strip_prefix(dir)
            .map_err(std::io::Error::other)?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(&path)?;
        let d = digest(&bytes);
        let status = request(&json!({"op": "upload-status", "package": package, "path": rel}))?;
        let mut offset = if status["complete"] == true {
            bytes.len()
        } else {
            status["received"].as_u64().unwrap_or(0) as usize
        };
        while offset < bytes.len()
            || (bytes.is_empty() && offset == 0 && status["complete"] != true)
        {
            let end = (offset + chunk).min(bytes.len());
            let r = request(
                &json!({"op": "upload", "package": package, "path": rel, "size": bytes.len(), "digest": d, "offset": offset, "data": crate::preview::base64(&bytes[offset..end])}),
            )?;
            match r["status"].as_str() {
                Some("partial") | Some("complete") => {}
                _ => return Err(std::io::Error::other(format!("{rel}: {r}"))),
            }
            if bytes.is_empty() {
                break;
            }
            offset = end;
        }
        files.insert(rel, json!(d));
    }
    request(
        &json!({"op": "command", "id": format!("stage-{package}"), "command": "stage", "args": {"package": package, "files": files}}),
    )
}
