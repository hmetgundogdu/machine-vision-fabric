//! Stage-parallel executor (the .NET `PipelinedGraphExecutor`): every node with inputs runs on its own
//! thread, connected by **bounded per-edge queues**, so while one stage works on cycle N its upstream
//! is already on N+1 and throughput approaches the slowest stage instead of the sum of them all.
//!
//! - **One message per edge per cycle.** A value, or a *void marker* when the producer emitted nothing
//!   on that port (closed `if`, untaken `switch` branch, skipped node, dropped cycle). A stage reads one
//!   message from every inbound edge, so a join pairs its inputs by position and never waits for a
//!   branch that was not taken; the cycle ids are checked to agree.
//! - **The driver** (the engine thread) runs every node without inbound edges — sources first, then
//!   `value` primitives and unconnected nodes — once per cycle, like the serial executor, and owns
//!   iteration: limits, the `loop` (`forever` rewinds inline, `count`), cancellation.
//! - **Backpressure is real.** A full edge queue blocks its producer. A full arena under `stall` waits
//!   for downstream to release a slot (bounded, then an actionable error); `drop` skips the cycle.
//! - **Order is preserved.** One thread per stage and FIFO queues keep source order; a replicated stage
//!   (`parallelism: N`, allowed by the contract's `maxParallelism`) runs N instances and re-orders
//!   their results by cycle before routing.
//! - **Quiesce points.** The run proceeds in *segments*: the driver stops feeding, the stages drain and
//!   join, and the engine has exclusive access again — the cycle boundary of the serial executor. That
//!   is where epoch checkpoints are captured (nothing in flight), control commands, pauses and live
//!   edits are applied. While a live replacement prepares or is in probation the run proceeds one
//!   serial cycle at a time, so cut-over and rollback keep their exact semantics.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mvf_contract::{BackendKind, Channel};
use mvf_graph::{Graph, LoopMode};

use crate::backend::{BackendRegistry, InstantiateRequest};
use crate::builtins::HostServices;
use crate::executor::{
    DataPlaneReport, EndReason, Engine, NodeOutcome, NodeSlot, Observer, RunOptions, SkipReason,
};
use crate::node::*;
use crate::recovery::{
    Recovery, RecoveryCtx, RecoveryEvent, RecoveryKind, note_event, recover_slot,
};
use crate::value::Value;

/// Default depth of each inter-stage queue (the .NET `--queue` default).
pub const DEFAULT_QUEUE: usize = 2;

/// How long a `stall` waits for downstream to release an arena slot before it gives up.
const STALL_LIMIT: Duration = Duration::from_secs(30);

/// One message on an edge: the cycle it belongs to and the value (`None` = void marker).
type Message = (u64, Option<Value>);

/// Arena slots a pipelined run needs (`DataPlaneSizing.RequiredSlots`): every out-of-process node can
/// hold its inbound queue plus three frames per instance (work queue, executing, re-order buffer);
/// an arena-publishing source holds a queue per outgoing edge. Never below `minimum`.
pub fn arena_slots_for(graph: &Graph, queue: usize, minimum: usize) -> usize {
    let queue = queue.max(1);
    let mut total = 1;
    for node in &graph.nodes {
        let out_of_process = node
            .contract
            .spec
            .artifacts
            .iter()
            .any(|a| matches!(a.backend(), BackendKind::Worker | BackendKind::Oci));
        if out_of_process {
            total += queue + 3 * node.parallelism.max(1) as usize;
        } else if node.is_source()
            && node.config.get("arena") == Some(&serde_json::Value::Bool(true))
        {
            let edges = graph.edges().filter(|e| e.from.node == node.index).count();
            total += queue * edges.max(1) + 2;
        }
    }
    total.max(minimum)
}

/// Marks the run as ending at `end`'s cycle **before** anything else is routed, so no node downstream
/// of a failure ever sees a later cycle (branches independent of the failure may have run ahead by at
/// most their queue depth — inherent to pipelining).
fn abort(abort_at: &AtomicU64, end: &EndReason) {
    let cycle = match end {
        EndReason::NodeFailed { cycle, .. } => *cycle,
        _ => 1,
    };
    abort_at.fetch_min(cycle.max(1), Ordering::SeqCst);
}

/// Events stages send to the driver (unbounded: a stage never blocks on reporting).
enum StageEvent {
    Node {
        cycle: u64,
        node: usize,
        generation: u64,
        outcome: NodeOutcome,
    },
    Done {
        cycle: u64,
    },
    Fatal(EndReason),
}

/// What a stage hands back when it joins.
#[derive(Default)]
struct StageReport {
    recoveries: Vec<RecoveryEvent>,
    live: Vec<crate::live::LiveEvent>,
    deliveries: DataPlaneReport,
}

/// Why a segment ended.
enum SegmentEnd {
    /// Quiesced at a boundary; the run continues.
    Quiesce,
    /// The run ends.
    End(EndReason),
}

/// A placeholder while a stage's handles are lent to its instance threads.
struct Lent;

impl NodeHandle for Lent {
    fn execute(&mut self, _: &ExecContext<'_>, _: &Inputs) -> Result<Execution, NodeError> {
        Err(NodeError::new(
            NodeErrorKind::Failed,
            "handle is lent to a parallel instance",
        ))
    }
}

