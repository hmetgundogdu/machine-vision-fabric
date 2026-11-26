//! Strict, bounded YAML loader shared by every MVF YAML document (node contracts, pipelines).
//!
//! MVF manifests are configuration, not a programming language, so the loader accepts a deliberately
//! small, predictable subset of YAML 1.2 and rejects everything else **before** any typed
//! deserialization happens:
//!
//! - exactly one document; no directives beyond the implicit/explicit document start;
//! - no anchors, aliases or explicit tags (`&a`, `*a`, `!!python/object`, …) — removes alias-bomb
//!   expansion and type-confusion attacks, and keeps every value visible where it is used;
//! - mapping keys must be scalars and are always strings; a duplicate key is an error, never
//!   "last one wins";
//! - bounded input size, nesting depth, event count and scalar length;
//! - plain scalars are resolved with the YAML 1.2 *core* schema subset (`null`/`~`/empty, `true`/
//!   `false`, decimal integers, finite decimal floats); quoted and block scalars are always strings.
//!   Non-finite floats (`.inf`, `.nan`), octal/hex integers and YAML 1.1 booleans (`yes`/`on`) are
//!   kept as strings, so a typed field that expects a number rejects them instead of guessing.
//!
//! The result is a [`serde_json::Value`] tree that the typed models deserialize with
//! `deny_unknown_fields`, through `serde_path_to_error` so every error carries a field path.

use saphyr_parser::{Event, Parser, ScalarStyle};
use serde::de::DeserializeOwned;
use serde_json::{Map, Number, Value};

/// Resource bounds applied while parsing. The defaults are generous for hand-written manifests and
/// small for an attacker.
#[derive(Debug, Clone, Copy)]
pub struct YamlLimits {
    /// Maximum input size in bytes.
    pub max_bytes: usize,
    /// Maximum nesting depth of mappings/sequences.
    pub max_depth: usize,
    /// Maximum number of parser events (≈ nodes) in the document.
    pub max_events: usize,
    /// Maximum length of a single scalar, in bytes.
    pub max_scalar_bytes: usize,
}

impl Default for YamlLimits {
    fn default() -> Self {
        Self {
            max_bytes: 256 * 1024,
            max_depth: 32,
            max_events: 20_000,
            max_scalar_bytes: 64 * 1024,
        }
    }
}

/// A YAML loading failure with a stable machine-readable code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}{}", location.map(|(l, c)| format!(" (line {l}, column {c})")).unwrap_or_default())]
pub struct YamlError {
    /// Stable error code, e.g. `yaml.duplicate-key`.
    pub code: &'static str,
    /// Human readable explanation.
    pub message: String,
    /// 1-based line and column, when known.
    pub location: Option<(usize, usize)>,
    /// Field path (`spec.ports.inputs[0].type`) for typed-deserialization errors.
    pub path: Option<String>,
}

impl YamlError {
    fn at(code: &'static str, message: impl Into<String>, line: usize, col: usize) -> Self {
        Self {
            code,
            message: message.into(),
            location: Some((line, col + 1)),
            path: None,
        }
    }
}

enum Frame {
    Seq(Vec<Value>),
    Map {
        map: Map<String, Value>,
        pending_key: Option<String>,
    },
}

/// Parses `input` into a JSON value tree under `limits`, enforcing every rule in the module docs.
pub fn parse_value(input: &str, limits: &YamlLimits) -> Result<Value, YamlError> {
    if input.len() > limits.max_bytes {
        return Err(YamlError {
            code: "yaml.too-large",
            message: format!(
                "document is {} bytes; the limit is {}",
                input.len(),
                limits.max_bytes
            ),
            location: None,
            path: None,
        });
    }

    let mut stack: Vec<Frame> = Vec::new();
    let mut root: Option<Value> = None;
    let mut documents = 0usize;
    let mut events = 0usize;

    for next in Parser::new_from_str(input) {
        let (event, span) = next.map_err(|e| YamlError {
            code: "yaml.syntax",
            message: e.info().to_string(),
            location: Some((e.marker().line(), e.marker().col() + 1)),
            path: None,
        })?;
        let (line, col) = (span.start.line(), span.start.col());

        events += 1;
        if events > limits.max_events {
            return Err(YamlError::at(
                "yaml.too-many-nodes",
                format!("document exceeds {} nodes", limits.max_events),
                line,
                col,
            ));
        }

        let completed: Option<Value> = match event {
            Event::StreamStart | Event::StreamEnd | Event::Nothing | Event::DocumentEnd => None,
            Event::DocumentStart(_) => {
                documents += 1;
                if documents > 1 {
                    return Err(YamlError::at(
                        "yaml.multiple-documents",
                        "exactly one YAML document is allowed",
                        line,
                        col,
                    ));
                }
                None
            }
            Event::Alias(_) => {
                return Err(YamlError::at(
                    "yaml.alias-forbidden",
                    "aliases (*name) are not allowed; write the value out explicitly",
                    line,
                    col,
                ));
            }
            Event::Scalar(text, style, anchor, tag) => {
                reject_anchor_or_tag(anchor, tag.is_some(), line, col)?;
                if text.len() > limits.max_scalar_bytes {
                    return Err(YamlError::at(
                        "yaml.scalar-too-long",
                        format!("scalar exceeds {} bytes", limits.max_scalar_bytes),
                        line,
                        col,
                    ));
                }
                Some(resolve_scalar(&text, style))
            }
            Event::SequenceStart(anchor, tag) => {
                open_collection(&stack, limits, anchor, tag.is_some(), line, col)?;
                stack.push(Frame::Seq(Vec::new()));
                None
            }
            Event::MappingStart(anchor, tag) => {
                open_collection(&stack, limits, anchor, tag.is_some(), line, col)?;
                stack.push(Frame::Map {
                    map: Map::new(),
                    pending_key: None,
                });
                None
            }
            Event::SequenceEnd | Event::MappingEnd => match stack.pop() {
                Some(Frame::Seq(items)) => Some(Value::Array(items)),
                Some(Frame::Map { map, .. }) => Some(Value::Object(map)),
                None => None,
            },
        };

        if let Some(value) = completed {
            match stack.last_mut() {
                None => root = Some(value),
                Some(Frame::Seq(items)) => items.push(value),
                Some(Frame::Map { map, pending_key }) => match pending_key.take() {
                    None => {
                        let key = match value {
                            Value::String(s) => s,
                            Value::Null => String::new(),
                            other => other.to_string(),
                        };
                        if map.contains_key(&key) {
                            return Err(YamlError::at(
                                "yaml.duplicate-key",
                                format!("duplicate mapping key `{key}`"),
                                line,
                                col,
                            ));
                        }
                        *pending_key = Some(key);
                    }
                    Some(key) => {
                        map.insert(key, value);
                    }
                },
            }
        }
    }

    root.ok_or(YamlError {
        code: "yaml.empty",
        message: "document is empty".to_string(),
        location: None,
        path: None,
    })
}

