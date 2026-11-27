//! Deterministic serial executor — the single graph scheduling authority.
//!
//! Cycle semantics (Gate A):
//! - cycles are numbered from 1 and are **atomic** with respect to cancellation and limits: the
//!   cancellation token and `maxCycles` are checked only at cycle boundaries;
//! - within a cycle, sources run first (declaration order), then every other node in the
//!   graph's deterministic topological order;
//! - a node runs when every *required* data input is present and every *connected* control input
//!   is present and `true`; otherwise it is skipped and its outputs are absent this cycle (absence
//!   propagates downstream). Optional data inputs may be absent;
//! - a source returning `Exhausted` ends the run before any non-source node runs in that cycle —
//!   unless the graph's `loop` is `forever`: then every rewindable source is rewound and the cycle
//!   runs again from frame 0 (the empty exhaustion pass is not counted); `count` ends after N cycles;
//! - an `on-demand` node is started the first time it runs (it has its inputs), not at run start;
//! - live edits (`value`/`select` settings, module bindings, pause/resume) apply at cycle boundaries;
//! - a node failure ends the run (`fail` policy) and is reported with the node, cycle and cause;
//! - every emitted value is re-checked against the producer's declared output port and type.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use mvf_contract::{Channel, NodeContract};
use mvf_graph::Graph;
use serde::Serialize;

use crate::backend::{BackendRegistry, InstantiateRequest};
use crate::builtins::HostServices;
use crate::live::{IterationReport, LiveEvent};
use crate::node::*;
use crate::recovery::{CheckpointState, Recovery, RecoveryEvent};
use crate::replace::{ControlCommand, Pending, ReplacementEvent, Standby};
use crate::value::Value;

/// Why a node did not run in a cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", content = "port", rename_all = "kebab-case")]
pub enum SkipReason {
    /// A required data input had no value this cycle.
    MissingInput(String),
    /// A connected control input was absent this cycle.
    ControlAbsent(String),
    /// A connected control input was `false`.
    ControlFalse(String),
    /// The node failed and its policy is `skip` (or a restart budget ran out into `skip`).
    Failure(String),
    /// The arena was full and the node's backpressure policy is `drop`.
    Dropped(String),
}

/// What happened to one node in one cycle.
#[derive(Debug, Clone)]
pub enum NodeOutcome {
    /// The node ran.
    Executed {
        /// Execution time.
        duration: Duration,
        /// Number of values emitted.
        outputs: usize,
    },
    /// The node was skipped.
    Skipped(SkipReason),
}

/// Receives execution events. All methods default to no-ops.
pub trait Observer {
    /// A node finished (or was skipped) in `cycle`.
    fn node(&mut self, cycle: u64, node: &str, generation: u64, outcome: &NodeOutcome) {
        let _ = (cycle, node, generation, outcome);
    }
    /// `cycle` completed.
    fn cycle_completed(&mut self, cycle: u64) {
        let _ = cycle;
    }
    /// Engine events recorded since the last call, in order: live replacement, recovery (restart,
    /// skip, drop, checkpoint) and live edits. Delivered at cycle boundaries and when the run ends,
    /// so an observer (a management feed, #33) sees them while the run goes on.
    fn events(&mut self, events: &EngineEvents<'_>) {
        let _ = events;
    }
    /// Whether this observer wants `node`'s output values (a sampled preview, #33). The default
    /// is no, so nothing is handed out. Each `true` asks for one [`Observer::outputs`] call for
    /// `node`:
    /// - **serial executor:** asked once per executed node, and `outputs` follows at once;
    /// - **pipelined executor:** sources are asked the same way, on the driver thread. Other
    ///   nodes are asked by the driver once per cycle while no sample of them is in flight, and
    ///   the outputs of the node's next execution arrive later, possibly with other nodes' calls
    ///   in between. None arrives if the node does not execute again before the run ends or
    ///   stops at a boundary.
    fn wants_outputs(&mut self, node: &str) -> bool {
        let _ = node;
        false
    }
    /// The outputs of a node it wanted, before they go downstream (serial) or while they do
    /// (pipelined). Values are shared, not copied: a frame is a lease on its arena slot, and
    /// holding it keeps that slot in use.
    fn outputs(
        &mut self,
        cycle: u64,
        node: &str,
        outputs: &std::collections::BTreeMap<String, Value>,
    ) {
        let _ = (cycle, node, outputs);
    }
}

