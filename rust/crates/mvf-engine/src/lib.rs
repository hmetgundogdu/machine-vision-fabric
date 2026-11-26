//! # mvf-engine — the single graph scheduling authority
//!
//! Binds a validated [`mvf_graph::Graph`] to node handles produced by execution backends and runs
//! it with deterministic cycle semantics (see [`executor`]).

pub mod backend;
pub mod bindings;
pub mod builtins;
mod dataset;
pub mod executor;
pub mod live;
pub mod node;
pub mod pipelined;
mod primitives;
pub mod recovery;
pub mod replace;
pub mod resolver;
pub mod services;
pub mod value;

pub use backend::{BackendRegistry, ExecutionBackend, InProcessBackend, InstantiateRequest};
pub use bindings::{Bindings, ValueSource};
pub use builtins::{Record, RecordedValue, Recorder};
pub use executor::{
    BuildError, DataPlaneReport, EndReason, Engine, NodeOutcome, NodeStats, NullObserver, Observer,
    RunOptions, RunReport, SkipReason,
};
pub use live::{IterationReport, LiveEvent, LiveEventKind};
pub use node::*;
pub use recovery::{RecoveryEvent, RecoveryKind};
pub use replace::{
    ControlHandle, NodeReference, PlannedCommand, ReplacementEvent, ReplacementEventKind,
    ReplacementRequest, UpdatePlan, parse_update_plan,
};
pub use resolver::HostResolver;
pub use services::{HostServices, LogEvent, Logs};
pub use value::*;

/// The backends this build provides by default.
pub fn default_backends() -> BackendRegistry {
    BackendRegistry::new().with(InProcessBackend)
}