impl Engine {
    /// Runs cycles stage-parallel until an end condition (`RunOptions::pipelined`).
    pub(crate) fn run_cycles_pipelined(
        &mut self,
        options: &RunOptions,
        observer: &mut dyn Observer,
    ) -> EndReason {
        let queue = options.queue.unwrap_or(DEFAULT_QUEUE).max(1);
        if let Err(end) = self.ensure_replicas(&options.cancel) {
            return end;
        }
        let mut produced_since_rewind = false;
        loop {
            self.on_boundary(&options.cancel);
            if options.cancel.is_cancelled() {
                return EndReason::Cancelled;
            }
            let limit = options.max_cycles.or(self.graph.max_cycles);
            if let Some(limit) = limit
                && self.next_cycle > limit
            {
                return EndReason::CycleLimit { limit };
            }
            if self
                .slots
                .iter()
                .any(|s| s.pending.is_some() || s.standby.is_some())
            {
                // Live replacement window: exact serial semantics, one cycle per boundary.
                if let Some(end) =
                    self.serial_step(&options.cancel, observer, &mut produced_since_rewind)
                {
                    return end;
                }
                continue;
            }
            match self.run_segment(options, observer, queue, &mut produced_since_rewind) {
                SegmentEnd::Quiesce => {}
                SegmentEnd::End(end) => return end,
            }
        }
    }

    /// One serial cycle with the loop's `forever`/`count` handling.
    fn serial_step(
        &mut self,
        cancel: &CancellationToken,
        observer: &mut dyn Observer,
        produced_since_rewind: &mut bool,
    ) -> Option<EndReason> {
        let cycle = self.next_cycle;
        let loop_spec = self.graph.loop_spec;
        if let Some(end) = self.run_cycle(cycle, cancel, observer) {
            if let EndReason::SourceExhausted { .. } = &end
                && loop_spec.is_some_and(|l| l.mode == LoopMode::Forever)
                && *produced_since_rewind
                && self.rewind_sources()
            {
                *produced_since_rewind = false;
                return None;
            }
            return Some(end);
        }
        *produced_since_rewind = true;
        self.next_cycle += 1;
        self.maybe_checkpoint(cycle);
        observer.cycle_completed(cycle);
        match loop_spec {
            Some(l) if l.mode == LoopMode::Count && l.count.is_some_and(|c| cycle >= c) => {
                Some(EndReason::LoopCompleted {
                    count: l.count.unwrap_or(cycle),
                })
            }
            _ => None,
        }
    }

    /// Builds and starts the extra instances of replicated nodes (once per run).
    fn ensure_replicas(&mut self, cancel: &CancellationToken) -> Result<(), EndReason> {
        for i in 0..self.slots.len() {
            let want = self.graph.nodes[i].parallelism.max(1) as usize;
            while self.slots[i].replicas.len() + 1 < want {
                let slot = &self.slots[i];
                let (mut handle, _) = self
                    .backends
                    .instantiate(&InstantiateRequest {
                        node_id: &slot.id,
                        contract: &slot.contract,
                        origin: &slot.origin,
                        config: &slot.config,
                        generation: slot.generation,
                        services: &self.services,
                    })
                    .map_err(|error| EndReason::StartFailed {
                        node: slot.id.clone(),
                        error,
                    })?;
                let ctx = LifecycleContext {
                    node_id: &slot.id,
                    generation: slot.generation,
                    budget: Duration::from_millis(
                        slot.contract.spec.lifecycle.readiness_timeout_ms,
                    ),
                    cancel,
                };
                handle.start(&ctx).map_err(|error| EndReason::StartFailed {
                    node: slot.id.clone(),
                    error,
                })?;
                self.slots[i].replicas.push(handle);
            }
        }
        Ok(())
    }