/// New engine events (see [`Observer::events`]).
#[derive(Debug, Clone, Copy)]
pub struct EngineEvents<'a> {
    /// Live replacement events.
    pub replacements: &'a [ReplacementEvent],
    /// Failure-policy, backpressure and checkpoint events.
    pub recoveries: &'a [RecoveryEvent],
    /// Live edits and pause/resume.
    pub live: &'a [LiveEvent],
}

/// An observer that ignores everything.
#[derive(Debug, Default)]
pub struct NullObserver;
impl Observer for NullObserver {}

/// Why a run ended.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum EndReason {
    /// `maxCycles` reached.
    CycleLimit {
        /// The limit.
        limit: u64,
    },
    /// A source has no more input.
    SourceExhausted {
        /// The source node.
        node: String,
    },
    /// The cancellation token was set.
    Cancelled,
    /// A node failed.
    NodeFailed {
        /// The failing node.
        node: String,
        /// The cycle.
        cycle: u64,
        /// The failure.
        error: NodeError,
    },
    /// The `loop` primitive's `count` was reached (a completed run, like an exhausted source).
    LoopCompleted {
        /// The count.
        count: u64,
    },
    /// A node could not be started.
    StartFailed {
        /// The node.
        node: String,
        /// The failure.
        error: NodeError,
    },
}

/// Per-node statistics.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeStats {
    /// Node id.
    pub id: String,
    /// Backend that runs the node.
    pub backend: String,
    /// Current generation.
    pub generation: u64,
    /// Cycles executed.
    pub executed: u64,
    /// Cycles skipped.
    pub skipped: u64,
    /// Total execution time, microseconds.
    pub total_us: u64,
    /// Slowest execution, microseconds.
    pub max_us: u64,
    /// Restarts performed by the `restart` failure policy.
    pub restarts: u64,
    /// Cycles skipped by the `skip` failure policy.
    pub failures_skipped: u64,
    /// Cycles dropped by the `drop` backpressure policy.
    pub dropped: u64,
    /// Cycle in which an `on-demand` node was first activated (absent: resident, or never needed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activated_at_cycle: Option<u64>,
}

/// Outcome of a run.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunReport {
    /// Pipeline name.
    pub pipeline: String,
    /// Why the run ended.
    pub end: EndReason,
    /// Cycles that ran to completion.
    pub cycles_completed: u64,
    /// Wall time of the run (start → stop), milliseconds.
    pub wall_time_ms: f64,
    /// Time spent starting nodes (activation + readiness), milliseconds.
    pub startup_ms: f64,
    /// Time spent running cycles (excluding start and shutdown), milliseconds.
    pub cycles_ms: f64,
    /// Per-node statistics.
    pub nodes: Vec<NodeStats>,
    /// Payload deliveries and shared-memory copy accounting.
    pub data_plane: DataPlaneReport,
    /// Live replacement events, in order.
    pub replacements: Vec<ReplacementEvent>,
    /// Failure-policy and backpressure events (restarts, skips, drops, checkpoints), in order.
    pub recoveries: Vec<RecoveryEvent>,
    /// Cycle restored from a durable checkpoint at start, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resumed_from_cycle: Option<u64>,
    /// Live edits and pause/resume, in order.
    pub live: Vec<LiveEvent>,
    /// What the `loop` primitive did, when the graph has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration: Option<IterationReport>,
}

/// Data-plane accounting for a run. A *delivery* is one payload handed to one consuming node; it
/// never copies bytes by itself (heap payloads share an `Arc`, arena payloads share a lease).
/// Copies are counted by the arena (`copiesIn`/`copiesOut`) and distinguished from producers'
/// output writes (`transformWrites`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataPlaneReport {
    /// Payload deliveries whose bytes live in the shared-memory arena.
    pub arena_deliveries: u64,
    /// Payload deliveries whose bytes live on the host heap.
    pub heap_deliveries: u64,
    /// Payload deliveries whose bytes live in device memory (handle only, no transfer).
    pub device_deliveries: u64,
    /// Arena counters, when the arena was created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arena: Option<mvf_memory::MetricsSnapshot>,
}

