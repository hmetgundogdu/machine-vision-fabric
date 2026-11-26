//! # mvf-engine — the single graph scheduling authority
//!
//! Binds a validated [`mvf_graph::Graph`] to node handles produced by execution backends and runs
//! it with deterministic cycle semantics (see [`executor`]).

pub mod backend;
pub mod builtins;
pub mod executor;
pub mod node;
pub mod resolver;
pub mod services;
pub mod value;

pub use backend::{BackendRegistry, ExecutionBackend, InProcessBackend, InstantiateRequest};
pub use builtins::{Record, RecordedValue, Recorder};
pub use executor::{
    BuildError, EndReason, Engine, NodeOutcome, NodeStats, NullObserver, Observer, RunOptions,
    RunReport, SkipReason,
};
pub use node::*;
pub use resolver::HostResolver;
pub use services::{HostServices, LogEvent, Logs};
pub use value::*;

/// The backends this build provides by default.
pub fn default_backends() -> BackendRegistry {
    BackendRegistry::new().with(InProcessBackend)
}
