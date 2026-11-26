//! # mvf-worker — supervised local stdio worker backend
//!
//! Runs a node as a separate OS process speaking the existing MVF worker protocol v1
//! (`protocol/README.md`), so modules written with the existing Python and C++ SDKs run unchanged
//! under the Rust host. Local only: stdio for control, the engine-owned shared-memory arena for
//! payloads; no network.
//!
//! Capabilities (honest, per host):
//! - isolation `process`: a crash or hang is contained and the process can be killed; **not** a
//!   security boundary (same user, filesystem and network);
//! - cancellation `enforceable`: a hung execution is terminated by killing the process;
//! - no resource limits are enforced (no cgroups/Job Objects in this backend);
//! - stateless replacement only.
//!
//! Lifecycle: spawn once at `start` (never per frame) → `hello` (+ `ready` after warmup) within the
//! contract's readiness budget → `execute`* → `shutdown` + bounded grace → kill. A handle that is
//! dropped kills and reaps its child.

mod pool;
pub mod process;
pub mod protocol;
pub mod spawner;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mvf_contract::{
    Artifact, BackendCapabilities, BackendKind, Cancellation, Channel, IsolationLevel,
    MemoryDomain, PortType, Replacement, Target, WorkerRuntime,
};
use mvf_engine::{
    Classification, ExecContext, Execution, ExecutionBackend, Frame, HostServices, Inputs,
    InstantiateRequest, LifecycleContext, NodeError, NodeErrorKind, NodeHandle, Outputs, Payload,
    StateBlob, StopOutcome, Value,
};
use mvf_graph::ContractOrigin;
use mvf_memory::{ArenaError, MediaType};

use crate::process::{LaunchSpec, WaitError, WorkerProcess};
use crate::protocol::ChildMessage;

/// The worker backend.
#[derive(Debug, Clone)]
pub struct WorkerBackend {
    python: OsString,
    python_sdk: Option<PathBuf>,
    verify_inputs: bool,
    warm_spares: usize,
    isolate_arenas: bool,
    pools: pool::PoolRegistry,
    /// The session this engine runs in (native windows need an interactive one).
    session: mvf_display::Session,
}

impl Default for WorkerBackend {
    fn default() -> Self {
        Self::from_env()
    }
}

impl WorkerBackend {
    /// Interpreter from `MVF_PYTHON` (default `python3`, `python` on Windows); SDK path from
    /// `MVF_PYTHON_SDK` or discovered by walking up from each entry to `src/sdk/python`.
    pub fn from_env() -> Self {
        let default = if cfg!(windows) { "python" } else { "python3" };
        Self {
            python: std::env::var_os("MVF_PYTHON").unwrap_or_else(|| default.into()),
            python_sdk: std::env::var_os("MVF_PYTHON_SDK").map(PathBuf::from),
            verify_inputs: std::env::var_os("MVF_VERIFY_INPUTS").is_some_and(|v| v == "1"),
            warm_spares: std::env::var("MVF_WARM_SPARES")
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0),
            isolate_arenas: std::env::var_os("MVF_ISOLATE_ARENAS").is_some_and(|v| v == "1"),
            pools: pool::PoolRegistry::default(),
            session: mvf_display::session::probe(),
        }
    }

    /// Overrides the probed session (tests; a host that knows better).
    pub fn with_session(mut self, session: mvf_display::Session) -> Self {
        self.session = session;
        self
    }

    /// Keeps `n` pre-warmed spare workers per node, so a restart or re-open skips the cold start
    /// (parity with the .NET `WarmWorkerPool`; also `MVF_WARM_SPARES=n`). Default 0.
    pub fn with_warm_spares(mut self, n: usize) -> Self {
        self.warm_spares = n;
        self
    }

    /// Gives every worker an arena of its own, whatever its contract says (also
    /// `MVF_ISOLATE_ARENAS=1`): no worker can see or touch another node's payloads.
    pub fn with_isolated_arenas(mut self, on: bool) -> Self {
        self.isolate_arenas = on;
        self
    }

    /// Fingerprints every input before and after `execute` and fails the node if a worker wrote to
    /// its read-only input (detection only; also enabled by `MVF_VERIFY_INPUTS=1`).
    pub fn with_input_verification(mut self, on: bool) -> Self {
        self.verify_inputs = on;
        self
    }

    /// Uses the Python SDK at `sdk` (the directory holding `mvf_sdk/`) for every module, instead of
    /// discovering `src/sdk/python` above each module (also `MVF_PYTHON_SDK`).
    pub fn with_python_sdk(mut self, sdk: impl Into<PathBuf>) -> Self {
        self.python_sdk = Some(sdk.into());
        self
    }

    /// Uses `python` as the interpreter.
    pub fn with_python(mut self, python: impl Into<OsString>) -> Self {
        self.python = python.into();
        self
    }

    fn sdk_for(&self, entry_dir: &Path) -> Option<PathBuf> {
        if let Some(sdk) = &self.python_sdk {
            return Some(sdk.clone());
        }
        let mut dir = Some(entry_dir);
        for _ in 0..8 {
            let d = dir?;
            let candidate = d.join("src/sdk/python");
            if candidate.join("mvf_sdk/__init__.py").is_file() {
                return Some(candidate);
            }
            dir = d.parent();
        }
        None
    }
}