/// Run options.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Overrides the pipeline's `run.maxCycles`.
    pub max_cycles: Option<u64>,
    /// Cooperative cancellation, checked at cycle boundaries and observable by nodes.
    pub cancel: CancellationToken,
    /// Overrides `run.checkpoint.everyCycles`.
    pub checkpoint_every: Option<u64>,
    /// Overrides `run.checkpoint.directory` (absolute or relative to the working directory).
    pub checkpoint_dir: Option<std::path::PathBuf>,
    /// Run stage-parallel (the pipelined executor) instead of one cycle at a time.
    pub pipelined: bool,
    /// Pipelined: depth of each inter-stage queue (default [`crate::pipelined::DEFAULT_QUEUE`]).
    pub queue: Option<usize>,
}

/// A node slot: the active handle plus its routing generation.
pub(crate) struct NodeSlot {
    pub(crate) id: String,
    pub(crate) contract: NodeContract,
    /// Origin and config of the active generation (a restart rebuilds exactly this).
    pub(crate) origin: mvf_graph::ContractOrigin,
    pub(crate) config: serde_json::Map<String, serde_json::Value>,
    pub(crate) on_error: mvf_graph::FailurePolicy,
    pub(crate) backpressure: mvf_graph::Backpressure,
    /// Deployment-time network binding.
    pub(crate) network: Option<mvf_contract::NetworkBinding>,
    /// Mounts of the active generation (a replacement may rebind them).
    pub(crate) storage: Vec<mvf_contract::BoundMount>,
    /// `resident` nodes start with the run; `on-demand` ones the first time they run.
    pub(crate) on_demand: bool,
    /// Whether the active handle has been started.
    pub(crate) activated: bool,
    /// A restored state waiting for an on-demand node's activation.
    pub(crate) pending_state: Option<StateBlob>,
    pub(crate) handle: Box<dyn NodeHandle>,
    /// Extra instances of a replicated node (pipelined `parallelism`), started on first use.
    pub(crate) replicas: Vec<Box<dyn NodeHandle>>,
    pub(crate) generation: u64,
    pub(crate) stats: NodeStats,
    /// Next generation number to hand out (monotonic per node).
    pub(crate) next_generation: u64,
    /// A candidate generation being prepared.
    pub(crate) pending: Option<Pending>,
    /// The previous generation, warm for rollback during probation.
    pub(crate) standby: Option<Standby>,
    /// Remaining probation cycles while `standby` is present.
    pub(crate) probation: u64,
}

/// A validated graph bound to node handles.
pub struct Engine {
    pub(crate) graph: Graph,
    pub(crate) slots: Vec<NodeSlot>,
    exec_order: Vec<usize>,
    /// Incoming edge per (node, input port).
    feeds: HashMap<(usize, String), (usize, String)>,
    pub(crate) next_cycle: u64,
    started: bool,
    pub(crate) services: HostServices,
    pub(crate) deliveries: DataPlaneReport,
    pub(crate) backends: BackendRegistry,
    pub(crate) control_tx: Sender<ControlCommand>,
    pub(crate) control_rx: Receiver<ControlCommand>,
    pub(crate) replacement_log: Vec<ReplacementEvent>,
    pub(crate) recovery_log: Vec<RecoveryEvent>,
    pub(crate) checkpoint: CheckpointState,
    pub(crate) live_log: Vec<LiveEvent>,
    /// Commands the pipelined driver received mid-segment, applied at the next boundary.
    pub(crate) stashed: Vec<ControlCommand>,
    pub(crate) iteration: IterationReport,
    /// Whole-graph pause (only with a `loop`), with an optional automatic resume.
    pub(crate) paused: Option<Option<Instant>>,
    /// (startup ms, cycle loop ms) of the last run.
    timings: (f64, f64),
    /// How many replacement, recovery and live events observers have been given.
    observed: [usize; 3],
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("pipeline", &self.graph.name)
            .field("nodes", &self.slots.len())
            .field("next_cycle", &self.next_cycle)
            .finish()
    }
}

