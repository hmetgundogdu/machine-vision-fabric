//! Semantic validation of a parsed [`NodeContract`]. Serde already rejected unknown fields, wrong
//! types and unknown enum values; this pass checks identifiers, versions, digests and policy
//! compatibility and reports **every** problem with a stable code and a field path.

use std::collections::HashSet;

use crate::model::*;

/// One validation problem.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ContractIssue {
    /// Stable code, e.g. `contract.port.duplicate-name`.
    pub code: &'static str,
    /// Field path, e.g. `spec.ports.inputs[1].name`.
    pub path: String,
    /// Human readable explanation.
    pub message: String,
}

impl std::fmt::Display for ContractIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}: {}", self.code, self.path, self.message)
    }
}

struct Issues(Vec<ContractIssue>);

impl Issues {
    fn push(&mut self, code: &'static str, path: impl Into<String>, message: impl Into<String>) {
        self.0.push(ContractIssue {
            code,
            path: path.into(),
            message: message.into(),
        });
    }
}

/// Maximum ports per direction, artifacts per node, and readiness budget (bounded inputs).
pub const MAX_PORTS: usize = 64;
/// Maximum number of artifacts.
pub const MAX_ARTIFACTS: usize = 16;
/// Maximum readiness/drain/execute timeout (10 minutes).
pub const MAX_TIMEOUT_MS: u64 = 600_000;

/// Returns true for a node/module identifier: `[a-z][a-z0-9]*` segments joined by `.`, `-` or `_`,
/// at most 128 characters.
pub fn is_valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    let mut prev_sep = false;
    for &b in bytes {
        let sep = matches!(b, b'.' | b'-' | b'_');
        if sep && prev_sep {
            return false;
        }
        if !(sep || b.is_ascii_lowercase() || b.is_ascii_digit()) {
            return false;
        }
        prev_sep = sep;
    }
    !prev_sep
}

/// Returns true for a port name: an ASCII identifier `[A-Za-z][A-Za-z0-9_]*`, at most 64 chars.
pub fn is_valid_port_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphabetic()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

/// Returns true for `sha256:` followed by exactly 64 lower-case hex digits.
pub fn is_valid_digest(digest: &str) -> bool {
    digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// A relative path that stays inside its base directory: no absolute paths, no drive letters,
/// no `..` segments, no backslashes, not empty.
pub fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && !path.contains('\0')
        && path.split('/').all(|seg| seg != ".." && !seg.is_empty())
}

/// Validates a contract, returning every issue found (empty = valid).
pub fn validate(contract: &NodeContract) -> Vec<ContractIssue> {
    let mut issues = Issues(Vec::new());

    if contract.api_version != NODE_CONTRACT_API_VERSION {
        issues.push(
            "contract.api-version.unsupported",
            "apiVersion",
            format!(
                "unsupported apiVersion `{}`; this runtime accepts `{NODE_CONTRACT_API_VERSION}`",
                contract.api_version
            ),
        );
    }
    if contract.kind != NODE_CONTRACT_KIND {
        issues.push(
            "contract.kind.unsupported",
            "kind",
            format!(
                "unsupported kind `{}`; expected `{NODE_CONTRACT_KIND}`",
                contract.kind
            ),
        );
    }

    if !is_valid_id(&contract.metadata.id) {
        issues.push(
            "contract.metadata.invalid-id",
            "metadata.id",
            format!(
                "`{}` is not a valid node id (lower-case segments joined by `.`, `-` or `_`)",
                contract.metadata.id
            ),
        );
    }
    if semver::Version::parse(&contract.metadata.version).is_err() {
        issues.push(
            "contract.metadata.invalid-version",
            "metadata.version",
            format!(
                "`{}` is not a semantic version (MAJOR.MINOR.PATCH)",
                contract.metadata.version
            ),
        );
    }

    let spec = &contract.spec;
    validate_ports(&spec.ports, &mut issues);
    validate_artifacts(spec, &mut issues);
    validate_policy(spec, &mut issues);

    issues.0
}

