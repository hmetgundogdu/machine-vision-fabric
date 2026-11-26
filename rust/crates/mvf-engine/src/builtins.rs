//! Trusted in-process builtins: simulator-first nodes that make the runtime testable without
//! hardware. Each builtin ships its own Universal Node Contract (YAML, embedded) so builtins go
//! through exactly the same contract validation and graph typing as external nodes.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mvf_contract::NodeContract;
use serde::{Deserialize, Serialize};

use crate::node::*;
use crate::value::*;

/// One recorded sink observation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Record {
    /// Cycle id.
    pub cycle: u64,
    /// Recording node id.
    pub node: String,
    /// Generation of the recording node.
    pub generation: u64,
    /// Input port.
    pub port: String,
    /// What arrived.
    pub value: RecordedValue,
}

/// A recorded value summary (bytes are fingerprinted, not stored).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum RecordedValue {
    /// A frame.
    Frame {
        /// Source id.
        source: String,
        /// Frame sequence.
        sequence: u64,
        /// Payload length.
        bytes: usize,
        /// FNV-1a 64 of the payload, hex.
        checksum: String,
    },
    /// A classification.
    Classification(Classification),
    /// JSON.
    Json {
        /// The value.
        value: serde_json::Value,
    },
}

type RecordStream = Arc<dyn Fn(&Record) + Send + Sync>;

/// Shared, thread-safe store of sink records, optionally streamed as they arrive.
#[derive(Clone, Default)]
pub struct Recorder {
    records: Arc<Mutex<Vec<Record>>>,
    stream: Option<RecordStream>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("records", &self.len())
            .finish()
    }
}

impl Recorder {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// A recorder that also calls `stream` for every record.
    pub fn streaming(stream: impl Fn(&Record) + Send + Sync + 'static) -> Self {
        Self {
            records: Arc::default(),
            stream: Some(Arc::new(stream)),
        }
    }

    /// Appends a record.
    pub fn push(&self, record: Record) {
        if let Some(stream) = &self.stream {
            stream(&record);
        }
        if let Ok(mut r) = self.records.lock() {
            r.push(record);
        }
    }

    /// A snapshot of all records.
    pub fn records(&self) -> Vec<Record> {
        self.records.lock().map(|r| r.clone()).unwrap_or_default()
    }

    /// Number of records.
    pub fn len(&self) -> usize {
        self.records.lock().map(|r| r.len()).unwrap_or(0)
    }

    /// True when nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Host services available to builtins at instantiation.
#[derive(Debug, Clone, Default)]
pub struct HostServices {
    /// Sink record store.
    pub recorder: Recorder,
}

type Factory = fn(
    &serde_json::Map<String, serde_json::Value>,
    &HostServices,
) -> Result<Box<dyn NodeHandle>, NodeError>;

struct Builtin {
    name: &'static str,
    contract: &'static str,
    factory: Factory,
}

const BUILTINS: &[Builtin] = &[
    Builtin {
        name: "synthetic-source",
        contract: include_str!("contracts/synthetic-source.yaml"),
        factory: SyntheticSource::create,
    },
    Builtin {
        name: "invert",
        contract: include_str!("contracts/invert.yaml"),
        factory: Invert::create,
    },
    Builtin {
        name: "passthrough",
        contract: include_str!("contracts/passthrough.yaml"),
        factory: Passthrough::create,
    },
    Builtin {
        name: "brightness-gate",
        contract: include_str!("contracts/brightness-gate.yaml"),
        factory: BrightnessGate::create,
    },
    Builtin {
        name: "record",
        contract: include_str!("contracts/record.yaml"),
        factory: RecordSink::create,
    },
    Builtin {
        name: "fault",
        contract: include_str!("contracts/fault.yaml"),
        factory: Fault::create,
    },
];

/// Names of every builtin this host provides.
pub fn builtin_names() -> Vec<&'static str> {
    BUILTINS.iter().map(|b| b.name).collect()
}

/// The embedded contract of builtin `name`.
pub fn builtin_contract(name: &str) -> Option<NodeContract> {
    let b = BUILTINS.iter().find(|b| b.name == name)?;
    // Embedded contracts are validated by a unit test; a broken one is a build defect.
    mvf_contract::parse_contract(b.contract).ok()
}