/// Building an engine failed for one node.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("node `{node}`: {error}")]
pub struct BuildError {
    /// Node id.
    pub node: String,
    /// Cause.
    pub error: NodeError,
}

impl Engine {
    /// Negotiates a backend for every node and instantiates it (not yet started).
    pub fn build(
        graph: Graph,
        backends: &BackendRegistry,
        services: &HostServices,
    ) -> Result<Self, BuildError> {
        let mut slots = Vec::with_capacity(graph.nodes.len());
        for node in &graph.nodes {
            // Binding pre-pass for module config: a stored edit comes back on the next run.
            let config = crate::live::overlay_bindings(node, &services.bindings).map_err(|e| {
                BuildError {
                    node: node.id.clone(),
                    error: NodeError::new(NodeErrorKind::InvalidConfig, e),
                }
            })?;
            let (handle, backend) = backends
                .instantiate(&InstantiateRequest {
                    node_id: &node.id,
                    contract: &node.contract,
                    origin: &node.origin,
                    config: &config,
                    generation: 1,
                    services,
                    network: node.network.as_ref(),
                    storage: &node.storage,
                })
                .map_err(|error| BuildError {
                    node: node.id.clone(),
                    error,
                })?;
            slots.push(NodeSlot {
                id: node.id.clone(),
                contract: node.contract.clone(),
                origin: node.origin.clone(),
                config,
                on_error: node.on_error.clone(),
                backpressure: node.backpressure,
                network: node.network.clone(),
                storage: node.storage.clone(),
                on_demand: node.contract.spec.lifecycle.activation
                    == mvf_contract::Activation::OnDemand,
                activated: false,
                pending_state: None,
                handle,
                replicas: Vec::new(),
                generation: 1,
                stats: NodeStats {
                    id: node.id.clone(),
                    backend: backend.as_str().to_string(),
                    generation: 1,
                    ..NodeStats::default()
                },
                next_generation: 2,
                pending: None,
                standby: None,
                probation: 0,
            });
        }
        crate::live::check_select_criteria(&graph, services)?;
        // The loop is the iteration authority, not a dataflow node: it never executes.
        let mut exec_order: Vec<usize> = graph
            .order
            .iter()
            .copied()
            .filter(|i| !graph.nodes[*i].is_loop())
            .collect();
        exec_order.sort_by_key(|i| !graph.nodes[*i].is_source());
        // `sort_by_key` is stable: sources keep declaration order among themselves (they all have
        // in-degree zero, so the topological order lists them by declaration index), non-sources
        // keep topological order.
        let feeds = graph
            .edges()
            .map(|e| {
                (
                    (e.to.node, e.to.port.clone()),
                    (e.from.node, e.from.port.clone()),
                )
            })
            .collect();
        let (control_tx, control_rx) = mpsc::channel();
        let iteration = IterationReport::for_graph(&graph);
        Ok(Self {
            graph,
            slots,
            exec_order,
            feeds,
            next_cycle: 1,
            started: false,
            services: services.clone(),
            deliveries: DataPlaneReport::default(),
            backends: backends.clone(),
            control_tx,
            control_rx,
            replacement_log: Vec::new(),
            recovery_log: Vec::new(),
            checkpoint: CheckpointState::default(),
            live_log: Vec::new(),
            stashed: Vec::new(),
            iteration,
            paused: None,
            timings: (0.0, 0.0),
            observed: [0; 3],
        })
    }

    /// Data-plane accounting so far.
    pub fn data_plane(&self) -> DataPlaneReport {
        DataPlaneReport {
            arena: self.services.arena_if_created().map(|a| a.metrics()),
            ..self.deliveries.clone()
        }
    }

    /// The validated graph.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Execution order (sources first), without the `loop`.
    pub(crate) fn exec_order(&self) -> &[usize] {
        &self.exec_order
    }

    /// Current statistics.
    pub fn stats(&self) -> Vec<NodeStats> {
        self.slots.iter().map(|s| s.stats.clone()).collect()
    }

