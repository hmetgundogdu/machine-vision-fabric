//! # mvf-graph — strict typed pipeline graph
//!
//! A pipeline is a directed graph of **logical nodes** (each described by a Universal Node Contract)
//! connected by **typed** edges. Data edges and control edges are separate lists in the document and
//! separate collections in the validated [`Graph`]; they are never collapsed into one link model.
//!
//! This crate is pure: it parses the pipeline document, resolves each node's contract through a
//! caller-supplied [`ContractResolver`], and validates the graph. It does not execute anything and has
//! no host dependencies. The engine (`mvf-engine`) owns execution.
//!
//! Supported semantics: the data/control graph is a DAG; one edge per input port (no implicit join);
//! fan-out from one output to many inputs; deterministic topological order with ties broken by
//! declaration order. Graph primitives ([`primitive`]) get synthesized contracts; iteration is owned
//! by an optional `loop` primitive, closed by node-level `loop` edges that move no value (#25).

pub mod primitive;
pub mod values;

use std::collections::{BTreeMap, HashMap, HashSet};

pub use primitive::{
    Direction, LoopConfig, LoopMode, ModuleBinding, ModuleBindings, PrimitiveKind, PrimitiveSpec,
    SelectConfig, SelectMode, ValueConfig,
};
pub use values::{ElemType, Shape, ValueType};

use mvf_contract::yaml::{self, YamlError, YamlLimits};
use mvf_contract::{Channel, ContractError, NodeContract, Port, PortType};
use serde::{Deserialize, Serialize};

/// The only accepted pipeline `apiVersion`.
pub const PIPELINE_API_VERSION: &str = "mvf.dev/v1alpha1";
/// The only accepted pipeline `kind`.
pub const PIPELINE_KIND: &str = "Pipeline";
/// Upper bound on nodes per pipeline.
pub const MAX_NODES: usize = 1024;
/// Upper bound on edges per channel.
pub const MAX_EDGES: usize = 4096;

/// A pipeline document as written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PipelineDocument {
    /// Must equal [`PIPELINE_API_VERSION`].
    pub api_version: String,
    /// Must equal [`PIPELINE_KIND`].
    pub kind: String,
    /// Name and description.
    pub metadata: PipelineMetadata,
    /// Nodes, edges and run bounds.
    pub spec: PipelineSpec,
}

/// Pipeline identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PipelineMetadata {
    /// Pipeline name.
    pub name: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Pipeline body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PipelineSpec {
    /// Node instances, in declaration order.
    pub nodes: Vec<NodeRef>,
    /// Data and control edges, kept apart.
    #[serde(default)]
    pub edges: Edges,
    /// Run bounds.
    #[serde(default)]
    pub run: RunSpec,
}

/// One node instance in a pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NodeRef {
    /// Instance id, unique in the pipeline.
    pub id: String,
    /// A host builtin by name (its contract is embedded in the host).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<String>,
    /// A module by contract id, found in the host's module roots (`<root>/<dir>/node.yaml`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// A node contract file, relative to the pipeline file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<String>,
    /// A graph primitive (`fork`, `if`, `switch`, `select`, `value`, `loop`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primitive: Option<PrimitiveKind>,
    /// Live-editable config fields, persisted under a binding key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bindings: Option<ModuleBindings>,
    /// Instances the pipelined executor runs side by side (≤ the contract's `maxParallelism`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<u32>,
    /// Deployment-time network binding (host address, published ports, advertised address).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<mvf_contract::NetworkBinding>,
    /// Instance configuration handed to the implementation (validated by it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Map<String, serde_json::Value>>,
    /// What to do when this node fails (overrides the run default for its role).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_error: Option<FailurePolicySpec>,
    /// What to do when this node cannot get a shared-memory slot (overrides the run default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backpressure: Option<Backpressure>,
}

/// Failure handling modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureMode {
    /// End the run, reporting the node, cycle and cause.
    #[default]
    Fail,
    /// Skip the node's outputs for this cycle and keep running (the .NET mid-graph default).
    Skip,
    /// Dispose the node, bring up a fresh instance (restoring its last checkpoint) and retry the
    /// same inputs; when the attempts run out, fall through to `fail`.
    Restart,
}

/// `onError`: a mode, or a mode with restart bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FailurePolicySpec {
    /// `onError: restart`
    Mode(FailureMode),
    /// `onError: { mode: restart, limit: 3, backoffMs: 100, maxBackoffMs: 2000 }`
    Detailed(FailurePolicy),
}

impl FailurePolicySpec {
    /// The policy with defaults filled in.
    pub fn policy(&self) -> FailurePolicy {
        match self {
            FailurePolicySpec::Mode(mode) => FailurePolicy {
                mode: *mode,
                ..FailurePolicy::default()
            },
            FailurePolicySpec::Detailed(p) => p.clone(),
        }
    }
}

