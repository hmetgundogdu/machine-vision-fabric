//! Graph primitives: `fork`, `if`, `switch`, `select`, `value`, `loop` — graph semantics that need no
//! module, with the .NET reference behaviour (`*PrimitiveNodeRunner`, `docs/value-and-select-design.md`,
//! `docs/loop-and-running-state-design.md`).
//!
//! A primitive gets a **synthesized** Universal Node Contract, so it is typed and validated exactly
//! like any other node: `fork`/`switch` outputs are the ports their outgoing edges name, `select` and
//! `value` port types come from their declared element type and shape. The engine runs primitives
//! in-process (`primitive.<kind>` artifacts); `loop` is portless and owned by the executor.

use std::collections::BTreeMap;

use mvf_contract::{Channel, NodeContract, PortType};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::values::{ElemType, Shape, ValueType};

/// Which primitive a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrimitiveKind {
    /// Broadcasts `frame` to every declared output.
    Fork,
    /// Passes `frame` to `acceptedFrame` while the `productPresent` control signal is true.
    If,
    /// Routes `frame` to the output named by the `class` label (else `default`).
    Switch,
    /// Narrows a collection by a criterion.
    Select,
    /// Emits one typed value the graph cannot compute.
    Value,
    /// The graph's iteration authority (portless).
    Loop,
}

impl PrimitiveKind {
    /// Stable name.
    pub fn as_str(self) -> &'static str {
        match self {
            PrimitiveKind::Fork => "fork",
            PrimitiveKind::If => "if",
            PrimitiveKind::Switch => "switch",
            PrimitiveKind::Select => "select",
            PrimitiveKind::Value => "value",
            PrimitiveKind::Loop => "loop",
        }
    }
}

/// `value` configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ValueConfig {
    /// Element type.
    #[serde(rename = "type")]
    pub elem: ElemType,
    /// `one` (default) or `list`.
    #[serde(default)]
    pub shape: Shape,
    /// JSON Schema (for a list: of each element).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Json>,
    /// A hand-authored constant (wins over everything).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub literal: Option<Json>,
    /// Machine-local binding key (`.mvf/bindings.json`, `MVF_BINDING_<KEY>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
    /// Operator prompt text (the Rust host is unattended; kept for document parity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Fallback when nothing else resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Json>,
}

impl ValueConfig {
    /// The emitted type.
    pub fn value_type(&self) -> ValueType {
        ValueType {
            elem: self.elem,
            shape: self.shape,
        }
    }
}

/// What `select` emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectMode {
    /// The first matching element.
    #[default]
    One,
    /// Every matching element (possibly none).
    Many,
}

/// `select` configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SelectConfig {
    /// `one` or `many`.
    #[serde(default)]
    pub mode: SelectMode,
    /// Element type of the collection (default `json`).
    #[serde(default, rename = "type")]
    pub elem: ElemType,
    /// A static criterion.
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    pub criterion: Option<Json>,
    /// Property of each element to compare with the criterion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// Binding key for the criterion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
    /// Operator prompt text (document parity; the Rust host never prompts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// Loop termination policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoopMode {
    /// Run until a source is exhausted.
    #[default]
    UntilExhausted,
    /// Rewind finite sources on exhaustion and keep going.
    Forever,
    /// Stop after `count` cycles.
    Count,
}

/// `loop` configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LoopConfig {
    /// Termination policy.
    #[serde(default)]
    pub mode: LoopMode,
    /// Cycles for `count`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

/// A primitive with its parsed configuration.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", tag = "primitive", content = "config")]
pub enum PrimitiveSpec {
    /// `fork`.
    Fork,
    /// `if`.
    If,
    /// `switch`.
    Switch,
    /// `select`.
    Select(SelectConfig),
    /// `value`.
    Value(ValueConfig),
    /// `loop`.
    Loop(LoopConfig),
}

/// Direction of a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Input.
    In,
    /// Output.
    Out,
}

impl PrimitiveSpec {
    /// Parses `config` for `kind`. Primitives without configuration accept none.
    pub fn parse(
        kind: PrimitiveKind,
        config: &serde_json::Map<String, Json>,
    ) -> Result<Self, String> {
        fn parse<T: serde::de::DeserializeOwned>(
            config: &serde_json::Map<String, Json>,
        ) -> Result<T, String> {
            serde_json::from_value(Json::Object(config.clone())).map_err(|e| e.to_string())
        }
        let none = |spec: PrimitiveSpec| {
            if config.is_empty() {
                Ok(spec)
            } else {
                Err(format!("`{}` takes no config", kind.as_str()))
            }
        };
        match kind {
            PrimitiveKind::Fork => none(PrimitiveSpec::Fork),
            PrimitiveKind::If => none(PrimitiveSpec::If),
            PrimitiveKind::Switch => none(PrimitiveSpec::Switch),
            PrimitiveKind::Select => parse(config).map(PrimitiveSpec::Select),
            PrimitiveKind::Value => parse(config).map(PrimitiveSpec::Value),
            PrimitiveKind::Loop => parse(config).map(PrimitiveSpec::Loop),
        }
    }

