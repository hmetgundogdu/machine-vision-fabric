//! `mvf-rs import` (#34): converts a legacy .NET pipeline package (`pipeline.json`) into a Rust
//! pipeline (`pipeline.yaml`), written next to it so relative paths keep their meaning, and then
//! validates it on this host.
//!
//! The conversion is mechanical and refuses what it cannot carry over instead of dropping it:
//! - **Ids** become valid contract ids (`saveInverted` → `save-inverted`); edges follow.
//! - **Modules** with a Rust builtin twin become that builtin (`mvf.folder-source` →
//!   `folder-source`, with the `sim-cam-1` camera id the .NET simulator stamps). Other modules stay
//!   `module: <id>` and must have a `node.yaml` contract in a module root; the report names every
//!   one that does not (a .NET in-process integration runs only as a worker under Rust).
//! - **Edges:** `a.port -> b.port` is a data or control edge by the ports' channel. `a -> b` without
//!   ports must close into a `loop`.
//! - **Failure policy:** `run.onError: { sources: fail, nodes: skip }`, which is what the .NET
//!   engine does (a failing source ends the run, a failing mid-graph node is skipped). The Rust
//!   default is to fail on any node.
//! - **Dropped on purpose:** `version` and `displayName` have no place in the Rust schema; both go
//!   into the header comment.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use mvf_contract::Channel;
use mvf_graph::ContractResolver;
use serde_json::{Map, Value, json};

/// Legacy modules the Rust host provides as builtins.
const BUILTINS: &[(&str, &str)] = &[
    ("mvf.folder-source", "folder-source"),
    ("mvf.dataset-writer", "dataset-writer"),
];

const PRIMITIVES: &[&str] = &["fork", "if", "switch", "select", "value", "loop"];

/// What the conversion changed, and what still needs attention.
#[derive(Debug, Default)]
pub struct Report {
    /// (legacy id, Rust id) for every id that had to change.
    pub renamed: Vec<(String, String)>,
    /// (node, legacy module, builtin).
    pub builtins: Vec<(String, String, String)>,
    /// Modules with no contract in the module roots: (node, module, hint).
    pub unresolved: Vec<(String, String, String)>,
    /// Other notes, one line each.
    pub notes: Vec<String>,
}

/// A contract id for a legacy node id: lowercase, words split at capitals, anything else a `-`.
pub fn rust_id(id: &str) -> String {
    let mut out = String::new();
    let mut prev_lower_or_digit = false;
    for c in id.chars() {
        if c.is_ascii_uppercase() {
            if prev_lower_or_digit {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower_or_digit = false;
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            prev_lower_or_digit = true;
        } else {
            out.push(if matches!(c, '.' | '_') { c } else { '-' });
            prev_lower_or_digit = false;
        }
    }
    // No doubled or edge separators, and a letter first.
    let mut clean = String::new();
    for c in out.chars() {
        let sep = matches!(c, '.' | '-' | '_');
        if sep && (clean.is_empty() || clean.ends_with(['.', '-', '_'])) {
            continue;
        }
        clean.push(c);
    }
    while clean.ends_with(['.', '-', '_']) {
        clean.pop();
    }
    if !clean.starts_with(|c: char| c.is_ascii_lowercase()) {
        clean.insert_str(0, "n-");
    }
    clean
}

fn object<'a>(v: &'a Value, what: &str) -> Result<&'a Map<String, Value>, String> {
    v.as_object()
        .ok_or_else(|| format!("{what} must be a JSON object"))
}

fn only(m: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<(), String> {
    match m.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!(
            "{what}: field `{k}` has no Rust equivalent this importer knows (allowed: {})",
            allowed.join(", ")
        )),
        None => Ok(()),
    }
}

/// The channel of `port` on a node, if its contract says (`None`: assume data).
fn channel(
    node: &Map<String, Value>,
    port: &str,
    input: bool,
    resolver: &dyn ContractResolver,
) -> Option<Channel> {
    if node.get("primitive").and_then(Value::as_str) == Some("if") {
        return (input && port == "productPresent").then_some(Channel::Control);
    }
    let contract = if let Some(b) = node.get("builtin").and_then(Value::as_str) {
        resolver.builtin(b)?
    } else {
        resolver
            .module(node.get("module").and_then(Value::as_str)?)
            .ok()?
            .1
    };
    let ports = if input {
        &contract.spec.ports.inputs
    } else {
        &contract.spec.ports.outputs
    };
    ports.iter().find(|p| p.name == port).map(|p| p.channel)
}

