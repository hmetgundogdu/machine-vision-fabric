//! Typed model of the Universal Node Contract (`apiVersion: mvf.dev/v1alpha1`, `kind: NodeContract`).
//!
//! Every struct denies unknown fields; every enum is closed. Values that need cross-field checks
//! (identifiers, semantic versions, digests, policy compatibility) are validated in
//! [`crate::validate`], which reports *all* problems at once with stable codes.

use serde::{Deserialize, Serialize};

/// The only accepted `apiVersion` for node contracts in this release.
pub const NODE_CONTRACT_API_VERSION: &str = "mvf.dev/v1alpha1";
/// The only accepted `kind` for node contracts.
pub const NODE_CONTRACT_KIND: &str = "NodeContract";

/// A complete node contract document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NodeContract {
    /// Must equal [`NODE_CONTRACT_API_VERSION`].
    pub api_version: String,
    /// Must equal [`NODE_CONTRACT_KIND`].
    pub kind: String,
    /// Stable logical identity of the node implementation.
    pub metadata: NodeMetadata,
    /// The typed interface and execution policy.
    pub spec: NodeSpec,
}

/// Stable logical identity. `id` + `version` identify one implementation generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NodeMetadata {
    /// Lower-case dotted/kebab identifier, e.g. `mvf.invert` or `vision.detector`.
    pub id: String,
    /// Semantic version of this implementation (`1.2.0`).
    pub version: String,
    /// Optional human description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Interface and policy of a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NodeSpec {
    /// Typed input and output ports.
    pub ports: Ports,
    /// Executable artifacts, in order of preference.
    pub artifacts: Vec<Artifact>,
    /// Backend selection and hardware placement.
    #[serde(default)]
    pub execution: Execution,
    /// Minimum isolation the node requires.
    #[serde(default)]
    pub isolation: Isolation,
    /// Memory domain and zero-copy requirement.
    #[serde(default)]
    pub memory: Memory,
    /// Activation, readiness, replacement and state contract.
    #[serde(default)]
    pub lifecycle: Lifecycle,
    /// Resource budget.
    #[serde(default)]
    pub resources: Resources,
    /// What a sandboxed node is granted. Everything not granted is denied.
    #[serde(default)]
    pub permissions: Permissions,
}

/// Grants for sandboxed execution (`isolation.minimum: sandbox` or stronger). A backend that cannot
/// provide a requested grant is rejected; nothing outside the grants is reachable. Backends weaker
/// than `sandbox` do not restrict anything, so permissions do not apply to them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Permissions {
    /// Network access.
    #[serde(default)]
    pub network: NetworkPolicy,
    /// Host device nodes made visible to the node.
    #[serde(default)]
    pub devices: Vec<DeviceGrant>,
    /// Container Device Interface devices (e.g. `nvidia.com/gpu=0`).
    #[serde(default)]
    pub cdi_devices: Vec<String>,
}

/// Network policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkPolicy {
    /// An empty network namespace: no interfaces besides a down loopback.
    #[default]
    None,
    /// Loopback only.
    Loopback,
}

/// One device grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DeviceGrant {
    /// Device path under `/dev/`.
    pub path: String,
    /// `r`, `w` or `rw`.
    #[serde(default = "default_rw")]
    pub access: DeviceAccess,
}

fn default_rw() -> DeviceAccess {
    DeviceAccess::Rw
}

/// Device access mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeviceAccess {
    /// Read.
    R,
    /// Write.
    W,
    /// Read and write.
    Rw,
}

/// Input and output ports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Ports {
    /// Input ports.
    #[serde(default)]
    pub inputs: Vec<Port>,
    /// Output ports.
    #[serde(default)]
    pub outputs: Vec<Port>,
}

/// One typed port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Port {
    /// Port name, unique per direction (`frame`, `ok`).
    pub name: String,
    /// Data or control channel — never interchangeable.
    pub channel: Channel,
    /// Payload type carried by the port.
    #[serde(rename = "type")]
    pub port_type: PortType,
    /// Inputs only: whether the node can run without this input connected.
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

/// Edge channel. Data edges move payloads; control edges move flow decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Channel {
    /// Frames, tensors, results, metadata.
    Data,
    /// Branch/gate/trigger decisions.
    Control,
}