impl ExecutionBackend for WorkerBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            kind: BackendKind::Worker,
            isolation: IsolationLevel::Process,
            targets: vec![Target::Cpu],
            // Arena-resident payloads cross the process boundary by handle (offset), never inline.
            zero_copy_domains: vec![MemoryDomain::Host],
            cancellation: Cancellation::Enforceable,
            enforceable_termination: true,
            enforces_memory_limit: false,
            enforces_cpu_limit: false,
            enforces_pids_limit: false,
            // Stateful through the protocol's `checkpoint`/`restore` messages.
            replacement: vec![Replacement::Stateless, Replacement::Stateful],
            grants: Default::default(),
            // Each node can get an arena of its own (`memory.arena: private`).
            private_arenas: true,
            // Resolved host paths in `MVF_VOLUME_*`; a worker runs with its user's filesystem, so
            // neither the mount set, read-only access nor scratch quotas are enforced.
            storage: mvf_contract::StorageSupport {
                mounts: true,
                enforced: false,
                hard_quota: false,
            },
            // A worker inherits the engine's session: native windows only in an interactive one.
            // Managed surfaces need no session at all (the host is a separate process).
            display: mvf_contract::DisplaySupport {
                native: self.session.interactive(),
                managed_surface: true,
                fullscreen: false,
                session: self.session.detail.clone(),
            },
        }
    }

    fn instantiate(
        &self,
        request: &InstantiateRequest<'_>,
        artifact: &Artifact,
    ) -> Result<Box<dyn NodeHandle>, NodeError> {
        let Artifact::Worker { runtime, entry, .. } = artifact else {
            return Err(unsupported("worker backend received a non-worker artifact"));
        };
        let ContractOrigin::File(contract_path) = request.origin else {
            return Err(unsupported(
                "worker artifacts must come from a contract file",
            ));
        };
        // Absolute: the child runs with this directory as its cwd, so a relative entry path would
        // be applied twice.
        let parent = Path::new(contract_path).parent().unwrap_or(Path::new("."));
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        let contract_dir = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
        let entry_path = contract_dir.join(entry);
        if !entry_path.is_file() {
            return Err(NodeError::new(
                NodeErrorKind::StartFailed,
                format!("worker entry `{}` does not exist", entry_path.display()),
            ));
        }
        let mapping = PortMapping::from_contract(request.contract)?;

        let mut env = vec![("PYTHONUNBUFFERED".to_string(), OsString::from("1"))];
        let (program, args) = match runtime {
            WorkerRuntime::Python => {
                if let Some(sdk) = self.sdk_for(&contract_dir) {
                    let mut path = sdk.into_os_string();
                    if let Some(existing) = std::env::var_os("PYTHONPATH") {
                        path.push(if cfg!(windows) { ";" } else { ":" });
                        path.push(existing);
                    }
                    env.push(("PYTHONPATH".into(), path));
                }
                (
                    self.python.clone(),
                    vec![entry_path.clone().into_os_string()],
                )
            }
            WorkerRuntime::Executable => (entry_path.clone().into_os_string(), vec![]),
        };
        let _ = mapping;
        let display = display_plan(request, &self.session)?;
        env.extend(display.env);
        let storage = if request.storage.is_empty() {
            None
        } else {
            let grant = request
                .services
                .storage
                .provision(
                    request.node_id,
                    request.generation,
                    request.storage,
                    request.services.package_dir.as_deref(),
                    mvf_storage::Scratch::HostDir,
                    None,
                )
                .map_err(|e| NodeError::new(NodeErrorKind::StartFailed, e.to_string()))?;
            env.extend(grant.host_env());
            request.services.log(
                request.node_id,
                request.generation,
                "info",
                format!(
                    "storage: {} (host paths in MVF_VOLUME_*; isolation `process`: read-only, the mount set and scratch sizes are not enforced)",
                    grant
                        .mounts
                        .iter()
                        .map(mvf_storage::Provisioned::describe)
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            );
            Some(grant)
        };
        if let Some(note) = display.note {
            request
                .services
                .log(request.node_id, request.generation, "info", note);
        }
        let spec = LaunchSpec {
            program,
            args,
            cwd: contract_dir,
            env,
            remove_env: display.remove_env,
        };
        let private = self.isolate_arenas
            || request.contract.spec.memory.arena == mvf_contract::ArenaSharing::Private;
        // A warm spare would carry another generation's scratch paths: none for nodes with mounts.
        let recipe = (self.warm_spares > 0 && request.storage.is_empty()).then(|| PoolRecipe {
            registry: self.pools.clone(),
            key: format!(
                "{}|{:?}|{:?}|{}|{:?}|{private}",
                request.node_id,
                spec.program,
                spec.args,
                spec.cwd.display(),
                spec.env
            ),
            target: self.warm_spares,
            spec: spec.clone(),
            budget: Duration::from_millis(request.contract.spec.lifecycle.readiness_timeout_ms),
        });
        build_worker_node(
            request,
            Box::new(ProcessLauncher { spec }),
            self.verify_inputs,
            private,
            recipe,
            true,
            storage,
        )
    }
}

/// Produces the command that starts a worker once the engine-owned arena exists, and cleans up
/// whatever it created when the worker is gone. A plain process and an OCI container are both
/// launchers; the protocol, supervision and data plane are identical.
pub trait Launcher: Send {
    /// The command to spawn. `arena` is the host path of the engine-owned arena.
    fn prepare(&mut self, arena: &Path) -> Result<LaunchSpec, NodeError>;
    /// Releases launcher resources (containers, bundles). Must be idempotent.
    fn cleanup(&mut self) {}
    /// This generation became the active one: take over what the launcher exposes outside the
    /// graph (published network listeners).
    fn promote(&mut self) -> Result<(), NodeError> {
        Ok(())
    }
}

/// Launches a local process with `MVF_ARENA_PATH` pointing at the arena.
#[derive(Debug, Clone)]
pub struct ProcessLauncher {
    /// Base command.
    pub spec: LaunchSpec,
}