/// Converts a legacy `pipeline.json` document. Returns the Rust document and the report.
pub fn convert(
    legacy: &Value,
    resolver: &dyn ContractResolver,
) -> Result<(Value, Report, Vec<String>), String> {
    let top = object(legacy, "pipeline.json")?;
    only(
        top,
        &["name", "version", "description", "nodes", "edges"],
        "pipeline.json",
    )?;
    let mut report = Report::default();
    let mut header = Vec::new();
    let name = top
        .get("name")
        .and_then(Value::as_str)
        .ok_or("pipeline.json: `name` is required")?;
    if let Some(v) = top.get("version").and_then(Value::as_str) {
        header.push(format!("Legacy pipeline version: {v}"));
    }

    // Nodes.
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut nodes: Vec<Map<String, Value>> = Vec::new();
    for (i, n) in top
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or("pipeline.json: `nodes` must be an array")?
        .iter()
        .enumerate()
    {
        let n = object(n, &format!("nodes[{i}]"))?;
        let legacy_id = n
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("nodes[{i}]: `id` is required"))?;
        let what = format!("node `{legacy_id}`");
        only(
            n,
            &[
                "id",
                "primitive",
                "module",
                "displayName",
                "config",
                "bindings",
            ],
            &what,
        )?;
        let id = rust_id(legacy_id);
        if !mvf_contract::validate::is_valid_id(&id) {
            return Err(format!(
                "{what}: no valid Rust id could be made (got `{id}`)"
            ));
        }
        if !seen.insert(id.clone()) {
            return Err(format!(
                "{what}: becomes `{id}`, which another node already uses; rename one of them"
            ));
        }
        if id != legacy_id {
            report.renamed.push((legacy_id.to_string(), id.clone()));
        }
        ids.insert(legacy_id.to_string(), id.clone());

        let mut out = Map::new();
        out.insert("id".into(), json!(id));
        match (
            n.get("primitive").and_then(Value::as_str),
            n.get("module").and_then(Value::as_str),
        ) {
            (Some(p), None) => {
                if !PRIMITIVES.contains(&p) {
                    return Err(format!("{what}: unknown primitive `{p}`"));
                }
                out.insert("primitive".into(), json!(p));
            }
            (None, Some(m)) => {
                if let Some((_, builtin)) = BUILTINS.iter().find(|(legacy, _)| *legacy == m) {
                    out.insert("builtin".into(), json!(builtin));
                    report
                        .builtins
                        .push((id.clone(), m.to_string(), builtin.to_string()));
                } else {
                    out.insert("module".into(), json!(m));
                    if let Err(e) = resolver.module(m) {
                        let hint = if m.starts_with("mvf.") {
                            "a .NET in-process integration: under the Rust runtime .NET code runs as a worker (Mvf.Worker SDK) with a node.yaml contract"
                        } else {
                            "add its node.yaml contract to a module root (--modules)"
                        };
                        report.unresolved.push((
                            id.clone(),
                            m.to_string(),
                            format!("{hint} ({e})"),
                        ));
                    }
                }
            }
            _ => {
                return Err(format!(
                    "{what}: exactly one of `primitive` or `module` is required"
                ));
            }
        }
        let mut config = match n.get("config") {
            None | Some(Value::Null) => Map::new(),
            Some(c) => object(c, &format!("{what} config"))?.clone(),
        };
        if out.get("builtin").and_then(Value::as_str) == Some("folder-source")
            && !config.contains_key("cameraId")
        {
            config.insert("cameraId".into(), json!("sim-cam-1"));
            report.notes.push(format!(
                "{id}: cameraId `sim-cam-1` added (the .NET folder source stamps it; Rust would use the node id)"
            ));
        }
        if !config.is_empty() {
            out.insert("config".into(), Value::Object(config));
        }
        if let Some(b) = n.get("bindings").filter(|b| !b.is_null()) {
            out.insert("bindings".into(), b.clone());
        }
        if let Some(d) = n.get("displayName").and_then(Value::as_str) {
            header.push(format!("{id}: {}", d.replace(['\n', '\r'], " ")));
        }
        nodes.push(out);
    }
    let by_id: BTreeMap<&str, &Map<String, Value>> = nodes
        .iter()
        .map(|n| (n["id"].as_str().unwrap_or_default(), n))
        .collect();

    // Edges.
    let (mut data, mut control, mut loops) = (Vec::new(), Vec::new(), Vec::new());
    let endpoint = |s: &str, what: &str| -> Result<(String, Option<String>), String> {
        let (node, port) = match s.split_once('.') {
            Some((n, p)) => (n, Some(p.to_string())),
            None => (s, None),
        };
        let id = ids
            .get(node)
            .ok_or_else(|| format!("{what}: no node `{node}`"))?;
        Ok((id.clone(), port))
    };
    for (i, e) in top
        .get("edges")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let e = object(e, &format!("edges[{i}]"))?;
        only(e, &["from", "to"], &format!("edges[{i}]"))?;
        let get = |k: &str| {
            e.get(k)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("edges[{i}]: `{k}` is required"))
        };
        let (from, to) = (get("from")?, get("to")?);
        let what = format!("edge {from} -> {to}");
        let (fnode, fport) = endpoint(from, &what)?;
        let (tnode, tport) = endpoint(to, &what)?;
        match (fport, tport) {
            (Some(fp), Some(tp)) => {
                let ch = channel(by_id[tnode.as_str()], &tp, true, resolver)
                    .or_else(|| channel(by_id[fnode.as_str()], &fp, false, resolver));
                let edge = json!({"from": format!("{fnode}.{fp}"), "to": format!("{tnode}.{tp}")});
                if ch == Some(Channel::Control) {
                    control.push(edge);
                } else {
                    data.push(edge);
                }
            }
            (None, None) => {
                if by_id[tnode.as_str()]
                    .get("primitive")
                    .and_then(Value::as_str)
                    != Some("loop")
                {
                    return Err(format!(
                        "{what}: an edge without ports must close into a `loop` node in the Rust runtime"
                    ));
                }
                loops.push(json!({"from": fnode, "to": tnode}));
            }
            _ => {
                return Err(format!(
                    "{what}: both ends need a port, or neither (an edge into a `loop`)"
                ));
            }
        }
    }

    let mut metadata = Map::new();
    metadata.insert("name".into(), json!(name));
    if let Some(d) = top.get("description").and_then(Value::as_str) {
        metadata.insert("description".into(), json!(d));
    }
    let mut edges = Map::new();
    edges.insert("data".into(), Value::Array(data));
    if !control.is_empty() {
        edges.insert("control".into(), Value::Array(control));
    }
    if !loops.is_empty() {
        edges.insert("loop".into(), Value::Array(loops));
    }
    let doc = json!({
        "apiVersion": mvf_graph::PIPELINE_API_VERSION,
        "kind": mvf_graph::PIPELINE_KIND,
        "metadata": metadata,
        "spec": {
            "nodes": nodes,
            "edges": edges,
            "run": {"onError": {"sources": "fail", "nodes": "skip"}},
        },
    });
    Ok((doc, report, header))
}

