//! Backend capability negotiation.
//!
//! A host advertises one [`BackendCapabilities`] per backend it actually provides. Negotiation walks
//! the contract's artifacts in declared order and picks the first (artifact, backend, target) that
//! satisfies **every** mandatory requirement. Anything optional that could not be honoured is
//! reported as a [`PlacementNote`]; anything mandatory that could not be honoured rejects the
//! candidate with a structured reason. Nothing is silently weakened.

use serde::Serialize;

use crate::model::*;

/// How a backend can stop work in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cancellation {
    /// The node observes a cancellation flag; a misbehaving node can ignore it.
    Cooperative,
    /// The host can forcibly terminate the execution (e.g. kill the process).
    Enforceable,
}

/// What a backend can do. Values describe the backend implementation on *this* host, not a wish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendCapabilities {
    /// Backend kind.
    pub kind: BackendKind,
    /// Strongest isolation the backend enforces.
    pub isolation: IsolationLevel,
    /// Targets the backend can execute on.
    pub targets: Vec<Target>,
    /// Memory domains the backend can hand off without a payload copy.
    pub zero_copy_domains: Vec<MemoryDomain>,
    /// Cancellation semantics.
    pub cancellation: Cancellation,
    /// Whether a hung or misbehaving node can be terminated without stopping the engine.
    pub enforceable_termination: bool,
    /// Whether the backend enforces a memory limit.
    pub enforces_memory_limit: bool,
    /// Whether the backend enforces a CPU limit.
    pub enforces_cpu_limit: bool,
    /// Whether the backend enforces a pids limit.
    pub enforces_pids_limit: bool,
    /// Live replacement modes the backend supports.
    pub replacement: Vec<Replacement>,
    /// Grants a sandboxing backend can provide (ignored for backends weaker than `sandbox`).
    #[serde(default)]
    pub grants: GrantSupport,
    /// Whether the backend can give a node an arena of its own (`memory.arena: private`).
    #[serde(default)]
    pub private_arenas: bool,
}

/// Permission grants a sandboxing backend can provide on this host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantSupport {
    /// Network policies it can enforce.
    pub network: Vec<NetworkPolicy>,
    /// Host device paths present and grantable.
    pub devices: Vec<String>,
    /// CDI device names resolvable on this host.
    pub cdi_devices: Vec<String>,
}

/// A non-fatal deviation from a preference, surfaced to the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlacementNote {
    /// Stable code, e.g. `placement.fallback-to-cpu`.
    pub code: &'static str,
    /// Explanation.
    pub message: String,
}

/// The negotiated placement of a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NegotiatedPlacement {
    /// Chosen backend.
    pub backend: BackendKind,
    /// Index into `spec.artifacts`.
    pub artifact_index: usize,
    /// Chosen target.
    pub target: Target,
    /// Whether hand-off is zero copy on the chosen path.
    pub zero_copy: bool,
    /// Deviations from preferences.
    pub notes: Vec<PlacementNote>,
}

/// Why one candidate was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RejectionReason {
    /// Stable code, e.g. `negotiation.isolation-insufficient`.
    pub code: &'static str,
    /// Explanation.
    pub message: String,
}

/// One rejected candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rejection {
    /// Index into `spec.artifacts`.
    pub artifact_index: usize,
    /// The artifact's backend.
    pub backend: BackendKind,
    /// Every unmet requirement.
    pub reasons: Vec<RejectionReason>,
}

/// Negotiation failed: no candidate satisfied the contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("no execution backend satisfies node `{node}`: {}", summarize(.rejections))]
pub struct NegotiationError {
    /// Node id.
    pub node: String,
    /// Every rejected candidate with its reasons.
    pub rejections: Vec<Rejection>,
}

