//! Generation-aware live node replacement.
//!
//! Protocol (one node at a time, single scheduling authority):
//! 1. **Request** — a [`ControlHandle`] sends a [`ReplacementRequest`]. At the next cycle boundary
//!    the engine validates it (node exists, not a source, replaceable, same replacement mode, state
//!    schema compatible, port compatible, backend negotiable) or rejects it with a stable code.
//! 2. **Prepare** — the candidate generation is instantiated and started (readiness) on a background
//!    thread while the active generation keeps serving cycles. At most two generations of a node
//!    exist at once.
//! 3. **Cut over** — at the first cycle boundary after the candidate is ready, the active generation
//!    is drained (nothing is in flight between serial cycles), state is migrated when the node is
//!    stateful, and routing switches atomically to the candidate's generation. Cut-over time is
//!    measured separately from preparation time.
//! 4. **Probation** — the previous generation stays warm for `probationCycles`. A failure of the
//!    candidate during probation **rolls back**: routing returns to the previous generation and the
//!    failed cycle is re-executed on it with the same inputs, so no frame is lost or duplicated in a
//!    lossless graph. Side effects the candidate already caused cannot be undone.
//! 5. **Commit** — after probation (or on request) the previous generation is stopped.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mvf_contract::{Channel, ContractError, NodeContract, Replacement, yaml};
use mvf_graph::{ContractOrigin, ContractResolver};
use serde::{Deserialize, Serialize};

use crate::backend::{BackendRegistry, InstantiateRequest};
use crate::executor::Engine;
use crate::node::*;
use crate::services::HostServices;

/// A request to replace one node's implementation.
#[derive(Debug, Clone)]
pub struct ReplacementRequest {
    /// Pipeline node id.
    pub node: String,
    /// Candidate contract.
    pub contract: NodeContract,
    /// Where the candidate contract came from.
    pub origin: ContractOrigin,
    /// Candidate config.
    pub config: serde_json::Map<String, serde_json::Value>,
    /// Cycles the previous generation stays warm for rollback (0 = commit at cut-over).
    pub probation_cycles: u64,
    /// Rebinds the candidate's mounts (mount -> pipeline volume), e.g. to a new model artifact;
    /// `None` keeps the active generation's bindings. The previous generation keeps its own, so a
    /// rollback returns to exactly the files it had.
    pub storage: Option<std::collections::BTreeMap<String, String>>,
}

/// How a candidate implementation is referenced (as in a pipeline node).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NodeReference {
    /// A host builtin.
    Builtin(String),
    /// A contract file relative to the resolver's base directory.
    Contract(String),
    /// An installed module by id.
    Module(String),
}

impl ReplacementRequest {
    /// Resolves `reference` with `resolver` into a request.
    pub fn resolve(
        resolver: &dyn ContractResolver,
        node: &str,
        reference: &NodeReference,
        config: serde_json::Map<String, serde_json::Value>,
        probation_cycles: u64,
    ) -> Result<Self, ContractError> {
        let (origin, contract) = match reference {
            NodeReference::Builtin(name) => (
                ContractOrigin::Builtin(name.clone()),
                resolver.builtin(name).ok_or_else(|| {
                    ContractError::Invalid(vec![mvf_contract::ContractIssue {
                        code: "replace.unknown-builtin",
                        path: name.clone(),
                        message: format!("this host has no builtin `{name}`"),
                    }])
                })?,
            ),
            NodeReference::Contract(path) => {
                let (p, c) = resolver.file(path)?;
                (ContractOrigin::File(p), c)
            }
            NodeReference::Module(id) => {
                let (p, c) = resolver.module(id)?;
                (ContractOrigin::File(p), c)
            }
        };
        Ok(Self {
            node: node.to_string(),
            contract,
            origin,
            config,
            probation_cycles,
            storage: None,
        })
    }

    /// Rebinds the candidate's mounts to other pipeline volumes.
    pub fn with_storage(mut self, storage: std::collections::BTreeMap<String, String>) -> Self {
        self.storage = Some(storage);
        self
    }
}

