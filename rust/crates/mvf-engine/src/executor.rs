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
//! - a source returning `Exhausted` ends the run before any non-source node runs in that cycle;
//! - a node failure ends the run (`fail` policy) and is reported with the node, cycle and cause;
//! - every emitted value is re-checked against the producer's declared output port and type.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use mvf_contract::{Channel, NodeContract};
use mvf_graph::Graph;
use serde::Serialize;

use crate::backend::{BackendRegistry, InstantiateRequest};
use crate::builtins::HostServices;
use crate::node::*;
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
    /// Per-node statistics.
    pub nodes: Vec<NodeStats>,
}

/// Run options.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Overrides the pipeline's `run.maxCycles`.
    pub max_cycles: Option<u64>,
    /// Cooperative cancellation, checked at cycle boundaries and observable by nodes.
    pub cancel: CancellationToken,
}

/// A node slot: the active handle plus its routing generation.
pub(crate) struct NodeSlot {
    pub(crate) id: String,
    pub(crate) contract: NodeContract,
    pub(crate) handle: Box<dyn NodeHandle>,
    pub(crate) generation: u64,
    pub(crate) stats: NodeStats,
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
            let (handle, backend) = backends
                .instantiate(&InstantiateRequest {
                    node_id: &node.id,
                    contract: &node.contract,
                    origin: &node.origin,
                    config: &node.config,
                    generation: 1,
                    services,
                })
                .map_err(|error| BuildError {
                    node: node.id.clone(),
                    error,
                })?;
            slots.push(NodeSlot {
                id: node.id.clone(),
                contract: node.contract.clone(),
                handle,
                generation: 1,
                stats: NodeStats {
                    id: node.id.clone(),
                    backend: backend.as_str().to_string(),
                    generation: 1,
                    ..NodeStats::default()
                },
            });
        }
        let mut exec_order: Vec<usize> = graph.order.clone();
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
        Ok(Self {
            graph,
            slots,
            exec_order,
            feeds,
            next_cycle: 1,
            started: false,
        })
    }

    /// The validated graph.
    pub fn graph(&self) -> &Graph {
        &self.graph
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
            let budget = Duration::from_millis(slot.contract.spec.lifecycle.readiness_timeout_ms);
            let ctx = LifecycleContext {
                node_id: &slot.id,
                generation: slot.generation,
                budget,
                cancel,
            };
            slot.handle.start(&ctx).map_err(|e| (slot.id.clone(), e))?;
        }
        self.started = true;
        Ok(())
    }

    /// Drains and stops every node (reverse execution order). Idempotent.
    pub fn shutdown(&mut self, cancel: &CancellationToken) {
        for &i in self.exec_order.iter().rev() {
            let slot = &mut self.slots[i];
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
        let end = match self.start(&options.cancel) {
            Err((node, error)) => EndReason::StartFailed { node, error },
            Ok(()) => self.run_cycles(options, observer, &mut |_| {}),
        };
        self.shutdown(&options.cancel);
        self.report(end, started_at)
    }

    pub(crate) fn report(&self, end: EndReason, started_at: Instant) -> RunReport {
        RunReport {
            pipeline: self.graph.name.clone(),
            end,
            cycles_completed: self.next_cycle - 1,
            wall_time_ms: started_at.elapsed().as_secs_f64() * 1e3,
            nodes: self.stats(),
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
        loop {
            boundary(self);
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
                return end;
            }
            self.next_cycle += 1;
            observer.cycle_completed(cycle);
        }
    }

    /// Executes one cycle. Returns `Some(end)` when the run must end.
    fn run_cycle(
        &mut self,
        cycle: u64,
        cancel: &CancellationToken,
        observer: &mut dyn Observer,
    ) -> Option<EndReason> {
        let mut values: HashMap<(usize, String), Value> = HashMap::new();
        for oi in 0..self.exec_order.len() {
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
                    (Some(v), _) => inputs.insert(port.name.clone(), v.clone()),
                    (None, Channel::Control) => {
                        skip.get_or_insert(SkipReason::ControlAbsent(port.name.clone()));
                    }
                    (None, Channel::Data) if port.required => {
                        skip.get_or_insert(SkipReason::MissingInput(port.name.clone()));
                    }
                    (None, Channel::Data) => {}
                }
            }

            let slot = &mut self.slots[i];
            if let Some(reason) = skip {
                slot.stats.skipped += 1;
                observer.node(
                    cycle,
                    &slot.id,
                    slot.generation,
                    &NodeOutcome::Skipped(reason),
                );
                continue;
            }

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

            let outputs = match result {
                Ok(Execution::Produced(outputs)) => outputs,
                Ok(Execution::Exhausted) if node.is_source() => {
                    return Some(EndReason::SourceExhausted {
                        node: slot.id.clone(),
                    });
                }
                Ok(Execution::Exhausted) => {
                    return Some(EndReason::NodeFailed {
                        node: slot.id.clone(),
                        cycle,
                        error: NodeError::new(
                            NodeErrorKind::InvalidOutput,
                            "only a source may report exhaustion",
                        ),
                    });
                }
                Err(error) => {
                    return Some(EndReason::NodeFailed {
                        node: slot.id.clone(),
                        cycle,
                        error,
                    });
                }
            };

            let count = outputs.len();
            for (port, value) in outputs.into_inner() {
                let declared = node
                    .contract
                    .spec
                    .ports
                    .outputs
                    .iter()
                    .find(|p| p.name == port);
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
                    return Some(EndReason::NodeFailed {
                        node: slot.id.clone(),
                        cycle,
                        error: NodeError::new(NodeErrorKind::InvalidOutput, message),
                    });
                }
                values.insert((i, port), value);
            }
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