/// A resolved failure policy (same shape and defaults as the .NET `NodeFailurePolicy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FailurePolicy {
    /// Mode.
    pub mode: FailureMode,
    /// Restart attempts before falling through to `fail`; `0` = forever.
    #[serde(default)]
    pub limit: u32,
    /// First backoff, doubled each attempt.
    #[serde(default = "default_backoff")]
    pub backoff_ms: u64,
    /// Upper bound of one backoff.
    #[serde(default = "default_max_backoff")]
    pub max_backoff_ms: u64,
}

fn default_backoff() -> u64 {
    500
}

fn default_max_backoff() -> u64 {
    5_000
}

impl Default for FailurePolicy {
    fn default() -> Self {
        Self {
            mode: FailureMode::Fail,
            limit: 0,
            backoff_ms: default_backoff(),
            max_backoff_ms: default_max_backoff(),
        }
    }
}

impl FailurePolicy {
    /// Whether restart attempt `attempt` (1-based) is allowed.
    pub fn allows(&self, attempt: u32) -> bool {
        self.mode == FailureMode::Restart && (self.limit == 0 || attempt <= self.limit)
    }

    /// Backoff before restart attempt `attempt` (1-based): exponential, capped.
    pub fn backoff(&self, attempt: u32) -> std::time::Duration {
        let shift = attempt.saturating_sub(1).min(30);
        std::time::Duration::from_millis(
            self.backoff_ms
                .saturating_mul(1u64 << shift)
                .min(self.max_backoff_ms),
        )
    }
}

/// Backpressure when a node cannot get a shared-memory slot because the arena is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backpressure {
    /// Lossless: never drop. The serial executor has no concurrent drain to wait for, so a full
    /// arena ends the run with an actionable error (raise the slot count).
    #[default]
    Stall,
    /// Lossy: drop this node's output for the cycle, count it, keep running.
    Drop,
}

/// Run-level failure defaults by role.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FailureDefaults {
    /// Default for source nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<FailurePolicySpec>,
    /// Default for every other node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<FailurePolicySpec>,
}

/// Periodic checkpointing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CheckpointSpec {
    /// Capture every N completed cycles (`0` disables).
    #[serde(default)]
    pub every_cycles: u64,
    /// Directory for durable checkpoints (relative to the pipeline file); absent = memory only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
}

/// Edges, by channel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Edges {
    /// Payload edges between data ports.
    #[serde(default)]
    pub data: Vec<EdgeRef>,
    /// Decision edges between control ports.
    #[serde(default)]
    pub control: Vec<EdgeRef>,
    /// Node-level edges closing the iteration into a `loop` primitive (`from: save, to: cycle`, ids
    /// only). They move no value, so they have no ports and take no part in ordering.
    #[serde(default, rename = "loop")]
    pub loop_edges: Vec<LoopEdgeRef>,
}

/// `from: node` → `to: loop` (node ids, no ports).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LoopEdgeRef {
    /// Closing node id.
    pub from: String,
    /// The `loop` node id.
    pub to: String,
}

/// `from: node.port` → `to: node.port`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EdgeRef {
    /// Source endpoint `node.port`.
    pub from: String,
    /// Target endpoint `node.port`.
    pub to: String,
}

/// Run bounds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RunSpec {
    /// Stop after this many cycles. `None` = until a source is exhausted or the run is cancelled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cycles: Option<u64>,
    /// Failure defaults by role (`{ sources: fail, nodes: skip }` reproduces the .NET defaults).
    #[serde(default)]
    pub on_error: FailureDefaults,
    /// Default backpressure policy.
    #[serde(default)]
    pub backpressure: Backpressure,
    /// Periodic checkpoints.
    #[serde(default)]
    pub checkpoint: CheckpointSpec,
}

/// Where a node's contract came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum ContractOrigin {
    /// Embedded builtin contract.
    Builtin(String),
    /// Contract file path, as resolved by the resolver.
    File(String),
    /// A graph primitive; its contract is synthesized from config and edges.
    Primitive(PrimitiveKind),
}

/// Resolves a node reference to its contract. Implemented by the host (builtins + files).
pub trait ContractResolver {
    /// The embedded contract of a builtin, or `None` if the host has no such builtin.
    fn builtin(&self, name: &str) -> Option<NodeContract>;
    /// Loads and validates a contract file referenced relative to the pipeline.
    fn file(&self, path: &str) -> Result<(String, NodeContract), ContractError>;
    /// Finds an installed module by contract id. Returns the contract file path and contract.
    fn module(&self, id: &str) -> Result<(String, NodeContract), ContractError> {
        Err(ContractError::Invalid(vec![mvf_contract::ContractIssue {
            code: "graph.module.unsupported",
            path: id.to_string(),
            message: "this host resolves no modules".into(),
        }]))
    }
}

