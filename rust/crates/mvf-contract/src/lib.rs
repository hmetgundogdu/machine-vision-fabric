//! # mvf-contract — the Universal Node Contract
//!
//! A node is a logical workload with a versioned typed interface, executable artifacts, placement,
//! isolation, memory, lifecycle and resource policy. This crate owns that contract and nothing else:
//! it has **no host-specific dependencies** (no processes, no shared memory, no GPU), so the graph,
//! the engine and every backend can share one definition.
//!
//! - [`yaml`] — the strict, bounded YAML loader used by every MVF YAML document;
//! - [`model`] — the typed contract (`apiVersion: mvf.dev/v1alpha1`, `kind: NodeContract`);
//! - [`validate`] — semantic validation with stable error codes and field paths;
//! - [`negotiate`] — backend capability negotiation (reject, never silently weaken).
//!
//! See `docs/rust-runtime/node-contract.md` for the specification.

pub mod model;
pub mod negotiate;
pub mod validate;
pub mod yaml;

pub use model::*;
pub use negotiate::{
    BackendCapabilities, Cancellation, NegotiatedPlacement, NegotiationError, PlacementNote,
    Rejection, RejectionReason, negotiate,
};
pub use validate::{ContractIssue, validate};

/// Loading a contract failed: either the YAML was rejected or the contract is invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    /// The YAML was malformed, unsafe, or did not match the schema.
    #[error(transparent)]
    Yaml(#[from] yaml::YamlError),
    /// The contract parsed but violates semantic rules.
    #[error("invalid node contract: {}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Invalid(Vec<ContractIssue>),
}

impl ContractError {
    /// Every stable error code carried by this error.
    pub fn codes(&self) -> Vec<&'static str> {
        match self {
            ContractError::Yaml(e) => vec![e.code],
            ContractError::Invalid(issues) => issues.iter().map(|i| i.code).collect(),
        }
    }
}

/// Parses and validates a node contract from YAML text with default limits.
pub fn parse_contract(input: &str) -> Result<NodeContract, ContractError> {
    let contract: NodeContract = yaml::from_str(input, &yaml::YamlLimits::default())?;
    let issues = validate(&contract);
    if issues.is_empty() {
        Ok(contract)
    } else {
        Err(ContractError::Invalid(issues))
    }
}