fn summarize(rejections: &[Rejection]) -> String {
    rejections
        .iter()
        .map(|r| {
            let codes: Vec<_> = r.reasons.iter().map(|x| x.code).collect();
            format!(
                "{}[{}]: {}",
                r.backend.as_str(),
                r.artifact_index,
                codes.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn reason(code: &'static str, message: impl Into<String>) -> RejectionReason {
    RejectionReason {
        code,
        message: message.into(),
    }
}

/// Negotiates a placement for `contract` among the backends this host provides.
pub fn negotiate(
    contract: &NodeContract,
    available: &[BackendCapabilities],
) -> Result<NegotiatedPlacement, NegotiationError> {
    let spec = &contract.spec;
    let pinned = spec.execution.backend.pinned();
    let mut rejections = Vec::new();

    // Preferred target first, then the CPU fallback if the contract allows it.
    let mut targets = vec![spec.execution.placement.target];
    if spec.execution.placement.fallback == Fallback::Cpu
        && spec.execution.placement.target != Target::Cpu
    {
        targets.push(Target::Cpu);
    }

    for target in &targets {
        for (index, artifact) in spec.artifacts.iter().enumerate() {
            let kind = artifact.backend();
            if pinned.is_some_and(|p| p != kind) {
                continue;
            }
            if !artifact.targets().contains(target) {
                continue;
            }
            let Some(caps) = available.iter().find(|c| c.kind == kind) else {
                push_rejection(
                    &mut rejections,
                    index,
                    kind,
                    reason(
                        "negotiation.backend-unavailable",
                        format!("this host provides no `{}` backend", kind.as_str()),
                    ),
                );
                continue;
            };
            match evaluate(spec, caps, *target) {
                Ok((zero_copy, mut notes)) => {
                    if *target != spec.execution.placement.target {
                        notes.insert(
                            0,
                            PlacementNote {
                                code: "placement.fallback-to-cpu",
                                message: format!(
                                    "preferred target {:?} unavailable; running on CPU per `fallback: cpu`",
                                    spec.execution.placement.target
                                )
                                .to_lowercase(),
                            },
                        );
                    }
                    return Ok(NegotiatedPlacement {
                        backend: kind,
                        artifact_index: index,
                        target: *target,
                        zero_copy,
                        notes,
                    });
                }
                Err(reasons) => {
                    for r in reasons {
                        push_rejection(&mut rejections, index, kind, r);
                    }
                }
            }
        }
    }

    if rejections.is_empty() {
        // Nothing was even evaluated: the pinned backend/target combination has no artifact.
        rejections.push(Rejection {
            artifact_index: 0,
            backend: pinned.unwrap_or(BackendKind::InProcess),
            reasons: vec![reason(
                "negotiation.no-candidate",
                "no artifact matches the pinned backend and placement target",
            )],
        });
    }

    Err(NegotiationError {
        node: contract.metadata.id.clone(),
        rejections,
    })
}

fn push_rejection(
    rejections: &mut Vec<Rejection>,
    index: usize,
    kind: BackendKind,
    r: RejectionReason,
) {
    match rejections
        .iter_mut()
        .find(|x| x.artifact_index == index && x.backend == kind)
    {
        Some(existing) => {
            if !existing.reasons.contains(&r) {
                existing.reasons.push(r);
            }
        }
        None => rejections.push(Rejection {
            artifact_index: index,
            backend: kind,
            reasons: vec![r],
        }),
    }
}

fn evaluate(
    spec: &NodeSpec,
    caps: &BackendCapabilities,
    target: Target,
) -> Result<(bool, Vec<PlacementNote>), Vec<RejectionReason>> {
    let mut reasons = Vec::new();
    let mut notes = Vec::new();

    if !caps.targets.contains(&target) {
        reasons.push(reason(
            "negotiation.target-unsupported",
            format!(
                "backend `{}` cannot run on {:?}",
                caps.kind.as_str(),
                target
            )
            .to_lowercase(),
        ));
    }

    if caps.isolation < spec.isolation.minimum {
        reasons.push(reason(
            "negotiation.isolation-insufficient",
            format!(
                "node requires isolation `{}` but backend `{}` enforces only `{}`",
                spec.isolation.minimum.as_str(),
                caps.kind.as_str(),
                caps.isolation.as_str()
            ),
        ));
    }

    // Device memory only exists on a GPU target; a CPU fallback runs in host memory.
    let domain = if target == Target::Gpu {
        spec.memory.domain
    } else {
        MemoryDomain::Host
    };
    let zero_copy = caps.zero_copy_domains.contains(&domain);
    match spec.memory.zero_copy {
        ZeroCopy::Required if !zero_copy => reasons.push(reason(
            "negotiation.zero-copy-unavailable",
            format!(
                "node requires zero-copy hand-off in {domain:?} memory; backend `{}` cannot provide it",
                caps.kind.as_str()
            )
            .replace("Host", "host")
            .replace("Device", "device"),
        )),
        ZeroCopy::Preferred if !zero_copy => notes.push(PlacementNote {
            code: "placement.copying-hand-off",
            message: format!(
                "zero copy preferred but backend `{}` copies payloads",
                caps.kind.as_str()
            ),
        }),
        _ => {}
    }
    if spec.memory.arena == crate::ArenaSharing::Private && !caps.private_arenas {
        reasons.push(reason(
            "negotiation.private-arena-unsupported",
            format!(
                "node requires an arena of its own; backend `{}` cannot isolate payload memory",
                caps.kind.as_str()
            ),
        ));
    }

    let resources = &spec.resources;
    let unenforced: Vec<&str> = [
        (
            resources.memory_mib.is_some() && !caps.enforces_memory_limit,
            "memory",
        ),
        (
            resources.cpu_millis.is_some() && !caps.enforces_cpu_limit,
            "cpu",
        ),
        (
            resources.pids.is_some() && !caps.enforces_pids_limit,
            "pids",
        ),
    ]
    .into_iter()
    .filter_map(|(miss, name)| miss.then_some(name))
    .collect();
    if !unenforced.is_empty() {
        let message = format!(
            "backend `{}` cannot enforce the {} limit(s)",
            caps.kind.as_str(),
            unenforced.join(", ")
        );
        match resources.enforcement {
            Enforcement::Required => {
                reasons.push(reason("negotiation.resource-limit-unenforceable", message))
            }
            Enforcement::BestEffort => notes.push(PlacementNote {
                code: "placement.advisory-limits",
                message,
            }),
        }
    }

    if caps.isolation >= IsolationLevel::Sandbox {
        let p = &spec.permissions;
        if !caps.grants.network.contains(&p.network) {
            reasons.push(reason(
                "negotiation.permission-unsupported",
                format!(
                    "network policy {:?} cannot be enforced by backend `{}`",
                    p.network,
                    caps.kind.as_str()
                )
                .to_lowercase(),
            ));
        }
        for d in &p.devices {
            if !caps.grants.devices.contains(&d.path) {
                reasons.push(reason(
                    "negotiation.device-unavailable",
                    format!(
                        "device `{}` is not present or not grantable on this host",
                        d.path
                    ),
                ));
            }
        }
        for c in &p.cdi_devices {
            if !caps.grants.cdi_devices.contains(c) {
                reasons.push(reason(
                    "negotiation.cdi-device-unavailable",
                    format!("CDI device `{c}` is not resolvable on this host (no CDI spec)"),
                ));
            }
        }
    }

    let replacement = spec.lifecycle.replacement;
    if replacement != Replacement::Unsupported && !caps.replacement.contains(&replacement) {
        reasons.push(reason(
            "negotiation.replacement-unsupported",
            format!(
                "node declares {replacement:?} replacement; backend `{}` does not support it",
                caps.kind.as_str()
            )
            .to_lowercase(),
        ));
    }

    if reasons.is_empty() {
        Ok((zero_copy, notes))
    } else {
        Err(reasons)
    }
}
