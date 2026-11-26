//! Failure policy, backpressure and epoch checkpoints — parity with the .NET engine's
//! `NodeFailurePolicy`, `BackpressurePolicy` and `CheckpointCoordinator`/`FileCheckpointStore`.
//!
//! - **Failure policy** (`onError`): `fail` ends the run; `skip` drops the node's outputs for the
//!   cycle and keeps running; `restart` disposes the node, brings up a fresh instance of the same
//!   implementation (restoring its last checkpoint) and **retries the same inputs**, with
//!   exponential backoff and one attempt budget per execution (a failed restart is an attempt);
//!   when the budget runs out the failure falls through to `fail`. Cancellation is never retried.
//!   The .NET defaults (a source ends the run, a mid-graph node skips) are
//!   `run.onError: { sources: fail, nodes: skip }`; the Rust default is `fail` for every role.
//! - **Backpressure**: a full arena is `stall` (lossless — the serial executor has nothing to wait
//!   for, so the run ends with an actionable error) or `drop` (skip the cycle, count it).
//! - **Checkpoints**: every N completed cycles each node's versioned state is captured at the cycle
//!   boundary (torn-free: nothing is in flight). With a directory, the epoch is written to
//!   `epoch-<cycle>.tmp/`, renamed to `epoch-<cycle>/`, and only then published by atomically
//!   replacing `CURRENT`; on start the published epoch is restored and cycle numbering resumes after
//!   it. A run that ends with its source exhausted (fully consumed) clears the directory; any other
//!   end keeps it for resume.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::backend::InstantiateRequest;
use crate::executor::{EndReason, Engine, RunOptions, SkipReason};
use crate::node::*;

/// What the executor does after a node failure.
pub(crate) enum Recovery {
    /// Execute the same inputs again (the node was restarted).
    Retry,
    /// Skip the node's outputs this cycle.
    Skip(SkipReason),
    /// End the run.
    End(EndReason),
}

/// Kinds of recovery events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryKind {
    /// The node was restarted and the cycle retried.
    Restarted,
    /// A restart attempt failed (counts against the budget).
    RestartFailed,
    /// The restart budget ran out; the failure fell through to `fail`.
    RestartsExhausted,
    /// The failure was skipped (`onError: skip`).
    Skipped,
    /// The cycle was dropped (`backpressure: drop`).
    Dropped,
    /// An epoch checkpoint was captured.
    Checkpointed,
    /// A node's state could not be captured (the epoch keeps its previous state).
    CheckpointFailed,
    /// A node's state was restored.
    Restored,
    /// A node's state could not be restored (the node starts from scratch).
    RestoreFailed,
}

/// One recovery event.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryEvent {
    /// Node id (absent for whole-epoch events).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Cycle.
    pub cycle: u64,
    /// Kind.
    pub kind: RecoveryKind,
    /// Restart attempt (1-based), when relevant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// Explanation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Checkpoint configuration and the latest captured states.
#[derive(Debug, Default)]
pub(crate) struct CheckpointState {
    pub(crate) every: u64,
    pub(crate) dir: Option<PathBuf>,
    /// Latest state per node id (memory; used to restore restarted nodes).
    pub(crate) last: HashMap<String, StateBlob>,
    pub(crate) resumed_from: Option<u64>,
}

const STATE_MAGIC: &[u8; 8] = b"MVFSTATE";

