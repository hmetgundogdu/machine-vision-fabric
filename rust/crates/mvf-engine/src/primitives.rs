//! In-process handles for the graph primitives, with the .NET reference semantics
//! (`ForkPrimitiveNodeRunner`, `IfPrimitiveNodeRunner`, `SwitchPrimitiveNodeRunner`,
//! `SelectPrimitiveNodeRunner`, `ValuePrimitiveNodeRunner`, `LoopPrimitiveNodeRunner`).
//!
//! Absence is the executor's job: a node whose required input is absent, or whose control input is
//! absent or false, is skipped and its outputs stay absent. So `if` only passes its frame on (the
//! executor already withheld it when `productPresent` was false), and a primitive that "emits
//! nothing" returns an empty output set, which downstream nodes see as absence — exactly a closed
//! gate in .NET.

use mvf_contract::PortType;
use mvf_graph::{PrimitiveKind, PrimitiveSpec, SelectConfig, SelectMode, ValueConfig, ValueType};
use serde_json::Value as Json;

use crate::backend::InstantiateRequest;
use crate::node::*;
use crate::value::Value;

/// Creates the handle for a primitive node.
pub(crate) fn instantiate(
    kind: PrimitiveKind,
    request: &InstantiateRequest<'_>,
) -> Result<Box<dyn NodeHandle>, NodeError> {
    let spec = PrimitiveSpec::parse(kind, request.config)
        .map_err(|e| NodeError::new(NodeErrorKind::InvalidConfig, e))?;
    let outputs: Vec<String> = request
        .contract
        .spec
        .ports
        .outputs
        .iter()
        .map(|p| p.name.clone())
        .collect();
    Ok(match spec {
        PrimitiveSpec::Fork => Box::new(Fork { outputs }),
        PrimitiveSpec::If => Box::new(If),
        PrimitiveSpec::Switch => Box::new(Switch { outputs }),
        PrimitiveSpec::Value(config) => {
            let (current, _) = request
                .services
                .bindings
                .resolve_value(request.node_id, &config)
                .map_err(|e| NodeError::new(NodeErrorKind::InvalidConfig, e))?;
            Box::new(ValueNode { config, current })
        }
        PrimitiveSpec::Select(config) => {
            let mut criterion = config.criterion.clone();
            if criterion.is_none()
                && let Some(key) = &config.binding
            {
                // With a `by`, the criterion is one property of an element (a string answer, as in
                // the .NET pre-pass), not an element of the collection.
                let ty = if config.by.as_deref().is_some_and(|b| !b.is_empty()) {
                    ValueType::one(mvf_graph::ElemType::String)
                } else {
                    ValueType::one(config.elem)
                };
                let stored = request.services.bindings.stored(key);
                criterion = match stored {
                    Some(v) if ty.elem == mvf_graph::ElemType::String => Some(v),
                    _ => request
                        .services
                        .bindings
                        .lookup(key, ty, None)
                        .map_err(|e| {
                            NodeError::new(
                                NodeErrorKind::InvalidConfig,
                                format!("select `{}`: {e}", request.node_id),
                            )
                        })?
                        .map(|(v, _)| v),
                };
            }
            Box::new(Select {
                config,
                criterion,
                live: None,
            })
        }
        PrimitiveSpec::Loop(_) => Box::new(Loop),
    })
}

fn frame_input(inputs: &Inputs) -> Result<&Value, NodeError> {
    inputs
        .get("frame")
        .ok_or_else(|| NodeError::new(NodeErrorKind::Failed, "missing required input `frame`"))
}

/// `fork`: the frame on every declared output, unchanged (shared, never copied).
#[derive(Debug)]
struct Fork {
    outputs: Vec<String>,
}

impl NodeHandle for Fork {
    fn execute(&mut self, _: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        let frame = frame_input(inputs)?;
        let mut out = Outputs::new();
        for port in &self.outputs {
            out = out.with(port.clone(), frame.clone());
        }
        Ok(Execution::Produced(out))
    }
}

/// `if`: `frame` → `acceptedFrame`. The executor runs it only while `productPresent` is true.
#[derive(Debug)]
struct If;

impl NodeHandle for If {
    fn execute(&mut self, _: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        let frame = frame_input(inputs)?;
        Ok(Execution::Produced(
            Outputs::new().with("acceptedFrame", frame.clone()),
        ))
    }
}

/// `switch`: the frame on the output named by the `class` label, else on `default`, else nowhere.
#[derive(Debug)]
struct Switch {
    outputs: Vec<String>,
}