impl Launcher for ProcessLauncher {
    fn prepare(&mut self, arena: &Path) -> Result<LaunchSpec, NodeError> {
        let mut spec = self.spec.clone();
        spec.env
            .push(("MVF_ARENA_PATH".into(), arena.as_os_str().to_owned()));
        Ok(spec)
    }
}

/// Builds a supervised protocol-v1 worker handle around any launcher (private arena when the
/// contract asks for one).
pub fn worker_node(
    request: &InstantiateRequest<'_>,
    launcher: Box<dyn Launcher>,
    verify_inputs: bool,
) -> Result<Box<dyn NodeHandle>, NodeError> {
    build_worker_node(request, launcher, verify_inputs, false, None, false, None)
}

fn build_worker_node(
    request: &InstantiateRequest<'_>,
    launcher: Box<dyn Launcher>,
    verify_inputs: bool,
    private: bool,
    recipe: Option<PoolRecipe>,
    trusted_network: bool,
    storage: Option<mvf_storage::Grant>,
) -> Result<Box<dyn NodeHandle>, NodeError> {
    let recipe_marker = trusted_network;
    Ok(Box::new(WorkerNode {
        node_id: request.node_id.to_string(),
        generation: request.generation,
        launcher,
        config: request.config.clone(),
        services: request.services.clone(),
        mapping: PortMapping::from_contract(request.contract)?,
        state_schema: request
            .contract
            .spec
            .lifecycle
            .state
            .as_ref()
            .map(|s| s.schema_version)
            .unwrap_or(1),
        process: None,
        capability: Capability::Classifier,
        next_id: 1,
        verify_inputs,
        features: Vec::new(),
        private: private
            || request.contract.spec.memory.arena == mvf_contract::ArenaSharing::Private,
        private_arena: None,
        network_note: recipe_marker.then(|| {
            format!(
                "network: shares the host network (isolation `process`; `{:?}` is not enforced)",
                request.contract.spec.permissions.network.mode()
            )
            .replace("None", "none")
            .replace("Loopback", "loopback")
            .replace("Host", "host")
        }),
        recipe,
        pool: None,
        produced: 0,
        storage,
    }))
}

/// The surfaces a managed-surface node asks for (title, sizes, input grants), from its contract.
pub fn surface_request(
    node_id: &str,
    contract: &mvf_contract::NodeContract,
) -> mvf_display::SurfaceRequest {
    let d = &contract.spec.permissions.display;
    let p = contract.spec.presentation.as_ref();
    mvf_display::SurfaceRequest {
        title: p
            .and_then(|p| p.title.clone())
            .unwrap_or_else(|| node_id.to_string()),
        width: p.and_then(|p| p.width).unwrap_or(d.max_width.min(1280)),
        height: p.and_then(|p| p.height).unwrap_or(d.max_height.min(720)),
        max_width: d.max_width,
        max_height: d.max_height,
        windows: d.max_windows,
        grants: mvf_display::Grants {
            keyboard: d.keyboard,
            mouse: d.mouse,
        },
    }
}

/// What a worker gets for `permissions.display`.
struct DisplayPlan {
    env: Vec<(String, OsString)>,
    remove_env: Vec<String>,
    note: Option<String>,
}

/// Native nodes keep the session's display environment; every other node has it withheld (least
/// privilege — a worker is not sandboxed, so this is hygiene, not enforcement). Managed-surface
/// nodes get their surfaces from the engine's registry: the same files for every generation.
fn display_plan(
    request: &InstantiateRequest<'_>,
    session: &mvf_display::Session,
) -> Result<DisplayPlan, NodeError> {
    use mvf_contract::DisplayMode;
    let d = &request.contract.spec.permissions.display;
    let withheld = || {
        mvf_display::session::DISPLAY_ENV
            .iter()
            .map(|k| k.to_string())
            .collect::<Vec<_>>()
    };
    let headless = |why: String| DisplayPlan {
        env: vec![],
        remove_env: withheld(),
        note: Some(format!("display: headless — {why}")),
    };
    match d.mode {
        DisplayMode::None => Ok(DisplayPlan {
            env: vec![],
            remove_env: withheld(),
            note: None,
        }),
        DisplayMode::Native if session.interactive() => Ok(DisplayPlan {
            env: vec![],
            remove_env: vec![],
            note: Some(format!(
                "display: native windows in the {} session (the application owns its windows \
                 and input; isolation `process` does not confine them)",
                session.detail
            )),
        }),
        // Negotiation admitted this only for `required: false`.
        DisplayMode::Native => Ok(headless(format!(
            "native windows unavailable ({})",
            session.detail
        ))),
        DisplayMode::ManagedSurface => {
            let surface_request = surface_request(request.node_id, request.contract);
            let surfaces = match request
                .services
                .display
                .surfaces(request.node_id, &surface_request)
            {
                Ok(s) => s,
                Err(e) if !d.required => return Ok(headless(e.to_string())),
                Err(e) => return Err(NodeError::new(NodeErrorKind::StartFailed, e.to_string())),
            };
            let joined = std::env::join_paths(&surfaces).map_err(|e| {
                NodeError::new(NodeErrorKind::StartFailed, format!("display surfaces: {e}"))
            })?;
            let dir = request
                .services
                .display
                .run_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default();
            Ok(DisplayPlan {
                env: vec![("MVF_DISPLAY_SURFACES".into(), joined)],
                remove_env: withheld(),
                note: Some(format!(
                    "display: {} managed surface(s) in {dir} (input: keyboard={}, mouse={}); a \
                     display host in the user session presents them, the graph never waits for it",
                    surfaces.len(),
                    d.keyboard,
                    d.mouse
                )),
            })
        }
    }
}

