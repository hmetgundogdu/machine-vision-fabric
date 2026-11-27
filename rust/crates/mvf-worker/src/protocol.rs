//! The worker control protocol: v1 (newline-delimited JSON, `protocol/README.md`) and v2 (binary
//! frames, `mvf_ipc`, `protocol/worker-v2.md`). Both carry only arena offsets, never payload bytes.
//! The host builds [`HostMessage`]s and receives [`ChildMessage`]s whatever the version.

use serde_json::{Value, json};

/// The JSON-lines protocol version.
pub const PROTOCOL_VERSION: u64 = 1;
/// The binary protocol version (#32).
pub const PROTOCOL_V2: u64 = 2;

/// A message to the child.
#[derive(Debug, Clone, PartialEq)]
pub enum HostMessage {
    /// Run one cycle on `input` (`None` for a source) into `out`.
    Execute {
        /// Correlation id.
        id: u64,
        /// Input frame `(camera, sequence, content type, offset)`.
        input: Option<(String, u64, String, u64)>,
        /// Output slot `(offset, capacity)`.
        out: Option<(u64, usize)>,
        /// Budget in microseconds (0 = none).
        deadline_us: u32,
    },
    /// Apply config.
    Configure {
        /// Correlation id.
        id: u64,
        /// The config.
        config: serde_json::Map<String, Value>,
    },
    /// Write state into a slot.
    Checkpoint {
        /// Correlation id.
        id: u64,
        /// Slot offset.
        offset: u64,
        /// Slot capacity.
        capacity: usize,
    },
    /// Restore from a slot.
    Restore {
        /// Correlation id.
        id: u64,
        /// Slot offset.
        offset: u64,
    },
    /// Liveness probe (v2).
    Ping {
        /// Correlation id.
        id: u64,
    },
    /// Abandon a request (v2, best effort).
    Cancel {
        /// The request.
        id: u64,
    },
    /// Exit.
    Shutdown,
}

impl HostMessage {
    /// The v1 JSON line (without the newline). `Ping`/`Cancel` have no v1 form.
    pub fn to_line(&self) -> Option<String> {
        Some(match self {
            Self::Execute {
                id,
                input: Some((camera, sequence, content_type, offset)),
                out,
                ..
            } => execute(*id, camera, *sequence, content_type, *offset, *out),
            Self::Execute {
                id,
                input: None,
                out: Some((offset, capacity)),
                ..
            } => execute_source(*id, *offset, *capacity),
            Self::Execute { .. } => return None,
            Self::Configure { id, config } => {
                json!({"type": "configure", "id": id, "config": config}).to_string()
            }
            Self::Checkpoint {
                id,
                offset,
                capacity,
            } => checkpoint(*id, *offset, *capacity),
            Self::Restore { id, offset } => restore(*id, *offset),
            Self::Shutdown => shutdown(),
            Self::Ping { .. } | Self::Cancel { .. } => return None,
        })
    }

    /// The v2 frame for `generation`.
    pub fn to_frame(&self, generation: u32) -> Vec<u8> {
        use mvf_ipc::frame::{Header, Kind};
        let (kind, id, body) = match self {
            Self::Execute {
                id,
                input,
                out,
                deadline_us,
            } => {
                let (camera, sequence, content_type, offset) = input
                    .clone()
                    .map(|(c, s, t, o)| (c, s, t, Some(o)))
                    .unwrap_or_default();
                let body = mvf_ipc::Execute {
                    input: offset,
                    sequence,
                    out: out.map(|(o, c)| (o, c as u64)),
                    deadline_us: *deadline_us,
                    camera,
                    content_type,
                }
                .encode();
                (Kind::Execute, *id, body)
            }
            Self::Configure { id, config } => (
                Kind::Configure,
                *id,
                Value::Object(config.clone()).to_string().into_bytes(),
            ),
            Self::Checkpoint {
                id,
                offset,
                capacity,
            } => (
                Kind::Checkpoint,
                *id,
                mvf_ipc::msg::slot(*offset, *capacity as u64),
            ),
            Self::Restore { id, offset } => (Kind::Restore, *id, offset.to_le_bytes().to_vec()),
            Self::Ping { id } => (Kind::Ping, *id, Vec::new()),
            Self::Cancel { id } => (Kind::Cancel, *id, Vec::new()),
            Self::Shutdown => (Kind::Shutdown, 0, Vec::new()),
        };
        let mut header = Header::new(kind, id, generation);
        header.length = body.len() as u32;
        let mut out = header.encode().to_vec();
        out.extend_from_slice(&body);
        out
    }
}

