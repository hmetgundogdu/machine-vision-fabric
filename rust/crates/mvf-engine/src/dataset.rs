//! `dataset-writer`: the Rust counterpart of the .NET `mvf.dataset-writer` sink, with the same
//! on-disk layout so outputs of the two engines can be compared file by file:
//!
//! ```text
//! {outputRoot}/{sessionPrefix}-{yyyyMMdd}-{HHmmss}-{ms}/
//!   images/   000001-{cameraId}-seq0001.bin
//!   metadata/ 000001-{cameraId}-seq0001.json
//!   session.json   (written when the node stops, if anything was written)
//! ```
//!
//! Differences: the session timestamp is UTC (the .NET writer uses local time), and frames carry no
//! file name or content type here, so the extension is always `.bin`.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::builtins::{HostServices, parse_config};
use crate::node::*;
use crate::value::Value;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DatasetWriterConfig {
    /// Output root, relative to the package directory unless absolute.
    output_root: String,
    /// Session directory prefix.
    #[serde(default = "session")]
    session_prefix: String,
    /// Accepted for config parity; frames carry no source path here.
    #[serde(default)]
    include_source_path_in_metadata: bool,
}

fn session() -> String {
    "session".into()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptureRecord {
    camera_id: String,
    sequence_number: u64,
    source_path: String,
    stored_image_path: String,
    metadata_path: String,
    captured_at_utc: String,
    content_type: String,
    content_length: usize,
}

/// Writes frames to a dataset session directory.
#[derive(Debug)]
pub struct DatasetWriter {
    root: PathBuf,
    prefix: String,
    session: Option<PathBuf>,
    global: u64,
    records: Vec<CaptureRecord>,
}

impl DatasetWriter {
    pub(crate) fn create(
        config: &serde_json::Map<String, serde_json::Value>,
        services: &HostServices,
    ) -> Result<Box<dyn NodeHandle>, NodeError> {
        let c: DatasetWriterConfig = parse_config(config)?;
        let _ = c.include_source_path_in_metadata;
        let root = PathBuf::from(&c.output_root);
        let root = if root.is_absolute() {
            root
        } else {
            services
                .package_dir
                .as_deref()
                .unwrap_or(std::path::Path::new("."))
                .join(root)
        };
        Ok(Box::new(Self {
            root,
            prefix: c.session_prefix,
            session: None,
            global: 0,
            records: Vec::new(),
        }))
    }

    fn io(e: std::io::Error, what: &std::path::Path) -> NodeError {
        NodeError::failed(format!("{}: {e}", what.display()))
    }

    fn flush(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        if self.records.is_empty() {
            return;
        }
        let summary = serde_json::json!({
            "sessionRoot": session.display().to_string(),
            "frameCount": self.records.len(),
            "finalizedAtUtc": utc_iso(SystemTime::now()),
            "records": self.records,
        });
        if let Ok(text) = serde_json::to_string_pretty(&summary) {
            let _ = std::fs::write(session.join("session.json"), text);
        }
    }
}

impl NodeHandle for DatasetWriter {
    fn start(&mut self, _: &LifecycleContext<'_>) -> Result<(), NodeError> {
        // Like the .NET sink: the session directory exists from activation on.
        let name = format!("{}-{}", self.prefix, session_stamp(SystemTime::now()));
        let session = self.root.join(name);
        for dir in ["images", "metadata"] {
            let d = session.join(dir);
            std::fs::create_dir_all(&d).map_err(|e| Self::io(e, &d))?;
        }
        self.session = Some(session);
        Ok(())
    }

    fn execute(&mut self, _: &ExecContext<'_>, inputs: &Inputs) -> Result<Execution, NodeError> {
        let Some(Value::Frame(frame)) = inputs.get("frame") else {
            return Err(NodeError::failed("missing required input `frame`"));
        };
        let Some(session) = self.session.clone() else {
            return Err(NodeError::failed("dataset-writer used before start"));
        };
        self.global += 1;
        let stem = format!(
            "{:06}-{}-seq{:04}",
            self.global, frame.meta.source, frame.meta.sequence
        );
        let image = session.join("images").join(format!("{stem}.bin"));
        let bytes = frame.payload.try_bytes().map_err(NodeError::failed)?;
        std::fs::write(&image, bytes).map_err(|e| Self::io(e, &image))?;
        let metadata = session.join("metadata").join(format!("{stem}.json"));
        let record = CaptureRecord {
            camera_id: frame.meta.source.clone(),
            sequence_number: frame.meta.sequence,
            source_path: String::new(),
            stored_image_path: image.display().to_string(),
            metadata_path: metadata.display().to_string(),
            captured_at_utc: utc_iso(SystemTime::now()),
            content_type: "application/octet-stream".into(),
            content_length: bytes.len(),
        };
        let text =
            serde_json::to_string_pretty(&record).map_err(|e| NodeError::failed(e.to_string()))?;
        std::fs::write(&metadata, text).map_err(|e| Self::io(e, &metadata))?;
        self.records.push(record);
        Ok(Execution::Produced(Outputs::new()))
    }

    fn drain(&mut self, _: &LifecycleContext<'_>) -> Result<(), NodeError> {
        self.flush();
        Ok(())
    }

    fn stop(&mut self, _: &LifecycleContext<'_>) -> StopOutcome {
        self.flush();
        self.session = None;
        StopOutcome::Graceful
    }
}

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn parts(t: SystemTime) -> (i64, u32, u32, u64, u64, u64, u32) {
    let since = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let (y, mo, d) = civil(i64::try_from(secs / 86_400).unwrap_or(0));
    let rem = secs % 86_400;
    (
        y,
        mo,
        d,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        since.subsec_micros(),
    )
}

/// `yyyyMMdd-HHmmss-fff` (UTC).
fn session_stamp(t: SystemTime) -> String {
    let (y, mo, d, h, mi, s, us) = parts(t);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}-{:03}", us / 1000)
}

/// RFC 3339 UTC with microseconds.
fn utc_iso(t: SystemTime) -> String {
    let (y, mo, d, h, mi, s, us) = parts(t);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{us:06}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamps_format_like_the_dotnet_writer() {
        let t = UNIX_EPOCH + Duration::from_millis(1_766_000_000_123);
        assert_eq!(session_stamp(t), "20251217-193320-123");
        assert_eq!(utc_iso(t), "2025-12-17T19:33:20.123000Z");
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }
}