impl NodeHandle for Switch {
    fn execute(&mut self, _: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        let frame = frame_input(inputs)?;
        let Some(Value::Classification(class)) = inputs.get("class") else {
            return Err(NodeError::new(
                NodeErrorKind::Failed,
                "missing required input `class`",
            ));
        };
        let target = if self.outputs.contains(&class.label) {
            Some(class.label.as_str())
        } else if self.outputs.iter().any(|p| p == "default") {
            Some("default")
        } else {
            None
        };
        let mut out = Outputs::new();
        if let Some(port) = target {
            out = out.with(port, frame.clone());
        }
        Ok(Execution::Produced(out))
    }
}

/// Wraps JSON on the port type a value type travels on.
fn typed(ty: ValueType, value: Json) -> Value {
    match ty.port_type() {
        PortType::Value => Value::Scalar(value),
        _ => Value::Json(value),
    }
}

fn json_of(value: &Value) -> Option<&Json> {
    match value {
        Value::Json(j) | Value::Scalar(j) => Some(j),
        _ => None,
    }
}

/// `value`: one resolved, typed value every cycle; live-settable at a cycle boundary.
#[derive(Debug)]
struct ValueNode {
    config: ValueConfig,
    current: Json,
}

impl NodeHandle for ValueNode {
    fn execute(&mut self, _: &ExecContext<'_>, _: &Inputs) -> Result<Execution, NodeError> {
        Ok(Execution::Produced(Outputs::new().with(
            "value",
            typed(self.config.value_type(), self.current.clone()),
        )))
    }

    fn set_value(&mut self, value: &Json) -> Result<(), NodeError> {
        self.config
            .value_type()
            .check(self.config.schema.as_ref(), value)
            .map_err(|e| NodeError::new(NodeErrorKind::InvalidConfig, e))?;
        self.current = value.clone();
        Ok(())
    }
}

/// `select`: narrows `items` by a criterion — the edge, else a live pick, else the static one.
#[derive(Debug)]
struct Select {
    config: SelectConfig,
    criterion: Option<Json>,
    live: Option<Json>,
}

impl Select {
    fn matches(&self, item: &Json, criterion: &Json) -> bool {
        if let Some(by) = self.config.by.as_deref().filter(|b| !b.is_empty()) {
            return item.get(by).is_some_and(|v| v == criterion);
        }
        if let Some(filter) = criterion.as_object() {
            let Some(candidate) = item.as_object() else {
                return false;
            };
            return filter.iter().all(|(k, v)| candidate.get(k) == Some(v));
        }
        item == criterion
    }
}

impl NodeHandle for Select {
    fn execute(&mut self, _: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        let Some(items) = inputs
            .get("items")
            .and_then(json_of)
            .and_then(Json::as_array)
        else {
            return Ok(Execution::Produced(Outputs::new()));
        };
        let criterion = inputs
            .get("criterion")
            .and_then(json_of)
            .or(self.live.as_ref())
            .or(self.criterion.as_ref());
        let Some(criterion) = criterion else {
            return Ok(Execution::Produced(Outputs::new()));
        };
        let elem = self.config.elem;
        let out = match self.config.mode {
            SelectMode::Many => {
                let narrowed: Vec<Json> = items
                    .iter()
                    .filter(|i| self.matches(i, criterion))
                    .cloned()
                    .collect();
                Outputs::new().with("selected", Value::Json(Json::Array(narrowed)))
            }
            SelectMode::One => match items.iter().find(|i| self.matches(i, criterion)) {
                Some(item) => {
                    Outputs::new().with("selected", typed(ValueType::one(elem), item.clone()))
                }
                // Nothing matched: emitting nothing is the honest answer (downstream is skipped).
                None => Outputs::new(),
            },
        };
        Ok(Execution::Produced(out))
    }

    fn set_value(&mut self, value: &Json) -> Result<(), NodeError> {
        if self.config.by.is_none() && !value.is_object() {
            ValueType::one(self.config.elem)
                .check(None, value)
                .map_err(|e| NodeError::new(NodeErrorKind::InvalidConfig, e))?;
        }
        self.live = Some(value.clone());
        Ok(())
    }
}

/// `loop`: portless marker; the executor owns iteration and pause. Never executed.
#[derive(Debug)]
struct Loop;

impl NodeHandle for Loop {
    fn execute(&mut self, _: &ExecContext<'_>, _: &Inputs) -> Result<Execution, NodeError> {
        Ok(Execution::Produced(Outputs::new()))
    }
}
