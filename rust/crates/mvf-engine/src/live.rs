//! Live operation of a running graph, applied at cycle boundaries (the quiesced point): `value` and
//! `select` settings, module config bindings, whole-graph pause, the `loop` primitive's rewind, and
//! lazy (`on-demand`) activation. .NET reference: `LiveValueRegistry`, `RunControl`,
//! `PipelineGraphExecutor` (re-activation queue, pause gate, `forever` rewind), `BindingPrePass`.

use std::time::{Duration, Instant};

use mvf_graph::{Graph, GraphNode, LoopMode, PrimitiveSpec};
use serde::Serialize;
use serde_json::Value as Json;

use crate::backend::InstantiateRequest;
use crate::bindings::Bindings;
use crate::builtins::HostServices;
use crate::executor::{BuildError, Engine};
use crate::node::*;

/// What a live operation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LiveEventKind {
    /// A `value`/`select` setting was accepted; the next cycle sees it.
    ValueSet,
    /// A module took an edited config field without reopening.
    Reconfigured,
    /// A module was reopened with an edited config field (the .NET re-activation model).
    Reopened,
    /// The edit was refused; nothing changed.
    Rejected,
    /// The whole graph paused (process alive, nodes warm).
    Paused,
    /// The graph resumed where it left off.
    Resumed,
    /// Every rewindable source was rewound by a `forever` loop.
    Rewound,
    /// An `on-demand` node was started because it was needed.
    Activated,
}

/// One live operation.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveEvent {
    /// The first cycle that observes it.
    pub cycle: u64,
    /// Node id (absent for whole-graph events).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Config field, for module bindings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// What happened.
    pub kind: LiveEventKind,
    /// Written to the binding store.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub persisted: bool,
    /// Explanation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// What the `loop` primitive did in a run.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IterationReport {
    /// Termination policy.
    pub mode: LoopMode,
    /// Times the sources were rewound (`forever`).
    pub rewinds: u64,
    /// Pauses.
    pub pauses: u64,
    /// Time spent paused, milliseconds.
    pub paused_ms: f64,
}

impl IterationReport {
    pub(crate) fn for_graph(graph: &Graph) -> Self {
        Self {
            mode: graph.loop_spec.map(|l| l.mode).unwrap_or_default(),
            ..Self::default()
        }
    }
}

/// Module-binding pre-pass: overlays stored edits onto a node's config, type-checked.
pub(crate) fn overlay_bindings(
    node: &GraphNode,
    bindings: &Bindings,
) -> Result<serde_json::Map<String, Json>, String> {
    let mut config = node.config.clone();
    for (field, binding) in &node.bindings {
        if let Some(stored) = bindings.stored(&binding.binding) {
            binding
                .value_type()
                .check(binding.schema.as_ref(), &stored)
                .map_err(|e| {
                    format!(
                        "stored edit `{}` for config `{field}` is not a valid {}: {e}",
                        binding.binding,
                        binding.value_type().describe()
                    )
                })?;
            config.insert(field.clone(), stored);
        }
    }
    Ok(config)
}

/// A `select` without a wired criterion and without `where` must resolve its binding before the
/// run (the .NET pre-pass would ask an operator; this host is unattended and fails instead).
pub(crate) fn check_select_criteria(
    graph: &Graph,
    services: &HostServices,
) -> Result<(), BuildError> {
    for node in &graph.nodes {
        let Some(PrimitiveSpec::Select(c)) = &node.primitive else {
            continue;
        };
        let wired = graph
            .edges()
            .any(|e| e.to.node == node.index && e.to.port == "criterion");
        if wired || c.criterion.is_some() {
            continue;
        }
        let resolved = c.binding.as_ref().is_some_and(|key| {
            services.bindings.stored(key).is_some() || services.bindings.environment(key).is_some()
        });
        if !resolved {
            let hint = match &c.binding {
                Some(key) => format!(
                    "binding `{key}` has no value; store one in {} or set {} (this host never prompts)",
                    services
                        .bindings
                        .location()
                        .map_or_else(|| "the binding store".into(), |p| p.display().to_string()),
                    crate::bindings::env_name(key)
                ),
                None => "no `where`, no binding and no `criterion` edge".into(),
            };
            return Err(BuildError {
                node: node.id.clone(),
                error: NodeError::new(
                    NodeErrorKind::InvalidConfig,
                    format!("select `{}`: {hint}", node.id),
                ),
            });
        }
    }
    Ok(())
}

