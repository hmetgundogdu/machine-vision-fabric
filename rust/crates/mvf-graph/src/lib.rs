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
//! Supported semantics (Gate A): DAG only (no loop primitive); one edge per input port (no implicit
//! join); fan-out from one output to many inputs; deterministic topological order with ties broken by
//! declaration order.

use std::collections::{BTreeMap, HashMap, HashSet};

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
    /// Instance configuration handed to the implementation (validated by it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Map<String, serde_json::Value>>,
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
}

/// Where a node's contract came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum ContractOrigin {
    /// Embedded builtin contract.
    Builtin(String),
    /// Contract file path, as resolved by the resolver.
    File(String),
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

    /// A source has no input ports.
    pub fn is_source(&self) -> bool {
        self.contract.spec.ports.inputs.is_empty()
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
        let resolved = match (&node.builtin, &node.contract, &node.module) {
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
                    "a node must reference exactly one of `builtin`, `contract` or `module`".into(),
                );
                None
            }
        };
        if let Some((origin, contract)) = resolved {
            nodes.push(GraphNode {
                id: node.id.clone(),
                index: i,
                contract,
                origin,
                config: node.config.clone().unwrap_or_default(),
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
                    "illegal cycle {}; loops are not supported in this executor",
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
    })
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