    /// The primitive kind.
    pub fn kind(&self) -> PrimitiveKind {
        match self {
            PrimitiveSpec::Fork => PrimitiveKind::Fork,
            PrimitiveSpec::If => PrimitiveKind::If,
            PrimitiveSpec::Switch => PrimitiveKind::Switch,
            PrimitiveSpec::Select(_) => PrimitiveKind::Select,
            PrimitiveSpec::Value(_) => PrimitiveKind::Value,
            PrimitiveSpec::Loop(_) => PrimitiveKind::Loop,
        }
    }

    /// Whether the output ports are the ones named by outgoing edges.
    pub fn outputs_from_edges(&self) -> bool {
        matches!(self, PrimitiveSpec::Fork | PrimitiveSpec::Switch)
    }

    /// The refined value type of a port, for primitives that carry typed values.
    pub fn refined(&self, direction: Direction, port: &str) -> Option<ValueType> {
        match (self, direction, port) {
            (PrimitiveSpec::Value(c), Direction::Out, "value") => Some(c.value_type()),
            (PrimitiveSpec::Select(c), Direction::In, "items") => Some(ValueType::list(c.elem)),
            (PrimitiveSpec::Select(c), Direction::In, "criterion") => Some(ValueType::one(c.elem)),
            (PrimitiveSpec::Select(c), Direction::Out, "selected") => Some(match c.mode {
                SelectMode::One => ValueType::one(c.elem),
                SelectMode::Many => ValueType::list(c.elem),
            }),
            _ => None,
        }
    }

    /// Synthesizes the node contract. `edge_outputs` are the output ports named by outgoing edges
    /// (used by `fork`/`switch`).
    pub fn contract(&self, edge_outputs: &[String]) -> Result<NodeContract, String> {
        let port = |name: &str, channel: Channel, ty: PortType, required: bool| {
            serde_json::json!({
                "name": name,
                "channel": channel_name(channel),
                "type": ty.as_str(),
                "required": required,
            })
        };
        let frames = |names: &[String]| -> Vec<Json> {
            names
                .iter()
                .map(|n| port(n, Channel::Data, PortType::Frame, true))
                .collect()
        };
        let (inputs, outputs): (Vec<Json>, Vec<Json>) = match self {
            PrimitiveSpec::Fork => (
                vec![port("frame", Channel::Data, PortType::Frame, true)],
                frames(edge_outputs),
            ),
            PrimitiveSpec::If => (
                vec![
                    port("frame", Channel::Data, PortType::Frame, true),
                    port("productPresent", Channel::Control, PortType::Signal, true),
                ],
                vec![port("acceptedFrame", Channel::Data, PortType::Frame, true)],
            ),
            PrimitiveSpec::Switch => (
                vec![
                    port("frame", Channel::Data, PortType::Frame, true),
                    port("class", Channel::Data, PortType::Classification, true),
                ],
                frames(edge_outputs),
            ),
            PrimitiveSpec::Select(c) => (
                vec![
                    port(
                        "items",
                        Channel::Data,
                        ValueType::list(c.elem).port_type(),
                        true,
                    ),
                    port(
                        "criterion",
                        Channel::Data,
                        ValueType::one(c.elem).port_type(),
                        false,
                    ),
                ],
                vec![port(
                    "selected",
                    Channel::Data,
                    self.refined(Direction::Out, "selected")
                        .map_or(PortType::Json, ValueType::port_type),
                    true,
                )],
            ),
            PrimitiveSpec::Value(c) => (
                Vec::new(),
                vec![port(
                    "value",
                    Channel::Data,
                    c.value_type().port_type(),
                    true,
                )],
            ),
            PrimitiveSpec::Loop(_) => (Vec::new(), Vec::new()),
        };
        let kind = self.kind().as_str();
        let contract = serde_json::json!({
            "apiVersion": mvf_contract::NODE_CONTRACT_API_VERSION,
            "kind": mvf_contract::NODE_CONTRACT_KIND,
            "metadata": {
                "id": format!("mvf.primitive.{kind}"),
                "version": "1.0.0",
                "description": format!("graph primitive `{kind}`"),
            },
            "spec": {
                "ports": { "inputs": inputs, "outputs": outputs },
                "artifacts": [ { "backend": "in-process", "builtin": format!("primitive.{kind}") } ],
                "lifecycle": { "activation": "resident", "replacement": "unsupported" },
            },
        });
        let contract: NodeContract = serde_json::from_value(contract).map_err(|e| e.to_string())?;
        // A loop is portless by design, which a module contract may not be; it is never executed.
        if !matches!(self, PrimitiveSpec::Loop(_)) {
            let issues = mvf_contract::validate(&contract);
            if !issues.is_empty() {
                return Err(issues
                    .iter()
                    .map(|i| format!("{}: {}", i.path, i.message))
                    .collect::<Vec<_>>()
                    .join("; "));
            }
        }
        Ok(contract)
    }
}

fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Data => "data",
        Channel::Control => "control",
    }
}

/// A live-editable module config field (`bindings` on a node).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModuleBinding {
    /// Element type of the field.
    #[serde(rename = "type")]
    pub elem: ElemType,
    /// Persistence key in the binding store.
    pub binding: String,
    /// Optional JSON Schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Json>,
}

impl ModuleBinding {
    /// The field's type.
    pub fn value_type(&self) -> ValueType {
        ValueType::one(self.elem)
    }
}

/// Live-editable fields by config key.
pub type ModuleBindings = BTreeMap<String, ModuleBinding>;