/// Instantiates builtin `name` with its instance config.
pub fn instantiate_builtin(
    name: &str,
    config: &serde_json::Map<String, serde_json::Value>,
    services: &HostServices,
) -> Result<Box<dyn NodeHandle>, NodeError> {
    let b = BUILTINS.iter().find(|b| b.name == name).ok_or_else(|| {
        NodeError::new(
            NodeErrorKind::Unsupported,
            format!("this host has no builtin `{name}`"),
        )
    })?;
    (b.factory)(config, services)
}

fn parse_config<T: for<'de> Deserialize<'de>>(
    config: &serde_json::Map<String, serde_json::Value>,
) -> Result<T, NodeError> {
    serde_path_to_error::deserialize(serde_json::Value::Object(config.clone())).map_err(|e| {
        NodeError::new(
            NodeErrorKind::InvalidConfig,
            format!("config `{}`: {}", e.path(), e.inner()),
        )
    })
}

fn frame_input<'a>(inputs: &'a Inputs, port: &str) -> Result<&'a Frame, NodeError> {
    match inputs.get(port) {
        Some(Value::Frame(f)) => Ok(f),
        Some(other) => Err(NodeError::new(
            NodeErrorKind::InvalidOutput,
            format!(
                "input `{port}` carried `{}`, expected frame",
                other.port_type().as_str()
            ),
        )),
        None => Err(NodeError::failed(format!("input `{port}` is missing"))),
    }
}

// ---------------------------------------------------------------- synthetic-source

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SyntheticSourceConfig {
    /// Number of frames before exhaustion; absent = endless.
    #[serde(default)]
    frames: Option<u64>,
    #[serde(default = "four")]
    width: u64,
    #[serde(default = "two")]
    height: u64,
    #[serde(default = "one")]
    channels: u64,
    #[serde(default)]
    seed: u64,
    /// Source id stamped on frames (default: `synthetic`).
    #[serde(default)]
    source_id: Option<String>,
    /// Optional pacing between frames, in milliseconds.
    #[serde(default)]
    interval_ms: u64,
}

fn one() -> u64 {
    1
}
fn two() -> u64 {
    2
}
fn four() -> u64 {
    4
}

/// Deterministic frame generator: `pixel[i] = seed + 7·sequence + i (mod 256)`.
#[derive(Debug)]
pub struct SyntheticSource {
    config: SyntheticSourceConfig,
    next_sequence: u64,
}

impl SyntheticSource {
    fn create(
        config: &serde_json::Map<String, serde_json::Value>,
        _: &HostServices,
    ) -> Result<Box<dyn NodeHandle>, NodeError> {
        let config: SyntheticSourceConfig = parse_config(config)?;
        let pixels = config
            .width
            .checked_mul(config.height)
            .and_then(|p| p.checked_mul(config.channels));
        match pixels {
            Some(p) if p > 0 && p <= 64 * 1024 * 1024 => {}
            _ => {
                return Err(NodeError::new(
                    NodeErrorKind::InvalidConfig,
                    "width × height × channels must be within 1..=64 MiB",
                ));
            }
        }
        Ok(Box::new(Self {
            config,
            next_sequence: 1,
        }))
    }

    /// The deterministic pixels of frame `sequence`.
    pub fn pixels(seed: u64, sequence: u64, len: usize) -> Vec<u8> {
        let base = seed.wrapping_add(sequence.wrapping_mul(7));
        (0..len)
            .map(|i| (base.wrapping_add(i as u64) & 0xff) as u8)
            .collect()
    }
}

impl NodeHandle for SyntheticSource {
    fn execute(&mut self, ctx: &ExecContext<'_>, _: &Inputs) -> Result<Execution, NodeError> {
        if self
            .config
            .frames
            .is_some_and(|limit| self.next_sequence > limit)
        {
            return Ok(Execution::Exhausted);
        }
        if self.config.interval_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.config.interval_ms));
        }
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let c = &self.config;
        let descriptor = Descriptor::image_u8(c.height, c.width, c.channels);
        let len = descriptor.byte_len().unwrap_or(0) as usize;
        let payload = Payload::heap(descriptor, Self::pixels(c.seed, sequence, len))
            .map_err(NodeError::failed)?;
        let source = c
            .source_id
            .clone()
            .unwrap_or_else(|| ctx.node_id.to_string());
        Ok(Execution::Produced(Outputs::new().with(
            "frame",
            Value::Frame(Frame {
                meta: FrameMeta { source, sequence },
                payload,
            }),
        )))
    }
}