fn validate_ports(ports: &Ports, issues: &mut Issues) {
    if ports.inputs.is_empty() && ports.outputs.is_empty() {
        issues.push(
            "contract.port.none",
            "spec.ports",
            "a node must declare at least one input or output port",
        );
    }
    for (direction, list) in [("inputs", &ports.inputs), ("outputs", &ports.outputs)] {
        if list.len() > MAX_PORTS {
            issues.push(
                "contract.port.too-many",
                format!("spec.ports.{direction}"),
                format!("at most {MAX_PORTS} ports per direction"),
            );
        }
        let mut seen = HashSet::new();
        for (i, port) in list.iter().enumerate() {
            let path = format!("spec.ports.{direction}[{i}]");
            if !is_valid_port_name(&port.name) {
                issues.push(
                    "contract.port.invalid-name",
                    format!("{path}.name"),
                    format!("`{}` is not a valid port name", port.name),
                );
            }
            if !seen.insert(port.name.as_str()) {
                issues.push(
                    "contract.port.duplicate-name",
                    format!("{path}.name"),
                    format!("port `{}` is declared twice in {direction}", port.name),
                );
            }
            if port.port_type.channel() != port.channel {
                issues.push(
                    "contract.port.channel-type-mismatch",
                    format!("{path}.type"),
                    format!(
                        "type `{}` cannot travel on a {:?} channel; `signal` is the only control type",
                        port.port_type.as_str(),
                        port.channel
                    )
                    .to_lowercase(),
                );
            }
            if direction == "outputs" && !port.required {
                issues.push(
                    "contract.port.optional-output",
                    format!("{path}.required"),
                    "`required` applies to inputs only",
                );
            }
        }
    }
}

fn validate_artifacts(spec: &NodeSpec, issues: &mut Issues) {
    if spec.artifacts.is_empty() {
        issues.push(
            "contract.artifact.none",
            "spec.artifacts",
            "a node must declare at least one executable artifact",
        );
    }
    if spec.artifacts.len() > MAX_ARTIFACTS {
        issues.push(
            "contract.artifact.too-many",
            "spec.artifacts",
            format!("at most {MAX_ARTIFACTS} artifacts"),
        );
    }
    for (i, artifact) in spec.artifacts.iter().enumerate() {
        let path = format!("spec.artifacts[{i}]");
        match artifact {
            Artifact::InProcess { builtin, targets } => {
                if !is_valid_id(builtin) {
                    issues.push(
                        "contract.artifact.invalid-builtin",
                        format!("{path}.builtin"),
                        format!("`{builtin}` is not a valid builtin name"),
                    );
                }
                check_targets(targets.as_deref(), &path, issues);
            }
            Artifact::Worker {
                entry,
                protocol,
                targets,
                ..
            } => {
                if !is_safe_relative_path(entry) {
                    issues.push(
                        "contract.artifact.unsafe-path",
                        format!("{path}.entry"),
                        format!("`{entry}` must be a relative path inside the node package"),
                    );
                }
                if *protocol != 1 {
                    issues.push(
                        "contract.artifact.unsupported-protocol",
                        format!("{path}.protocol"),
                        format!("worker protocol {protocol} is not supported; only 1 exists"),
                    );
                }
                check_targets(targets.as_deref(), &path, issues);
            }
            Artifact::Oci {
                image,
                digest,
                targets,
            } => {
                if image.trim().is_empty() || image.len() > 512 {
                    issues.push(
                        "contract.artifact.invalid-image",
                        format!("{path}.image"),
                        "image reference must be non-empty and at most 512 characters",
                    );
                }
                if !is_valid_digest(digest) {
                    issues.push(
                        "contract.artifact.invalid-digest",
                        format!("{path}.digest"),
                        format!("`{digest}` is not `sha256:<64 lower-case hex>`"),
                    );
                }
                check_targets(targets.as_deref(), &path, issues);
            }
            Artifact::Wasm { module, digest } => {
                if !is_safe_relative_path(module) {
                    issues.push(
                        "contract.artifact.unsafe-path",
                        format!("{path}.module"),
                        format!("`{module}` must be a relative path inside the node package"),
                    );
                }
                if !is_valid_digest(digest) {
                    issues.push(
                        "contract.artifact.invalid-digest",
                        format!("{path}.digest"),
                        format!("`{digest}` is not `sha256:<64 lower-case hex>`"),
                    );
                }
            }
            Artifact::Compute {
                kernel,
                entry_point,
                ..
            } => {
                if !(is_safe_relative_path(kernel) || is_valid_id(kernel)) {
                    issues.push(
                        "contract.artifact.unsafe-path",
                        format!("{path}.kernel"),
                        format!(
                            "`{kernel}` must be a registered kernel name or a relative path inside the node package"
                        ),
                    );
                }
                if !is_valid_port_name(entry_point) {
                    issues.push(
                        "contract.artifact.invalid-entry-point",
                        format!("{path}.entryPoint"),
                        format!("`{entry_point}` is not a valid entry point identifier"),
                    );
                }
            }
        }
    }

    if let Some(pinned) = spec.execution.backend.pinned()
        && !spec.artifacts.iter().any(|a| a.backend() == pinned)
    {
        issues.push(
            "contract.execution.backend-without-artifact",
            "spec.execution.backend",
            format!(
                "backend `{}` is pinned but no artifact targets it",
                pinned.as_str()
            ),
        );
    }
}