    /// Runs cycles until a quiesce point or the end, then drains and joins every stage.
    fn run_segment(
        &mut self,
        options: &RunOptions,
        observer: &mut dyn Observer,
        queue: usize,
        produced_since_rewind: &mut bool,
    ) -> SegmentEnd {
        let cancel = &options.cancel;
        let limit = options.max_cycles.or(self.graph.max_cycles);
        let first = self.next_cycle;
        let graph = &self.graph;
        let edges: Vec<_> = graph.edges().cloned().collect();
        let has_inbound: Vec<bool> = (0..graph.nodes.len())
            .map(|i| edges.iter().any(|e| e.to.node == i))
            .collect();
        let order: Vec<usize> = self.exec_order().to_vec();
        let drivers: Vec<usize> = order.iter().copied().filter(|&i| !has_inbound[i]).collect();
        let stages: Vec<usize> = order.iter().copied().filter(|&i| has_inbound[i]).collect();

        // One bounded queue per edge.
        let mut senders: Vec<Option<SyncSender<Message>>> = Vec::with_capacity(edges.len());
        let mut receivers: Vec<Option<Receiver<Message>>> = Vec::with_capacity(edges.len());
        for _ in &edges {
            let (tx, rx) = mpsc::sync_channel(queue);
            senders.push(Some(tx));
            receivers.push(Some(rx));
        }
        let outbound = |i: usize,
                        senders: &mut Vec<Option<SyncSender<Message>>>|
         -> Vec<(String, SyncSender<Message>)> {
            edges
                .iter()
                .enumerate()
                .filter(|(_, e)| e.from.node == i)
                .filter_map(|(k, e)| senders[k].take().map(|tx| (e.from.port.clone(), tx)))
                .collect()
        };

        let (events_tx, events_rx) = mpsc::channel::<StageEvent>();
        let abort_at = AtomicU64::new(u64::MAX);
        let every = self.checkpoint.every;
        let loop_spec = graph.loop_spec;
        let ctx = RecoveryCtx {
            backends: &self.backends,
            services: &self.services,
            last_states: &self.checkpoint.last,
            can_wait: true,
        };

        // Lend each slot to its owner: stage threads get theirs, the driver keeps the rest.
        let mut lent: Vec<Option<&mut NodeSlot>> = self.slots.iter_mut().map(Some).collect();
        let mut driver_slots: Vec<(usize, &mut NodeSlot)> = drivers
            .iter()
            .filter_map(|&i| lent[i].take().map(|s| (i, s)))
            .collect();
        let driver_out: Vec<Vec<(String, SyncSender<Message>)>> =
            drivers.iter().map(|&i| outbound(i, &mut senders)).collect();
        let mut stage_parts = Vec::with_capacity(stages.len());
        for &i in &stages {
            let inbound: Vec<(String, Receiver<Message>)> = edges
                .iter()
                .enumerate()
                .filter(|(_, e)| e.to.node == i)
                .filter_map(|(k, e)| receivers[k].take().map(|rx| (e.to.port.clone(), rx)))
                .collect();
            let out = outbound(i, &mut senders);
            if let Some(slot) = lent[i].take() {
                stage_parts.push((i, slot, inbound, out));
            }
        }
        let stage_count = stage_parts.len() as u64;
        let nodes = &graph.nodes;
        let control_rx = &self.control_rx;
        let stashed = &mut self.stashed;
        let live_log = &mut self.live_log;
        let iteration = &mut self.iteration;

        let mut reports: Vec<StageReport> = Vec::new();
        let mut driver_recoveries: Vec<RecoveryEvent> = Vec::new();
        let mut driver_deliveries = DataPlaneReport::default();
        let mut completed = first - 1;
        let mut done: BTreeMap<u64, u64> = BTreeMap::new();
        let mut fatal: Option<EndReason> = None;

        let outcome = std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(stage_parts.len());
            for (i, slot, inbound, out) in stage_parts {
                let events = events_tx.clone();
                let node = &nodes[i];
                let ctx = &ctx;
                let abort_at = &abort_at;
                handles.push(scope.spawn(move || {
                    if slot.replicas.is_empty() {
                        run_stage(i, node, slot, inbound, out, ctx, cancel, &events, abort_at)
                    } else {
                        run_parallel_stage(
                            i, node, slot, inbound, out, ctx, cancel, &events, abort_at,
                        )
                    }
                }));
            }
            drop(events_tx);

            let forward = |ev: StageEvent,
                           completed: &mut u64,
                           done: &mut BTreeMap<u64, u64>,
                           fatal: &mut Option<EndReason>,
                           observer: &mut dyn Observer| match ev {
                StageEvent::Node {
                    cycle,
                    node,
                    generation,
                    outcome,
                } => observer.node(cycle, &nodes[node].id, generation, &outcome),
                StageEvent::Done { cycle } => {
                    *done.entry(cycle).or_default() += 1;
                    while done.get(&(*completed + 1)).copied() == Some(stage_count) {
                        done.remove(&(*completed + 1));
                        *completed += 1;
                        observer.cycle_completed(*completed);
                    }
                }
                StageEvent::Fatal(end) => {
                    let cycle = match &end {
                        EndReason::NodeFailed { cycle, .. } => *cycle,
                        _ => 0,
                    };
                    abort_at.fetch_min(cycle.max(1), Ordering::SeqCst);
                    let earlier = match (&*fatal, &end) {
                        (
                            Some(EndReason::NodeFailed { cycle: a, .. }),
                            EndReason::NodeFailed { cycle: b, .. },
                        ) => b < a,
                        (Some(_), _) => false,
                        (None, _) => true,
                    };
                    if earlier {
                        *fatal = Some(end);
                    }
                }
            };

            // ---- the driver ----
            let mut cycle = first;
            let end = 'driver: loop {
                while let Ok(ev) = events_rx.try_recv() {
                    forward(ev, &mut completed, &mut done, &mut fatal, observer);
                }
                if abort_at.load(Ordering::SeqCst) != u64::MAX {
                    break 'driver None;
                }
                if cancel.is_cancelled() {
                    break 'driver Some(EndReason::Cancelled);
                }
                if let Some(limit) = limit
                    && cycle > limit
                {
                    break 'driver Some(EndReason::CycleLimit { limit });
                }
                if cycle > first {
                    if let Ok(command) = control_rx.try_recv() {
                        stashed.push(command);
                        break 'driver None; // quiesce: commands apply at a boundary
                    }
                    if every > 0 && (cycle - 1).is_multiple_of(every) {
                        break 'driver None; // epoch barrier after cycle - 1
                    }
                }
                // Every driver node, in execution order (sources first).
                let mut produced: Vec<(usize, BTreeMap<String, Value>)> = Vec::new();
                let mut k = 0;
                while k < driver_slots.len() {
                    let (i, slot) = &mut driver_slots[k];
                    let i = *i;
                    match drive(
                        &nodes[i],
                        slot,
                        cycle,
                        &ctx,
                        cancel,
                        observer,
                        &mut driver_recoveries,
                        live_log,
                    ) {
                        Driven::Produced(outputs) => produced.push((k, outputs)),
                        Driven::Skipped => produced.push((k, BTreeMap::new())),
                        Driven::Exhausted => {
                            if loop_spec.is_some_and(|l| l.mode == LoopMode::Forever)
                                && *produced_since_rewind
                            {
                                let mut rewound = 0;
                                for (j, s) in driver_slots.iter_mut() {
                                    if nodes[*j].is_source()
                                        && matches!(s.handle.rewind(), Ok(true))
                                    {
                                        rewound += 1;
                                    }
                                }
                                if rewound > 0 {
                                    *produced_since_rewind = false;
                                    iteration.rewinds += 1;
                                    live_log.push(crate::live::LiveEvent {
                                        cycle,
                                        node: None,
                                        field: None,
                                        kind: crate::live::LiveEventKind::Rewound,
                                        persisted: false,
                                        message: Some(format!("{rewound} source(s) rewound")),
                                    });
                                    produced.clear();
                                    k = 0;
                                    continue;
                                }
                            }
                            break 'driver Some(EndReason::SourceExhausted {
                                node: nodes[i].id.clone(),
                            });
                        }
                        Driven::End(end) => break 'driver Some(end),
                    }
                    k += 1;
                }
                // Route: one message per outgoing edge (value or void). A full queue blocks here.
                for (k, outputs) in produced {
                    for (port, tx) in &driver_out[k] {
                        let value = outputs.get(port).cloned();
                        if let Some(v) = &value {
                            count_delivery(v, &mut driver_deliveries);
                        }
                        let _ = tx.send((cycle, value));
                    }
                }
                *produced_since_rewind = true;
                if stage_count == 0 {
                    completed = cycle;
                    observer.cycle_completed(cycle);
                }
                let finished = cycle;
                cycle += 1;
                if let Some(l) = loop_spec
                    && l.mode == LoopMode::Count
                    && l.count.is_some_and(|c| finished >= c)
                {
                    break 'driver Some(EndReason::LoopCompleted {
                        count: l.count.unwrap_or(finished),
                    });
                }
            };
            // Drain: closing the driver's queues lets every stage finish what is in flight.
            drop(driver_out);
            for h in handles {
                if let Ok(report) = h.join() {
                    reports.push(report);
                }
                while let Ok(ev) = events_rx.try_recv() {
                    forward(ev, &mut completed, &mut done, &mut fatal, observer);
                }
            }
            while let Ok(ev) = events_rx.try_recv() {
                forward(ev, &mut completed, &mut done, &mut fatal, observer);
            }
            end
        });

        // Merge what the stages and the driver recorded.
        let mut recoveries = driver_recoveries;
        for r in &mut reports {
            recoveries.append(&mut r.recoveries);
            self.live_log.append(&mut r.live);
            self.deliveries.arena_deliveries += r.deliveries.arena_deliveries;
            self.deliveries.heap_deliveries += r.deliveries.heap_deliveries;
            self.deliveries.device_deliveries += r.deliveries.device_deliveries;
        }
        self.deliveries.arena_deliveries += driver_deliveries.arena_deliveries;
        self.deliveries.heap_deliveries += driver_deliveries.heap_deliveries;
        self.deliveries.device_deliveries += driver_deliveries.device_deliveries;
        recoveries.sort_by_key(|e| e.cycle);
        self.recovery_log.extend(recoveries);
        self.next_cycle = completed + 1;
        if completed >= first {
            self.maybe_checkpoint(completed);
        }
        match (fatal, outcome) {
            (Some(end), _) => SegmentEnd::End(end),
            (None, Some(end)) => SegmentEnd::End(end),
            (None, None) => SegmentEnd::Quiesce,
        }
    }
}