/// Commands accepted at cycle boundaries.
#[derive(Debug)]
pub enum ControlCommand {
    /// Stage and promote a candidate.
    Replace(Box<ReplacementRequest>),
    /// Return to the previous generation (only during probation).
    Rollback {
        /// Node id.
        node: String,
    },
    /// Stop the previous generation now.
    Commit {
        /// Node id.
        node: String,
    },
    /// Set a `value` node's value or a `select`'s criterion.
    SetValue {
        /// Node id.
        node: String,
        /// The new value (type-checked before it is accepted).
        value: serde_json::Value,
    },
    /// Edit a module config field declared in the node's `bindings`.
    Configure {
        /// Node id.
        node: String,
        /// Config field.
        field: String,
        /// The new value (type-checked before it is accepted).
        value: serde_json::Value,
    },
    /// Pause the whole graph (requires a `loop`); resumes by itself after `for_ms` when given.
    Pause {
        /// Automatic resume after this many milliseconds.
        for_ms: Option<u64>,
    },
    /// Resume a paused graph.
    Resume,
}

/// Cloneable, thread-safe handle to send commands to a running engine.
#[derive(Debug, Clone)]
pub struct ControlHandle {
    tx: Sender<ControlCommand>,
}

impl ControlHandle {
    pub(crate) fn new(tx: Sender<ControlCommand>) -> Self {
        Self { tx }
    }

    /// Requests a replacement; false if the engine is gone.
    pub fn replace(&self, request: ReplacementRequest) -> bool {
        self.tx
            .send(ControlCommand::Replace(Box::new(request)))
            .is_ok()
    }

    /// Requests a rollback; false if the engine is gone.
    pub fn rollback(&self, node: &str) -> bool {
        self.tx
            .send(ControlCommand::Rollback {
                node: node.to_string(),
            })
            .is_ok()
    }

    /// Requests an early commit; false if the engine is gone.
    pub fn commit(&self, node: &str) -> bool {
        self.tx
            .send(ControlCommand::Commit {
                node: node.to_string(),
            })
            .is_ok()
    }

    /// Sets a `value`/`select` node's live value at the next boundary; false if the engine is gone.
    pub fn set_value(&self, node: &str, value: serde_json::Value) -> bool {
        self.tx
            .send(ControlCommand::SetValue {
                node: node.to_string(),
                value,
            })
            .is_ok()
    }

    /// Edits a bound module config field at the next boundary; false if the engine is gone.
    pub fn configure(&self, node: &str, field: &str, value: serde_json::Value) -> bool {
        self.tx
            .send(ControlCommand::Configure {
                node: node.to_string(),
                field: field.to_string(),
                value,
            })
            .is_ok()
    }

    /// Pauses the graph at the next boundary; false if the engine is gone.
    pub fn pause(&self, for_ms: Option<u64>) -> bool {
        self.tx.send(ControlCommand::Pause { for_ms }).is_ok()
    }

    /// Resumes a paused graph; false if the engine is gone.
    pub fn resume(&self) -> bool {
        self.tx.send(ControlCommand::Resume).is_ok()
    }
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReplacementEventKind {
    /// Accepted; preparation started.
    Requested,
    /// Refused (validation, readiness or state migration); the active generation is unchanged.
    Rejected,
    /// Routing switched to the candidate.
    Promoted,
    /// The previous generation was stopped.
    Committed,
    /// Routing returned to the previous generation.
    RolledBack,
}

/// One replacement event.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacementEvent {
    /// Node id.
    pub node: String,
    /// Event kind.
    pub kind: ReplacementEventKind,
    /// The next cycle to run when the event happened (events happen at boundaries).
    pub cycle: u64,
    /// Generation active before the event.
    pub from_generation: u64,
    /// Generation active after the event (or the candidate's, for requested/rejected).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_generation: Option<u64>,
    /// Contract `id@version` active before.
    pub from_version: String,
    /// Contract `id@version` of the candidate / restored generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_version: Option<String>,
    /// Stable code for rejections and rollbacks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
    /// Explanation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Candidate instantiate + readiness time (promoted only), while cycles kept running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preparation_ms: Option<f64>,
    /// Time the boundary was held for drain + state migration + switch (promoted/rolled-back).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cutover_us: Option<u64>,
}

type Prepared = Result<(Box<dyn NodeHandle>, Duration), NodeError>;

pub(crate) struct Pending {
    request: ReplacementRequest,
    generation: u64,
    rx: Receiver<Prepared>,
    cancel: CancellationToken,
    thread: Option<JoinHandle<()>>,
}

pub(crate) struct Standby {
    handle: Box<dyn NodeHandle>,
    generation: u64,
    contract: NodeContract,
    origin: ContractOrigin,
    config: serde_json::Map<String, serde_json::Value>,
    storage: Vec<mvf_contract::BoundMount>,
    backend: String,
}