// ---- a small YAML writer: block structure, JSON-quoted scalars (valid YAML 1.2) ----

fn key(k: &str) -> String {
    let plain = k.starts_with(|c: char| c.is_ascii_alphabetic())
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && !matches!(
            k.to_ascii_lowercase().as_str(),
            "null" | "true" | "false" | "yes" | "no" | "on" | "off" | "y" | "n"
        );
    if plain {
        k.to_string()
    } else {
        Value::String(k.to_string()).to_string()
    }
}

/// Keys in reading order (a pipeline's `id` first, `nodes` before `edges`); others alphabetical.
const ORDER: &[&str] = &[
    "apiVersion",
    "kind",
    "metadata",
    "spec",
    "name",
    "description",
    "id",
    "primitive",
    "builtin",
    "module",
    "config",
    "bindings",
    "sources",
    "nodes",
    "edges",
    "run",
    "data",
    "control",
    "loop",
    "from",
    "to",
];

fn ordered(m: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut entries: Vec<_> = m.iter().collect();
    entries.sort_by_key(|(k, _)| {
        (
            ORDER.iter().position(|o| o == k).unwrap_or(ORDER.len()),
            k.to_string(),
        )
    });
    entries
}

fn flow(v: &Value) -> String {
    match v {
        Value::Object(m) if m.is_empty() => "{}".into(),
        Value::Object(m) => format!(
            "{{ {} }}",
            ordered(m)
                .into_iter()
                .map(|(k, v)| format!("{}: {}", key(k), flow(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Array(a) => format!("[{}]", a.iter().map(flow).collect::<Vec<_>>().join(", ")),
        scalar => scalar.to_string(),
    }
}

fn inline(v: &Value) -> bool {
    !matches!(v, Value::Object(_) | Value::Array(_)) || flow(v).len() <= 72
}

fn map(m: &Map<String, Value>, indent: usize, first_inline: bool, out: &mut String) {
    for (i, (k, v)) in ordered(m).into_iter().enumerate() {
        if !(first_inline && i == 0) {
            out.push_str(&" ".repeat(indent));
        }
        out.push_str(&key(k));
        if inline(v) {
            out.push_str(": ");
            out.push_str(&flow(v));
            out.push('\n');
        } else {
            out.push_str(":\n");
            block(v, indent + 2, out);
        }
    }
}

fn block(v: &Value, indent: usize, out: &mut String) {
    match v {
        Value::Object(m) => map(m, indent, false, out),
        Value::Array(a) => {
            for item in a {
                out.push_str(&" ".repeat(indent));
                out.push_str("- ");
                match item {
                    Value::Object(m) if !inline(item) => map(m, indent + 2, true, out),
                    _ => {
                        out.push_str(&flow(item));
                        out.push('\n');
                    }
                }
            }
        }
        scalar => {
            out.push_str(&" ".repeat(indent));
            out.push_str(&scalar.to_string());
            out.push('\n');
        }
    }
}

/// The YAML text of a converted document, with a header comment.
pub fn yaml(doc: &Value, source: &Path, header: &[String]) -> String {
    let mut out = format!(
        "# Converted from {} by `mvf-rs import` (MVF {}, issue #34).\n",
        source.display(),
        env!("CARGO_PKG_VERSION")
    );
    out.push_str("# run.onError reproduces the .NET engine: a failing source ends the run, a failing\n# mid-graph node is skipped (the Rust default is to fail on any node).\n");
    for line in header {
        out.push_str(&format!("# {line}\n"));
    }
    if let Value::Object(m) = doc {
        map(m, 0, false, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_ids_become_contract_ids() {
        for (legacy, rust) in [
            ("saveInverted", "save-inverted"),
            ("cam", "cam"),
            ("camera1", "camera1"),
            ("pickOne", "pick-one"),
            ("Camera 2", "camera-2"),
            ("HTTPSource", "httpsource"),
            ("2nd", "n-2nd"),
            ("a__b", "a_b"),
            ("x.", "x"),
        ] {
            assert_eq!(rust_id(legacy), rust, "{legacy}");
            assert!(mvf_contract::validate::is_valid_id(rust), "{rust}");
        }
    }

    /// Whatever a legacy config holds comes back unchanged through the YAML the importer writes.
    #[test]
    fn the_yaml_writer_round_trips_awkward_values() {
        let config = json!({
            "true": "null",
            "on": "yes",
            "a: b": "#not a comment",
            "path": "C:\\frames\\cam 1",
            "multi": "line one\nline two\t\"quoted\"",
            "unicode": "kamera görüntüsü ✓",
            "empty": "",
            "list": [1, -2.5, true, null, "x", {"k": [{}]}],
            "nested": {"deep": {"deeper": {"long": "x".repeat(120)}}},
            "n": 0,
            "big": 18446744073709551615u64,
        });
        let doc = json!({
            "apiVersion": mvf_graph::PIPELINE_API_VERSION,
            "kind": mvf_graph::PIPELINE_KIND,
            "metadata": {"name": "round-trip", "description": "desc: with # and \"quotes\""},
            "spec": {
                "nodes": [{"id": "cam", "builtin": "folder-source", "config": config}],
                "edges": {"data": []},
            },
        });
        let text = yaml(&doc, Path::new("pipeline.json"), &["a: header line".into()]);
        let parsed = mvf_graph::parse_document(&text).expect("valid YAML pipeline");
        assert_eq!(
            Value::Object(parsed.spec.nodes[0].config.clone().expect("config")),
            config,
            "{text}"
        );
        assert_eq!(
            parsed.metadata.description.as_deref(),
            Some("desc: with # and \"quotes\"")
        );
    }
}