/// A node in the validated graph.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    /// Instance id.
    pub id: String,
    /// Declaration index.
    pub index: usize,
    /// Resolved contract.
    pub contract: NodeContract,
    /// Origin of the contract.
    pub origin: ContractOrigin,
    /// Instance config (empty when absent).
    pub config: serde_json::Map<String, serde_json::Value>,
    /// Effective failure policy (node override, else the run default for its role).
    pub on_error: FailurePolicy,
    /// Effective backpressure policy.
    pub backpressure: Backpressure,
    /// The primitive, when the node is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primitive: Option<PrimitiveSpec>,
    /// Live-editable config fields.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: ModuleBindings,
    /// Instances in the pipelined executor (1 = no replication).
    pub parallelism: u32,
    /// Deployment-time network binding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<mvf_contract::NetworkBinding>,
}

impl GraphNode {
    /// Input port by name.
    pub fn input(&self, name: &str) -> Option<&Port> {
        self.contract
            .spec
            .ports
            .inputs
            .iter()
            .find(|p| p.name == name)
    }

    /// Output port by name.
    pub fn output(&self, name: &str) -> Option<&Port> {
        self.contract
            .spec
            .ports
            .outputs
            .iter()
            .find(|p| p.name == name)
    }

    /// A source is a non-primitive node without input ports (a `value` has none either, but it is
    /// a constant, not a stream).
    pub fn is_source(&self) -> bool {
        self.primitive.is_none() && self.contract.spec.ports.inputs.is_empty()
    }

    /// True for the `loop` primitive (portless; owned by the executor, never executed).
    pub fn is_loop(&self) -> bool {
        matches!(self.primitive, Some(PrimitiveSpec::Loop(_)))
    }
}

/// One endpoint of a validated edge.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Endpoint {
    /// Node declaration index.
    pub node: usize,
    /// Port name.
    pub port: String,
}

/// A validated, typed edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    /// Producer endpoint.
    pub from: Endpoint,
    /// Consumer endpoint.
    pub to: Endpoint,
    /// Channel (equal on both ends).
    pub channel: Channel,
    /// Payload type (equal on both ends).
    pub port_type: PortType,
}

/// A validated graph, ready for the engine.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Graph {
    /// Pipeline name.
    pub name: String,
    /// Nodes in declaration order.
    pub nodes: Vec<GraphNode>,
    /// Data edges.
    pub data_edges: Vec<Edge>,
    /// Control edges.
    pub control_edges: Vec<Edge>,
    /// Deterministic topological order (node indices).
    pub order: Vec<usize>,
    /// Run bound.
    pub max_cycles: Option<u64>,
    /// Periodic checkpoints.
    pub checkpoint: CheckpointSpec,
    /// The iteration authority, when the graph declares a `loop`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_spec: Option<LoopSpec>,
    /// Node-level loop edges `(from, loop)` by node index.
    pub loop_edges: Vec<(usize, usize)>,
}

/// The graph's `loop` primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopSpec {
    /// Node index of the loop.
    pub node: usize,
    /// Termination policy.
    pub mode: LoopMode,
    /// Cycles for `count`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

impl Graph {
    /// Node index by id.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    /// All edges (data then control).
    pub fn edges(&self) -> impl Iterator<Item = &Edge> {
        self.data_edges.iter().chain(self.control_edges.iter())
    }

    /// Edges consumed by `node`.
    pub fn incoming(&self, node: usize) -> impl Iterator<Item = &Edge> {
        self.edges().filter(move |e| e.to.node == node)
    }

    /// Number of consumers of `node.port` (fan-out degree).
    pub fn consumers(&self, node: usize, port: &str) -> usize {
        self.edges()
            .filter(|e| e.from.node == node && e.from.port == port)
            .count()
    }
}

/// One graph validation problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphIssue {
    /// Stable code, e.g. `graph.edge.type-mismatch`.
    pub code: &'static str,
    /// Location, e.g. `spec.edges.data[2]` or `spec.nodes[0]`.
    pub path: String,
    /// Explanation.
    pub message: String,
}

impl std::fmt::Display for GraphIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}: {}", self.code, self.path, self.message)
    }
}

/// Building a graph failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    /// The pipeline YAML was malformed, unsafe or off-schema.
    #[error(transparent)]
    Yaml(#[from] YamlError),
    /// The graph is invalid; every issue is listed.
    #[error("invalid pipeline: {}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Invalid(Vec<GraphIssue>),
}

impl GraphError {
    /// Every stable code in this error.
    pub fn codes(&self) -> Vec<&'static str> {
        match self {
            GraphError::Yaml(e) => vec![e.code],
            GraphError::Invalid(issues) => issues.iter().map(|i| i.code).collect(),
        }
    }
}

/// Parses a pipeline document with the strict YAML loader.
pub fn parse_document(text: &str) -> Result<PipelineDocument, GraphError> {
    Ok(yaml::from_str(text, &YamlLimits::default())?)
}

