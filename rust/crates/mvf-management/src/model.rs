//! The management API's data model (API version 1): the topology, node views, and the latency
//! histogram behind them. Everything a client sees is JSON (see `protocol/management-v1.md`).

use mvf_graph::Graph;
use serde::Serialize;
use serde_json::{Value, json};

/// The API version this runtime speaks.
pub const API: u64 = 1;

/// A port as a client sees it: name, payload type and channel.
#[derive(Debug, Clone, Serialize)]
pub struct PortView {
    /// Port name.
    pub name: String,
    /// Payload type.
    #[serde(rename = "type")]
    pub port_type: String,
    /// `data` or `control`.
    pub channel: String,
}

/// A node in the topology. Config values are deliberately absent: they may hold credentials, and
/// management is not a configuration dump.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeTopology {
    /// Instance id.
    pub id: String,
    /// Contract `id@version`.
    pub contract: String,
    /// The primitive (`value`, `select`, `loop`, …), when the node is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primitive: Option<String>,
    /// Backend that runs it.
    pub backend: String,
    /// Instances in the pipelined executor.
    pub parallelism: u32,
    /// Input ports.
    pub inputs: Vec<PortView>,
    /// Output ports.
    pub outputs: Vec<PortView>,
    /// Config fields that can be edited live (names only).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub live_fields: Vec<String>,
}

/// The graph as a client sees it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Topology {
    /// Pipeline name.
    pub pipeline: String,
    /// Nodes in declaration order.
    pub nodes: Vec<NodeTopology>,
    /// Edges: `{"data": [...], "control": [...]}`, each `{from: "node.port", to, type}`.
    pub edges: Value,
}

impl Topology {
    /// The topology of `graph`, with each node's backend from `backends` (node id → backend).
    pub fn of(graph: &Graph, backends: &[(String, String)]) -> Self {
        let port = |p: &mvf_contract::Port| PortView {
            name: p.name.clone(),
            port_type: p.port_type.as_str().to_string(),
            channel: match p.channel {
                mvf_contract::Channel::Data => "data".into(),
                mvf_contract::Channel::Control => "control".into(),
            },
        };
        let nodes = graph
            .nodes
            .iter()
            .map(|n| NodeTopology {
                id: n.id.clone(),
                contract: format!("{}@{}", n.contract.metadata.id, n.contract.metadata.version),
                primitive: n
                    .primitive
                    .as_ref()
                    .and_then(|p| serde_json::to_value(p).ok())
                    .and_then(|v| {
                        v.get("primitive")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    }),
                backend: backends
                    .iter()
                    .find(|(id, _)| *id == n.id)
                    .map(|(_, b)| b.clone())
                    .unwrap_or_default(),
                parallelism: n.parallelism,
                inputs: n.contract.spec.ports.inputs.iter().map(port).collect(),
                outputs: n.contract.spec.ports.outputs.iter().map(port).collect(),
                live_fields: n.bindings.keys().cloned().collect(),
            })
            .collect();
        let edge = |e: &mvf_graph::Edge| {
            json!({
                "from": format!("{}.{}", graph.nodes[e.from.node].id, e.from.port),
                "to": format!("{}.{}", graph.nodes[e.to.node].id, e.to.port),
                "type": e.port_type.as_str(),
            })
        };
        Self {
            pipeline: graph.name.clone(),
            nodes,
            edges: json!({
                "data": graph.data_edges.iter().map(edge).collect::<Vec<_>>(),
                "control": graph.control_edges.iter().map(edge).collect::<Vec<_>>(),
            }),
        }
    }
}

/// Log-scale latency histogram: 8 buckets per power of two from 1 µs, so a percentile is within
/// about ±4.5 % of the true value. 2 KiB per node, no allocation after creation.
#[derive(Debug, Clone)]
pub struct Histogram {
    buckets: Box<[u64; Self::BUCKETS]>,
    count: u64,
    max: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: Box::new([0; Self::BUCKETS]),
            count: 0,
            max: 0,
        }
    }
}

impl Histogram {
    const SUB: u32 = 8;
    const BUCKETS: usize = 8 * 40;

    fn index(us: u64) -> usize {
        if us < 2 {
            return 0;
        }
        let octave = 63 - us.leading_zeros(); // floor(log2)
        let fraction = ((us - (1 << octave)) * u64::from(Self::SUB)) >> octave;
        ((octave * Self::SUB) as usize + fraction as usize).min(Self::BUCKETS - 1)
    }

    fn lower(index: usize) -> f64 {
        let octave = (index as u32) / Self::SUB;
        let fraction = (index as u32) % Self::SUB;
        (1u64 << octave) as f64 * (1.0 + f64::from(fraction) / f64::from(Self::SUB))
    }

    /// Records one value, in microseconds.
    pub fn record(&mut self, us: u64) {
        self.buckets[Self::index(us)] += 1;
        self.count += 1;
        self.max = self.max.max(us);
    }

    /// Values recorded.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// The `p` quantile (0–1), in microseconds: the midpoint of its bucket, capped at the maximum.
    pub fn quantile(&self, p: f64) -> f64 {
        if self.count == 0 {
            return 0.0;
        }
        let rank = ((self.count as f64) * p).ceil().max(1.0) as u64;
        let mut seen = 0;
        for (i, n) in self.buckets.iter().enumerate() {
            seen += n;
            if seen >= rank {
                let mid = (Self::lower(i) + Self::lower(i + 1)) / 2.0;
                return mid.min(self.max as f64);
            }
        }
        self.max as f64
    }

    /// The largest value recorded.
    pub fn max(&self) -> u64 {
        self.max
    }
}
