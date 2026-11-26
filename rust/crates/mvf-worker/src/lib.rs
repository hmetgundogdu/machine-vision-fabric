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

pub mod process;
pub mod protocol;

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
    StopOutcome, Value,
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
        }
    }

    /// Fingerprints every input before and after `execute` and fails the node if a worker wrote to
    /// its read-only input (detection only; also enabled by `MVF_VERIFY_INPUTS=1`).
    pub fn with_input_verification(mut self, on: bool) -> Self {
        self.verify_inputs = on;
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
            replacement: vec![Replacement::Stateless],
            grants: Default::default(),
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
        let contract_dir = Path::new(contract_path)
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
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
        worker_node(
            request,
            Box::new(ProcessLauncher {
                spec: LaunchSpec {
                    program,
                    args,
                    cwd: contract_dir,
                    env,
                },
            }),
            self.verify_inputs,
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

/// Builds a supervised protocol-v1 worker handle around any launcher.
pub fn worker_node(
    request: &InstantiateRequest<'_>,
    launcher: Box<dyn Launcher>,
    verify_inputs: bool,
) -> Result<Box<dyn NodeHandle>, NodeError> {
    Ok(Box::new(WorkerNode {
        node_id: request.node_id.to_string(),
        generation: request.generation,
        launcher,
        config: request.config.clone(),
        services: request.services.clone(),
        mapping: PortMapping::from_contract(request.contract)?,
        process: None,
        capability: Capability::Classifier,
        next_id: 1,
        verify_inputs,
    }))
}

fn unsupported(message: impl Into<String>) -> NodeError {
    NodeError::new(NodeErrorKind::Unsupported, message)
}

/// How contract ports map onto protocol v1 messages (one frame in; frame/classification/json out).
#[derive(Debug, Clone)]
struct PortMapping {
    input: String,
    frame_out: Option<String>,
    class_out: Option<String>,
    json_out: Option<String>,
}

impl PortMapping {
    fn from_contract(contract: &mvf_contract::NodeContract) -> Result<Self, NodeError> {
        let ports = &contract.spec.ports;
        let [input] = ports.inputs.as_slice() else {
            return Err(unsupported(
                "protocol v1 workers take exactly one input port (a `frame` data port)",
            ));
        };
        if input.channel != Channel::Data || input.port_type != PortType::Frame {
            return Err(unsupported(
                "protocol v1 workers take exactly one input port (a `frame` data port)",
            ));
        }
        let mut mapping = Self {
            input: input.name.clone(),
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
        let ok = match capability {
            Capability::Processor => outputs == (true, false, false),
            Capability::Classifier => outputs == (false, true, false),
            Capability::Analyzer => true,
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
}

struct WorkerNode {
    node_id: String,
    generation: u64,
    launcher: Box<dyn Launcher>,
    config: serde_json::Map<String, serde_json::Value>,
    services: HostServices,
    mapping: PortMapping,
    process: Option<WorkerProcess>,
    capability: Capability,
    next_id: u64,
    verify_inputs: bool,
}

impl WorkerNode {
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

    fn arena(&self) -> Result<std::sync::Arc<mvf_memory::Arena>, NodeError> {
        self.services
            .arena()
            .map_err(|e| NodeError::new(NodeErrorKind::Resource, e.to_string()))
    }
}

fn resource(e: ArenaError) -> NodeError {
    NodeError::new(NodeErrorKind::Resource, e.to_string())
}

impl NodeHandle for WorkerNode {
    fn start(&mut self, ctx: &LifecycleContext<'_>) -> Result<(), NodeError> {
        let started = Instant::now();
        let deadline = started + ctx.budget;
        let arena = self.arena()?;
        let spec = self.launcher.prepare(arena.path())?;
        let process =
            WorkerProcess::spawn(&spec, &self.node_id, self.generation, &self.services.logs)
                .map_err(|e| {
                    NodeError::new(
                        NodeErrorKind::StartFailed,
                        format!("could not spawn {:?}: {e}", spec.program),
                    )
                })?;
        let pid = process.pid();
        self.process = Some(process);
        self.log(format!("spawned pid={pid}"));

        let hello = self
            .process
            .as_mut()
            .map(|p| {
                p.wait_for(Some(deadline), Some(ctx.cancel), |m| {
                    matches!(m, ChildMessage::Hello { .. })
                })
            })
            .unwrap_or(Err(WaitError::Cancelled));
        let (protocol, capability, ready, features) = match hello {
            Ok(ChildMessage::Hello {
                protocol,
                capability,
                ready,
                features,
                ..
            }) => (protocol, capability, ready, features),
            Ok(_) => unreachable!("accept() only admits hello"),
            Err(e) => return Err(self.fail(e, "readiness (waiting for hello)")),
        };
        if protocol != protocol::PROTOCOL_VERSION {
            self.process.as_mut().map(WorkerProcess::kill);
            return Err(NodeError::new(
                NodeErrorKind::Protocol,
                format!("worker speaks protocol {protocol}; this host speaks 1"),
            ));
        }
        self.capability = match capability.as_str() {
            "classifier" => Capability::Classifier,
            "processor" => Capability::Processor,
            "analyzer" => Capability::Analyzer,
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
        if !ready {
            let r = self
                .process
                .as_mut()
                .map(|p| {
                    p.wait_for(Some(deadline), Some(ctx.cancel), |m| {
                        matches!(m, ChildMessage::Ready)
                    })
                })
                .unwrap_or(Err(WaitError::Cancelled));
            if let Err(e) = r {
                return Err(self.fail(e, "readiness (waiting for ready)"));
            }
        }
        if !self.config.is_empty() {
            if !features.iter().any(|f| f == "configure") {
                self.process.as_mut().map(WorkerProcess::kill);
                return Err(NodeError::new(
                    NodeErrorKind::Unsupported,
                    "the node has config but the worker does not advertise the `configure` feature",
                ));
            }
            let id = self.next_id;
            self.next_id += 1;
            let line = serde_json::json!({"type": "configure", "id": id, "config": self.config})
                .to_string();
            let reply = match self.process.as_mut() {
                Some(p) => p.send(&line).and_then(|_| {
                    p.wait_for(Some(deadline), Some(ctx.cancel), |m| match m {
                        ChildMessage::Other(t) => t == "configured",
                        ChildMessage::Error { id: e, .. } => *e == Some(id),
                        _ => false,
                    })
                }),
                None => Err(WaitError::Cancelled),
            };
            match reply {
                Ok(ChildMessage::Error { message, .. }) => {
                    self.process.as_mut().map(WorkerProcess::kill);
                    return Err(NodeError::new(
                        NodeErrorKind::InvalidConfig,
                        format!("worker rejected config: {message}"),
                    ));
                }
                Ok(_) => {}
                Err(e) => return Err(self.fail(e, "configure")),
            }
        }
        self.log(format!(
            "ready pid={pid} capability={:?} in {} ms",
            self.capability,
            started.elapsed().as_millis()
        ));
        Ok(())
    }

    fn execute(&mut self, ctx: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        let frame = match inputs.get(&self.mapping.input) {
            Some(Value::Frame(f)) => f.clone(),
            _ => {
                return Err(NodeError::failed(format!(
                    "input `{}` is missing",
                    self.mapping.input
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
            Capability::Processor | Capability::Analyzer => {
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

    fn stop(&mut self, ctx: &LifecycleContext<'_>) -> StopOutcome {
        let Some(mut process) = self.process.take() else {
            return StopOutcome::AlreadyStopped;
        };
        let pid = process.pid();
        let outcome = process.shutdown(ctx.budget.max(Duration::from_millis(1)));
        let exit = process.exit();
        drop(process);
        self.launcher.cleanup();
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
    }
}