/// Decodes a v2 frame from the child into the version-independent [`ChildMessage`]. `Err` is a
/// protocol violation.
pub fn decode_frame(header: &mvf_ipc::frame::Header, body: &[u8]) -> Result<ChildMessage, String> {
    use mvf_ipc::frame::Kind;
    use mvf_ipc::msg::ResultFlags;
    let json = |b: &[u8]| -> Result<Value, String> {
        if b.is_empty() {
            return Ok(Value::Object(Default::default()));
        }
        serde_json::from_slice(b).map_err(|e| format!("{:?} body is not JSON: {e}", header.kind))
    };
    Ok(match header.kind {
        Kind::Hello => {
            let v = json(body)?;
            let s = |k: &str| {
                v.get(k)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            ChildMessage::Hello {
                protocol: v.get("protocol").and_then(Value::as_u64).unwrap_or(0),
                module_id: s("moduleId"),
                capability: s("capability"),
                ready: v.get("ready").and_then(Value::as_bool).unwrap_or(true),
                features: v
                    .get("features")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|f| f.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        }
        Kind::Ready => ChildMessage::Ready,
        Kind::Result => {
            let r = mvf_ipc::ResultMsg::decode(body).map_err(|e| e.to_string())?;
            let tail = r.tail.unwrap_or(Value::Null);
            ChildMessage::Result {
                id: header.id,
                frame: if r.flags & ResultFlags::FRAME != 0 {
                    Some(Some(r.out))
                } else if r.flags & ResultFlags::DROP != 0 {
                    Some(None)
                } else {
                    None
                },
                classification: tail.get("classification").filter(|c| !c.is_null()).cloned(),
                value: tail.get("value").filter(|c| !c.is_null()).cloned(),
                camera_id: tail
                    .get("cameraId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                sequence: (r.sequence != 0).then_some(r.sequence),
                exhausted: r.flags & ResultFlags::EXHAUSTED != 0,
            }
        }
        Kind::Error => {
            let v = json(body)?;
            ChildMessage::Error {
                id: (header.id != 0).then_some(header.id),
                message: v
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }
        }
        Kind::Configured => ChildMessage::Other("configured".into()),
        Kind::State => ChildMessage::State {
            id: header.id,
            empty: header.flags & 1 != 0,
        },
        Kind::Restored => ChildMessage::Restored { id: header.id },
        Kind::Pong => ChildMessage::Other("pong".into()),
        other => return Err(format!("a child does not send {other:?} frames")),
    })
}

/// A message from the child.
#[derive(Debug, Clone, PartialEq)]
pub enum ChildMessage {
    /// Handshake.
    Hello {
        /// Protocol version.
        protocol: u64,
        /// Module id.
        module_id: String,
        /// `classifier` | `processor` | `analyzer`.
        capability: String,
        /// `false` = warming up; `ready` follows.
        ready: bool,
        /// Optional features (e.g. `configure`).
        features: Vec<String>,
    },
    /// Warmup finished.
    Ready,
    /// Reply to `execute`.
    Result {
        /// Request id.
        id: u64,
        /// `None` = field absent; `Some(None)` = `"frame": null` (drop); `Some(Some(o))` = offset.
        frame: Option<Option<u64>>,
        /// Classification output.
        classification: Option<Value>,
        /// Structured metadata output.
        value: Option<Value>,
        /// Source capability: the produced frame's camera id.
        camera_id: Option<String>,
        /// Source capability: the produced frame's sequence.
        sequence: Option<u64>,
        /// Source capability: the stream is exhausted (no frame).
        exhausted: bool,
    },
    /// Per-request failure.
    Error {
        /// Request id, when the failure belongs to one.
        id: Option<u64>,
        /// Message.
        message: String,
    },
    /// Reply to `checkpoint`: the state was written to the slot, or `empty` (stateless).
    State {
        /// Request id.
        id: u64,
        /// The module has no state to capture.
        empty: bool,
    },
    /// Reply to `restore`.
    Restored {
        /// Request id.
        id: u64,
    },
    /// Diagnostics.
    Log {
        /// Level.
        level: String,
        /// Message.
        message: String,
    },
    /// Any other well-formed message (forward compatible; ignored by this host).
    Other(String),
}

/// Parses one stdout line. `Err` carries the raw line for diagnostics.
pub fn parse_line(line: &str) -> Result<ChildMessage, String> {
    let v: Value = serde_json::from_str(line).map_err(|_| line.to_string())?;
    let ty = v
        .get("type")
        .and_then(Value::as_str)
        .ok_or(line.to_string())?;
    let s = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Ok(match ty {
        "hello" => ChildMessage::Hello {
            protocol: v.get("protocol").and_then(Value::as_u64).unwrap_or(0),
            module_id: s("moduleId"),
            capability: s("capability"),
            ready: v.get("ready").and_then(Value::as_bool).unwrap_or(true),
            features: v
                .get("features")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|f| f.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        },
        "ready" => ChildMessage::Ready,
        "result" => ChildMessage::Result {
            id: v
                .get("id")
                .and_then(Value::as_u64)
                .ok_or(line.to_string())?,
            frame: match v.get("frame") {
                None => None,
                Some(Value::Null) => Some(None),
                Some(f) => Some(Some(
                    f.get("shm")
                        .and_then(|s| s.get("offset"))
                        .and_then(Value::as_u64)
                        .ok_or(line.to_string())?,
                )),
            },
            classification: v.get("classification").filter(|c| !c.is_null()).cloned(),
            value: v.get("value").filter(|c| !c.is_null()).cloned(),
            camera_id: v
                .get("frame")
                .and_then(|f| f.get("cameraId"))
                .and_then(Value::as_str)
                .map(str::to_string),
            sequence: v
                .get("frame")
                .and_then(|f| f.get("sequence"))
                .and_then(Value::as_u64),
            exhausted: v.get("exhausted").and_then(Value::as_bool).unwrap_or(false),
        },
        "error" => ChildMessage::Error {
            id: v.get("id").and_then(Value::as_u64),
            message: s("message"),
        },
        "state" => ChildMessage::State {
            id: v
                .get("id")
                .and_then(Value::as_u64)
                .ok_or(line.to_string())?,
            empty: v.get("empty").and_then(Value::as_bool).unwrap_or(false),
        },
        "restored" => ChildMessage::Restored {
            id: v
                .get("id")
                .and_then(Value::as_u64)
                .ok_or(line.to_string())?,
        },
        "log" => ChildMessage::Log {
            level: s("level"),
            message: s("message"),
        },
        other => ChildMessage::Other(other.to_string()),
    })
}

/// `execute` request. `out` = (offset, capacity) of a host-owned output slot.
pub fn execute(
    id: u64,
    camera_id: &str,
    sequence: u64,
    content_type: &str,
    offset: u64,
    out: Option<(u64, usize)>,
) -> String {
    let mut msg = json!({
        "type": "execute",
        "id": id,
        "frame": {
            "cameraId": camera_id,
            "sequence": sequence,
            "contentType": content_type,
            "shm": { "offset": offset },
        },
    });
    if let Some((offset, capacity)) = out {
        msg["out"] = json!({ "offset": offset, "capacity": capacity });
    }
    msg.to_string()
}

/// `execute` for a source (capability `source`, a v1 extension): only an output slot.
pub fn execute_source(id: u64, offset: u64, capacity: usize) -> String {
    json!({
        "type": "execute",
        "id": id,
        "out": { "offset": offset, "capacity": capacity },
    })
    .to_string()
}

/// `checkpoint` request: the child writes its state into the reserved slot.
pub fn checkpoint(id: u64, offset: u64, capacity: usize) -> String {
    json!({ "type": "checkpoint", "id": id, "out": { "offset": offset, "capacity": capacity } })
        .to_string()
}

/// `restore` request: the child rehydrates from the slot at `offset`.
pub fn restore(id: u64, offset: u64) -> String {
    json!({ "type": "restore", "id": id, "shm": { "offset": offset } }).to_string()
}

/// `shutdown` request.
pub fn shutdown() -> String {
    json!({ "type": "shutdown" }).to_string()
}