fn count_delivery(value: &Value, d: &mut DataPlaneReport) {
    if let Some(p) = value.payload() {
        if p.lease().is_some() {
            d.arena_deliveries += 1;
        } else if p.device_buffer().is_some() {
            d.device_deliveries += 1;
        } else {
            d.heap_deliveries += 1;
        }
    }
}

enum Driven {
    Produced(BTreeMap<String, Value>),
    Skipped,
    Exhausted,
    End(EndReason),
}

/// Runs one driver node (no inbound edges) for `cycle`.
#[allow(clippy::too_many_arguments)]
fn drive(
    node: &mvf_graph::GraphNode,
    slot: &mut NodeSlot,
    cycle: u64,
    ctx: &RecoveryCtx<'_>,
    cancel: &CancellationToken,
    observer: &mut dyn Observer,
    log: &mut Vec<RecoveryEvent>,
    live: &mut Vec<crate::live::LiveEvent>,
) -> Driven {
    let inputs = Inputs::new();
    match execute_slot(slot, node, cycle, &inputs, ctx, cancel, log, Some(live)) {
        Executed::Outputs(outputs, duration) => {
            observer.node(
                cycle,
                &slot.id,
                slot.generation,
                &NodeOutcome::Executed {
                    duration,
                    outputs: outputs.len(),
                },
            );
            Driven::Produced(outputs)
        }
        Executed::Skipped(reason) => {
            observer.node(
                cycle,
                &slot.id,
                slot.generation,
                &NodeOutcome::Skipped(reason),
            );
            Driven::Skipped
        }
        Executed::Exhausted => Driven::Exhausted,
        Executed::End(end) => Driven::End(end),
    }
}

