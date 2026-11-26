//! Execution backends. A backend turns a negotiated artifact into a [`NodeHandle`]; the executor
//! only ever sees handles, so no backend-specific semantics leak into the graph.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use mvf_contract::{
    Artifact, BackendCapabilities, BackendKind, Cancellation, IsolationLevel, MemoryDomain,
    NegotiatedPlacement, NegotiationError, NodeContract, Replacement, Target,
};
use mvf_graph::ContractOrigin;

use crate::builtins::{HostServices, instantiate_builtin};
use crate::node::*;

/// Everything a backend needs to instantiate one node generation.
#[derive(Debug)]
pub struct InstantiateRequest<'a> {
    /// Node instance id.
    pub node_id: &'a str,
    /// The node's contract.
    pub contract: &'a NodeContract,
    /// Where the contract came from (file contracts resolve artifact paths against it).
    pub origin: &'a ContractOrigin,
    /// Instance config.
    pub config: &'a serde_json::Map<String, serde_json::Value>,
    /// Generation being created.
    pub generation: u64,
    /// Host services.
    pub services: &'a HostServices,
}

/// One execution backend.
pub trait ExecutionBackend: Send + Sync + std::fmt::Debug {
    /// What this backend can do **on this host**.
    fn capabilities(&self) -> BackendCapabilities;

    /// Creates a (not yet started) handle for `artifact`.
    fn instantiate(
        &self,
        request: &InstantiateRequest<'_>,
        artifact: &Artifact,
    ) -> Result<Box<dyn NodeHandle>, NodeError>;
}

/// The backends available on this host.
#[derive(Debug, Clone, Default)]
pub struct BackendRegistry {
    backends: Vec<Arc<dyn ExecutionBackend>>,
}

impl BackendRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a backend.
    pub fn with(mut self, backend: impl ExecutionBackend + 'static) -> Self {
        self.backends.push(Arc::new(backend));
        self
    }

    /// Capabilities of every registered backend.
    pub fn capabilities(&self) -> Vec<BackendCapabilities> {
        self.backends.iter().map(|b| b.capabilities()).collect()
    }

    /// Negotiates a placement for `contract`.
    pub fn negotiate(
        &self,
        contract: &NodeContract,
    ) -> Result<NegotiatedPlacement, NegotiationError> {
        mvf_contract::negotiate(contract, &self.capabilities())
    }

    /// Negotiates and instantiates. Unsupported requirements fail closed.
    pub fn instantiate(
        &self,
        request: &InstantiateRequest<'_>,
    ) -> Result<(Box<dyn NodeHandle>, BackendKind), NodeError> {
        let placement = self
            .negotiate(request.contract)
            .map_err(|e| NodeError::new(NodeErrorKind::Unsupported, e.to_string()))?;
        let backend = self
            .backends
            .iter()
            .find(|b| b.capabilities().kind == placement.backend)
            .ok_or_else(|| {
                NodeError::new(NodeErrorKind::Unsupported, "negotiated backend vanished")
            })?;
        let artifact = &request.contract.spec.artifacts[placement.artifact_index];
        Ok((backend.instantiate(request, artifact)?, placement.backend))
    }
}

/// Trusted in-process backend for builtins compiled into the host.
///
/// Isolation `none`: the node shares the engine's failure and security domain. A Rust panic is caught
/// at this boundary and reported as [`NodeErrorKind::Panicked`]; a native fault (abort, segfault in
/// FFI) or an infinite loop is **not** containable here — that is what the worker backend is for.
#[derive(Debug, Default, Clone, Copy)]
pub struct InProcessBackend;

impl ExecutionBackend for InProcessBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            kind: BackendKind::InProcess,
            isolation: IsolationLevel::None,
            targets: vec![Target::Cpu],
            zero_copy_domains: vec![MemoryDomain::Host],
            cancellation: Cancellation::Cooperative,
            enforceable_termination: false,
            enforces_memory_limit: false,
            enforces_cpu_limit: false,
            enforces_pids_limit: false,
            replacement: vec![Replacement::Stateless, Replacement::Stateful],
            grants: Default::default(),
        }
    }

    fn instantiate(
        &self,
        request: &InstantiateRequest<'_>,
        artifact: &Artifact,
    ) -> Result<Box<dyn NodeHandle>, NodeError> {
        let Artifact::InProcess { builtin, .. } = artifact else {
            return Err(NodeError::new(
                NodeErrorKind::Unsupported,
                "in-process backend received a non in-process artifact",
            ));
        };
        let inner = instantiate_builtin(builtin, request.config, request.services)?;
        Ok(Box::new(PanicGuard {
            inner,
            poisoned: false,
        }))
    }
}

/// Catches panics at the in-process backend boundary and poisons the handle afterwards (its state
/// may be inconsistent, so it never runs again).
struct PanicGuard {
    inner: Box<dyn NodeHandle>,
    poisoned: bool,
}

impl PanicGuard {
    fn guard<T>(
        &mut self,
        call: impl FnOnce(&mut dyn NodeHandle) -> Result<T, NodeError>,
    ) -> Result<T, NodeError> {
        if self.poisoned {
            return Err(NodeError::new(
                NodeErrorKind::Panicked,
                "node is poisoned by an earlier panic",
            ));
        }
        let inner = &mut *self.inner;
        match catch_unwind(AssertUnwindSafe(|| call(inner))) {
            Ok(result) => result,
            Err(payload) => {
                self.poisoned = true;
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "non-string panic payload".into());
                Err(NodeError::new(NodeErrorKind::Panicked, message))
            }
        }
    }
}

impl NodeHandle for PanicGuard {
    fn start(&mut self, ctx: &LifecycleContext<'_>) -> Result<(), NodeError> {
        self.guard(|n| n.start(ctx))
    }

    fn execute(&mut self, ctx: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        self.guard(|n| n.execute(ctx, inputs))
    }

    fn drain(&mut self, ctx: &LifecycleContext<'_>) -> Result<(), NodeError> {
        self.guard(|n| n.drain(ctx))
    }

    fn stop(&mut self, ctx: &LifecycleContext<'_>) -> StopOutcome {
        if self.poisoned {
            return StopOutcome::AlreadyStopped;
        }
        self.guard(|n| Ok(n.stop(ctx)))
            .unwrap_or(StopOutcome::AlreadyStopped)
    }

    fn export_state(&mut self) -> Result<Option<StateBlob>, NodeError> {
        self.guard(|n| n.export_state())
    }

    fn import_state(&mut self, state: &StateBlob) -> Result<(), NodeError> {
        self.guard(|n| n.import_state(state))
    }
}
