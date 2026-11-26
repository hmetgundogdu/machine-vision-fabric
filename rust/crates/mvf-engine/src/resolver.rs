//! Host contract resolver: embedded builtin contracts plus contract files inside the pipeline
//! package directory.

use std::path::{Path, PathBuf};

use mvf_contract::{ContractError, NodeContract, parse_contract, validate::is_safe_relative_path};
use mvf_graph::ContractResolver;

use crate::builtins::builtin_contract;

/// Resolves `builtin:` names and `contract:` files relative to a package directory. Contract files
/// must stay inside the package (no absolute paths, no `..`).
#[derive(Debug, Clone)]
pub struct HostResolver {
    base_dir: PathBuf,
}

impl HostResolver {
    /// Resolves files relative to `base_dir` (normally the pipeline file's directory).
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// The package directory.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }
}

impl ContractResolver for HostResolver {
    fn builtin(&self, name: &str) -> Option<NodeContract> {
        builtin_contract(name)
    }

    fn file(&self, path: &str) -> Result<(String, NodeContract), ContractError> {
        let io_error = |message: String| {
            ContractError::Yaml(mvf_contract::yaml::YamlError {
                code: "contract.file.unreadable",
                message,
                location: None,
                path: None,
            })
        };
        if !is_safe_relative_path(path) {
            return Err(io_error(format!(
                "`{path}` must be a relative path inside the pipeline package"
            )));
        }
        let full = self.base_dir.join(path);
        let text = std::fs::read_to_string(&full)
            .map_err(|e| io_error(format!("{}: {e}", full.display())))?;
        Ok((full.display().to_string(), parse_contract(&text)?))
    }
}