enum Executed {
    Outputs(BTreeMap<String, Value>, Duration),
    Skipped(SkipReason),
    Exhausted,
    End(EndReason),
}

/// Executes `slot` once for `cycle` under its policies: on-demand activation, failure policy,
/// pipelined `stall` (wait for a drain), output validation.
#[allow(clippy::too_many_arguments)]
fn execute_slot(
    slot: &mut NodeSlot,
    node: &mvf_graph::GraphNode,
    cycle: u64,
    inputs: &Inputs,
    ctx: &RecoveryCtx<'_>,
    cancel: &CancellationToken,
    log: &mut Vec<RecoveryEvent>,
    live: Option<&mut Vec<crate::live::LiveEvent>>,
) -> Executed {
    let mut attempts = 0u32;
    let mut stalled: Option<Instant> = None;
    if !slot.activated {
        let started = activate_slot(slot, cycle, cancel);
        match started {
            Ok(()) => {
                if let Some(live) = live {
                    live.push(crate::live::LiveEvent {
                        cycle,
                        node: Some(slot.id.clone()),
                        field: None,
                        kind: crate::live::LiveEventKind::Activated,
                        persisted: false,
                        message: None,
                    });
                }
            }
            Err(error) => match recover_slot(slot, ctx, cycle, error, &mut attempts, cancel, log) {
                Recovery::Retry => {}
                Recovery::Skip(reason) => return Executed::Skipped(reason),
                Recovery::End(end) => return Executed::End(end),
                Recovery::Wait(error) => {
                    return Executed::End(EndReason::NodeFailed {
                        node: slot.id.clone(),
                        cycle,
                        error,
                    });
                }
            },
        }
    }
    loop {
        let deadline = match slot.contract.spec.lifecycle.execute_timeout_ms {
            0 => None,
            ms => Some(Instant::now() + Duration::from_millis(ms)),
        };
        let exec = ExecContext {
            cycle,
            node_id: &slot.id,
            generation: slot.generation,
            cancel,
            deadline,
        };
        let t0 = Instant::now();
        let result = slot.handle.execute(&exec, inputs);
        let elapsed = t0.elapsed();
        let us = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        slot.stats.executed += 1;
        slot.stats.total_us = slot.stats.total_us.saturating_add(us);
        slot.stats.max_us = slot.stats.max_us.max(us);
        let validated = match result {
            Ok(Execution::Produced(outputs)) => {
                crate::executor::validate_outputs(&slot.contract, outputs)
            }
            Ok(Execution::Exhausted) if node.is_source() => return Executed::Exhausted,
            Ok(Execution::Exhausted) => Err(NodeError::new(
                NodeErrorKind::InvalidOutput,
                "only a source may report exhaustion",
            )),
            Err(e) => Err(e),
        };
        match validated {
            Ok(outputs) => return Executed::Outputs(outputs, elapsed),
            Err(error) => match recover_slot(slot, ctx, cycle, error, &mut attempts, cancel, log) {
                Recovery::Retry => {}
                Recovery::Skip(reason) => return Executed::Skipped(reason),
                Recovery::End(end) => return Executed::End(end),
                Recovery::Wait(error) => {
                    let since = *stalled.get_or_insert_with(Instant::now);
                    if since.elapsed() > STALL_LIMIT || cancel.is_cancelled() {
                        let mut e = error;
                        e.message = format!(
                            "{} (backpressure: stall waited {} s for downstream to release an arena slot — raise the arena slot count, or set `backpressure: drop` on this node)",
                            e.message,
                            STALL_LIMIT.as_secs()
                        );
                        return Executed::End(EndReason::NodeFailed {
                            node: slot.id.clone(),
                            cycle,
                            error: e,
                        });
                    }
                    slot.stats.executed -= 1;
                    std::thread::sleep(Duration::from_millis(1));
                }
            },
        }
    }
}

fn activate_slot(
    slot: &mut NodeSlot,
    cycle: u64,
    cancel: &CancellationToken,
) -> Result<(), NodeError> {
    let ctx = LifecycleContext {
        node_id: &slot.id,
        generation: slot.generation,
        budget: Duration::from_millis(slot.contract.spec.lifecycle.readiness_timeout_ms),
        cancel,
    };
    slot.handle.start(&ctx)?;
    slot.activated = true;
    slot.stats.activated_at_cycle.get_or_insert(cycle);
    if let Some(state) = slot.pending_state.take() {
        slot.handle.import_state(&state)?;
    }
    Ok(())
}

/// One cycle's messages, by input port.
type Joined = (u64, Vec<(String, Option<Value>)>);

/// Reads one message from every inbound edge; `None` when upstream closed. Fails loudly when the
/// edges are out of step (the void-marker invariant was broken).
fn join_inputs(
    node: &mvf_graph::GraphNode,
    inbound: &[(String, Receiver<Message>)],
) -> Option<Result<Joined, String>> {
    let mut cycle = None;
    let mut values = Vec::with_capacity(inbound.len());
    for (port, rx) in inbound {
        let (c, v) = rx.recv().ok()?;
        match cycle {
            None => cycle = Some(c),
            Some(first) if first != c => {
                return Some(Err(format!(
                    "node `{}` joined inputs from different cycles ({first} vs {c}) — edge messages are out of step",
                    node.id
                )));
            }
            Some(_) => {}
        }
        values.push((port.clone(), v));
    }
    cycle.map(|c| Ok((c, values)))
}