    /// Starts every node in execution order, each within its contract's readiness budget.
    pub fn start(&mut self, cancel: &CancellationToken) -> Result<(), (String, NodeError)> {
        if self.started {
            return Ok(());
        }
        for &i in &self.exec_order {
            let slot = &mut self.slots[i];
            if slot.on_demand {
                continue; // started the first time it runs
            }
            let budget = Duration::from_millis(slot.contract.spec.lifecycle.readiness_timeout_ms);
            let ctx = LifecycleContext {
                node_id: &slot.id,
                generation: slot.generation,
                budget,
                cancel,
            };
            slot.handle.start(&ctx).map_err(|e| (slot.id.clone(), e))?;
            slot.activated = true;
        }
        self.started = true;
        Ok(())
    }

    /// Drains and stops every node (reverse execution order). Idempotent.
    pub fn shutdown(&mut self, cancel: &CancellationToken) {
        self.shutdown_replacements(cancel);
        for &i in self.exec_order.iter().rev() {
            let slot = &mut self.slots[i];
            if !slot.activated {
                continue;
            }
            slot.activated = false;
            for mut replica in std::mem::take(&mut slot.replicas) {
                replica.stop(&LifecycleContext {
                    node_id: &slot.id,
                    generation: slot.generation,
                    budget: Duration::from_millis(slot.contract.spec.lifecycle.drain_timeout_ms),
                    cancel,
                });
            }
            let ctx = LifecycleContext {
                node_id: &slot.id,
                generation: slot.generation,
                budget: Duration::from_millis(slot.contract.spec.lifecycle.drain_timeout_ms),
                cancel,
            };
            let _ = slot.handle.drain(&ctx);
            slot.handle.stop(&ctx);
        }
        self.started = false;
    }

    /// Starts, runs cycles until an end condition, and shuts down.
    pub fn run(&mut self, options: &RunOptions, observer: &mut dyn Observer) -> RunReport {
        let started_at = Instant::now();
        self.configure_checkpoints(options);
        let started = self.start(&options.cancel);
        self.timings.0 = started_at.elapsed().as_secs_f64() * 1e3;
        let end = match started {
            Err((node, error)) => EndReason::StartFailed { node, error },
            Ok(()) => {
                self.resume_from_checkpoint();
                let cancel = options.cancel.clone();
                let t0 = Instant::now();
                let end = if options.pipelined {
                    self.run_cycles_pipelined(options, observer)
                } else {
                    self.run_cycles(options, observer, &mut |e: &mut Engine| {
                        e.on_boundary(&cancel)
                    })
                };
                self.timings.1 = t0.elapsed().as_secs_f64() * 1e3;
                end
            }
        };
        if matches!(
            end,
            EndReason::SourceExhausted { .. } | EndReason::LoopCompleted { .. }
        ) {
            // A cleanly, fully consumed run has nothing to resume.
            self.clear_checkpoints();
        }
        self.shutdown(&options.cancel);
        self.flush_events(observer);
        self.report(end, started_at)
    }

    /// Gives `observer` the replacement, recovery and live events recorded since the last call.
    pub(crate) fn flush_events(&mut self, observer: &mut dyn Observer) {
        let [r, c, l] = self.observed;
        if r == self.replacement_log.len()
            && c == self.recovery_log.len()
            && l == self.live_log.len()
        {
            return;
        }
        observer.events(&EngineEvents {
            replacements: &self.replacement_log[r.min(self.replacement_log.len())..],
            recoveries: &self.recovery_log[c.min(self.recovery_log.len())..],
            live: &self.live_log[l.min(self.live_log.len())..],
        });
        self.observed = [
            self.replacement_log.len(),
            self.recovery_log.len(),
            self.live_log.len(),
        ];
    }

    pub(crate) fn report(&self, end: EndReason, started_at: Instant) -> RunReport {
        RunReport {
            pipeline: self.graph.name.clone(),
            end,
            cycles_completed: self.next_cycle - 1,
            wall_time_ms: started_at.elapsed().as_secs_f64() * 1e3,
            startup_ms: self.timings.0,
            cycles_ms: self.timings.1,
            nodes: self.stats(),
            data_plane: self.data_plane(),
            replacements: self.replacement_log.clone(),
            recoveries: self.recovery_log.clone(),
            resumed_from_cycle: self.checkpoint.resumed_from,
            live: self.live_log.clone(),
            iteration: self.graph.loop_spec.map(|_| self.iteration.clone()),
        }
    }