/// Parses, resolves and validates a pipeline.
pub fn build_graph(text: &str, resolver: &dyn ContractResolver) -> Result<Graph, GraphError> {
    let doc = parse_document(text)?;
    build_from_document(&doc, resolver)
}

/// Resolves and validates a parsed pipeline document, reporting every issue at once.
pub fn build_from_document(
    doc: &PipelineDocument,
    resolver: &dyn ContractResolver,
) -> Result<Graph, GraphError> {
    let mut issues = Vec::new();
    let mut issue = |code: &'static str, path: String, message: String| {
        issues.push(GraphIssue {
            code,
            path,
            message,
        })
    };

    if doc.api_version != PIPELINE_API_VERSION {
        issue(
            "graph.api-version.unsupported",
            "apiVersion".into(),
            format!(
                "unsupported apiVersion `{}`; expected `{PIPELINE_API_VERSION}`",
                doc.api_version
            ),
        );
    }
    if doc.kind != PIPELINE_KIND {
        issue(
            "graph.kind.unsupported",
            "kind".into(),
            format!(
                "unsupported kind `{}`; expected `{PIPELINE_KIND}`",
                doc.kind
            ),
        );
    }
    let spec = &doc.spec;
    if spec.nodes.is_empty() {
        issue(
            "graph.node.none",
            "spec.nodes".into(),
            "a pipeline needs at least one node".into(),
        );
    }
    if spec.nodes.len() > MAX_NODES {
        issue(
            "graph.node.too-many",
            "spec.nodes".into(),
            format!("at most {MAX_NODES} nodes"),
        );
    }
    if spec.edges.data.len() > MAX_EDGES || spec.edges.control.len() > MAX_EDGES {
        issue(
            "graph.edge.too-many",
            "spec.edges".into(),
            format!("at most {MAX_EDGES} edges per channel"),
        );
    }
    if spec.run.max_cycles == Some(0) {
        issue(
            "graph.run.zero-cycles",
            "spec.run.maxCycles".into(),
            "maxCycles must be positive when given".into(),
        );
    }

    // Output ports of `fork`/`switch` are the ones their outgoing data edges name (declaration order).
    let mut edge_outputs: HashMap<&str, Vec<String>> = HashMap::new();
    for edge in &spec.edges.data {
        if let Some((node, port)) = split_endpoint(&edge.from) {
            let ports = edge_outputs.entry(node).or_default();
            if !ports.iter().any(|p| p == port) {
                ports.push(port.to_string());
            }
        }
    }

    // ---- nodes ----
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut ids: HashMap<&str, usize> = HashMap::new();
    for (i, node) in spec.nodes.iter().enumerate() {
        let path = format!("spec.nodes[{i}]");
        if !mvf_contract::validate::is_valid_id(&node.id) {
            issue(
                "graph.node.invalid-id",
                format!("{path}.id"),
                format!("`{}` is not a valid node id", node.id),
            );
        }
        if ids.insert(node.id.as_str(), i).is_some() {
            issue(
                "graph.node.duplicate-id",
                format!("{path}.id"),
                format!("node id `{}` is declared twice", node.id),
            );
            continue;
        }
        let mut primitive = None;
        let resolved = match (&node.builtin, &node.contract, &node.module) {
            _ if node.primitive.is_some()
                && (node.builtin.is_some() || node.contract.is_some() || node.module.is_some()) =>
            {
                issue(
                    "graph.node.ambiguous-implementation",
                    path.clone(),
                    "a node must reference exactly one of `builtin`, `contract`, `module` or `primitive`"
                        .into(),
                );
                None
            }
            (None, None, None) if node.primitive.is_some() => {
                let kind = node.primitive.unwrap_or(PrimitiveKind::Fork);
                let config = node.config.clone().unwrap_or_default();
                match PrimitiveSpec::parse(kind, &config) {
                    Err(e) => {
                        issue(
                            "graph.primitive.invalid-config",
                            format!("{path}.config"),
                            format!("`{}` config: {e}", kind.as_str()),
                        );
                        None
                    }
                    Ok(parsed) => {
                        let outputs = if parsed.outputs_from_edges() {
                            edge_outputs
                                .get(node.id.as_str())
                                .cloned()
                                .unwrap_or_default()
                        } else {
                            Vec::new()
                        };
                        let mut ok = true;
                        for port in &outputs {
                            if !mvf_contract::validate::is_valid_port_name(port) {
                                issue(
                                    "graph.primitive.invalid-port",
                                    path.clone(),
                                    format!(
                                        "`{}` output `{port}` is not a valid port name",
                                        node.id
                                    ),
                                );
                                ok = false;
                            }
                        }
                        validate_primitive(&parsed, &path, &mut issue);
                        let contract = if ok {
                            parsed.contract(&outputs)
                        } else {
                            Err(String::new())
                        };
                        match contract {
                            Ok(c) => {
                                primitive = Some(parsed);
                                Some((ContractOrigin::Primitive(kind), c))
                            }
                            Err(e) => {
                                if !e.is_empty() {
                                    issue("graph.primitive.invalid-config", path.clone(), e);
                                }
                                None
                            }
                        }
                    }
                }
            }
            (None, None, Some(id)) => match resolver.module(id) {
                Ok((resolved_path, c)) => Some((ContractOrigin::File(resolved_path), c)),
                Err(e) => {
                    issue(
                        "graph.node.unknown-module",
                        format!("{path}.module"),
                        format!("module `{id}` could not be resolved: {e}"),
                    );
                    None
                }
            },
            (Some(name), None, None) => match resolver.builtin(name) {
                Some(c) => Some((ContractOrigin::Builtin(name.clone()), c)),
                None => {
                    issue(
                        "graph.node.unknown-builtin",
                        format!("{path}.builtin"),
                        format!("this host has no builtin `{name}`"),
                    );
                    None
                }
            },
            (None, Some(file), None) => match resolver.file(file) {
                Ok((resolved_path, c)) => Some((ContractOrigin::File(resolved_path), c)),
                Err(e) => {
                    issue(
                        "graph.node.invalid-contract",
                        format!("{path}.contract"),
                        format!("contract `{file}` could not be loaded: {e}"),
                    );
                    None
                }
            },
            _ => {
                issue(
                    "graph.node.ambiguous-implementation",
                    path.clone(),
                    "a node must reference exactly one of `builtin`, `contract`, `module` or `primitive`"
                        .into(),
                );
                None
            }
        };
        let bindings = node.bindings.clone().unwrap_or_default();
        for (field, binding) in &bindings {
            let at = format!("{path}.bindings.{field}");
            if binding.binding.trim().is_empty() {
                issue(
                    "graph.binding.invalid-key",
                    at.clone(),
                    "a module binding needs a non-empty `binding` key".into(),
                );
            }
            if let Some(schema) = &binding.schema
                && let Err(e) = values::validate_schema(schema)
            {
                issue("graph.binding.invalid-schema", at.clone(), e);
            } else if let Some(current) = node.config.as_ref().and_then(|c| c.get(field))
                && let Err(e) = binding.value_type().check(binding.schema.as_ref(), current)
            {
                issue(
                    "graph.binding.config-type-mismatch",
                    at,
                    format!("config `{field}` does not match its binding: {e}"),
                );
            }
        }
        if node.primitive.is_some() && !bindings.is_empty() {
            issue(
                "graph.binding.on-primitive",
                format!("{path}.bindings"),
                "primitives have no module config to bind; tune a `value` node instead".into(),
            );
        }
        if let Some((_, contract)) = &resolved
            && let Some(p) = node.parallelism
            && (p == 0 || p > contract.spec.execution.max_parallelism)
        {
            issue(
                "graph.node.invalid-parallelism",
                format!("{path}.parallelism"),
                format!(
                    "`{}` asks for parallelism {p} but its contract allows 1..={}; a node opts into replication with `spec.execution.maxParallelism`, which asserts it keeps no state across frames",
                    node.id, contract.spec.execution.max_parallelism
                ),
            );
        }
        if let (Some((_, contract)), Some(binding)) = (&resolved, &node.network) {
            validate_binding(contract, binding, &format!("{path}.network"), &mut issue);
        }
        if let Some((origin, contract)) = resolved {
            let role_default = if primitive.is_none() && contract.spec.ports.inputs.is_empty() {
                &spec.run.on_error.sources
            } else {
                &spec.run.on_error.nodes
            };
            let on_error = node
                .on_error
                .as_ref()
                .or(role_default.as_ref())
                .map(FailurePolicySpec::policy)
                .unwrap_or_default();
            nodes.push(GraphNode {
                id: node.id.clone(),
                index: i,
                contract,
                origin,
                config: node.config.clone().unwrap_or_default(),
                on_error,
                backpressure: node.backpressure.unwrap_or(spec.run.backpressure),
                primitive,
                bindings,
                parallelism: node.parallelism.unwrap_or(1),
                network: node.network.clone(),
            });
        }
    }

    // Edge validation only makes sense when every node resolved; otherwise endpoint errors would
    // duplicate the node errors above.
    let all_resolved = nodes.len() == spec.nodes.len();
    let by_id: HashMap<&str, &GraphNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let mut data_edges = Vec::new();
    let mut control_edges = Vec::new();
    let mut seen_edges = HashSet::new();
    let mut fed_inputs: HashMap<(usize, String), String> = HashMap::new();

    if all_resolved {
        for (channel, list, out) in [
            (Channel::Data, &spec.edges.data, &mut data_edges),
            (Channel::Control, &spec.edges.control, &mut control_edges),
        ] {
            let channel_name = match channel {
                Channel::Data => "data",
                Channel::Control => "control",
            };
            for (i, edge) in list.iter().enumerate() {
                let path = format!("spec.edges.{channel_name}[{i}]");
                let (Some(from), Some(to)) = (split_endpoint(&edge.from), split_endpoint(&edge.to))
                else {
                    issue(
                        "graph.edge.invalid-endpoint",
                        path,
                        format!(
                            "endpoints must be `node.port`; got `{}` -> `{}`",
                            edge.from, edge.to
                        ),
                    );
                    continue;
                };
                let src = by_id.get(from.0);
                let dst = by_id.get(to.0);
                if src.is_none() {
                    issue(
                        "graph.edge.missing-source-node",
                        format!("{path}.from"),
                        format!("no node `{}`", from.0),
                    );
                }
                if dst.is_none() {
                    issue(
                        "graph.edge.missing-target-node",
                        format!("{path}.to"),
                        format!("no node `{}`", to.0),
                    );
                }
                let (Some(src), Some(dst)) = (src, dst) else {
                    continue;
                };
                let out_port = src.output(from.1);
                let in_port = dst.input(to.1);
                if out_port.is_none() {
                    issue(
                        "graph.edge.missing-source-port",
                        format!("{path}.from"),
                        format!("node `{}` has no output port `{}`", src.id, from.1),
                    );
                }
                if in_port.is_none() {
                    issue(
                        "graph.edge.missing-target-port",
                        format!("{path}.to"),
                        format!("node `{}` has no input port `{}`", dst.id, to.1),
                    );
                }
                let (Some(out_port), Some(in_port)) = (out_port, in_port) else {
                    continue;
                };
                let mut ok = true;
                if out_port.channel != channel || in_port.channel != channel {
                    issue(
                        "graph.edge.channel-mismatch",
                        path.clone(),
                        format!(
                            "a {channel_name} edge must connect {channel_name} ports; `{}` is {:?} and `{}` is {:?}",
                            edge.from, out_port.channel, edge.to, in_port.channel
                        )
                        .replace("Data", "data")
                        .replace("Control", "control"),
                    );
                    ok = false;
                }
                if out_port.port_type != in_port.port_type {
                    issue(
                        "graph.edge.type-mismatch",
                        path.clone(),
                        format!(
                            "`{}` produces `{}` but `{}` accepts `{}`",
                            edge.from,
                            out_port.port_type.as_str(),
                            edge.to,
                            in_port.port_type.as_str()
                        ),
                    );
                    ok = false;
                }
                // Between two typed-value primitives the refined type must agree as well
                // (`select.criterion` carries the element type of `select.items`).
                if ok {
                    let produced = src
                        .primitive
                        .as_ref()
                        .and_then(|p| p.refined(Direction::Out, from.1));
                    let accepted = dst
                        .primitive
                        .as_ref()
                        .and_then(|p| p.refined(Direction::In, to.1));
                    if let (Some(produced), Some(accepted)) = (produced, accepted)
                        && produced != accepted
                    {
                        issue(
                            "graph.edge.value-type-mismatch",
                            path.clone(),
                            format!(
                                "`{}` produces `{}` but `{}` accepts `{}`",
                                edge.from,
                                produced.describe(),
                                edge.to,
                                accepted.describe()
                            ),
                        );
                        ok = false;
                    }
                }
                if !seen_edges.insert((edge.from.clone(), edge.to.clone())) {
                    issue(
                        "graph.edge.duplicate",
                        path.clone(),
                        format!("edge `{}` -> `{}` is declared twice", edge.from, edge.to),
                    );
                    ok = false;
                } else if let Some(previous) =
                    fed_inputs.insert((dst.index, to.1.to_string()), edge.from.clone())
                {
                    issue(
                        "graph.input.multiple-edges",
                        format!("{path}.to"),
                        format!(
                            "input `{}` is already fed by `{previous}`; implicit joins are not supported",
                            edge.to
                        ),
                    );
                    ok = false;
                }
                if ok {
                    out.push(Edge {
                        from: Endpoint {
                            node: src.index,
                            port: from.1.to_string(),
                        },
                        to: Endpoint {
                            node: dst.index,
                            port: to.1.to_string(),
                        },
                        channel,
                        port_type: out_port.port_type,
                    });
                }
            }
        }

        for node in &nodes {
            for port in &node.contract.spec.ports.inputs {
                if port.required && !fed_inputs.contains_key(&(node.index, port.name.clone())) {
                    issue(
                        "graph.input.unconnected-required",
                        format!("spec.nodes[{}]", node.index),
                        format!(
                            "required input `{}.{}` has no incoming edge",
                            node.id, port.name
                        ),
                    );
                }
            }
        }
        if !nodes.is_empty() && !nodes.iter().any(GraphNode::is_source) {
            issue(
                "graph.node.no-source",
                "spec.nodes".into(),
                "a pipeline needs at least one source node (a node without inputs)".into(),
            );
        }
    }

    // ---- loop ----
    let loops: Vec<&GraphNode> = nodes.iter().filter(|n| n.is_loop()).collect();
    if loops.len() > 1 {
        issue(
            "graph.loop.multiple",
            "spec.nodes".into(),
            format!(
                "a graph has at most one `loop` (the iteration authority); found {}",
                loops.len()
            ),
        );
    }
    let loop_spec = loops.first().and_then(|n| match &n.primitive {
        Some(PrimitiveSpec::Loop(c)) => Some(LoopSpec {
            node: n.index,
            mode: c.mode,
            count: c.count,
        }),
        _ => None,
    });
    let mut loop_edges = Vec::new();
    if all_resolved {
        for (i, edge) in spec.edges.loop_edges.iter().enumerate() {
            let path = format!("spec.edges.loop[{i}]");
            let src = by_id.get(edge.from.as_str());
            let dst = by_id.get(edge.to.as_str());
            if src.is_none() {
                issue(
                    "graph.edge.missing-source-node",
                    format!("{path}.from"),
                    format!("no node `{}` (loop edges name nodes, not ports)", edge.from),
                );
            }
            match dst {
                None => issue(
                    "graph.edge.missing-target-node",
                    format!("{path}.to"),
                    format!("no node `{}`", edge.to),
                ),
                Some(d) if !d.is_loop() => issue(
                    "graph.edge.invalid-loop-target",
                    format!("{path}.to"),
                    format!("`{}` is not a `loop` primitive", edge.to),
                ),
                Some(d) => {
                    if let Some(s) = src {
                        if s.is_loop() {
                            issue(
                                "graph.edge.invalid-loop-source",
                                format!("{path}.from"),
                                "a loop edge closes a node into the loop, not the loop into itself"
                                    .into(),
                            );
                        } else {
                            loop_edges.push((s.index, d.index));
                        }
                    }
                }
            }
        }
    }

    let edges_for_order: Vec<(usize, usize)> = data_edges
        .iter()
        .chain(control_edges.iter())
        .map(|e| (e.from.node, e.to.node))
        .collect();
    let order = match topological_order(nodes.len(), &edges_for_order) {
        Ok(order) => order,
        Err(cycle) => {
            let names: Vec<_> = cycle.iter().map(|i| nodes[*i].id.as_str()).collect();
            issue(
                "graph.cycle",
                "spec.edges".into(),
                format!(
                    "illegal cycle {}; data and control edges must form a DAG (close iteration with a `loop` primitive)",
                    names.join(" -> ")
                ),
            );
            Vec::new()
        }
    };

    if !issues.is_empty() {
        return Err(GraphError::Invalid(issues));
    }
    Ok(Graph {
        name: doc.metadata.name.clone(),
        nodes,
        data_edges,
        control_edges,
        order,
        max_cycles: spec.run.max_cycles,
        checkpoint: spec.run.checkpoint.clone(),
        loop_spec,
        loop_edges,
    })
}