/// Applies the serial gating rules: required data present, connected control present and true.
fn gate(
    node: &mvf_graph::GraphNode,
    values: Vec<(String, Option<Value>)>,
    deliveries: &mut DataPlaneReport,
) -> Result<Inputs, SkipReason> {
    let mut inputs = Inputs::new();
    let mut skip = None;
    let by_port: HashMap<String, Option<Value>> = values.into_iter().collect();
    for port in &node.contract.spec.ports.inputs {
        let Some(value) = by_port.get(&port.name) else {
            continue; // optional and unconnected
        };
        match (value, port.channel) {
            (Some(Value::Signal(false)), Channel::Control) => {
                skip.get_or_insert(SkipReason::ControlFalse(port.name.clone()));
            }
            (Some(v), _) => {
                count_delivery(v, deliveries);
                inputs.insert(port.name.clone(), v.clone());
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
    match skip {
        Some(reason) => Err(reason),
        None => Ok(inputs),
    }
}

fn send_all(out: &[(String, SyncSender<Message>)], cycle: u64, outputs: &BTreeMap<String, Value>) {
    for (port, tx) in out {
        let _ = tx.send((cycle, outputs.get(port).cloned()));
    }
}

/// One stage thread: join, gate, execute, route — one cycle at a time, in order.
#[allow(clippy::too_many_arguments)]
fn run_stage(
    index: usize,
    node: &mvf_graph::GraphNode,
    slot: &mut NodeSlot,
    inbound: Vec<(String, Receiver<Message>)>,
    out: Vec<(String, SyncSender<Message>)>,
    ctx: &RecoveryCtx<'_>,
    cancel: &CancellationToken,
    events: &Sender<StageEvent>,
    abort_at: &AtomicU64,
) -> StageReport {
    let mut report = StageReport::default();
    let empty = BTreeMap::new();
    while let Some(joined) = join_inputs(node, &inbound) {
        let (cycle, values) = match joined {
            Ok(j) => j,
            Err(message) => {
                abort_at.fetch_min(1, Ordering::SeqCst);
                let _ = events.send(StageEvent::Fatal(EndReason::NodeFailed {
                    node: slot.id.clone(),
                    cycle: 0,
                    error: NodeError::new(NodeErrorKind::Failed, message),
                }));
                return report;
            }
        };
        // A failure earlier in the run: later cycles are not executed (as if the run had ended).
        if cycle >= abort_at.load(Ordering::SeqCst) {
            send_all(&out, cycle, &empty);
            continue;
        }
        let outcome = match gate(node, values, &mut report.deliveries) {
            Err(reason) => {
                slot.stats.skipped += 1;
                send_all(&out, cycle, &empty);
                NodeOutcome::Skipped(reason)
            }
            Ok(inputs) => {
                match execute_slot(
                    slot,
                    node,
                    cycle,
                    &inputs,
                    ctx,
                    cancel,
                    &mut report.recoveries,
                    Some(&mut report.live),
                ) {
                    Executed::Outputs(outputs, duration) => {
                        send_all(&out, cycle, &outputs);
                        NodeOutcome::Executed {
                            duration,
                            outputs: outputs.len(),
                        }
                    }
                    Executed::Skipped(reason) => {
                        send_all(&out, cycle, &empty);
                        NodeOutcome::Skipped(reason)
                    }
                    Executed::Exhausted => {
                        send_all(&out, cycle, &empty);
                        NodeOutcome::Skipped(SkipReason::Failure("exhausted".into()))
                    }
                    Executed::End(end) => {
                        abort(abort_at, &end);
                        let _ = events.send(StageEvent::Fatal(end));
                        send_all(&out, cycle, &empty);
                        continue;
                    }
                }
            }
        };
        let _ = events.send(StageEvent::Node {
            cycle,
            node: index,
            generation: slot.generation,
            outcome,
        });
        let _ = events.send(StageEvent::Done { cycle });
    }
    report
}

/// A replicated stage: N instances pull from a shared work queue; results are re-ordered by cycle
/// before routing (strict order at the sinks).
#[allow(clippy::too_many_arguments)]
fn run_parallel_stage(
    index: usize,
    node: &mvf_graph::GraphNode,
    slot: &mut NodeSlot,
    inbound: Vec<(String, Receiver<Message>)>,
    out: Vec<(String, SyncSender<Message>)>,
    ctx: &RecoveryCtx<'_>,
    cancel: &CancellationToken,
    events: &Sender<StageEvent>,
    abort_at: &AtomicU64,
) -> StageReport {
    type Work = (u64, Inputs);
    type Done = (
        u64,
        Result<(BTreeMap<String, Value>, Duration), Executed>,
        Vec<RecoveryEvent>,
        u64,
    );
    let mut report = StageReport::default();
    let mut handles: Vec<Box<dyn NodeHandle>> =
        vec![std::mem::replace(&mut slot.handle, Box::new(Lent))];
    handles.append(&mut slot.replicas);
    let instances = handles.len();
    let (work_tx, work_rx) = mpsc::sync_channel::<Work>(instances);
    let work_rx = Arc::new(Mutex::new(work_rx));
    let (done_tx, done_rx) = mpsc::channel::<Done>();
    let meta = SlotMeta::of(slot);
    let empty = BTreeMap::new();
    let returned = std::thread::scope(|scope| {
        let mut workers = Vec::with_capacity(instances);
        for handle in handles {
            let work_rx = Arc::clone(&work_rx);
            let done_tx = done_tx.clone();
            let meta = &meta;
            workers.push(scope.spawn(move || {
                let mut handle = handle;
                loop {
                    let next = work_rx.lock().ok().and_then(|rx| rx.recv().ok());
                    let Some((cycle, inputs)) = next else {
                        break;
                    };
                    let mut log = Vec::new();
                    let (result, restarts) =
                        meta.execute(&mut handle, node, cycle, &inputs, ctx, cancel, &mut log);
                    let _ = done_tx.send((cycle, result, log, restarts));
                }
                handle
            }));
        }
        drop(done_tx);

        // Dispatch and emit in cycle order.
        let mut pending: BTreeMap<u64, Option<Done>> = BTreeMap::new();
        let mut next_emit: Option<u64> = None;
        let mut in_flight = 0usize;
        let emit = |pending: &mut BTreeMap<u64, Option<Done>>,
                    next_emit: &mut Option<u64>,
                    slot: &mut NodeSlot,
                    report: &mut StageReport| {
            while let Some(c) = *next_emit {
                let Some(entry) = pending.remove(&c) else {
                    break;
                };
                let outcome = match entry {
                    None => {
                        send_all(&out, c, &empty);
                        None
                    }
                    Some((_, result, mut log, restarts)) => {
                        report.recoveries.append(&mut log);
                        slot.stats.restarts += restarts;
                        Some(match result {
                            Ok((outputs, duration)) => {
                                let us = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
                                slot.stats.executed += 1;
                                slot.stats.total_us = slot.stats.total_us.saturating_add(us);
                                slot.stats.max_us = slot.stats.max_us.max(us);
                                send_all(&out, c, &outputs);
                                NodeOutcome::Executed {
                                    duration,
                                    outputs: outputs.len(),
                                }
                            }
                            Err(Executed::Skipped(reason)) => {
                                slot.stats.skipped += 1;
                                send_all(&out, c, &empty);
                                NodeOutcome::Skipped(reason)
                            }
                            Err(Executed::End(end)) => {
                                abort(abort_at, &end);
                                let _ = events.send(StageEvent::Fatal(end));
                                send_all(&out, c, &empty);
                                NodeOutcome::Skipped(SkipReason::Failure("run ended".into()))
                            }
                            Err(_) => {
                                send_all(&out, c, &empty);
                                NodeOutcome::Skipped(SkipReason::Failure("exhausted".into()))
                            }
                        })
                    }
                };
                if let Some(outcome) = outcome {
                    let _ = events.send(StageEvent::Node {
                        cycle: c,
                        node: index,
                        generation: slot.generation,
                        outcome,
                    });
                }
                let _ = events.send(StageEvent::Done { cycle: c });
                *next_emit = Some(c + 1);
            }
        };
        while let Some(joined) = join_inputs(node, &inbound) {
            let (cycle, values) = match joined {
                Ok(j) => j,
                Err(message) => {
                    abort_at.fetch_min(1, Ordering::SeqCst);
                    let _ = events.send(StageEvent::Fatal(EndReason::NodeFailed {
                        node: slot.id.clone(),
                        cycle: 0,
                        error: NodeError::new(NodeErrorKind::Failed, message),
                    }));
                    break;
                }
            };
            next_emit.get_or_insert(cycle);
            if cycle >= abort_at.load(Ordering::SeqCst) {
                pending.insert(cycle, None);
            } else {
                match gate(node, values, &mut report.deliveries) {
                    Err(reason) => {
                        slot.stats.skipped += 1;
                        let _ = events.send(StageEvent::Node {
                            cycle,
                            node: index,
                            generation: slot.generation,
                            outcome: NodeOutcome::Skipped(reason),
                        });
                        pending.insert(cycle, None);
                    }
                    Ok(inputs) => {
                        in_flight += 1;
                        if work_tx.send((cycle, inputs)).is_err() {
                            break;
                        }
                    }
                }
            }
            while let Ok(d) = done_rx.try_recv() {
                in_flight -= 1;
                pending.insert(d.0, Some(d));
            }
            emit(&mut pending, &mut next_emit, slot, &mut report);
        }
        drop(work_tx);
        while in_flight > 0 {
            match done_rx.recv() {
                Ok(d) => {
                    in_flight -= 1;
                    pending.insert(d.0, Some(d));
                    emit(&mut pending, &mut next_emit, slot, &mut report);
                }
                Err(_) => break,
            }
        }
        emit(&mut pending, &mut next_emit, slot, &mut report);
        workers
            .into_iter()
            .filter_map(|w| w.join().ok())
            .collect::<Vec<_>>()
    });
    let mut returned = returned.into_iter();
    if let Some(first) = returned.next() {
        slot.handle = first;
    }
    slot.replicas.extend(returned);
    report
}

/// What an instance of a replicated node needs to execute and restart (stateless by contract).
struct SlotMeta {
    id: String,
    contract: mvf_contract::NodeContract,
    origin: mvf_graph::ContractOrigin,
    config: serde_json::Map<String, serde_json::Value>,
    generation: u64,
    on_error: mvf_graph::FailurePolicy,
    backpressure: mvf_graph::Backpressure,
}

impl SlotMeta {
    fn of(slot: &NodeSlot) -> Self {
        Self {
            id: slot.id.clone(),
            contract: slot.contract.clone(),
            origin: slot.origin.clone(),
            config: slot.config.clone(),
            generation: slot.generation,
            on_error: slot.on_error.clone(),
            backpressure: slot.backpressure,
        }
    }

    /// Executes on one instance under the node's policies; a restart rebuilds that instance.
    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    fn execute(
        &self,
        handle: &mut Box<dyn NodeHandle>,
        node: &mvf_graph::GraphNode,
        cycle: u64,
        inputs: &Inputs,
        ctx: &RecoveryCtx<'_>,
        cancel: &CancellationToken,
        log: &mut Vec<RecoveryEvent>,
    ) -> (Result<(BTreeMap<String, Value>, Duration), Executed>, u64) {
        let mut attempts = 0u32;
        let mut restarts = 0u64;
        let mut stalled: Option<Instant> = None;
        loop {
            let exec = ExecContext {
                cycle,
                node_id: &self.id,
                generation: self.generation,
                cancel,
                deadline: match self.contract.spec.lifecycle.execute_timeout_ms {
                    0 => None,
                    ms => Some(Instant::now() + Duration::from_millis(ms)),
                },
            };
            let t0 = Instant::now();
            let result = handle.execute(&exec, inputs);
            let elapsed = t0.elapsed();
            let error = match result {
                Ok(Execution::Produced(outputs)) => {
                    match crate::executor::validate_outputs(&self.contract, outputs) {
                        Ok(outputs) => return (Ok((outputs, elapsed)), restarts),
                        Err(e) => e,
                    }
                }
                Ok(Execution::Exhausted) => NodeError::new(
                    NodeErrorKind::InvalidOutput,
                    "only a source may report exhaustion",
                ),
                Err(e) => e,
            };
            let fail = |error: NodeError| {
                Err(Executed::End(EndReason::NodeFailed {
                    node: self.id.clone(),
                    cycle,
                    error,
                }))
            };
            if error.kind == NodeErrorKind::Backpressure {
                match self.backpressure {
                    mvf_graph::Backpressure::Drop => {
                        note_event(
                            ctx.services,
                            log,
                            Some((&self.id, self.generation)),
                            cycle,
                            RecoveryKind::Dropped,
                            None,
                            Some(error.message.clone()),
                        );
                        return (
                            Err(Executed::Skipped(SkipReason::Dropped(error.message))),
                            restarts,
                        );
                    }
                    mvf_graph::Backpressure::Stall => {
                        let since = *stalled.get_or_insert_with(Instant::now);
                        if since.elapsed() > STALL_LIMIT || cancel.is_cancelled() {
                            return (fail(error), restarts);
                        }
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                }
            }
            if error.kind == NodeErrorKind::Cancelled || cancel.is_cancelled() {
                return (fail(error), restarts);
            }
            match self.on_error.mode {
                mvf_graph::FailureMode::Fail => return (fail(error), restarts),
                mvf_graph::FailureMode::Skip => {
                    note_event(
                        ctx.services,
                        log,
                        Some((&self.id, self.generation)),
                        cycle,
                        RecoveryKind::Skipped,
                        None,
                        Some(error.message.clone()),
                    );
                    return (
                        Err(Executed::Skipped(SkipReason::Failure(error.message))),
                        restarts,
                    );
                }
                mvf_graph::FailureMode::Restart => {
                    attempts += 1;
                    if !self.on_error.allows(attempts) {
                        note_event(
                            ctx.services,
                            log,
                            Some((&self.id, self.generation)),
                            cycle,
                            RecoveryKind::RestartsExhausted,
                            Some(attempts - 1),
                            Some(error.message.clone()),
                        );
                        return (fail(error), restarts);
                    }
                    std::thread::sleep(self.on_error.backoff(attempts));
                    match self.rebuild(ctx.backends, ctx.services, cancel) {
                        Ok(fresh) => {
                            let mut old = std::mem::replace(handle, fresh);
                            old.stop(&LifecycleContext {
                                node_id: &self.id,
                                generation: self.generation,
                                budget: Duration::from_millis(500),
                                cancel,
                            });
                            restarts += 1;
                            note_event(
                                ctx.services,
                                log,
                                Some((&self.id, self.generation)),
                                cycle,
                                RecoveryKind::Restarted,
                                Some(attempts),
                                Some(error.message.clone()),
                            );
                        }
                        Err(e) => note_event(
                            ctx.services,
                            log,
                            Some((&self.id, self.generation)),
                            cycle,
                            RecoveryKind::RestartFailed,
                            Some(attempts),
                            Some(e.message),
                        ),
                    }
                    let _ = node;
                }
            }
        }
    }

    fn rebuild(
        &self,
        backends: &BackendRegistry,
        services: &HostServices,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn NodeHandle>, NodeError> {
        let (mut handle, _) = backends.instantiate(&InstantiateRequest {
            node_id: &self.id,
            contract: &self.contract,
            origin: &self.origin,
            config: &self.config,
            generation: self.generation,
            services,
        })?;
        let ctx = LifecycleContext {
            node_id: &self.id,
            generation: self.generation,
            budget: Duration::from_millis(self.contract.spec.lifecycle.readiness_timeout_ms),
            cancel,
        };
        if let Err(e) = handle.start(&ctx) {
            handle.stop(&ctx);
            return Err(e);
        }
        Ok(handle)
    }
}