    /// Runs cycles on started nodes. `boundary` runs at every cycle boundary (before each cycle)
    /// with exclusive access to the engine — the only place routing may change.
    pub(crate) fn run_cycles(
        &mut self,
        options: &RunOptions,
        observer: &mut dyn Observer,
        boundary: &mut dyn FnMut(&mut Engine),
    ) -> EndReason {
        let limit = options.max_cycles.or(self.graph.max_cycles);
        let loop_spec = self.graph.loop_spec;
        let mut produced_since_rewind = false;
        loop {
            boundary(self);
            self.flush_events(observer);
            if options.cancel.is_cancelled() {
                return EndReason::Cancelled;
            }
            if let Some(limit) = limit
                && self.next_cycle > limit
            {
                return EndReason::CycleLimit { limit };
            }
            let cycle = self.next_cycle;
            if let Some(end) = self.run_cycle(cycle, &options.cancel, observer) {
                // `forever`: a finite source that ran dry is rewound and the cycle runs again. Only
                // after a pass that produced frames, so an empty source cannot spin.
                if let EndReason::SourceExhausted { .. } = &end
                    && loop_spec.is_some_and(|l| l.mode == mvf_graph::LoopMode::Forever)
                    && produced_since_rewind
                    && self.rewind_sources()
                {
                    produced_since_rewind = false;
                    continue;
                }
                return end;
            }
            produced_since_rewind = true;
            self.next_cycle += 1;
            self.maybe_checkpoint(cycle);
            observer.cycle_completed(cycle);
            self.flush_events(observer);
            if let Some(l) = loop_spec
                && l.mode == mvf_graph::LoopMode::Count
                && let Some(count) = l.count
                && cycle >= count
            {
                return EndReason::LoopCompleted { count };
            }
        }
    }