// ---------------------------------------------------------------- invert

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoConfig {}

/// `b → 255 − b`. Writes a new output buffer (a transform write, not a transfer copy).
#[derive(Debug)]
pub struct Invert;

impl Invert {
    fn create(
        config: &serde_json::Map<String, serde_json::Value>,
        _: &HostServices,
    ) -> Result<Box<dyn NodeHandle>, NodeError> {
        let _: NoConfig = parse_config(config)?;
        Ok(Box::new(Invert))
    }
}

impl NodeHandle for Invert {
    fn execute(&mut self, _: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        let frame = frame_input(inputs, "frame")?;
        let inverted: Vec<u8> = frame.payload.bytes().iter().map(|b| 255 - b).collect();
        let payload =
            Payload::heap(frame.payload.descriptor.clone(), inverted).map_err(NodeError::failed)?;
        Ok(Execution::Produced(Outputs::new().with(
            "frame",
            Value::Frame(Frame {
                meta: frame.meta.clone(),
                payload,
            }),
        )))
    }
}

// ---------------------------------------------------------------- passthrough

/// Forwards its input frame unchanged — the hand-off shares the payload, no byte copy.
#[derive(Debug)]
pub struct Passthrough;

impl Passthrough {
    fn create(
        config: &serde_json::Map<String, serde_json::Value>,
        _: &HostServices,
    ) -> Result<Box<dyn NodeHandle>, NodeError> {
        let _: NoConfig = parse_config(config)?;
        Ok(Box::new(Passthrough))
    }
}

impl NodeHandle for Passthrough {
    fn execute(&mut self, _: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        let frame = frame_input(inputs, "frame")?;
        Ok(Execution::Produced(
            Outputs::new().with("frame", Value::Frame(frame.clone())),
        ))
    }
}

// ---------------------------------------------------------------- brightness-gate

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrightnessConfig {
    #[serde(default = "default_threshold")]
    threshold: f64,
}

fn default_threshold() -> f64 {
    128.0
}

/// Mean-byte classifier: emits a `class` (bright/dark) and a `bright` control signal.
#[derive(Debug)]
pub struct BrightnessGate {
    threshold: f64,
}

impl BrightnessGate {
    fn create(
        config: &serde_json::Map<String, serde_json::Value>,
        _: &HostServices,
    ) -> Result<Box<dyn NodeHandle>, NodeError> {
        let c: BrightnessConfig = parse_config(config)?;
        if !c.threshold.is_finite() {
            return Err(NodeError::new(
                NodeErrorKind::InvalidConfig,
                "threshold must be finite",
            ));
        }
        Ok(Box::new(Self {
            threshold: c.threshold,
        }))
    }
}

impl NodeHandle for BrightnessGate {
    fn execute(&mut self, _: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        let frame = frame_input(inputs, "frame")?;
        let bytes = frame.payload.bytes();
        let mean = if bytes.is_empty() {
            0.0
        } else {
            bytes.iter().map(|b| f64::from(*b)).sum::<f64>() / bytes.len() as f64
        };
        let bright = mean >= self.threshold;
        Ok(Execution::Produced(
            Outputs::new()
                .with(
                    "class",
                    Value::Classification(Classification {
                        label: if bright { "bright" } else { "dark" }.into(),
                        measurement: Some(mean),
                        unit: Some("mean-byte".into()),
                        details: None,
                    }),
                )
                .with("bright", Value::Signal(bright)),
        ))
    }
}

// ---------------------------------------------------------------- record

/// Records whatever arrives on its optional `frame`, `class` and `json` inputs.
#[derive(Debug)]
pub struct RecordSink {
    recorder: Recorder,
}

impl RecordSink {
    fn create(
        config: &serde_json::Map<String, serde_json::Value>,
        services: &HostServices,
    ) -> Result<Box<dyn NodeHandle>, NodeError> {
        let _: NoConfig = parse_config(config)?;
        Ok(Box::new(Self {
            recorder: services.recorder.clone(),
        }))
    }
}