/// A deployment binding may only publish ports the contract declares as inbound listeners.
fn validate_binding(
    contract: &NodeContract,
    binding: &mvf_contract::NetworkBinding,
    path: &str,
    issue: &mut impl FnMut(&'static str, String, String),
) {
    for (field, value) in [
        ("hostIp", &binding.host_ip),
        ("advertise", &binding.advertise),
    ] {
        if let Some(v) = value
            && v.parse::<std::net::IpAddr>().is_err()
        {
            issue(
                "graph.network.invalid-address",
                format!("{path}.{field}"),
                format!("`{v}` is not an IP address"),
            );
        }
    }
    let inbound = contract.spec.permissions.network.inbound();
    for (i, b) in binding.ports.iter().enumerate() {
        let declared = inbound.iter().any(|r| {
            r.protocol == b.protocol
                && (r.port == Some(b.port)
                    || r.port_range
                        .as_deref()
                        .and_then(|t| mvf_contract::parse_port_range(t).ok())
                        .is_some_and(|(lo, hi)| (lo..=hi).contains(&b.port)))
        });
        if !declared {
            issue(
                "graph.network.unbound-port",
                format!("{path}.ports[{i}]"),
                format!(
                    "{} port {} is not an inbound listener of the node's contract",
                    b.protocol.as_str(),
                    b.port
                ),
            );
        }
        if b.host_port == 0 {
            issue(
                "graph.network.invalid-port",
                format!("{path}.ports[{i}].hostPort"),
                "host port 0 is not a port".into(),
            );
        }
    }
    if !binding.ports.is_empty() && inbound.is_empty() {
        issue(
            "graph.network.unbound-port",
            format!("{path}.ports"),
            "the node's contract declares no inbound listener".into(),
        );
    }
}

/// Configuration rules of `value`, `select` and `loop` (the .NET `pipeline.node.*` value codes).
fn validate_primitive(
    spec: &PrimitiveSpec,
    path: &str,
    issue: &mut impl FnMut(&'static str, String, String),
) {
    match spec {
        PrimitiveSpec::Value(c) => {
            let schema_ok = match &c.schema {
                Some(schema) => match values::validate_schema(schema) {
                    Ok(()) => true,
                    Err(e) => {
                        issue(
                            "graph.value.invalid-schema",
                            format!("{path}.config.schema"),
                            e,
                        );
                        false
                    }
                },
                None => true,
            };
            if schema_ok {
                for (field, code, value) in [
                    ("literal", "graph.value.literal-type-mismatch", &c.literal),
                    ("default", "graph.value.default-type-mismatch", &c.default),
                ] {
                    if let Some(v) = value
                        && let Err(e) = c.value_type().check(c.schema.as_ref(), v)
                    {
                        issue(
                            code,
                            format!("{path}.config.{field}"),
                            format!("{field} is not a valid {}: {e}", c.value_type().describe()),
                        );
                    }
                }
            }
            if c.literal.is_none() && c.binding.is_none() && c.default.is_none() {
                issue(
                    "graph.value.unresolvable",
                    format!("{path}.config"),
                    "a value needs a `literal`, a `binding` or a `default`; with none there is nowhere for it to come from"
                        .into(),
                );
            }
            if c.binding.as_deref().is_some_and(|b| b.trim().is_empty()) {
                issue(
                    "graph.binding.invalid-key",
                    format!("{path}.config.binding"),
                    "`binding` must not be empty".into(),
                );
            }
        }
        PrimitiveSpec::Select(c) => {
            if let Some(criterion) = &c.criterion
                && c.by.is_none()
                && let Err(e) = ValueType::one(c.elem).check(None, criterion)
                && !criterion.is_object()
            {
                issue(
                    "graph.select.type-mismatch",
                    format!("{path}.config.where"),
                    format!(
                        "`where` is not a {}: {e}",
                        ValueType::one(c.elem).describe()
                    ),
                );
            }
            if c.by.as_deref().is_some_and(|b| b.trim().is_empty()) {
                issue(
                    "graph.select.invalid-by",
                    format!("{path}.config.by"),
                    "`by` must name a property".into(),
                );
            }
        }
        PrimitiveSpec::Loop(c) => match (c.mode, c.count) {
            (LoopMode::Count, None | Some(0)) => issue(
                "graph.loop.invalid-count",
                format!("{path}.config.count"),
                "mode `count` needs a positive `count`".into(),
            ),
            (LoopMode::UntilExhausted | LoopMode::Forever, Some(_)) => issue(
                "graph.loop.invalid-count",
                format!("{path}.config.count"),
                "`count` applies only to mode `count`".into(),
            ),
            _ => {}
        },
        PrimitiveSpec::Fork | PrimitiveSpec::If | PrimitiveSpec::Switch => {}
    }
}

/// Splits `node.port`. Node ids may contain dots, so the port is the last segment.
fn split_endpoint(text: &str) -> Option<(&str, &str)> {
    let (node, port) = text.rsplit_once('.')?;
    (!node.is_empty() && !port.is_empty()).then_some((node, port))
}

/// Kahn's algorithm with ties broken by declaration index (deterministic). Returns a cycle (node
/// indices, first repeated at the end) when the graph is not a DAG.
pub fn topological_order(count: usize, edges: &[(usize, usize)]) -> Result<Vec<usize>, Vec<usize>> {
    let mut indegree = vec![0usize; count];
    let mut next: Vec<Vec<usize>> = vec![Vec::new(); count];
    for &(a, b) in edges {
        indegree[b] += 1;
        next[a].push(b);
    }
    let mut ready: BTreeMap<usize, ()> = (0..count)
        .filter(|i| indegree[*i] == 0)
        .map(|i| (i, ()))
        .collect();
    let mut order = Vec::with_capacity(count);
    while let Some((&n, _)) = ready.iter().next() {
        ready.remove(&n);
        order.push(n);
        for &m in &next[n] {
            indegree[m] -= 1;
            if indegree[m] == 0 {
                ready.insert(m, ());
            }
        }
    }
    if order.len() == count {
        return Ok(order);
    }
    // Find a concrete cycle among the remaining nodes for the error message.
    let remaining: HashSet<usize> = (0..count).filter(|i| indegree[*i] > 0).collect();
    let start = *remaining.iter().min().unwrap_or(&0);
    let mut path = vec![start];
    let mut visited = HashSet::from([start]);
    let mut current = start;
    loop {
        let Some(&step) = next[current].iter().find(|m| remaining.contains(m)) else {
            return Err(path);
        };
        if visited.contains(&step) {
            let pos = path.iter().position(|p| *p == step).unwrap_or(0);
            let mut cycle = path[pos..].to_vec();
            cycle.push(step);
            return Err(cycle);
        }
        visited.insert(step);
        path.push(step);
        current = step;
    }
}