impl Engine {
    /// The candidate's mounts: the request's rebinding, else the active generation's bindings,
    /// validated against the candidate contract and the pipeline's volumes.
    fn candidate_storage(
        &self,
        i: usize,
        request: &ReplacementRequest,
    ) -> Result<Vec<mvf_contract::BoundMount>, String> {
        let slot = &self.slots[i];
        let map = request.storage.clone().unwrap_or_else(|| {
            slot.storage
                .iter()
                .filter_map(|b| {
                    b.volume
                        .as_ref()
                        .map(|v| (b.mount.name.clone(), v.name.clone()))
                })
                .collect()
        });
        mvf_graph::bind_storage(
            &slot.id,
            &request.contract,
            &map,
            &self.graph.volumes,
            &self.graph.name,
            "storage",
        )
        .map_err(|issues| {
            issues
                .iter()
                .map(|g| format!("{}: {}", g.code, g.message))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }
}

fn label(contract: &NodeContract) -> String {
    format!("{}@{}", contract.metadata.id, contract.metadata.version)
}

impl Engine {
    /// A handle for sending replacement commands to this engine (usable from other threads).
    pub fn control(&self) -> ControlHandle {
        ControlHandle::new(self.control_tx.clone())
    }

    /// Replacement events so far.
    pub fn replacements(&self) -> &[ReplacementEvent] {
        &self.replacement_log
    }

    fn event(
        &mut self,
        i: usize,
        kind: ReplacementEventKind,
        to: Option<(u64, String)>,
        code: Option<&'static str>,
        message: Option<String>,
    ) -> &mut ReplacementEvent {
        let slot = &self.slots[i];
        let (to_generation, to_version) = to.unzip();
        self.replacement_log.push(ReplacementEvent {
            node: slot.id.clone(),
            kind,
            cycle: self.next_cycle,
            from_generation: slot.generation,
            to_generation,
            from_version: label(&slot.contract),
            to_version,
            code,
            message,
            preparation_ms: None,
            cutover_us: None,
        });
        let last = self.replacement_log.len() - 1;
        &mut self.replacement_log[last]
    }

    fn reject_unknown(&mut self, node: &str, message: String) {
        self.replacement_log.push(ReplacementEvent {
            node: node.to_string(),
            kind: ReplacementEventKind::Rejected,
            cycle: self.next_cycle,
            from_generation: 0,
            to_generation: None,
            from_version: String::new(),
            to_version: None,
            code: Some("replace.unknown-node"),
            message: Some(message),
            preparation_ms: None,
            cutover_us: None,
        });
    }

    /// Runs at every cycle boundary: commands, ready candidates, finished probations.
    /// Applies one control command (at a boundary, or while paused).
    pub(crate) fn handle_command(&mut self, command: ControlCommand, cancel: &CancellationToken) {
        match command {
            ControlCommand::SetValue { node, value } => self.set_live_value(&node, value),
            ControlCommand::Configure { node, field, value } => {
                self.configure_live(&node, &field, value, cancel)
            }
            ControlCommand::Pause { for_ms } => self.pause(for_ms),
            ControlCommand::Resume => self.resume(),
            ControlCommand::Replace(request) => self.begin_replacement(*request, cancel),
            ControlCommand::Rollback { node } => match self.graph.index_of(&node) {
                Some(i) if self.slots[i].standby.is_some() => {
                    self.rollback(i, "replace.operator-rollback", "rollback requested".into())
                }
                Some(i) => {
                    let _ = self.event(
                        i,
                        ReplacementEventKind::Rejected,
                        None,
                        Some("replace.nothing-to-roll-back"),
                        Some("no previous generation is warm for this node".into()),
                    );
                }
                None => self.reject_unknown(&node, format!("no node `{node}`")),
            },
            ControlCommand::Commit { node } => {
                if let Some(i) = self.graph.index_of(&node) {
                    self.commit(i);
                } else {
                    self.reject_unknown(&node, format!("no node `{node}`"));
                }
            }
        }
    }

    pub(crate) fn on_boundary(&mut self, cancel: &CancellationToken) {
        for command in std::mem::take(&mut self.stashed) {
            self.handle_command(command, cancel);
        }
        while let Ok(command) = self.control_rx.try_recv() {
            self.handle_command(command, cancel);
        }
        self.poll_candidates(cancel);
        self.wait_while_paused(cancel);
        self.poll_candidates(cancel);
    }

    fn poll_candidates(&mut self, cancel: &CancellationToken) {
        for i in 0..self.slots.len() {
            let ready = match &self.slots[i].pending {
                None => continue,
                Some(p) => match p.rx.try_recv() {
                    Ok(prepared) => Some(prepared),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some(Err(NodeError::new(
                        NodeErrorKind::StartFailed,
                        "candidate preparation thread ended without a result",
                    ))),
                },
            };
            if let Some(prepared) = ready {
                let Some(mut pending) = self.slots[i].pending.take() else {
                    continue;
                };
                if let Some(t) = pending.thread.take() {
                    let _ = t.join();
                }
                match prepared {
                    Ok((handle, prep)) => self.cutover(i, pending, handle, prep, cancel),
                    Err(e) => {
                        let to = Some((pending.generation, label(&pending.request.contract)));
                        let _ = self.event(
                            i,
                            ReplacementEventKind::Rejected,
                            to,
                            Some("replace.readiness-failed"),
                            Some(format!("candidate failed to start: {e}")),
                        );
                    }
                }
            }
        }
        for i in 0..self.slots.len() {
            if self.slots[i].standby.is_some() && self.slots[i].probation == 0 {
                self.commit(i);
            }
        }
    }

    fn validate_request(
        &self,
        i: usize,
        request: &ReplacementRequest,
    ) -> Result<(), (&'static str, String)> {
        let slot = &self.slots[i];
        let node = &self.graph.nodes[i];
        if node.is_source() {
            return Err((
                "replace.source-unsupported",
                "replacing a source node is not supported in this slice".into(),
            ));
        }
        if slot.pending.is_some() || slot.standby.is_some() {
            return Err((
                "replace.in-progress",
                "a replacement of this node is already preparing or in probation".into(),
            ));
        }
        let active = slot.contract.spec.lifecycle.replacement;
        let candidate = request.contract.spec.lifecycle.replacement;
        if active == Replacement::Unsupported {
            return Err((
                "replace.not-replaceable",
                format!(
                    "the active contract `{}` declares `replacement: unsupported`",
                    label(&slot.contract)
                ),
            ));
        }
        if candidate != active {
            return Err((
                "replace.mode-mismatch",
                format!("active node is {active:?} but the candidate declares {candidate:?}")
                    .to_lowercase(),
            ));
        }
        if active == Replacement::Stateful {
            let (Some(from), Some(to)) = (
                &slot.contract.spec.lifecycle.state,
                &request.contract.spec.lifecycle.state,
            ) else {
                return Err((
                    "replace.state-incompatible",
                    "stateful contracts must declare state".into(),
                ));
            };
            if to.schema_version != from.schema_version
                && !to.migrates_from.contains(&from.schema_version)
            {
                return Err((
                    "replace.state-incompatible",
                    format!(
                        "candidate state schema {} cannot import schema {} (migratesFrom {:?})",
                        to.schema_version, from.schema_version, to.migrates_from
                    ),
                ));
            }
        }
        // Ports: every connected port must exist on the candidate with the same channel and type;
        // the candidate may not require inputs the graph does not feed.
        let ports = &request.contract.spec.ports;
        for edge in self.graph.edges().filter(|e| e.to.node == i) {
            match ports.inputs.iter().find(|p| p.name == edge.to.port) {
                Some(p) if p.channel == edge.channel && p.port_type == edge.port_type => {}
                _ => {
                    return Err((
                        "replace.port-incompatible",
                        format!(
                            "candidate lacks connected input `{}` ({} {})",
                            edge.to.port,
                            match edge.channel {
                                Channel::Data => "data",
                                Channel::Control => "control",
                            },
                            edge.port_type.as_str()
                        ),
                    ));
                }
            }
        }
        for p in ports.inputs.iter().filter(|p| p.required) {
            if !self
                .graph
                .edges()
                .any(|e| e.to.node == i && e.to.port == p.name)
            {
                return Err((
                    "replace.port-incompatible",
                    format!(
                        "candidate requires input `{}` which the graph does not feed",
                        p.name
                    ),
                ));
            }
        }
        for edge in self.graph.edges().filter(|e| e.from.node == i) {
            match ports.outputs.iter().find(|p| p.name == edge.from.port) {
                Some(p) if p.channel == edge.channel && p.port_type == edge.port_type => {}
                _ => {
                    return Err((
                        "replace.port-incompatible",
                        format!(
                            "candidate lacks connected output `{}` ({})",
                            edge.from.port,
                            edge.port_type.as_str()
                        ),
                    ));
                }
            }
        }
        self.candidate_storage(i, request)
            .map_err(|e| ("replace.storage-invalid", e))?;
        self.backends
            .negotiate(&request.contract)
            .map(|_| ())
            .map_err(|e| ("replace.negotiation", e.to_string()))
    }

    fn begin_replacement(&mut self, request: ReplacementRequest, run_cancel: &CancellationToken) {
        let Some(i) = self.graph.index_of(&request.node) else {
            let node = request.node.clone();
            self.reject_unknown(&node, format!("no node `{node}`"));
            return;
        };
        let generation = self.slots[i].next_generation;
        let to = Some((generation, label(&request.contract)));
        if let Err((code, message)) = self.validate_request(i, &request) {
            let _ = self.event(
                i,
                ReplacementEventKind::Rejected,
                to,
                Some(code),
                Some(message),
            );
            return;
        }
        self.slots[i].next_generation += 1;
        let _ = self.event(i, ReplacementEventKind::Requested, to, None, None);

        let (tx, rx) = mpsc::channel();
        let cancel = CancellationToken::new();
        let thread_cancel = cancel.clone();
        let backends: BackendRegistry = self.backends.clone();
        let services: HostServices = self.services.clone();
        let req = request.clone();
        let node_id = self.slots[i].id.clone();
        let network = self.slots[i].network.clone();
        // Validated in `validate_request`.
        let storage = self.candidate_storage(i, &request).unwrap_or_default();
        let run_cancel = run_cancel.clone();
        let thread = std::thread::spawn(move || {
            let t0 = Instant::now();
            let prepared = backends
                .instantiate(&InstantiateRequest {
                    node_id: &node_id,
                    contract: &req.contract,
                    origin: &req.origin,
                    config: &req.config,
                    generation,
                    services: &services,
                    network: network.as_ref(),
                    storage: &storage,
                })
                .and_then(|(mut handle, _)| {
                    let budget =
                        Duration::from_millis(req.contract.spec.lifecycle.readiness_timeout_ms);
                    // Either the run's or the preparation's own cancellation aborts readiness.
                    let watch = CancellationToken::new();
                    let ctx = LifecycleContext {
                        node_id: &node_id,
                        generation,
                        budget,
                        cancel: &watch,
                    };
                    let watcher = {
                        let (a, b, w) = (run_cancel.clone(), thread_cancel.clone(), watch.clone());
                        std::thread::spawn(move || {
                            while !w.is_cancelled() {
                                if a.is_cancelled() || b.is_cancelled() {
                                    w.cancel();
                                }
                                std::thread::sleep(Duration::from_millis(5));
                            }
                        })
                    };
                    let started = handle.start(&ctx);
                    watch.cancel();
                    let _ = watcher.join();
                    match started {
                        Ok(()) => Ok((handle, t0.elapsed())),
                        Err(e) => {
                            handle.stop(&ctx);
                            Err(e)
                        }
                    }
                });
            // If the engine is gone the handle is dropped here, which reaps a worker.
            let _ = tx.send(prepared);
        });
        self.slots[i].pending = Some(Pending {
            request,
            generation,
            rx,
            cancel,
            thread: Some(thread),
        });
    }

    fn cutover(
        &mut self,
        i: usize,
        pending: Pending,
        mut candidate: Box<dyn NodeHandle>,
        preparation: Duration,
        cancel: &CancellationToken,
    ) {
        let t0 = Instant::now();
        let request = pending.request;
        let to = Some((pending.generation, label(&request.contract)));
        let stateful = request.contract.spec.lifecycle.replacement == Replacement::Stateful;
        {
            let slot = &mut self.slots[i];
            let ctx = LifecycleContext {
                node_id: &slot.id,
                generation: slot.generation,
                budget: Duration::from_millis(slot.contract.spec.lifecycle.drain_timeout_ms),
                cancel,
            };
            if let Err(e) = slot.handle.drain(&ctx) {
                candidate.stop(&ctx);
                let _ = self.event(
                    i,
                    ReplacementEventKind::Rejected,
                    to,
                    Some("replace.drain-failed"),
                    Some(e.to_string()),
                );
                return;
            }
        }
        if stateful {
            let migrated = self.slots[i]
                .handle
                .export_state()
                .and_then(|state| match state {
                    Some(s) => candidate.import_state(&s),
                    None => Ok(()),
                });
            if let Err(e) = migrated {
                let slot = &self.slots[i];
                let ctx = LifecycleContext {
                    node_id: &slot.id,
                    generation: pending.generation,
                    budget: Duration::from_millis(request.contract.spec.lifecycle.drain_timeout_ms),
                    cancel,
                };
                candidate.stop(&ctx);
                let _ = self.event(
                    i,
                    ReplacementEventKind::Rejected,
                    to,
                    Some("replace.state-migration-failed"),
                    Some(e.to_string()),
                );
                return;
            }
        }
        // The candidate takes over what it exposes outside the graph (published listeners) before
        // the first cycle routes to it; if it cannot, the active generation keeps serving.
        if let Err(e) = candidate.promote() {
            let slot = &self.slots[i];
            let ctx = LifecycleContext {
                node_id: &slot.id,
                generation: pending.generation,
                budget: Duration::from_millis(request.contract.spec.lifecycle.drain_timeout_ms),
                cancel,
            };
            candidate.stop(&ctx);
            let _ = self.event(
                i,
                ReplacementEventKind::Rejected,
                to,
                Some("replace.endpoint-handover-failed"),
                Some(e.to_string()),
            );
            return;
        }
        let backend = self
            .backends
            .negotiate(&request.contract)
            .map(|p| p.backend.as_str().to_string())
            .unwrap_or_default();
        let candidate_storage = self.candidate_storage(i, &request).unwrap_or_default();
        let slot = &mut self.slots[i];
        let previous = std::mem::replace(&mut slot.handle, candidate);
        let previous_contract = std::mem::replace(&mut slot.contract, request.contract.clone());
        let previous_generation = slot.generation;
        let previous_origin = std::mem::replace(&mut slot.origin, request.origin.clone());
        let previous_config = std::mem::replace(&mut slot.config, request.config.clone());
        let previous_storage = std::mem::replace(&mut slot.storage, candidate_storage);
        slot.standby = Some(Standby {
            handle: previous,
            generation: previous_generation,
            contract: previous_contract.clone(),
            origin: previous_origin,
            config: previous_config,
            storage: previous_storage,
            backend: std::mem::replace(&mut slot.stats.backend, backend),
        });
        slot.generation = pending.generation;
        slot.stats.generation = pending.generation;
        slot.probation = request.probation_cycles;
        let cutover = t0.elapsed();
        self.replacement_log.push(ReplacementEvent {
            node: slot.id.clone(),
            kind: ReplacementEventKind::Promoted,
            cycle: self.next_cycle,
            from_generation: previous_generation,
            to_generation: Some(pending.generation),
            from_version: label(&previous_contract),
            to_version: Some(label(&request.contract)),
            code: None,
            message: None,
            preparation_ms: Some(preparation.as_secs_f64() * 1e3),
            cutover_us: Some(u64::try_from(cutover.as_micros()).unwrap_or(u64::MAX)),
        });
    }

    /// Routes back to the warm previous generation and stops the failed candidate.
    pub(crate) fn rollback(&mut self, i: usize, code: &'static str, message: String) {
        let t0 = Instant::now();
        let slot = &mut self.slots[i];
        let Some(standby) = slot.standby.take() else {
            return;
        };
        let mut failed = std::mem::replace(&mut slot.handle, standby.handle);
        let failed_contract = std::mem::replace(&mut slot.contract, standby.contract);
        let failed_generation = std::mem::replace(&mut slot.generation, standby.generation);
        slot.origin = standby.origin;
        slot.config = standby.config;
        slot.storage = standby.storage;
        slot.stats.generation = standby.generation;
        slot.stats.backend = standby.backend;
        slot.probation = 0;
        // The restored generation takes its listeners back before the failed one stops.
        let handover = slot.handle.promote().err();
        let cancel = CancellationToken::new();
        let ctx = LifecycleContext {
            node_id: &slot.id,
            generation: failed_generation,
            budget: Duration::from_millis(failed_contract.spec.lifecycle.drain_timeout_ms),
            cancel: &cancel,
        };
        failed.stop(&ctx);
        let restored = label(&slot.contract);
        let generation = slot.generation;
        let cutover = t0.elapsed();
        self.replacement_log.push(ReplacementEvent {
            node: slot.id.clone(),
            kind: ReplacementEventKind::RolledBack,
            cycle: self.next_cycle,
            from_generation: failed_generation,
            to_generation: Some(generation),
            from_version: label(&failed_contract),
            to_version: Some(restored),
            code: Some(code),
            message: Some(match handover {
                Some(e) => format!("{message}; endpoint handover back failed: {e}"),
                None => message,
            }),
            preparation_ms: None,
            cutover_us: Some(u64::try_from(cutover.as_micros()).unwrap_or(u64::MAX)),
        });
    }

    fn commit(&mut self, i: usize) {
        let slot = &mut self.slots[i];
        let Some(mut standby) = slot.standby.take() else {
            return;
        };
        slot.probation = 0;
        let cancel = CancellationToken::new();
        let ctx = LifecycleContext {
            node_id: &slot.id,
            generation: standby.generation,
            budget: Duration::from_millis(standby.contract.spec.lifecycle.drain_timeout_ms),
            cancel: &cancel,
        };
        standby.handle.stop(&ctx);
        let to = Some((slot.generation, label(&slot.contract)));
        let ev = self.event(i, ReplacementEventKind::Committed, to, None, None);
        ev.from_generation = standby.generation;
        ev.from_version = label(&standby.contract);
    }

    /// Stops standby generations and aborts preparations (used by shutdown).
    pub(crate) fn shutdown_replacements(&mut self, cancel: &CancellationToken) {
        for i in 0..self.slots.len() {
            if let Some(mut pending) = self.slots[i].pending.take() {
                pending.cancel.cancel();
                if let Some(t) = pending.thread.take() {
                    let _ = t.join();
                }
                if let Ok(Ok((mut handle, _))) = pending.rx.try_recv() {
                    let slot = &self.slots[i];
                    handle.stop(&LifecycleContext {
                        node_id: &slot.id,
                        generation: pending.generation,
                        budget: Duration::from_millis(
                            pending.request.contract.spec.lifecycle.drain_timeout_ms,
                        ),
                        cancel,
                    });
                }
                let to = Some((pending.generation, label(&pending.request.contract)));
                let _ = self.event(
                    i,
                    ReplacementEventKind::Rejected,
                    to,
                    Some("replace.aborted"),
                    Some("the run ended while the candidate was still preparing".into()),
                );
            }
            if self.slots[i].standby.is_some() {
                self.commit(i);
            }
        }
    }
}

// ------------------------------------------------------------------ update plans (CLI / files)

/// The only accepted `kind` for update plans.
pub const UPDATE_PLAN_KIND: &str = "UpdatePlan";

/// A scripted sequence of updates applied while a pipeline runs (`mvf run --updates plan.yaml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdatePlan {
    /// Must equal `mvf.dev/v1alpha1`.
    pub api_version: String,
    /// Must equal [`UPDATE_PLAN_KIND`].
    pub kind: String,
    /// Steps.
    pub spec: UpdatePlanSpec,
}

/// Update plan body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdatePlanSpec {
    /// Steps, applied in order of `atCycle`.
    pub updates: Vec<UpdateStep>,
}