impl Engine {
    fn live_event(
        &mut self,
        node: Option<&str>,
        field: Option<&str>,
        kind: LiveEventKind,
        persisted: bool,
        message: Option<String>,
    ) {
        self.live_log.push(LiveEvent {
            cycle: self.next_cycle,
            node: node.map(str::to_string),
            field: field.map(str::to_string),
            kind,
            persisted,
            message,
        });
    }

    /// Sets a `value` node's value or a `select`'s criterion; type-checked, persisted to its binding.
    pub(crate) fn set_live_value(&mut self, node: &str, value: Json) {
        let Some(i) = self.graph.index_of(node) else {
            return self.live_event(
                Some(node),
                None,
                LiveEventKind::Rejected,
                false,
                Some(format!("no node `{node}`")),
            );
        };
        let binding = match &self.graph.nodes[i].primitive {
            Some(PrimitiveSpec::Value(c)) => c.binding.clone(),
            Some(PrimitiveSpec::Select(c)) => {
                let fed = self
                    .graph
                    .edges()
                    .any(|e| e.to.node == i && e.to.port == "criterion");
                if fed {
                    return self.live_event(
                        Some(node),
                        None,
                        LiveEventKind::Rejected,
                        false,
                        Some(
                            "its criterion comes from an edge, which would overwrite the setting next cycle; tune the value behind the edge"
                                .into(),
                        ),
                    );
                }
                c.binding.clone()
            }
            _ => {
                return self.live_event(
                    Some(node),
                    None,
                    LiveEventKind::Rejected,
                    false,
                    Some("only `value` and `select` nodes have a live value".into()),
                );
            }
        };
        if let Err(e) = self.slots[i].handle.set_value(&value) {
            return self.live_event(
                Some(node),
                None,
                LiveEventKind::Rejected,
                false,
                Some(e.message),
            );
        }
        let (persisted, message) = self.persist(binding.as_deref(), value);
        self.live_event(
            Some(node),
            None,
            LiveEventKind::ValueSet,
            persisted,
            message,
        );
    }

    /// Edits a module config field declared in the node's `bindings`: live when the node can take
    /// it, otherwise the node is reopened with the new config.
    pub(crate) fn configure_live(
        &mut self,
        node: &str,
        field: &str,
        value: Json,
        cancel: &CancellationToken,
    ) {
        let Some(i) = self.graph.index_of(node) else {
            return self.live_event(
                Some(node),
                Some(field),
                LiveEventKind::Rejected,
                false,
                Some(format!("no node `{node}`")),
            );
        };
        let Some(binding) = self.graph.nodes[i].bindings.get(field).cloned() else {
            return self.live_event(
                Some(node),
                Some(field),
                LiveEventKind::Rejected,
                false,
                Some(format!("`{field}` is not a declared binding of `{node}`")),
            );
        };
        if let Err(e) = binding.value_type().check(binding.schema.as_ref(), &value) {
            return self.live_event(
                Some(node),
                Some(field),
                LiveEventKind::Rejected,
                false,
                Some(e),
            );
        }
        let mut config = self.slots[i].config.clone();
        config.insert(field.to_string(), value.clone());
        let live = if self.slots[i].activated {
            self.slots[i].handle.reconfigure(&config)
        } else {
            Ok(false)
        };
        let kind = match live {
            Ok(true) => {
                self.slots[i].config = config;
                LiveEventKind::Reconfigured
            }
            Ok(false) | Err(_) => match self.reopen(i, config, cancel) {
                Ok(()) => LiveEventKind::Reopened,
                Err(e) => {
                    return self.live_event(
                        Some(node),
                        Some(field),
                        LiveEventKind::Rejected,
                        false,
                        Some(format!("reopening with the new config failed: {e}")),
                    );
                }
            },
        };
        let (persisted, message) = self.persist(Some(&binding.binding), value);
        self.live_event(Some(node), Some(field), kind, persisted, message);
    }

    fn persist(&self, binding: Option<&str>, value: Json) -> (bool, Option<String>) {
        match binding {
            // A value with no binding is tunable but not persisted: its only durable home would
            // be the portable pipeline file.
            None => (false, Some("no binding: tuned for this run only".into())),
            Some(key) => match self.services.bindings.store(key, value) {
                Ok(()) => (self.services.bindings.location().is_some(), None),
                Err(e) => (false, Some(format!("not persisted: {e}"))),
            },
        }
    }