fn unsupported(message: impl Into<String>) -> NodeError {
    NodeError::new(NodeErrorKind::Unsupported, message)
}

/// How contract ports map onto protocol v1 messages (one frame in; frame/classification/json out).
#[derive(Debug, Clone)]
struct PortMapping {
    /// The frame input; `None` for a source.
    input: Option<String>,
    frame_out: Option<String>,
    class_out: Option<String>,
    json_out: Option<String>,
}

impl PortMapping {
    fn from_contract(contract: &mvf_contract::NodeContract) -> Result<Self, NodeError> {
        let ports = &contract.spec.ports;
        // A source (no input) is the `source` capability; everything else takes one frame.
        let input = match ports.inputs.as_slice() {
            [] => None,
            [input] if input.channel == Channel::Data && input.port_type == PortType::Frame => {
                Some(input.name.clone())
            }
            _ => {
                return Err(unsupported(
                    "protocol v1 workers take no input (a source) or exactly one `frame` data input",
                ));
            }
        };
        let mut mapping = Self {
            input,
            frame_out: None,
            class_out: None,
            json_out: None,
        };
        for port in &ports.outputs {
            let slot = match port.port_type {
                PortType::Frame => &mut mapping.frame_out,
                PortType::Classification => &mut mapping.class_out,
                PortType::Json => &mut mapping.json_out,
                other => {
                    return Err(unsupported(format!(
                        "protocol v1 workers cannot emit `{}` (port `{}`)",
                        other.as_str(),
                        port.name
                    )));
                }
            };
            if slot.replace(port.name.clone()).is_some() {
                return Err(unsupported(format!(
                    "protocol v1 workers emit at most one `{}` output",
                    port.port_type.as_str()
                )));
            }
        }
        Ok(mapping)
    }

