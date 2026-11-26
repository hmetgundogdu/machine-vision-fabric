//! MVF worker protocol v1 (control plane): newline-delimited JSON over the child's stdio.
//! Specified in `protocol/README.md`; payload bytes never travel here, only `shm` offsets.

use serde_json::{Value, json};

/// The only protocol version.
pub const PROTOCOL_VERSION: u64 = 1;

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
    },
    /// Per-request failure.
    Error {
        /// Request id, when the failure belongs to one.
        id: Option<u64>,
        /// Message.
        message: String,
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
        },
        "error" => ChildMessage::Error {
            id: v.get("id").and_then(Value::as_u64),
            message: s("message"),
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

/// `shutdown` request.
pub fn shutdown() -> String {
    json!({ "type": "shutdown" }).to_string()
}
