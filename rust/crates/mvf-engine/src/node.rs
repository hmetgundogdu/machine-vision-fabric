//! The runtime-owned node handle: the one interface every backend implements, so the graph executor
//! has no backend-specific semantics.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::value::Value;

/// Cooperative cancellation flag shared between the caller and every node.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    /// A fresh, uncancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Per-execution context.
#[derive(Debug)]
pub struct ExecContext<'a> {
    /// Cycle id (1-based, monotonic).
    pub cycle: u64,
    /// Node instance id.
    pub node_id: &'a str,
    /// Routing generation of the node implementation executing this cycle.
    pub generation: u64,
    /// Cooperative cancellation.
    pub cancel: &'a CancellationToken,
    /// Execution deadline, when the contract declares `executeTimeoutMs`.
    pub deadline: Option<Instant>,
}

/// Lifecycle context for start/drain/stop.
#[derive(Debug)]
pub struct LifecycleContext<'a> {
    /// Node instance id.
    pub node_id: &'a str,
    /// Generation being started/stopped.
    pub generation: u64,
    /// Time budget for the operation.
    pub budget: Duration,
    /// Cooperative cancellation.
    pub cancel: &'a CancellationToken,
}

/// Values on a node's connected input ports for one cycle.
#[derive(Debug, Clone, Default)]
pub struct Inputs(BTreeMap<String, Value>);

impl Inputs {
    /// Empty inputs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets an input.
    pub fn insert(&mut self, port: impl Into<String>, value: Value) {
        self.0.insert(port.into(), value);
    }

    /// The value on `port`, if present this cycle.
    pub fn get(&self, port: &str) -> Option<&Value> {
        self.0.get(port)
    }

    /// Iterates present inputs.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.0.iter()
    }
}

/// Values a node emits for one cycle. A node may leave any output absent (drop/filter).
#[derive(Debug, Clone, Default)]
pub struct Outputs(BTreeMap<String, Value>);

impl Outputs {
    /// Empty outputs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style insert.
    pub fn with(mut self, port: impl Into<String>, value: Value) -> Self {
        self.0.insert(port.into(), value);
        self
    }

    /// Sets an output.
    pub fn insert(&mut self, port: impl Into<String>, value: Value) {
        self.0.insert(port.into(), value);
    }

    /// The emitted value on `port`.
    pub fn get(&self, port: &str) -> Option<&Value> {
        self.0.get(port)
    }

    /// Consumes into (port, value) pairs.
    pub fn into_inner(self) -> BTreeMap<String, Value> {
        self.0
    }

    /// Number of emitted values.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when nothing was emitted.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Result of one execution.
#[derive(Debug)]
pub enum Execution {
    /// The node ran and emitted these outputs (possibly none).
    Produced(Outputs),
    /// A source has no more input; the run ends at this cycle boundary.
    Exhausted,
}

/// Classification of a node failure. The distinction between a managed error, a caught panic and a
/// native process fault is part of the backend contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeErrorKind {
    /// The node returned an error (managed failure).
    Failed,
    /// The node panicked; the panic was caught at the backend boundary.
    Panicked,
    /// The node's process exited or was killed (native fault, crash, OOM kill).
    ProcessExited,
    /// A start/readiness/execute/stop deadline elapsed.
    Timeout,
    /// The operation observed cancellation.
    Cancelled,
    /// The node violated the wire protocol.
    Protocol,
    /// The request is not supported by the backend or contract (fails closed).
    Unsupported,
    /// Invalid instance configuration.
    InvalidConfig,
    /// The node emitted an undeclared port or a value of the wrong type.
    InvalidOutput,
    /// A resource (arena slot, device memory) was unavailable.
    Resource,
    /// The shared-memory arena was full (bounded admission); handled by the node's backpressure
    /// policy (`stall` ends the run, `drop` skips the cycle).
    Backpressure,
    /// The node could not be started (spawn failure, readiness failure).
    StartFailed,
}