/// One scripted step: submitted at the boundary before `atCycle`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UpdateStep {
    /// Pipeline node id.
    pub node: String,
    /// Submit at the boundary before this cycle.
    pub at_cycle: u64,
    /// `replace` (default when a reference is given), `rollback` or `commit`.
    #[serde(default)]
    pub action: Option<UpdateAction>,
    /// Candidate builtin.
    #[serde(default)]
    pub builtin: Option<String>,
    /// Candidate contract file (relative to the plan file).
    #[serde(default)]
    pub contract: Option<String>,
    /// Candidate module id.
    #[serde(default)]
    pub module: Option<String>,
    /// Candidate config.
    #[serde(default)]
    pub config: Option<serde_json::Map<String, serde_json::Value>>,
    /// Rollback window in cycles (default 10).
    #[serde(default)]
    pub probation_cycles: Option<u64>,
    /// `set`/`configure`: the new value.
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    /// `configure`: the bound config field.
    #[serde(default)]
    pub field: Option<String>,
    /// `pause`: resume automatically after this many milliseconds.
    #[serde(default)]
    pub for_ms: Option<u64>,
    /// `replace`: rebind the candidate's mounts (mount -> pipeline volume), e.g. a new model.
    #[serde(default)]
    pub storage: Option<std::collections::BTreeMap<String, String>>,
}