    /// Checks the worker's advertised capability against the contract's outputs.
    fn accepts(&self, capability: Capability) -> Result<(), String> {
        let outputs = (
            self.frame_out.is_some(),
            self.class_out.is_some(),
            self.json_out.is_some(),
        );
        let ok = match (capability, self.input.is_some()) {
            (Capability::Source, false) => outputs == (true, false, false),
            (Capability::Source, true) | (_, false) => false,
            (Capability::Processor, true) => outputs == (true, false, false),
            (Capability::Classifier, true) => outputs == (false, true, false),
            (Capability::Analyzer, true) => true,
        };
        if ok {
            Ok(())
        } else {
            Err(format!(
                "worker advertises `{capability:?}` but the contract declares outputs (frame, classification, json) = {outputs:?}"
            ))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capability {
    Classifier,
    Processor,
    Analyzer,
    /// No input; one frame per execute (protocol v1 extension).
    Source,
}

struct WorkerNode {
    node_id: String,
    generation: u64,
    launcher: Box<dyn Launcher>,
    config: serde_json::Map<String, serde_json::Value>,
    services: HostServices,
    mapping: PortMapping,
    /// Schema version stamped on exported state (`lifecycle.state.schemaVersion`, default 1).
    state_schema: u32,
    process: Option<WorkerProcess>,
    capability: Capability,
    next_id: u64,
    verify_inputs: bool,
    /// Features the running worker advertised in `hello`.
    features: Vec<String>,
    /// Private-arena mode (`spec.memory.arena: private`, or the host forces it).
    private: bool,
    /// The arena only this node's process maps (private mode).
    private_arena: Option<std::sync::Arc<mvf_memory::Arena>>,
    /// Warm-spare recipe (process launchers with `MVF_WARM_SPARES` > 0).
    recipe: Option<PoolRecipe>,
    pool: Option<std::sync::Arc<pool::WarmPool>>,
    /// Logged once started: what the node's network actually is (trusted workers share the host's).
    network_note: Option<String>,
    /// Frames a source produced (default sequence numbers).
    produced: u64,
    /// This generation's mounts; scratch and artifact views go when it stops.
    storage: Option<mvf_storage::Grant>,
}

impl WorkerNode {
    /// Sends `configure` and waits for `configured` (or the worker's error).
    fn send_configure(
        &mut self,
        config: &serde_json::Map<String, serde_json::Value>,
        deadline: Instant,
        cancel: &mvf_engine::CancellationToken,
    ) -> Result<(), NodeError> {
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::json!({"type": "configure", "id": id, "config": config}).to_string();
        let reply = match self.process.as_mut() {
            Some(p) => p.send(&line).and_then(|_| {
                p.wait_for(Some(deadline), Some(cancel), |m| match m {
                    ChildMessage::Other(t) => t == "configured",
                    ChildMessage::Error { id: e, .. } => *e == Some(id),
                    _ => false,
                })
            }),
            None => Err(WaitError::Cancelled),
        };
        match reply {
            Ok(ChildMessage::Error { message, .. }) => Err(NodeError::new(
                NodeErrorKind::InvalidConfig,
                format!("worker rejected config: {message}"),
            )),
            Ok(_) => Ok(()),
            Err(e) => Err(self.fail(e, "configure")),
        }
    }

    fn log(&self, message: impl Into<String>) {
        self.services
            .log(&self.node_id, self.generation, "supervisor", message);
    }

    /// Kills the child (if any) and converts a wait failure into a classified node error.
    fn fail(&mut self, error: WaitError, context: &str) -> NodeError {
        let process = self.process.as_mut();
        let e = match error {
            WaitError::Timeout => {
                if let Some(p) = process {
                    p.kill();
                }
                NodeError::new(
                    NodeErrorKind::Timeout,
                    format!("{context}: deadline elapsed; worker terminated"),
                )
            }
            WaitError::Cancelled => {
                if let Some(p) = process {
                    p.kill();
                }
                NodeError::new(
                    NodeErrorKind::Cancelled,
                    format!("{context}: cancelled; worker terminated"),
                )
            }
            WaitError::Exited(info) => info.to_error(&format!("during {context}")),
        };
        self.launcher.cleanup();
        self.log(e.message.clone());
        e
    }

    /// The arena this node's process maps: its own in private mode, else the engine's.
    fn arena(&self) -> Result<std::sync::Arc<mvf_memory::Arena>, NodeError> {
        if let Some(private) = &self.private_arena {
            return Ok(std::sync::Arc::clone(private));
        }
        self.services
            .arena()
            .map_err(|e| NodeError::new(NodeErrorKind::Resource, e.to_string()))
    }

    /// A private arena has the engine arena's slot size and directory, but only the slots one node
    /// needs (its input copy, its output, a state slot and a few outputs still held downstream):
    /// `MVF_PRIVATE_ARENA_SLOTS`, default 8, never more than the engine arena. Every private arena
    /// reserves `slotSize × slots` in the backing directory (`/dev/shm` on Linux), so it is kept
    /// small; a pipelined node whose outputs queue deeper waits (`stall`) or drops.
    fn private_config(&self) -> mvf_memory::ArenaConfig {
        let mut config = self.services.arena_config.clone();
        let slots = std::env::var("MVF_PRIVATE_ARENA_SLOTS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(8);
        config.slot_count = slots.min(config.slot_count.max(1));
        config
    }

    /// A source cycle: hand the worker an output slot, get a frame (or exhaustion) back. The
    /// frame stays in shared memory; downstream nodes share the lease.
    fn produce(&mut self, ctx: &ExecContext<'_>) -> Result<Execution, NodeError> {
        if let Some(info) = self.process.as_mut().and_then(WorkerProcess::exit) {
            return Err(info.to_error("before execute"));
        }
        let arena = self.arena()?;
        let reservation = arena.reserve().map_err(resource)?;
        let id = self.next_id;
        self.next_id += 1;
        let line = protocol::execute_source(id, reservation.offset(), reservation.capacity());
        let reply = match self.process.as_mut() {
            Some(p) => p.send(&line).and_then(|_| {
                p.wait_for(ctx.deadline, Some(ctx.cancel), |m| match m {
                    ChildMessage::Result { id: r, .. } => *r == id,
                    ChildMessage::Error { id: r, .. } => *r == Some(id),
                    _ => false,
                })
            }),
            None => {
                return Err(NodeError::new(
                    NodeErrorKind::ProcessExited,
                    "worker is not running",
                ));
            }
        };
        let (frame, camera, sequence, exhausted) = match reply {
            Ok(ChildMessage::Result {
                frame,
                camera_id,
                sequence,
                exhausted,
                ..
            }) => (frame, camera_id, sequence, exhausted),
            Ok(ChildMessage::Error { message, .. }) => {
                return Err(NodeError::failed(format!("worker error: {message}")));
            }
            Ok(_) => unreachable!("accept() only admits result/error"),
            Err(e) => return Err(self.fail(e, &format!("execute (cycle {})", ctx.cycle))),
        };
        if exhausted {
            return Ok(Execution::Exhausted);
        }
        let Some(Some(offset)) = frame else {
            return Err(NodeError::new(
                NodeErrorKind::Protocol,
                "a source must return a frame or `exhausted`",
            ));
        };
        if offset != reservation.offset() {
            return Err(NodeError::new(
                NodeErrorKind::Protocol,
                format!(
                    "worker returned offset {offset} but owns only the output slot at {}",
                    reservation.offset()
                ),
            ));
        }
        let lease = reservation.seal().map_err(|e| {
            NodeError::new(
                NodeErrorKind::InvalidOutput,
                format!("worker output rejected: {e}"),
            )
        })?;
        self.produced += 1;
        let port = self
            .mapping
            .frame_out
            .clone()
            .unwrap_or_else(|| "frame".into());
        Ok(Execution::Produced(Outputs::new().with(
            port,
            Value::Frame(Frame {
                meta: mvf_engine::FrameMeta {
                    source: camera.unwrap_or_else(|| self.node_id.clone()),
                    sequence: sequence.unwrap_or(self.produced),
                },
                payload: Payload::arena(lease),
            }),
        )))
    }

    /// The node's warm pool, created (and warming) on first use; shared with later generations.
    fn warm_pool(&mut self) -> Option<std::sync::Arc<pool::WarmPool>> {
        if self.pool.is_none()
            && let Some(recipe) = &self.recipe
        {
            let spawn = spare_spawner(
                recipe,
                &self.node_id,
                &self.services,
                self.private.then(|| self.private_config()),
            );
            self.pool = recipe
                .registry
                .get_or_start(&recipe.key, recipe.target, || spawn);
        }
        self.pool.clone()
    }
}

/// Why a handshake failed.
enum Handshake {
    Wait(WaitError, &'static str),
    Protocol(u64),
}

/// `hello`, the protocol check and (when the child warms up first) `ready`, within `deadline`.
fn handshake(
    process: &mut WorkerProcess,
    deadline: Instant,
    cancel: &mvf_engine::CancellationToken,
) -> Result<pool::Hello, Handshake> {
    let hello = process
        .wait_for(Some(deadline), Some(cancel), |m| {
            matches!(m, ChildMessage::Hello { .. })
        })
        .map_err(|e| Handshake::Wait(e, "readiness (waiting for hello)"))?;
    let ChildMessage::Hello {
        protocol,
        capability,
        ready,
        features,
        ..
    } = hello
    else {
        unreachable!("accept() only admits hello");
    };
    if protocol != protocol::PROTOCOL_VERSION {
        return Err(Handshake::Protocol(protocol));
    }
    if !ready {
        process
            .wait_for(Some(deadline), Some(cancel), |m| {
                matches!(m, ChildMessage::Ready)
            })
            .map_err(|e| Handshake::Wait(e, "readiness (waiting for ready)"))?;
    }
    Ok(pool::Hello {
        capability,
        features,
    })
}

/// How to warm spares for one node.
#[derive(Debug, Clone)]
struct PoolRecipe {
    registry: pool::PoolRegistry,
    key: String,
    target: usize,
    spec: LaunchSpec,
    budget: Duration,
}

/// Builds the warmup closure: launch, handshake, hand over (with its own arena in private mode).
fn spare_spawner(
    recipe: &PoolRecipe,
    node: &str,
    services: &HostServices,
    private: Option<mvf_memory::ArenaConfig>,
) -> std::sync::Arc<dyn Fn() -> Result<pool::Spare, NodeError> + Send + Sync> {
    let spec = recipe.spec.clone();
    let budget = recipe.budget;
    let node = node.to_string();
    let services = services.clone();
    std::sync::Arc::new(move || {
        let own = private
            .as_ref()
            .map(mvf_memory::Arena::create)
            .transpose()
            .map_err(resource)?;
        let arena = match &own {
            Some(a) => std::sync::Arc::clone(a),
            None => services.arena().map_err(resource)?,
        };
        let launch = ProcessLauncher { spec: spec.clone() }.prepare(arena.path())?;
        let mut process = WorkerProcess::spawn(&launch, &node, 0, &services.logs)
            .map_err(|e| NodeError::new(NodeErrorKind::StartFailed, e.to_string()))?;
        let cancel = mvf_engine::CancellationToken::new();
        let pid = process.pid();
        let hello = handshake(&mut process, Instant::now() + budget, &cancel).map_err(|e| {
            NodeError::new(
                NodeErrorKind::StartFailed,
                match e {
                    Handshake::Wait(_, context) => format!("warm spare: {context} failed"),
                    Handshake::Protocol(p) => format!("warm spare speaks protocol {p}"),
                },
            )
        })?;
        services.log(&node, 0, "supervisor", format!("warmed spare pid={pid}"));
        Ok(pool::Spare {
            process,
            hello,
            arena: own,
        })
    })
}

fn resource(e: ArenaError) -> NodeError {
    NodeError::from_arena(&e)
}

impl NodeHandle for WorkerNode {
    fn start(&mut self, ctx: &LifecycleContext<'_>) -> Result<(), NodeError> {
        let started = Instant::now();
        let deadline = started + ctx.budget;
        // A warm spare (already handshaked) when the pool has one; otherwise a cold spawn.
        let spare = self.warm_pool().and_then(|pool| {
            let spare = pool.acquire();
            if let Some(s) = &spare {
                s.process.set_generation(self.generation);
                self.log(format!(
                    "took warm spare pid={} (warm starts {}, spares ready {})",
                    s.process.pid(),
                    pool.hits(),
                    pool.ready_count()
                ));
            }
            spare
        });
        let hello = match spare {
            Some(spare) => {
                self.private_arena = spare.arena;
                self.process = Some(spare.process);
                spare.hello
            }
            None => {
                self.private_arena = match self.private {
                    true => {
                        Some(mvf_memory::Arena::create(&self.private_config()).map_err(resource)?)
                    }
                    false => None,
                };
                let arena = self.arena()?;
                let spec = self.launcher.prepare(arena.path())?;
                let process = WorkerProcess::spawn(
                    &spec,
                    &self.node_id,
                    self.generation,
                    &self.services.logs,
                )
                .map_err(|e| {
                    NodeError::new(
                        NodeErrorKind::StartFailed,
                        format!("could not spawn {:?}: {e}", spec.program),
                    )
                })?;
                let pid = process.pid();
                self.process = Some(process);
                self.log(format!("spawned pid={pid}"));
                let shaken = match self.process.as_mut() {
                    Some(p) => handshake(p, deadline, ctx.cancel),
                    None => Err(Handshake::Wait(
                        WaitError::Cancelled,
                        "readiness (waiting for hello)",
                    )),
                };
                match shaken {
                    Ok(hello) => hello,
                    Err(Handshake::Wait(e, context)) => return Err(self.fail(e, context)),
                    Err(Handshake::Protocol(protocol)) => {
                        self.process.as_mut().map(WorkerProcess::kill);
                        return Err(NodeError::new(
                            NodeErrorKind::Protocol,
                            format!("worker speaks protocol {protocol}; this host speaks 1"),
                        ));
                    }
                }
            }
        };
        let pid = self.process.as_ref().map(WorkerProcess::pid).unwrap_or(0);
        let crate::pool::Hello {
            capability,
            features,
        } = hello;
        self.capability = match capability.as_str() {
            "classifier" => Capability::Classifier,
            "processor" => Capability::Processor,
            "analyzer" => Capability::Analyzer,
            "source" => Capability::Source,
            other => {
                self.process.as_mut().map(WorkerProcess::kill);
                return Err(NodeError::new(
                    NodeErrorKind::Protocol,
                    format!("unknown worker capability `{other}`"),
                ));
            }
        };
        if let Err(message) = self.mapping.accepts(self.capability) {
            self.process.as_mut().map(WorkerProcess::kill);
            return Err(NodeError::new(NodeErrorKind::StartFailed, message));
        }
        self.features = features;
        if !self.config.is_empty() {
            if !self.features.iter().any(|f| f == "configure") {
                self.process.as_mut().map(WorkerProcess::kill);
                return Err(NodeError::new(
                    NodeErrorKind::Unsupported,
                    "the node has config but the worker does not advertise the `configure` feature",
                ));
            }
            let config = self.config.clone();
            if let Err(e) = self.send_configure(&config, deadline, ctx.cancel) {
                self.process.as_mut().map(WorkerProcess::kill);
                return Err(e);
            }
        }
        self.log(format!(
            "ready pid={pid} capability={:?} in {} ms",
            self.capability,
            started.elapsed().as_millis()
        ));
        if let Some(note) = self.network_note.clone() {
            self.log(note);
        }
        Ok(())
    }

    fn execute(&mut self, ctx: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        let Some(input_port) = self.mapping.input.clone() else {
            return self.produce(ctx);
        };
        let frame = match inputs.get(&input_port) {
            Some(Value::Frame(f)) => f.clone(),
            _ => {
                return Err(NodeError::failed(format!(
                    "input `{input_port}` is missing"
                )));
            }
        };
        if let Some(info) = self.process.as_mut().and_then(WorkerProcess::exit) {
            return Err(info.to_error("before execute"));
        }
        let arena = self.arena()?;
        // A payload already in this arena is handed over by handle (zero copy); a heap payload is
        // copied in once (a counted transfer copy).
        let input = match frame.payload.lease() {
            Some(lease) if std::sync::Arc::ptr_eq(lease.arena(), &arena) => {
                arena.note_handoff();
                lease.clone()
            }
            _ => arena
                .publish_copy(
                    &frame.payload.descriptor,
                    frame
                        .payload
                        .try_bytes()
                        .map_err(|e| NodeError::new(NodeErrorKind::Resource, e))?,
                )
                .map_err(resource)?,
        };
        let fingerprint = self
            .verify_inputs
            .then(|| mvf_engine::fnv1a64(input.payload()));
        let out = match self.capability {
            Capability::Classifier => None,
            Capability::Processor | Capability::Analyzer | Capability::Source => {
                Some(arena.reserve().map_err(resource)?)
            }
        };
        let id = self.next_id;
        self.next_id += 1;
        let content_type = match frame.payload.descriptor.media {
            MediaType::Image => "image/raw",
            MediaType::Json => "application/json",
            _ => "application/octet-stream",
        };
        let line = protocol::execute(
            id,
            &frame.meta.source,
            frame.meta.sequence,
            content_type,
            input.offset(),
            out.as_ref().map(|r| (r.offset(), r.capacity())),
        );
        let reply = match self.process.as_mut() {
            Some(p) => p.send(&line).and_then(|_| {
                p.wait_for(ctx.deadline, Some(ctx.cancel), |m| match m {
                    ChildMessage::Result { id: r, .. } => *r == id,
                    ChildMessage::Error { id: r, .. } => *r == Some(id),
                    _ => false,
                })
            }),
            None => {
                return Err(NodeError::new(
                    NodeErrorKind::ProcessExited,
                    "worker is not running",
                ));
            }
        };
        if let Some(before) = fingerprint
            && mvf_engine::fnv1a64(input.payload()) != before
        {
            // Detected, not prevented: workers map the arena read-write (see stage 04).
            return Err(NodeError::new(
                NodeErrorKind::InvalidOutput,
                "worker modified its read-only input payload",
            ));
        }
        drop(input);
        let (frame_out, classification, value) = match reply {
            Ok(ChildMessage::Result {
                frame,
                classification,
                value,
                ..
            }) => (frame, classification, value),
            Ok(ChildMessage::Error { message, .. }) => {
                return Err(NodeError::failed(format!("worker error: {message}")));
            }
            Ok(_) => unreachable!("accept() only admits result/error"),
            Err(e) => return Err(self.fail(e, &format!("execute (cycle {})", ctx.cycle))),
        };

        let mut outputs = Outputs::new();
        if let Some(Some(offset)) = frame_out {
            let (Some(reservation), Some(port)) = (out, self.mapping.frame_out.clone()) else {
                return Err(NodeError::new(
                    NodeErrorKind::Protocol,
                    "worker returned a frame but has no frame output",
                ));
            };
            if offset != reservation.offset() {
                return Err(NodeError::new(
                    NodeErrorKind::Protocol,
                    format!(
                        "worker returned offset {offset} but owns only the output slot at {}",
                        reservation.offset()
                    ),
                ));
            }
            let lease = reservation.seal().map_err(|e| {
                NodeError::new(
                    NodeErrorKind::InvalidOutput,
                    format!("worker output rejected: {e}"),
                )
            })?;
            // The output stays in shared memory; downstream nodes share the lease (zero copy).
            let payload = Payload::arena(lease);
            outputs.insert(
                port,
                Value::Frame(Frame {
                    meta: frame.meta.clone(),
                    payload,
                }),
            );
        }
        if let (Some(c), Some(port)) = (classification, self.mapping.class_out.clone()) {
            let label = c
                .get("label")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    NodeError::new(
                        NodeErrorKind::InvalidOutput,
                        "classification without a label",
                    )
                })?;
            outputs.insert(
                port,
                Value::Classification(Classification {
                    label: label.to_string(),
                    measurement: c.get("measurement").and_then(serde_json::Value::as_f64),
                    unit: c.get("unit").and_then(|u| u.as_str()).map(str::to_string),
                    details: c
                        .get("details")
                        .and_then(|d| d.as_str())
                        .map(str::to_string),
                }),
            );
        }
        if let (Some(v), Some(port)) = (value, self.mapping.json_out.clone()) {
            outputs.insert(port, Value::Json(v));
        }
        Ok(Execution::Produced(outputs))
    }

    /// `checkpoint` → `state`: the module serializes its state into a host-owned slot (as
    /// `[descriptor | bytes]`); the blob keeps the header so `restore` can hand it back verbatim.
    fn export_state(&mut self) -> Result<Option<StateBlob>, NodeError> {
        let arena = self.arena()?;
        let slot = arena.reserve().map_err(resource)?;
        let id = self.next_id;
        self.next_id += 1;
        let line = protocol::checkpoint(id, slot.offset(), slot.capacity());
        let deadline = Some(Instant::now() + Duration::from_secs(10));
        let reply = match self.process.as_mut() {
            Some(p) => p.send(&line).and_then(|_| {
                p.wait_for(deadline, None, |m| match m {
                    ChildMessage::State { id: r, .. } => *r == id,
                    ChildMessage::Error { id: r, .. } => *r == Some(id),
                    _ => false,
                })
            }),
            None => {
                return Err(NodeError::new(
                    NodeErrorKind::ProcessExited,
                    "worker is not running",
                ));
            }
        };
        match reply {
            Ok(ChildMessage::State { empty: true, .. }) => Ok(None),
            Ok(ChildMessage::State { .. }) => {
                let lease = slot.seal().map_err(|e| {
                    NodeError::new(
                        NodeErrorKind::InvalidOutput,
                        format!("worker state rejected: {e}"),
                    )
                })?;
                let mut bytes = vec![0u8; mvf_memory::HEADER_SIZE];
                lease
                    .descriptor()
                    .write_header(&mut bytes)
                    .map_err(|e| NodeError::failed(e.to_string()))?;
                bytes.extend_from_slice(lease.payload());
                Ok(Some(StateBlob {
                    schema_version: self.state_schema,
                    bytes,
                }))
            }
            Ok(ChildMessage::Error { message, .. }) => Err(NodeError::failed(format!(
                "worker checkpoint failed: {message}"
            ))),
            Ok(_) => unreachable!("accept() only admits state/error"),
            Err(e) => Err(self.fail(e, "checkpoint")),
        }
    }

    /// `restore` → `restored`: the blob from [`export_state`] is placed in a slot by copy (it lives
    /// on the host between runs) and the module rehydrates from it.
    fn promote(&mut self) -> Result<(), NodeError> {
        self.launcher.promote()
    }

    fn reconfigure(
        &mut self,
        config: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<bool, NodeError> {
        // Live only when the running worker speaks `configure`; otherwise the engine reopens it.
        if self.process.is_none() || !self.features.iter().any(|f| f == "configure") {
            return Ok(false);
        }
        let cancel = mvf_engine::CancellationToken::new();
        self.send_configure(config, Instant::now() + Duration::from_secs(10), &cancel)?;
        self.config = config.clone();
        Ok(true)
    }

    fn import_state(&mut self, state: &StateBlob) -> Result<(), NodeError> {
        let header = state.bytes.get(..mvf_memory::HEADER_SIZE).ok_or_else(|| {
            NodeError::new(
                NodeErrorKind::InvalidConfig,
                "worker state shorter than its header",
            )
        })?;
        let descriptor = mvf_memory::Descriptor::read_header(header).map_err(|e| {
            NodeError::new(
                NodeErrorKind::InvalidConfig,
                format!("worker state header: {e}"),
            )
        })?;
        let arena = self.arena()?;
        let lease = arena
            .publish_copy(&descriptor, &state.bytes[mvf_memory::HEADER_SIZE..])
            .map_err(resource)?;
        let id = self.next_id;
        self.next_id += 1;
        let line = protocol::restore(id, lease.offset());
        let deadline = Some(Instant::now() + Duration::from_secs(10));
        let reply = match self.process.as_mut() {
            Some(p) => p.send(&line).and_then(|_| {
                p.wait_for(deadline, None, |m| match m {
                    ChildMessage::Restored { id: r } => *r == id,
                    ChildMessage::Error { id: r, .. } => *r == Some(id),
                    _ => false,
                })
            }),
            None => {
                return Err(NodeError::new(
                    NodeErrorKind::ProcessExited,
                    "worker is not running",
                ));
            }
        };
        drop(lease);
        match reply {
            Ok(ChildMessage::Restored { .. }) => Ok(()),
            Ok(ChildMessage::Error { message, .. }) => Err(NodeError::failed(format!(
                "worker restore failed: {message}"
            ))),
            Ok(_) => unreachable!("accept() only admits restored/error"),
            Err(e) => Err(self.fail(e, "restore")),
        }
    }

    fn stop(&mut self, ctx: &LifecycleContext<'_>) -> StopOutcome {
        let Some(mut process) = self.process.take() else {
            return StopOutcome::AlreadyStopped;
        };
        let pid = process.pid();
        let outcome = process.shutdown(ctx.budget.max(Duration::from_millis(1)));
        let exit = process.exit();
        drop(process);
        self.launcher.cleanup();
        if let Some(private) = &self.private_arena {
            let m = private.metrics();
            self.log(format!(
                "private arena: copiesIn={} bytesCopiedIn={} handoffs={}",
                m.copies_in, m.bytes_copied_in, m.zero_copy_handoffs
            ));
        }
        self.log(format!(
            "stopped pid={pid} outcome={outcome:?} exit={exit:?}"
        ));
        outcome
    }
}

impl Drop for WorkerNode {
    fn drop(&mut self) {
        // The process (if any) is killed and reaped by its own Drop first.
        self.process.take();
        self.launcher.cleanup();
        // Scratch and artifact views live as long as the generation (a stop/start re-open keeps
        // them); volume data is never touched.
        if let Some(mut grant) = self.storage.take() {
            for note in grant.release() {
                self.log(note);
            }
        }
    }
}