    /// Replaces node `i` with a fresh generation built from `config` (no state carried — a source
    /// restarts at its first frame, the accepted cost of not changing the module contract).
    fn reopen(
        &mut self,
        i: usize,
        config: serde_json::Map<String, Json>,
        cancel: &CancellationToken,
    ) -> Result<(), NodeError> {
        let generation = self.slots[i].next_generation;
        let (mut handle, _) = {
            let slot = &self.slots[i];
            self.backends.instantiate(&InstantiateRequest {
                node_id: &slot.id,
                contract: &slot.contract,
                origin: &slot.origin,
                config: &config,
                generation,
                services: &self.services,
                network: slot.network.as_ref(),
                storage: &slot.storage,
            })?
        };
        let slot = &mut self.slots[i];
        slot.next_generation += 1;
        if slot.activated {
            let ctx = LifecycleContext {
                node_id: &slot.id,
                generation,
                budget: Duration::from_millis(slot.contract.spec.lifecycle.readiness_timeout_ms),
                cancel,
            };
            if let Err(e) = handle.start(&ctx) {
                handle.stop(&ctx);
                return Err(e);
            }
        }
        let mut old = std::mem::replace(&mut slot.handle, handle);
        let old_generation = std::mem::replace(&mut slot.generation, generation);
        slot.stats.generation = generation;
        slot.config = config;
        if slot.activated {
            old.stop(&LifecycleContext {
                node_id: &slot.id,
                generation: old_generation,
                budget: Duration::from_millis(500),
                cancel,
            });
        }
        Ok(())
    }

    /// Pauses the whole graph (only meaningful with a `loop`, as in .NET).
    pub(crate) fn pause(&mut self, for_ms: Option<u64>) {
        if self.graph.loop_spec.is_none() {
            return self.live_event(
                None,
                None,
                LiveEventKind::Rejected,
                false,
                Some("pause is the loop's running state; this graph has no `loop`".into()),
            );
        }
        if self.paused.is_none() {
            self.iteration.pauses += 1;
        }
        self.paused = Some(for_ms.map(|ms| Instant::now() + Duration::from_millis(ms)));
        self.live_event(None, None, LiveEventKind::Paused, false, None);
    }

    pub(crate) fn resume(&mut self) {
        if self.paused.take().is_some() {
            self.live_event(None, None, LiveEventKind::Resumed, false, None);
        }
    }

    /// Idles at the boundary while paused: no node runs and no cycle is counted, the process stays
    /// alive and nodes stay warm. Commands keep being served (resume, edits). Pause is not cancel.
    pub(crate) fn wait_while_paused(&mut self, cancel: &CancellationToken) {
        let started = Instant::now();
        while let Some(until) = self.paused {
            if cancel.is_cancelled() {
                break;
            }
            if until.is_some_and(|t| Instant::now() >= t) {
                self.resume();
                break;
            }
            match self.control_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(command) => self.handle_command(command, cancel),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let paused = started.elapsed();
        if paused > Duration::from_micros(50) {
            self.iteration.paused_ms += paused.as_secs_f64() * 1e3;
        }
    }

    /// `forever`: rewinds every rewindable source. False when none could be rewound.
    pub(crate) fn rewind_sources(&mut self) -> bool {
        let mut rewound = 0usize;
        for i in 0..self.slots.len() {
            if !self.graph.nodes[i].is_source() {
                continue;
            }
            match self.slots[i].handle.rewind() {
                Ok(true) => rewound += 1,
                Ok(false) => {}
                Err(e) => {
                    let id = self.slots[i].id.clone();
                    self.live_event(
                        Some(&id),
                        None,
                        LiveEventKind::Rejected,
                        false,
                        Some(format!("rewind failed: {e}")),
                    );
                }
            }
        }
        if rewound > 0 {
            self.iteration.rewinds += 1;
            self.live_event(
                None,
                None,
                LiveEventKind::Rewound,
                false,
                Some(format!("{rewound} source(s) rewound")),
            );
        }
        rewound > 0
    }

    /// Starts an `on-demand` node the first time it runs and hands it any restored state.
    pub(crate) fn activate(
        &mut self,
        i: usize,
        cycle: u64,
        cancel: &CancellationToken,
    ) -> Result<(), NodeError> {
        let slot = &mut self.slots[i];
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
        let id = slot.id.clone();
        self.live_event(Some(&id), None, LiveEventKind::Activated, false, None);
        Ok(())
    }
}