fn check_targets(targets: Option<&[Target]>, path: &str, issues: &mut Issues) {
    if let Some(targets) = targets {
        if targets.is_empty() {
            issues.push(
                "contract.artifact.no-targets",
                format!("{path}.targets"),
                "`targets` must not be empty when given",
            );
        }
        let unique: HashSet<_> = targets.iter().collect();
        if unique.len() != targets.len() {
            issues.push(
                "contract.artifact.duplicate-target",
                format!("{path}.targets"),
                "a target is listed twice",
            );
        }
    }
}

fn validate_policy(spec: &NodeSpec, issues: &mut Issues) {
    if spec.execution.max_parallelism == 0 || spec.execution.max_parallelism > 64 {
        issues.push(
            "contract.execution.invalid-max-parallelism",
            "spec.execution.maxParallelism".to_string(),
            "maxParallelism must be within 1..=64",
        );
    }
    if spec.execution.max_parallelism > 1
        && spec.lifecycle.replacement == crate::Replacement::Stateful
    {
        issues.push(
            "contract.execution.stateful-parallelism",
            "spec.execution.maxParallelism".to_string(),
            "a stateful node cannot be replicated: N instances would hold N diverging states",
        );
    }
    let placement = &spec.execution.placement;
    let artifact_targets: HashSet<Target> =
        spec.artifacts.iter().flat_map(|a| a.targets()).collect();

    if !spec.artifacts.is_empty() && !artifact_targets.contains(&placement.target) {
        let can_fallback =
            placement.fallback == Fallback::Cpu && artifact_targets.contains(&Target::Cpu);
        if !can_fallback {
            issues.push(
                "contract.placement.no-artifact-for-target",
                "spec.execution.placement.target",
                format!(
                    "placement targets `{:?}` but no artifact supports it and fallback does not allow CPU",
                    placement.target
                )
                .to_lowercase(),
            );
        }
    }
    if placement.target == Target::Cpu && placement.fallback == Fallback::Cpu {
        issues.push(
            "contract.placement.redundant-fallback",
            "spec.execution.placement.fallback",
            "a CPU placement cannot fall back to CPU; use `deny`",
        );
    }

    if spec.memory.domain == MemoryDomain::Device && placement.target != Target::Gpu {
        issues.push(
            "contract.memory.device-without-gpu",
            "spec.memory.domain",
            "device memory requires `placement.target: gpu`",
        );
    }
    if spec.memory.domain == MemoryDomain::Device
        && spec.memory.zero_copy == ZeroCopy::Required
        && placement.fallback == Fallback::Cpu
    {
        issues.push(
            "contract.memory.zero-copy-conflicts-with-fallback",
            "spec.memory.zeroCopy",
            "device zero-copy is required, so a CPU fallback (which needs a device→host transfer) is incompatible",
        );
    }

    let lifecycle = &spec.lifecycle;
    for (field, value, min) in [
        ("readinessTimeoutMs", lifecycle.readiness_timeout_ms, 1),
        ("drainTimeoutMs", lifecycle.drain_timeout_ms, 1),
        ("executeTimeoutMs", lifecycle.execute_timeout_ms, 0),
    ] {
        if value < min || value > MAX_TIMEOUT_MS {
            issues.push(
                "contract.lifecycle.timeout-out-of-range",
                format!("spec.lifecycle.{field}"),
                format!("{field} must be within {min}..={MAX_TIMEOUT_MS} ms"),
            );
        }
    }
    match (lifecycle.replacement, &lifecycle.state) {
        (Replacement::Stateful, None) => issues.push(
            "contract.lifecycle.stateful-without-state",
            "spec.lifecycle.state",
            "stateful replacement requires a versioned `state` contract",
        ),
        (Replacement::Stateless | Replacement::Unsupported, Some(_)) => issues.push(
            "contract.lifecycle.state-without-stateful",
            "spec.lifecycle.replacement",
            "a `state` contract is only meaningful with `replacement: stateful`",
        ),
        (_, Some(state)) => {
            if state.schema_version == 0 {
                issues.push(
                    "contract.lifecycle.invalid-state-version",
                    "spec.lifecycle.state.schemaVersion",
                    "schemaVersion starts at 1",
                );
            }
            if state
                .migrates_from
                .iter()
                .any(|v| *v == 0 || *v >= state.schema_version)
            {
                issues.push(
                    "contract.lifecycle.invalid-migration",
                    "spec.lifecycle.state.migratesFrom",
                    "migratesFrom may only list older, positive schema versions",
                );
            }
        }
        _ => {}
    }

    for (i, d) in spec.permissions.devices.iter().enumerate() {
        let ok = d.path.strip_prefix("/dev/").is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .split('/')
                    .all(|s| !s.is_empty() && s != ".." && s != ".")
        });
        if !ok {
            issues.push(
                "contract.permissions.invalid-device",
                format!("spec.permissions.devices[{i}].path"),
                format!("`{}` must be a path under /dev/", d.path),
            );
        }
    }
    for (i, name) in spec.permissions.cdi_devices.iter().enumerate() {
        let valid = name.split_once('=').is_some_and(|(kind, dev)| {
            kind.split_once('/')
                .is_some_and(|(vendor, class)| vendor.contains('.') && !class.is_empty())
                && !dev.is_empty()
        });
        if !valid {
            issues.push(
                "contract.permissions.invalid-cdi-device",
                format!("spec.permissions.cdiDevices[{i}]"),
                format!("`{name}` is not `vendor.com/class=name`"),
            );
        }
    }

    let resources = &spec.resources;
    for (field, value) in [
        ("memoryMib", resources.memory_mib),
        ("cpuMillis", resources.cpu_millis),
        ("pids", resources.pids),
    ] {
        if value == Some(0) {
            issues.push(
                "contract.resources.zero-limit",
                format!("spec.resources.{field}"),
                "a declared limit must be positive",
            );
        }
    }
    if resources.enforcement == Enforcement::Required
        && resources.memory_mib.is_none()
        && resources.cpu_millis.is_none()
        && resources.pids.is_none()
    {
        issues.push(
            "contract.resources.required-without-limits",
            "spec.resources.enforcement",
            "`enforcement: required` needs at least one declared limit",
        );
    }
}