    /// Executes one cycle. Returns `Some(end)` when the run must end.
    pub(crate) fn run_cycle(
        &mut self,
        cycle: u64,
        cancel: &CancellationToken,
        observer: &mut dyn Observer,
    ) -> Option<EndReason> {
        let mut values: HashMap<(usize, String), Value> = HashMap::new();
        'nodes: for oi in 0..self.exec_order.len() {
            let i = self.exec_order[oi];
            let node = &self.graph.nodes[i];
            let mut inputs = Inputs::new();
            let mut skip = None;
            for port in &node.contract.spec.ports.inputs {
                let Some(source) = self.feeds.get(&(i, port.name.clone())) else {
                    continue; // optional and unconnected
                };
                match (values.get(source), port.channel) {
                    (Some(Value::Signal(false)), Channel::Control) => {
                        skip.get_or_insert(SkipReason::ControlFalse(port.name.clone()));
                    }
                    (Some(v), _) => {
                        if let Some(p) = v.payload() {
                            if p.lease().is_some() {
                                self.deliveries.arena_deliveries += 1;
                            } else if p.device_buffer().is_some() {
                                self.deliveries.device_deliveries += 1;
                            } else {
                                self.deliveries.heap_deliveries += 1;
                            }
                        }
                        inputs.insert(port.name.clone(), v.clone())
                    }
                    (None, Channel::Control) => {
                        skip.get_or_insert(SkipReason::ControlAbsent(port.name.clone()));
                    }
                    (None, Channel::Data) if port.required => {
                        skip.get_or_insert(SkipReason::MissingInput(port.name.clone()));
                    }
                    (None, Channel::Data) => {}
                }
            }

            if let Some(reason) = skip {
                let slot = &mut self.slots[i];
                slot.stats.skipped += 1;
                observer.node(
                    cycle,
                    &slot.id,
                    slot.generation,
                    &NodeOutcome::Skipped(reason),
                );
                continue;
            }

            let is_source = self.graph.nodes[i].is_source();
            let mut attempts = 0u32;
            // On-demand: started the first time it has what it needs to run.
            if !self.slots[i].activated
                && let Err(error) = self.activate(i, cycle, cancel)
            {
                match self.recover(i, cycle, error, &mut attempts, cancel) {
                    Recovery::Retry => {}
                    Recovery::Skip(reason) => {
                        let slot = &self.slots[i];
                        observer.node(
                            cycle,
                            &slot.id,
                            slot.generation,
                            &NodeOutcome::Skipped(reason),
                        );
                        continue 'nodes;
                    }
                    Recovery::End(end) => return Some(end),
                    Recovery::Wait(error) => {
                        return Some(EndReason::NodeFailed {
                            node: self.slots[i].id.clone(),
                            cycle,
                            error,
                        });
                    }
                }
            }
            // Executes on the active generation. A candidate that fails during probation is rolled
            // back and the same inputs are re-executed on the restored generation (lossless).
            let (outputs, elapsed) = loop {
                let slot = &mut self.slots[i];
                let deadline = match slot.contract.spec.lifecycle.execute_timeout_ms {
                    0 => None,
                    ms => Some(Instant::now() + Duration::from_millis(ms)),
                };
                let ctx = ExecContext {
                    cycle,
                    node_id: &slot.id,
                    generation: slot.generation,
                    cancel,
                    deadline,
                };
                let t0 = Instant::now();
                let result = slot.handle.execute(&ctx, &inputs);
                let elapsed = t0.elapsed();
                let us = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
                slot.stats.executed += 1;
                slot.stats.total_us = slot.stats.total_us.saturating_add(us);
                slot.stats.max_us = slot.stats.max_us.max(us);

                let validated = match result {
                    Ok(Execution::Produced(outputs)) => validate_outputs(&slot.contract, outputs),
                    Ok(Execution::Exhausted) if is_source => {
                        return Some(EndReason::SourceExhausted {
                            node: slot.id.clone(),
                        });
                    }
                    Ok(Execution::Exhausted) => Err(NodeError::new(
                        NodeErrorKind::InvalidOutput,
                        "only a source may report exhaustion",
                    )),
                    Err(error) => Err(error),
                };
                match validated {
                    Ok(outputs) => {
                        if slot.standby.is_some() {
                            slot.probation = slot.probation.saturating_sub(1);
                        }
                        break (outputs, elapsed);
                    }
                    Err(error) if slot.standby.is_some() => {
                        let message = format!(
                            "generation {} failed at cycle {cycle}: {error}",
                            slot.generation
                        );
                        self.rollback(i, "replace.candidate-failed", message);
                    }
                    Err(error) => match self.recover(i, cycle, error, &mut attempts, cancel) {
                        Recovery::Retry => {}
                        Recovery::Skip(reason) => {
                            let slot = &self.slots[i];
                            observer.node(
                                cycle,
                                &slot.id,
                                slot.generation,
                                &NodeOutcome::Skipped(reason),
                            );
                            continue 'nodes;
                        }
                        Recovery::End(end) => return Some(end),
                        // The serial executor never waits (`can_wait: false`).
                        Recovery::Wait(error) => {
                            return Some(EndReason::NodeFailed {
                                node: self.slots[i].id.clone(),
                                cycle,
                                error,
                            });
                        }
                    },
                }
            };

            let count = outputs.len();
            if observer.wants_outputs(&self.slots[i].id) {
                observer.outputs(cycle, &self.slots[i].id, &outputs);
            }
            for (port, value) in outputs {
                values.insert((i, port), value);
            }
            let slot = &self.slots[i];
            observer.node(
                cycle,
                &slot.id,
                slot.generation,
                &NodeOutcome::Executed {
                    duration: elapsed,
                    outputs: count,
                },
            );
        }
        None
    }
}

/// Re-checks every emitted value against the active generation's declared outputs.
pub(crate) fn validate_outputs(
    contract: &NodeContract,
    outputs: Outputs,
) -> Result<std::collections::BTreeMap<String, Value>, NodeError> {
    let outputs = outputs.into_inner();
    for (port, value) in &outputs {
        let declared = contract.spec.ports.outputs.iter().find(|p| &p.name == port);
        let error = match declared {
            None => Some(format!("emitted undeclared output port `{port}`")),
            Some(p) if p.port_type != value.port_type() => Some(format!(
                "output `{port}` is declared `{}` but carried `{}`",
                p.port_type.as_str(),
                value.port_type().as_str()
            )),
            Some(_) => None,
        };
        if let Some(message) = error {
            return Err(NodeError::new(NodeErrorKind::InvalidOutput, message));
        }
    }
    Ok(outputs)
}