/// Explicit actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateAction {
    /// Stage and promote a candidate.
    Replace,
    /// Roll back to the previous generation.
    Rollback,
    /// Commit early.
    Commit,
    /// Set a `value`/`select` node's live value (`value`).
    Set,
    /// Edit a bound module config field (`field`, `value`).
    Configure,
    /// Pause the graph (`node`: the loop; optional `forMs`).
    Pause,
    /// Resume the graph (`node`: the loop).
    Resume,
}

/// A validated plan step ready to submit.
#[derive(Debug, Clone)]
pub enum PlannedCommand {
    /// Replacement.
    Replace(Box<ReplacementRequest>),
    /// Rollback.
    Rollback(String),
    /// Commit.
    Commit(String),
    /// Live value.
    SetValue(String, serde_json::Value),
    /// Module binding edit.
    Configure(String, String, serde_json::Value),
    /// Pause, with an optional automatic resume.
    Pause(Option<u64>),
    /// Resume.
    Resume,
}

impl PlannedCommand {
    /// Sends the command through `control`.
    pub fn submit(&self, control: &ControlHandle) -> bool {
        match self {
            PlannedCommand::Replace(r) => control.replace((**r).clone()),
            PlannedCommand::Rollback(n) => control.rollback(n),
            PlannedCommand::Commit(n) => control.commit(n),
            PlannedCommand::SetValue(n, v) => control.set_value(n, v.clone()),
            PlannedCommand::Configure(n, f, v) => control.configure(n, f, v.clone()),
            PlannedCommand::Pause(ms) => control.pause(*ms),
            PlannedCommand::Resume => control.resume(),
        }
    }
}