fn open_collection(
    stack: &[Frame],
    limits: &YamlLimits,
    anchor: usize,
    tagged: bool,
    line: usize,
    col: usize,
) -> Result<(), YamlError> {
    reject_anchor_or_tag(anchor, tagged, line, col)?;
    if stack.len() >= limits.max_depth {
        return Err(YamlError::at(
            "yaml.too-deep",
            format!("nesting exceeds a depth of {}", limits.max_depth),
            line,
            col,
        ));
    }
    // Mapping keys must be scalars: a collection arriving while a map waits for its key is a
    // complex key (`? [a, b] : c`).
    if let Some(Frame::Map {
        pending_key: None, ..
    }) = stack.last()
    {
        return Err(YamlError::at(
            "yaml.complex-key",
            "mapping keys must be plain strings",
            line,
            col,
        ));
    }
    Ok(())
}

fn reject_anchor_or_tag(
    anchor: usize,
    tagged: bool,
    line: usize,
    col: usize,
) -> Result<(), YamlError> {
    if anchor != 0 {
        return Err(YamlError::at(
            "yaml.anchor-forbidden",
            "anchors (&name) are not allowed",
            line,
            col,
        ));
    }
    if tagged {
        return Err(YamlError::at(
            "yaml.tag-forbidden",
            "explicit tags (!tag) are not allowed",
            line,
            col,
        ));
    }
    Ok(())
}

/// YAML 1.2 core-schema subset for plain scalars; everything quoted or block-styled is a string.
fn resolve_scalar(text: &str, style: ScalarStyle) -> Value {
    if style != ScalarStyle::Plain {
        return Value::String(text.to_string());
    }
    match text {
        "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    if is_decimal_integer(text) {
        if let Ok(i) = text.parse::<i64>() {
            return Value::Number(i.into());
        }
        if let Ok(u) = text.parse::<u64>() {
            return Value::Number(u.into());
        }
        return Value::String(text.to_string());
    }
    if is_decimal_float(text)
        && let Some(n) = text.parse::<f64>().ok().and_then(Number::from_f64)
    {
        return Value::Number(n);
    }
    Value::String(text.to_string())
}

fn is_decimal_integer(text: &str) -> bool {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

fn is_decimal_float(text: &str) -> bool {
    // [-+]? ( \.[0-9]+ | [0-9]+ ( \. [0-9]* )? ) ( [eE] [-+]? [0-9]+ )?
    let s = text.strip_prefix(['-', '+']).unwrap_or(text);
    let (mantissa, exponent) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((int, frac)) => {
            !(int.is_empty() && frac.is_empty())
                && int.bytes().all(|b| b.is_ascii_digit())
                && frac.bytes().all(|b| b.is_ascii_digit())
        }
        None => !mantissa.is_empty() && mantissa.bytes().all(|b| b.is_ascii_digit()),
    };
    let exponent_ok = exponent.is_none_or(|e| {
        let d = e.strip_prefix(['-', '+']).unwrap_or(e);
        !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
    });
    mantissa_ok && exponent_ok && (mantissa.contains('.') || exponent.is_some())
}

/// Parses and deserializes a strict YAML document into `T`, reporting the failing field path.
pub fn from_str<T: DeserializeOwned>(input: &str, limits: &YamlLimits) -> Result<T, YamlError> {
    let value = parse_value(input, limits)?;
    serde_path_to_error::deserialize(value).map_err(|e| {
        let path = e.path().to_string();
        YamlError {
            code: "yaml.schema",
            message: e.into_inner().to_string(),
            location: None,
            path: Some(path),
        }
    })
}