impl NodeHandle for RecordSink {
    fn execute(&mut self, ctx: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        for (port, value) in inputs.iter() {
            let recorded = match value {
                Value::Frame(f) => RecordedValue::Frame {
                    source: f.meta.source.clone(),
                    sequence: f.meta.sequence,
                    bytes: f.payload.len(),
                    checksum: format!("{:016x}", f.payload.checksum()),
                },
                Value::Classification(c) => RecordedValue::Classification(c.clone()),
                Value::Json(v) => RecordedValue::Json { value: v.clone() },
                // Control inputs gate the sink; they are not recorded.
                _ => continue,
            };
            self.recorder.push(Record {
                cycle: ctx.cycle,
                node: ctx.node_id.to_string(),
                generation: ctx.generation,
                port: port.clone(),
                value: recorded,
            });
        }
        Ok(Execution::Produced(Outputs::new()))
    }
}

// ---------------------------------------------------------------- fault (test fixture)

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FaultConfig {
    #[serde(default)]
    fail_at_cycle: Option<u64>,
    #[serde(default)]
    panic_at_cycle: Option<u64>,
    #[serde(default)]
    delay_ms: u64,
    #[serde(default)]
    fail_start: bool,
}

/// Pass-through test fixture that can fail, panic, stall or refuse to start on demand.
#[derive(Debug)]
pub struct Fault {
    config: FaultConfig,
}

impl Fault {
    fn create(
        config: &serde_json::Map<String, serde_json::Value>,
        _: &HostServices,
    ) -> Result<Box<dyn NodeHandle>, NodeError> {
        Ok(Box::new(Self {
            config: parse_config(config)?,
        }))
    }
}

impl NodeHandle for Fault {
    fn start(&mut self, _: &LifecycleContext<'_>) -> Result<(), NodeError> {
        if self.config.fail_start {
            return Err(NodeError::new(
                NodeErrorKind::StartFailed,
                "fault: configured to fail readiness",
            ));
        }
        Ok(())
    }

    fn execute(&mut self, ctx: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        if self.config.delay_ms > 0 {
            // Cooperative: observe cancellation while stalling.
            let until = std::time::Instant::now() + Duration::from_millis(self.config.delay_ms);
            while std::time::Instant::now() < until {
                if ctx.cancel.is_cancelled() {
                    return Err(NodeError::new(
                        NodeErrorKind::Cancelled,
                        "fault: cancelled while stalling",
                    ));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        if self.config.panic_at_cycle == Some(ctx.cycle) {
            panic!("fault: configured panic at cycle {}", ctx.cycle);
        }
        if self.config.fail_at_cycle == Some(ctx.cycle) {
            return Err(NodeError::failed(format!(
                "fault: configured failure at cycle {}",
                ctx.cycle
            )));
        }
        let frame = frame_input(inputs, "frame")?;
        Ok(Execution::Produced(
            Outputs::new().with("frame", Value::Frame(frame.clone())),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_contract_is_valid_and_names_its_builtin() {
        for b in BUILTINS {
            let contract = mvf_contract::parse_contract(b.contract)
                .unwrap_or_else(|e| panic!("builtin `{}` contract invalid: {e}", b.name));
            let names_itself = contract.spec.artifacts.iter().any(|a| {
                matches!(a, mvf_contract::Artifact::InProcess { builtin, .. } if builtin == b.name)
            });
            assert!(names_itself, "builtin `{}` artifact mismatch", b.name);
        }
    }

    #[test]
    fn synthetic_pixels_are_deterministic() {
        assert_eq!(SyntheticSource::pixels(0, 1, 4), vec![7, 8, 9, 10]);
        assert_eq!(SyntheticSource::pixels(250, 1, 2), vec![1, 2]);
    }

    #[test]
    fn unknown_config_fields_are_rejected() {
        let mut config = serde_json::Map::new();
        config.insert("framez".into(), serde_json::json!(3));
        let err = instantiate_builtin("synthetic-source", &config, &HostServices::default())
            .err()
            .expect("rejected");
        assert_eq!(err.kind, NodeErrorKind::InvalidConfig);
        assert!(err.message.contains("framez"), "{}", err.message);
    }
}