/// Parses an update plan with the strict YAML loader and resolves every candidate.
pub fn parse_update_plan(
    text: &str,
    resolver: &dyn ContractResolver,
) -> Result<Vec<(u64, PlannedCommand)>, String> {
    let plan: UpdatePlan =
        yaml::from_str(text, &yaml::YamlLimits::default()).map_err(|e| e.to_string())?;
    if plan.api_version != mvf_contract::NODE_CONTRACT_API_VERSION || plan.kind != UPDATE_PLAN_KIND
    {
        return Err(format!(
            "expected apiVersion {} and kind {UPDATE_PLAN_KIND}",
            mvf_contract::NODE_CONTRACT_API_VERSION
        ));
    }
    let mut steps = Vec::new();
    for (n, step) in plan.spec.updates.iter().enumerate() {
        if step.at_cycle == 0 {
            return Err(format!("spec.updates[{n}].atCycle must be positive"));
        }
        let reference = match (&step.builtin, &step.contract, &step.module) {
            (Some(b), None, None) => Some(NodeReference::Builtin(b.clone())),
            (None, Some(c), None) => Some(NodeReference::Contract(c.clone())),
            (None, None, Some(m)) => Some(NodeReference::Module(m.clone())),
            (None, None, None) => None,
            _ => {
                return Err(format!(
                    "spec.updates[{n}]: give at most one of builtin/contract/module"
                ));
            }
        };
        let live = (
            step.value.is_some(),
            step.field.is_some(),
            step.for_ms.is_some(),
        );
        let command = match (step.action, reference) {
            (Some(UpdateAction::Set), None) if live.0 && !live.1 && !live.2 => {
                PlannedCommand::SetValue(step.node.clone(), step.value.clone().unwrap_or_default())
            }
            (Some(UpdateAction::Configure), None) if live.0 && live.1 && !live.2 => {
                PlannedCommand::Configure(
                    step.node.clone(),
                    step.field.clone().unwrap_or_default(),
                    step.value.clone().unwrap_or_default(),
                )
            }
            (Some(UpdateAction::Pause), None) if !live.0 && !live.1 => {
                PlannedCommand::Pause(step.for_ms)
            }
            (Some(UpdateAction::Resume), None) if live == (false, false, false) => {
                PlannedCommand::Resume
            }
            (
                Some(
                    UpdateAction::Set
                    | UpdateAction::Configure
                    | UpdateAction::Pause
                    | UpdateAction::Resume,
                ),
                _,
            ) => {
                return Err(format!(
                    "spec.updates[{n}]: `set` takes `value`; `configure` takes `field` and `value`; `pause` takes an optional `forMs`; `resume` takes nothing — and none takes a candidate"
                ));
            }
            (_, _) if live != (false, false, false) => {
                return Err(format!(
                    "spec.updates[{n}]: `value`/`field`/`forMs` belong to `set`, `configure` and `pause`"
                ));
            }
            (Some(UpdateAction::Rollback), None) => PlannedCommand::Rollback(step.node.clone()),
            (Some(UpdateAction::Commit), None) => PlannedCommand::Commit(step.node.clone()),
            (None | Some(UpdateAction::Replace), Some(reference)) => {
                let request = ReplacementRequest::resolve(
                    resolver,
                    &step.node,
                    &reference,
                    step.config.clone().unwrap_or_default(),
                    step.probation_cycles.unwrap_or(10),
                )
                .map_err(|e| format!("spec.updates[{n}]: {e}"))?;
                PlannedCommand::Replace(Box::new(match &step.storage {
                    Some(map) => request.with_storage(map.clone()),
                    None => request,
                }))
            }
            _ => {
                return Err(format!(
                    "spec.updates[{n}]: `replace` needs a candidate reference; `rollback`/`commit` take none"
                ));
            }
        };
        steps.push((step.at_cycle, command));
    }
    steps.sort_by_key(|(c, _)| *c);
    Ok(steps)
}