fn encode(state: &StateBlob) -> Vec<u8> {
    let mut out = Vec::with_capacity(20 + state.bytes.len());
    out.extend_from_slice(STATE_MAGIC);
    out.extend_from_slice(&state.schema_version.to_le_bytes());
    out.extend_from_slice(&(state.bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&state.bytes);
    out
}

fn decode(bytes: &[u8]) -> Result<StateBlob, String> {
    if bytes.len() < 20 || &bytes[..8] != STATE_MAGIC {
        return Err("not an MVF state file".into());
    }
    let schema_version = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    let mut len = [0u8; 8];
    len.copy_from_slice(&bytes[12..20]);
    let len = u64::from_le_bytes(len) as usize;
    if bytes.len() != 20 + len {
        return Err("truncated state file".into());
    }
    Ok(StateBlob {
        schema_version,
        bytes: bytes[20..].to_vec(),
    })
}

/// Node ids are validated identifiers (`[a-z0-9.-_]`), so they are safe file names; still refuse
/// anything with a separator.
fn state_file(dir: &Path, node: &str) -> Option<PathBuf> {
    (!node.is_empty() && !node.contains(['/', '\\']) && node != "." && node != "..")
        .then(|| dir.join(format!("{node}.state")))
}

impl Engine {
    fn note(
        &mut self,
        node: Option<&str>,
        cycle: u64,
        kind: RecoveryKind,
        attempt: Option<u32>,
        message: Option<String>,
    ) {
        if let Some(n) = node {
            let generation = self
                .slots
                .iter()
                .find(|s| s.id == n)
                .map(|s| s.generation)
                .unwrap_or(0);
            let level = match kind {
                RecoveryKind::Checkpointed | RecoveryKind::Restored | RecoveryKind::Restarted => {
                    "supervisor"
                }
                _ => "warn",
            };
            self.services.log(
                n,
                generation,
                level,
                format!(
                    "{kind:?}{}{}",
                    attempt
                        .map(|a| format!(" (attempt {a})"))
                        .unwrap_or_default(),
                    message
                        .as_deref()
                        .map(|m| format!(": {m}"))
                        .unwrap_or_default()
                ),
            );
        }
        self.recovery_log.push(RecoveryEvent {
            node: node.map(str::to_string),
            cycle,
            kind,
            attempt,
            message,
        });
    }

    /// Decides what a failure of node `i` in `cycle` means under its policies.
    pub(crate) fn recover(
        &mut self,
        i: usize,
        cycle: u64,
        error: NodeError,
        attempts: &mut u32,
        cancel: &CancellationToken,
    ) -> Recovery {
        let id = self.slots[i].id.clone();
        if error.kind == NodeErrorKind::Backpressure {
            return match self.slots[i].backpressure {
                mvf_graph::Backpressure::Drop => {
                    self.slots[i].stats.dropped += 1;
                    self.note(
                        Some(&id),
                        cycle,
                        RecoveryKind::Dropped,
                        None,
                        Some(error.message.clone()),
                    );
                    Recovery::Skip(SkipReason::Dropped(error.message))
                }
                mvf_graph::Backpressure::Stall => {
                    let mut e = error;
                    e.message = format!(
                        "{} (backpressure: stall is lossless and the serial executor cannot wait for a drain — raise the arena slot count, or set `backpressure: drop` on this node)",
                        e.message
                    );
                    Recovery::End(EndReason::NodeFailed {
                        node: id,
                        cycle,
                        error: e,
                    })
                }
            };
        }
        if error.kind == NodeErrorKind::Cancelled || cancel.is_cancelled() {
            return Recovery::End(EndReason::NodeFailed {
                node: id,
                cycle,
                error,
            });
        }
        let policy = self.slots[i].on_error.clone();
        match policy.mode {
            mvf_graph::FailureMode::Fail => Recovery::End(EndReason::NodeFailed {
                node: id,
                cycle,
                error,
            }),
            mvf_graph::FailureMode::Skip => {
                self.slots[i].stats.failures_skipped += 1;
                self.note(
                    Some(&id),
                    cycle,
                    RecoveryKind::Skipped,
                    None,
                    Some(error.message.clone()),
                );
                Recovery::Skip(SkipReason::Failure(error.message))
            }
            mvf_graph::FailureMode::Restart => loop {
                *attempts += 1;
                if !policy.allows(*attempts) {
                    self.note(
                        Some(&id),
                        cycle,
                        RecoveryKind::RestartsExhausted,
                        Some(*attempts - 1),
                        Some(error.message.clone()),
                    );
                    return Recovery::End(EndReason::NodeFailed {
                        node: id,
                        cycle,
                        error,
                    });
                }
                let until = Instant::now() + policy.backoff(*attempts);
                while Instant::now() < until {
                    if cancel.is_cancelled() {
                        return Recovery::End(EndReason::Cancelled);
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                match self.restart_node(i, cancel) {
                    Ok(()) => {
                        self.slots[i].stats.restarts += 1;
                        self.note(
                            Some(&id),
                            cycle,
                            RecoveryKind::Restarted,
                            Some(*attempts),
                            Some(error.message.clone()),
                        );
                        return Recovery::Retry;
                    }
                    Err(e) => self.note(
                        Some(&id),
                        cycle,
                        RecoveryKind::RestartFailed,
                        Some(*attempts),
                        Some(e.message),
                    ),
                }
            },
        }
    }

    /// Hard restart: stop the failed instance, build and start a fresh one of the active
    /// implementation (contract, origin, config), restore its last captured state.
    fn restart_node(&mut self, i: usize, cancel: &CancellationToken) -> Result<(), NodeError> {
        let generation = self.slots[i].next_generation;
        self.slots[i].next_generation += 1;
        let (mut handle, _) = {
            let slot = &self.slots[i];
            self.backends.instantiate(&InstantiateRequest {
                node_id: &slot.id,
                contract: &slot.contract,
                origin: &slot.origin,
                config: &slot.config,
                generation,
                services: &self.services,
            })?
        };
        let slot = &self.slots[i];
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
        let restored = self
            .checkpoint
            .last
            .get(&slot.id)
            .map(|state| handle.import_state(state));
        let id = slot.id.clone();
        let slot = &mut self.slots[i];
        let mut old = std::mem::replace(&mut slot.handle, handle);
        let old_generation = std::mem::replace(&mut slot.generation, generation);
        slot.stats.generation = generation;
        slot.activated = true;
        slot.pending_state = None;
        old.stop(&LifecycleContext {
            node_id: &id,
            generation: old_generation,
            budget: Duration::from_millis(500),
            cancel,
        });
        let cycle = self.next_cycle;
        match restored {
            Some(Ok(())) => self.note(
                Some(&id),
                cycle,
                RecoveryKind::Restored,
                None,
                Some("restarted instance restored its last checkpoint".into()),
            ),
            Some(Err(e)) => self.note(
                Some(&id),
                cycle,
                RecoveryKind::RestoreFailed,
                None,
                Some(e.message),
            ),
            None => {}
        }
        Ok(())
    }

    pub(crate) fn configure_checkpoints(&mut self, options: &RunOptions) {
        let spec = &self.graph.checkpoint;
        self.checkpoint.every = options.checkpoint_every.unwrap_or(spec.every_cycles);
        self.checkpoint.dir = options.checkpoint_dir.clone().or_else(|| {
            spec.directory.as_ref().map(|d| {
                self.services
                    .package_dir
                    .as_deref()
                    .unwrap_or(Path::new("."))
                    .join(d)
            })
        });
    }

    /// Captures every node's state after `cycle` when it is a checkpoint boundary.
    pub(crate) fn maybe_checkpoint(&mut self, cycle: u64) {
        let every = self.checkpoint.every;
        if every == 0 || !cycle.is_multiple_of(every) {
            return;
        }
        let mut captured: Vec<(String, StateBlob)> = Vec::new();
        for i in 0..self.slots.len() {
            let id = self.slots[i].id.clone();
            if !self.slots[i].activated {
                // An on-demand node that has not run yet keeps whatever state it was restored with.
                if let Some(state) = self.slots[i]
                    .pending_state
                    .clone()
                    .or_else(|| self.checkpoint.last.get(&id).cloned())
                {
                    captured.push((id, state));
                }
                continue;
            }
            match self.slots[i].handle.export_state() {
                Ok(Some(state)) => captured.push((id, state)),
                Ok(None) => {}
                Err(e) => self.note(
                    Some(&id),
                    cycle,
                    RecoveryKind::CheckpointFailed,
                    None,
                    Some(e.message),
                ),
            }
        }
        for (id, state) in &captured {
            self.checkpoint.last.insert(id.clone(), state.clone());
        }
        let persisted = match self.checkpoint.dir.clone() {
            Some(dir) => match persist(&dir, cycle, &captured) {
                Ok(()) => format!("{} node state(s) in {}", captured.len(), dir.display()),
                Err(e) => {
                    self.note(None, cycle, RecoveryKind::CheckpointFailed, None, Some(e));
                    return;
                }
            },
            None => format!("{} node state(s) in memory", captured.len()),
        };
        self.note(
            None,
            cycle,
            RecoveryKind::Checkpointed,
            None,
            Some(persisted),
        );
    }

    /// Restores the published epoch, if any, and resumes cycle numbering after it.
    pub(crate) fn resume_from_checkpoint(&mut self) {
        let Some(dir) = self.checkpoint.dir.clone() else {
            return;
        };
        let Some((cycle, states)) = load(&dir) else {
            return;
        };
        for i in 0..self.slots.len() {
            let id = self.slots[i].id.clone();
            let Some(state) = states.get(&id) else {
                continue;
            };
            if !self.slots[i].activated {
                // Restored the moment the on-demand node is first used (as in .NET).
                self.slots[i].pending_state = Some(state.clone());
                self.checkpoint.last.insert(id.clone(), state.clone());
                continue;
            }
            match self.slots[i].handle.import_state(state) {
                Ok(()) => {
                    self.checkpoint.last.insert(id.clone(), state.clone());
                    self.note(
                        Some(&id),
                        cycle,
                        RecoveryKind::Restored,
                        None,
                        Some(format!("resumed from the checkpoint after cycle {cycle}")),
                    );
                }
                Err(e) => self.note(
                    Some(&id),
                    cycle,
                    RecoveryKind::RestoreFailed,
                    None,
                    Some(e.message),
                ),
            }
        }
        self.next_cycle = cycle + 1;
        self.checkpoint.resumed_from = Some(cycle);
    }

    /// Deletes the durable checkpoint (after a clean, fully consumed run).
    pub(crate) fn clear_checkpoints(&mut self) {
        if let Some(dir) = &self.checkpoint.dir
            && dir.is_dir()
        {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn persist(dir: &Path, cycle: u64, states: &[(String, StateBlob)]) -> Result<(), String> {
    let io = |e: std::io::Error| format!("{}: {e}", dir.display());
    std::fs::create_dir_all(dir).map_err(io)?;
    let tmp = dir.join(format!("epoch-{cycle}.tmp"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(io)?;
    for (node, state) in states {
        let Some(file) = state_file(&tmp, node) else {
            continue;
        };
        std::fs::write(&file, encode(state)).map_err(io)?;
    }
    let epoch = dir.join(format!("epoch-{cycle}"));
    let _ = std::fs::remove_dir_all(&epoch);
    std::fs::rename(&tmp, &epoch).map_err(io)?;
    // Publish atomically: readers only ever see a complete epoch.
    let current_tmp = dir.join("CURRENT.tmp");
    std::fs::write(&current_tmp, cycle.to_string()).map_err(io)?;
    std::fs::rename(&current_tmp, dir.join("CURRENT")).map_err(io)?;
    // Older epochs are garbage now.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("epoch-") && name != format!("epoch-{cycle}") {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    Ok(())
}

fn load(dir: &Path) -> Option<(u64, HashMap<String, StateBlob>)> {
    let cycle: u64 = std::fs::read_to_string(dir.join("CURRENT"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let epoch = dir.join(format!("epoch-{cycle}"));
    let mut states = HashMap::new();
    for e in std::fs::read_dir(&epoch).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(node) = name.strip_suffix(".state") else {
            continue;
        };
        if let Ok(bytes) = std::fs::read(e.path())
            && let Ok(state) = decode(&bytes)
        {
            states.insert(node.to_string(), state);
        }
    }
    Some((cycle, states))
}
