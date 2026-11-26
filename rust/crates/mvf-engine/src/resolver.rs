//! Host contract resolver: embedded builtin contracts, contract files inside the pipeline package
//! directory, and installed modules (`<module root>/<dir>/node.yaml`) found by contract id — the Rust
//! counterpart of the .NET engine's `"module": "py.…"` references and `integrations/` discovery.

use std::path::{Path, PathBuf};

use mvf_contract::{
    ContractError, ContractIssue, NodeContract, parse_contract, validate::is_safe_relative_path,
};
use mvf_graph::ContractResolver;

use crate::builtins::builtin_contract;

/// File name of a module's contract inside its directory.
pub const MODULE_CONTRACT_FILE: &str = "node.yaml";

/// Resolves `builtin:` names, `contract:` files relative to a package directory, and `module:` ids
/// in module roots. Contract files must stay inside the package (no absolute paths, no `..`).
#[derive(Debug, Clone)]
pub struct HostResolver {
    base_dir: PathBuf,
    module_roots: Vec<PathBuf>,
}

fn unreadable(code: &'static str, path: &str, message: String) -> ContractError {
    ContractError::Invalid(vec![ContractIssue {
        code,
        path: path.to_string(),
        message,
    }])
}

impl HostResolver {
    /// Resolves files relative to `base_dir` (normally the pipeline file's directory).
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            module_roots: Vec::new(),
        }
    }

    /// Adds a module root searched for `<dir>/node.yaml`.
    pub fn with_module_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.module_roots.push(root.into());
        self
    }

    /// The package directory.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Module roots.
    pub fn module_roots(&self) -> &[PathBuf] {
        &self.module_roots
    }

    /// Walks up from `start` (at most `levels`) and returns the first `modules/` directory that
    /// contains at least one module contract.
    pub fn discover_module_root(start: &Path, levels: usize) -> Option<PathBuf> {
        let start = std::fs::canonicalize(start).ok()?;
        let mut dir = Some(start.as_path());
        for _ in 0..=levels {
            let d = dir?;
            let candidate = d.join("modules");
            if candidate.is_dir()
                && std::fs::read_dir(&candidate)
                    .ok()?
                    .flatten()
                    .any(|e| e.path().join(MODULE_CONTRACT_FILE).is_file())
            {
                return Some(candidate);
            }
            dir = d.parent();
        }
        None
    }
}

impl ContractResolver for HostResolver {
    fn builtin(&self, name: &str) -> Option<NodeContract> {
        builtin_contract(name)
    }

    fn file(&self, path: &str) -> Result<(String, NodeContract), ContractError> {
        if !is_safe_relative_path(path) {
            return Err(unreadable(
                "contract.file.unsafe-path",
                path,
                format!("`{path}` must be a relative path inside the pipeline package"),
            ));
        }
        let full = self.base_dir.join(path);
        let text = std::fs::read_to_string(&full).map_err(|e| {
            unreadable(
                "contract.file.unreadable",
                path,
                format!("{}: {e}", full.display()),
            )
        })?;
        Ok((full.display().to_string(), parse_contract(&text)?))
    }

    fn module(&self, id: &str) -> Result<(String, NodeContract), ContractError> {
        for root in &self.module_roots {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            dirs.sort();
            for dir in dirs {
                let file = dir.join(MODULE_CONTRACT_FILE);
                let Ok(text) = std::fs::read_to_string(&file) else {
                    continue;
                };
                // A broken contract of some *other* module must not break this lookup; a broken
                // contract of the requested module is reported.
                match parse_contract(&text) {
                    Ok(c) if c.metadata.id == id => return Ok((file.display().to_string(), c)),
                    Ok(_) => {}
                    Err(e) if text.contains(id) => return Err(e),
                    Err(_) => {}
                }
            }
        }
        Err(unreadable(
            "graph.module.not-found",
            id,
            format!(
                "no `{MODULE_CONTRACT_FILE}` with id `{id}` in module roots {:?}",
                self.module_roots
            ),
        ))
    }
}