/// Payload types. The set is closed; `signal` is the only control type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PortType {
    /// An image frame in the data plane (typed descriptor + bytes).
    Frame,
    /// A numeric tensor in the data plane.
    Tensor,
    /// A label + measurement result.
    Classification,
    /// Structured JSON metadata.
    Json,
    /// A typed scalar value.
    Value,
    /// A boolean control decision.
    Signal,
}

impl PortType {
    /// The channel this type belongs to.
    pub fn channel(self) -> Channel {
        match self {
            PortType::Signal => Channel::Control,
            _ => Channel::Data,
        }
    }

    /// Stable lower-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            PortType::Frame => "frame",
            PortType::Tensor => "tensor",
            PortType::Classification => "classification",
            PortType::Json => "json",
            PortType::Value => "value",
            PortType::Signal => "signal",
        }
    }
}

/// Execution backends known to the contract. Knowing a backend does not mean a host provides it;
/// availability comes from [`crate::BackendCapabilities`] at negotiation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    /// Trusted native module compiled into the host (shares the engine failure domain).
    InProcess,
    /// Supervised local worker process speaking the MVF stdio protocol.
    Worker,
    /// OCI image executed by a daemonless OCI backend.
    Oci,
    /// WASM component in an embedded sandbox.
    Wasm,
    /// Direct accelerator compute artifact (e.g. a WGSL compute shader).
    Compute,
}

impl BackendKind {
    /// Stable lower-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::InProcess => "in-process",
            BackendKind::Worker => "worker",
            BackendKind::Oci => "oci",
            BackendKind::Wasm => "wasm",
            BackendKind::Compute => "compute",
        }
    }
}

/// An executable artifact for one backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "backend",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Artifact {
    /// A builtin compiled into the host.
    InProcess {
        /// Registered builtin name, e.g. `invert`.
        builtin: String,
        /// Targets this artifact can run on (default `[cpu]`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        targets: Option<Vec<Target>>,
    },
    /// A local worker process.
    Worker {
        /// Interpreter/launcher.
        runtime: WorkerRuntime,
        /// Entry script or executable, relative to the contract file.
        entry: String,
        /// MVF stdio protocol version (only `1` exists).
        protocol: u32,
        /// Targets this artifact can run on (default `[cpu]`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        targets: Option<Vec<Target>>,
    },
    /// An OCI image.
    Oci {
        /// Image reference (`name:tag`), informative.
        image: String,
        /// Content digest (`sha256:<64 hex>`), required for execution.
        digest: String,
        /// Targets this artifact can run on (default `[cpu]`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        targets: Option<Vec<Target>>,
    },
    /// A WASM component.
    Wasm {
        /// Component file, relative to the contract file.
        module: String,
        /// Content digest (`sha256:<64 hex>`).
        digest: String,
    },
    /// A direct accelerator compute artifact.
    Compute {
        /// Compute API.
        api: ComputeApi,
        /// Kernel/shader file, relative to the contract file, or a registered kernel name.
        kernel: String,
        /// Entry point inside the kernel source.
        entry_point: String,
    },
}

impl Artifact {
    /// The backend that can execute this artifact.
    pub fn backend(&self) -> BackendKind {
        match self {
            Artifact::InProcess { .. } => BackendKind::InProcess,
            Artifact::Worker { .. } => BackendKind::Worker,
            Artifact::Oci { .. } => BackendKind::Oci,
            Artifact::Wasm { .. } => BackendKind::Wasm,
            Artifact::Compute { .. } => BackendKind::Compute,
        }
    }

    /// Targets the artifact can run on. A compute artifact is GPU-only; everything else defaults to
    /// CPU. The negotiation never translates an artifact to a target it did not declare.
    pub fn targets(&self) -> Vec<Target> {
        match self {
            Artifact::InProcess { targets, .. }
            | Artifact::Worker { targets, .. }
            | Artifact::Oci { targets, .. } => targets.clone().unwrap_or_else(|| vec![Target::Cpu]),
            Artifact::Wasm { .. } => vec![Target::Cpu],
            Artifact::Compute { .. } => vec![Target::Gpu],
        }
    }
}

/// Worker launchers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerRuntime {
    /// A Python script using the MVF Python SDK.
    Python,
    /// A native executable (e.g. built with the C++ SDK).
    Executable,
}