/// A node failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct NodeError {
    /// Failure class.
    pub kind: NodeErrorKind,
    /// Explanation (including the inner cause).
    pub message: String,
    /// Process exit code, for `ProcessExited`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Terminating signal (Unix), for `ProcessExited`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

impl NodeError {
    /// A failure of `kind`.
    pub fn new(kind: NodeErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            exit_code: None,
            signal: None,
        }
    }

    /// A managed failure.
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(NodeErrorKind::Failed, message)
    }

    /// Maps an arena error: a full arena is backpressure, anything else a resource failure.
    pub fn from_arena(e: &mvf_memory::ArenaError) -> Self {
        let kind = match e {
            mvf_memory::ArenaError::Full(_) => NodeErrorKind::Backpressure,
            _ => NodeErrorKind::Resource,
        };
        Self::new(kind, e.to_string())
    }
}

/// Opaque, versioned node state used for stateful replacement and recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StateBlob {
    /// State schema version (matches `lifecycle.state.schemaVersion`).
    pub schema_version: u32,
    /// Serialized state.
    pub bytes: Vec<u8>,
}

/// How a stop completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopOutcome {
    /// The node stopped within its budget.
    Graceful,
    /// The node had to be forcibly terminated.
    Forced,
    /// The node was already gone (crashed or never started).
    AlreadyStopped,
}

/// The runtime-owned handle to one node generation.
///
/// Lifecycle: `start` (activation + readiness) → `execute`* → `drain` → `stop`. Every backend
/// implements the same calls; the executor never branches on the backend kind.
pub trait NodeHandle: Send {
    /// Activates the node and waits for readiness within `ctx.budget`.
    fn start(&mut self, ctx: &LifecycleContext<'_>) -> Result<(), NodeError> {
        let _ = ctx;
        Ok(())
    }

    /// Runs one cycle.
    fn execute(&mut self, ctx: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError>;

    /// Finishes in-flight work so the node holds nothing between cycles.
    fn drain(&mut self, ctx: &LifecycleContext<'_>) -> Result<(), NodeError> {
        let _ = ctx;
        Ok(())
    }

    /// Releases every resource. Must be idempotent.
    fn stop(&mut self, ctx: &LifecycleContext<'_>) -> StopOutcome {
        let _ = ctx;
        StopOutcome::Graceful
    }

    /// Exports versioned state for stateful replacement. `Ok(None)` = stateless.
    fn export_state(&mut self) -> Result<Option<StateBlob>, NodeError> {
        Ok(None)
    }

    /// Imports state produced by a previous generation.
    fn import_state(&mut self, state: &StateBlob) -> Result<(), NodeError> {
        Err(NodeError::new(
            NodeErrorKind::Unsupported,
            format!(
                "this node cannot import state (schema version {})",
                state.schema_version
            ),
        ))
    }

    /// Called when this generation becomes the active one (live replacement cut-over, or the
    /// previous generation restored by a rollback): it takes over whatever it exposes outside the
    /// graph, such as published network listeners. An error aborts the promotion.
    fn promote(&mut self) -> Result<(), NodeError> {
        Ok(())
    }

    /// Restarts a finite stream from its beginning (the `loop` primitive's `forever` mode).
    /// `Ok(false)` = not rewindable: `forever` then degrades to `until-exhausted` for this source.
    fn rewind(&mut self) -> Result<bool, NodeError> {
        Ok(false)
    }

    /// Replaces the node's live value (`value`, `select` criterion). Called at a cycle boundary;
    /// the next cycle sees it. Must type-check before accepting.
    fn set_value(&mut self, value: &serde_json::Value) -> Result<(), NodeError> {
        let _ = value;
        Err(NodeError::new(
            NodeErrorKind::Unsupported,
            "this node has no live value",
        ))
    }

    /// Takes an edited config without reopening. `Ok(false)` = cannot (the engine then reopens the
    /// node with the new config, the .NET re-activation model); an error also reopens it.
    fn reconfigure(
        &mut self,
        config: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<bool, NodeError> {
        let _ = config;
        Ok(false)
    }
}