/// Compute APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComputeApi {
    /// WebGPU (wgpu) compute with WGSL kernels — Metal/Vulkan/DX12 underneath.
    Wgpu,
}

/// Hardware targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Target {
    /// General purpose CPU.
    Cpu,
    /// A GPU device.
    Gpu,
}

/// Backend selection and placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Execution {
    /// `auto` or one specific backend.
    #[serde(default)]
    pub backend: BackendPreference,
    /// Preferred target and what to do when it is unavailable.
    #[serde(default)]
    pub placement: Placement,
    /// How many instances of this node the pipelined executor may run side by side. Above 1 the
    /// author asserts the node keeps no state across frames (the .NET `maxParallelism`). Default 1.
    #[serde(default = "one_instance")]
    pub max_parallelism: u32,
}

fn one_instance() -> u32 {
    1
}

impl Default for Execution {
    fn default() -> Self {
        Self {
            backend: BackendPreference::Auto,
            placement: Placement::default(),
            max_parallelism: 1,
        }
    }
}

/// `auto` lets negotiation pick among declared artifacts in order; a named backend pins it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendPreference {
    /// Pick the first artifact whose backend satisfies every requirement.
    #[default]
    Auto,
    /// Pin [`BackendKind::InProcess`].
    InProcess,
    /// Pin [`BackendKind::Worker`].
    Worker,
    /// Pin [`BackendKind::Oci`].
    Oci,
    /// Pin [`BackendKind::Wasm`].
    Wasm,
    /// Pin [`BackendKind::Compute`].
    Compute,
}

impl BackendPreference {
    /// The pinned backend, if any.
    pub fn pinned(self) -> Option<BackendKind> {
        match self {
            BackendPreference::Auto => None,
            BackendPreference::InProcess => Some(BackendKind::InProcess),
            BackendPreference::Worker => Some(BackendKind::Worker),
            BackendPreference::Oci => Some(BackendKind::Oci),
            BackendPreference::Wasm => Some(BackendKind::Wasm),
            BackendPreference::Compute => Some(BackendKind::Compute),
        }
    }
}

/// Target placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Placement {
    /// Preferred target.
    #[serde(default = "default_cpu")]
    pub target: Target,
    /// What happens when the preferred target cannot be satisfied.
    #[serde(default)]
    pub fallback: Fallback,
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            target: Target::Cpu,
            fallback: Fallback::Deny,
        }
    }
}

fn default_cpu() -> Target {
    Target::Cpu
}

/// Placement fallback policy. The default is `deny`: never silently weaken placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Fallback {
    /// Reject the node when the preferred target is unavailable.
    #[default]
    Deny,
    /// Fall back to a CPU artifact, reported as a placement note.
    Cpu,
}

/// Minimum isolation. Replaces the ambiguous `isolation.required: true` boolean.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Isolation {
    /// Weakest acceptable isolation level.
    #[serde(default)]
    pub minimum: IsolationLevel,
    /// Sandbox syscall filter: `denylist` (default; blocks known-dangerous syscalls) or
    /// `allowlist` (default deny; only the syscalls an unprivileged container needs). Needs
    /// `minimum: sandbox`.
    #[serde(default)]
    pub syscalls: SyscallPolicy,
    /// `required`: run only under a Linux security module profile (AppArmor or SELinux) the host
    /// provides; fails closed where none is available. Needs `minimum: sandbox`.
    #[serde(default)]
    pub lsm: LsmRequirement,
}

/// Seccomp policy of a sandboxed node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyscallPolicy {
    /// Allow everything except a list of dangerous syscalls.
    #[default]
    Denylist,
    /// Deny everything except an allowlist (Docker's default profile, minus the denylist).
    Allowlist,
}

/// Whether a Linux security module profile is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LsmRequirement {
    /// Use one when the host configures it.
    #[default]
    Optional,
    /// Refuse to run without one.
    Required,
}

/// Ordered isolation levels. Each level states what it does **and does not** guarantee.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum IsolationLevel {
    /// Trusted, in the engine process: shares the engine's failure and security domain.
    #[default]
    None,
    /// Separate OS process: a crash is contained, but this is **not** a security boundary.
    Process,
    /// Enforced security sandbox (WASM sandbox, namespaces/cgroups/seccomp, AppContainer).
    Sandbox,
    /// Hardware virtualisation boundary.
    Vm,
}

impl IsolationLevel {
    /// Stable lower-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            IsolationLevel::None => "none",
            IsolationLevel::Process => "process",
            IsolationLevel::Sandbox => "sandbox",
            IsolationLevel::Vm => "vm",
        }
    }
}

/// Memory domain and zero-copy requirement.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Memory {
    /// Where the node's payload buffers live.
    #[serde(default)]
    pub domain: MemoryDomain,
    /// Whether payload hand-off must avoid copies.
    #[serde(default)]
    pub zero_copy: ZeroCopy,
    /// `shared`: the engine-wide arena (zero-copy between workers, but every worker can read and
    /// write every slot). `private`: an arena only this node's process maps; inputs from elsewhere
    /// are copied in (one counted copy), so no other node's payload is ever visible to it.
    #[serde(default)]
    pub arena: ArenaSharing,
}

/// Which shared-memory arena an out-of-process node maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArenaSharing {
    /// The engine-wide arena.
    #[default]
    Shared,
    /// An arena of its own.
    Private,
}

/// Memory domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryDomain {
    /// Host memory (shared-memory arena for cross-process hand-off).
    #[default]
    Host,
    /// Device-resident memory (GPU buffers) on the node's device.
    Device,
}

/// Zero-copy requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ZeroCopy {
    /// Hand-off must not copy; negotiation fails if the backend cannot guarantee it.
    Required,
    /// Use zero copy when available; a copying path is reported as a placement note.
    #[default]
    Preferred,
    /// Copying is acceptable.
    Disabled,
}

/// Lifecycle contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Lifecycle {
    /// When the node is activated.
    #[serde(default)]
    pub activation: Activation,
    /// Upper bound for activation + readiness, in milliseconds.
    #[serde(default = "default_readiness_ms")]
    pub readiness_timeout_ms: u64,
    /// Upper bound for a graceful drain/stop, in milliseconds.
    #[serde(default = "default_drain_ms")]
    pub drain_timeout_ms: u64,
    /// Upper bound for one execution, in milliseconds (`0` = no deadline).
    #[serde(default)]
    pub execute_timeout_ms: u64,
    /// Live replacement support.
    #[serde(default)]
    pub replacement: Replacement,
    /// Versioned state contract, required for stateful replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<StateContract>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            activation: Activation::Resident,
            readiness_timeout_ms: default_readiness_ms(),
            drain_timeout_ms: default_drain_ms(),
            execute_timeout_ms: 0,
            replacement: Replacement::Unsupported,
            state: None,
        }
    }
}

fn default_readiness_ms() -> u64 {
    30_000
}

fn default_drain_ms() -> u64 {
    5_000
}

/// Activation profile (MVF lifecycle defaults: sources, PLC and models are resident).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Activation {
    /// Activated once and kept warm.
    #[default]
    Resident,
    /// Activated when first needed.
    OnDemand,
}

/// What kind of live replacement the node supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Replacement {
    /// The node cannot be replaced without a graph restart.
    #[default]
    Unsupported,
    /// The node keeps no state across cycles; a new generation starts empty.
    Stateless,
    /// The node exports/imports versioned state (see [`StateContract`]).
    Stateful,
}

/// Versioned state contract for stateful replacement and recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StateContract {
    /// Schema version this implementation exports.
    pub schema_version: u32,
    /// Older schema versions this implementation can import (migrate from).
    #[serde(default)]
    pub migrates_from: Vec<u32>,
}

/// Resource budget and how strictly it must be enforced.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Resources {
    /// Memory limit in MiB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mib: Option<u64>,
    /// CPU limit in millicores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_millis: Option<u64>,
    /// Maximum number of processes/threads (pids limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids: Option<u64>,
    /// Whether limits must be enforced by the backend or are advisory.
    #[serde(default)]
    pub enforcement: Enforcement,
}

/// Resource enforcement policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Enforcement {
    /// Limits are advisory; a backend that cannot enforce them is still acceptable (noted).
    #[default]
    BestEffort,
    /// Reject any backend that cannot enforce every declared limit.
    Required,
}
