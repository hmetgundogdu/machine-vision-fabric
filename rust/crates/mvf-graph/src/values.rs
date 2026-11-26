//! Typed values the graph cannot compute (`value`, `select`): element types, shapes, and the JSON
//! Schema subset the .NET engine enforces (`ControlValueType`, `JsonSchemaCheck`).
//!
//! A value has an **element type** (`string | int | number | bool | json`) and a **shape**
//! (`one | list`). Its port type in the contract is derived from both: scalars travel on `value`
//! ports, JSON and lists on `json` ports. Between two primitives the graph checks the refined type
//! as well, so `select.criterion` must carry the element type of `select.items` — the same rule as
//! `pipeline.node.select-type-mismatch` in the .NET validator.

use mvf_contract::PortType;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// Element type of a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ElemType {
    /// A JSON string.
    String,
    /// An integral number.
    Int,
    /// Any number.
    Number,
    /// `true` / `false`.
    Bool,
    /// Any non-null JSON (optionally constrained by a schema).
    #[default]
    Json,
}

impl ElemType {
    /// Stable lower-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            ElemType::String => "string",
            ElemType::Int => "int",
            ElemType::Number => "number",
            ElemType::Bool => "bool",
            ElemType::Json => "json",
        }
    }

    /// Does `value` have this element type?
    pub fn matches(self, value: &Json) -> bool {
        match self {
            ElemType::String => value.is_string(),
            ElemType::Int => value.is_i64() || value.is_u64(),
            ElemType::Number => value.is_number(),
            ElemType::Bool => value.is_boolean(),
            ElemType::Json => !value.is_null(),
        }
    }
}

/// One value, or a homogeneous collection of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Shape {
    /// A single value.
    #[default]
    One,
    /// A JSON array whose every element has the element type.
    List,
}

/// The refined type of a primitive's value port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueType {
    /// Element type.
    pub elem: ElemType,
    /// Shape.
    pub shape: Shape,
}

impl ValueType {
    /// A single value of `elem`.
    pub fn one(elem: ElemType) -> Self {
        Self {
            elem,
            shape: Shape::One,
        }
    }

    /// A list of `elem`.
    pub fn list(elem: ElemType) -> Self {
        Self {
            elem,
            shape: Shape::List,
        }
    }

    /// The contract port type carrying this value: scalars on `value`, JSON and lists on `json`.
    pub fn port_type(self) -> PortType {
        match (self.shape, self.elem) {
            (Shape::List, _) | (Shape::One, ElemType::Json) => PortType::Json,
            (Shape::One, _) => PortType::Value,
        }
    }

    /// `value:int`, `list:json`, … (the .NET `control/value:<t>` / `control/list:<t>` spelling).
    pub fn describe(self) -> String {
        let shape = match self.shape {
            Shape::One => "value",
            Shape::List => "list",
        };
        format!("{shape}:{}", self.elem.as_str())
    }

    /// Checks type and shape, then the schema (per element for a list). The one entry point for a
    /// literal, a stored binding, an environment value and a live edit, as in .NET.
    pub fn check(self, schema: Option<&Json>, value: &Json) -> Result<(), String> {
        match self.shape {
            Shape::One => {
                if !self.elem.matches(value) {
                    return Err(format!(
                        "$: expected {} but found {}",
                        self.elem.as_str(),
                        kind_of(value)
                    ));
                }
                validate(schema, value, "$")
            }
            Shape::List => {
                let Some(items) = value.as_array() else {
                    return Err(format!(
                        "$: expected a list of {} but found {}",
                        self.elem.as_str(),
                        kind_of(value)
                    ));
                };
                for (i, item) in items.iter().enumerate() {
                    if !self.elem.matches(item) {
                        return Err(format!(
                            "$[{i}]: expected {} but found {} (a list is homogeneous)",
                            self.elem.as_str(),
                            kind_of(item)
                        ));
                    }
                    validate(schema, item, &format!("$[{i}]"))?;
                }
                Ok(())
            }
        }
    }
}

const KNOWN_TYPES: [&str; 7] = [
    "object", "array", "string", "number", "integer", "boolean", "null",
];

/// Checks that `schema` is usable: an object or boolean whose enforced keywords are well formed.
/// Unknown keywords are ignored (as JSON Schema requires of a validator), so `$ref`/`allOf` are
/// accepted and under-enforced, never rejected — the .NET trade-off.
pub fn validate_schema(schema: &Json) -> Result<(), String> {
    if schema.is_boolean() {
        return Ok(());
    }
    let Some(obj) = schema.as_object() else {
        return Err("a schema must be a JSON object (or a boolean)".into());
    };
    if let Some(types) = obj.get("type").filter(|t| !t.is_null()) {
        let declared: Vec<Option<&str>> = match types.as_array() {
            Some(a) => a.iter().map(Json::as_str).collect(),
            None => vec![types.as_str()],
        };
        for t in declared {
            if !t.is_some_and(|t| KNOWN_TYPES.contains(&t)) {
                return Err(format!(
                    "'type' must be one of {} but was '{}'",
                    KNOWN_TYPES.join(", "),
                    t.unwrap_or("null")
                ));
            }
        }
    }
    if obj
        .get("required")
        .is_some_and(|r| !r.is_null() && !r.is_array())
    {
        return Err("'required' must be an array of property names".into());
    }
    if obj
        .get("enum")
        .is_some_and(|e| !e.is_null() && !e.is_array())
    {
        return Err("'enum' must be an array".into());
    }
    if let Some(pattern) = obj.get("pattern").filter(|p| !p.is_null()) {
        let Some(text) = pattern.as_str() else {
            return Err("'pattern' must be a string".into());
        };
        regex::Regex::new(text)
            .map_err(|e| format!("'pattern' is not a valid regular expression: {e}"))?;
    }
    if let Some(properties) = obj.get("properties").filter(|p| !p.is_null()) {
        let Some(properties) = properties.as_object() else {
            return Err("'properties' must be an object".into());
        };
        for (name, sub) in properties {
            validate_schema(sub).map_err(|e| format!("property '{name}': {e}"))?;
        }
    }
    if let Some(items) = obj.get("items").filter(|i| !i.is_null()) {
        validate_schema(items).map_err(|e| format!("'items': {e}"))?;
    }
    Ok(())
}

/// Validates `value` against `schema` (the enforced subset), reporting the first violation with a
/// JSON path (`$.serial`, `$[1]`).
pub fn validate(schema: Option<&Json>, value: &Json, path: &str) -> Result<(), String> {
    let Some(schema) = schema else {
        return Ok(());
    };
    if let Some(accepts) = schema.as_bool() {
        return if accepts {
            Ok(())
        } else {
            Err(format!("{path}: schema is 'false', which accepts nothing"))
        };
    }
    let Some(obj) = schema.as_object() else {
        return Ok(());
    };
    let kind = kind_of(value);
    if let Some(types) = obj.get("type").filter(|t| !t.is_null()) {
        let declared: Vec<&str> = match types.as_array() {
            Some(a) => a.iter().filter_map(Json::as_str).collect(),
            None => types.as_str().into_iter().collect(),
        };
        if !declared.iter().any(|t| matches_type(t, kind, value)) {
            return Err(format!(
                "{path}: expected {} but found {kind}",
                declared.join(" or ")
            ));
        }
    }
    if let Some(constant) = obj.get("const")
        && constant != value
    {
        return Err(format!("{path}: must be {constant}"));
    }
    if let Some(allowed) = obj.get("enum").and_then(Json::as_array)
        && !allowed.contains(value)
    {
        let names: Vec<String> = allowed.iter().map(ToString::to_string).collect();
        return Err(format!("{path}: must be one of {}", names.join(", ")));
    }
    match value {
        Json::Object(o) => validate_object(obj, o, path),
        Json::Array(a) => validate_array(obj, a, path),
        Json::String(s) => validate_string(obj, s, path),
        Json::Number(_) => validate_number(obj, value, path),
        _ => Ok(()),
    }
}

fn validate_object(
    schema: &serde_json::Map<String, Json>,
    value: &serde_json::Map<String, Json>,
    path: &str,
) -> Result<(), String> {
    if let Some(required) = schema.get("required").and_then(Json::as_array) {
        for name in required.iter().filter_map(Json::as_str) {
            if !value.contains_key(name) {
                return Err(format!("{path}: missing required property '{name}'"));
            }
        }
    }
    let properties = schema.get("properties").and_then(Json::as_object);
    if let Some(properties) = properties {
        for (name, sub) in properties {
            if let Some(v) = value.get(name) {
                validate(Some(sub), v, &format!("{path}.{name}"))?;
            }
        }
    }
    if let Some(additional) = schema.get("additionalProperties").filter(|a| !a.is_null()) {
        let forbidden = additional.as_bool() == Some(false);
        for (name, v) in value {
            if properties.is_some_and(|p| p.contains_key(name)) {
                continue;
            }
            if forbidden {
                return Err(format!("{path}: property '{name}' is not allowed"));
            }
            if additional.is_object() {
                validate(Some(additional), v, &format!("{path}.{name}"))?;
            }
        }
    }
    Ok(())
}

fn validate_array(
    schema: &serde_json::Map<String, Json>,
    value: &[Json],
    path: &str,
) -> Result<(), String> {
    let count = value.len() as u64;
    if let Some(min) = schema.get("minItems").and_then(Json::as_u64)
        && count < min
    {
        return Err(format!(
            "{path}: needs at least {min} item(s) but has {count}"
        ));
    }
    if let Some(max) = schema.get("maxItems").and_then(Json::as_u64)
        && count > max
    {
        return Err(format!(
            "{path}: allows at most {max} item(s) but has {count}"
        ));
    }
    if let Some(items) = schema.get("items").filter(|i| !i.is_null()) {
        for (i, v) in value.iter().enumerate() {
            validate(Some(items), v, &format!("{path}[{i}]"))?;
        }
    }
    Ok(())
}

fn validate_string(
    schema: &serde_json::Map<String, Json>,
    value: &str,
    path: &str,
) -> Result<(), String> {
    // .NET counts UTF-16 code units; JSON Schema counts code points. Code points here.
    let length = value.chars().count() as u64;
    if let Some(min) = schema.get("minLength").and_then(Json::as_u64)
        && length < min
    {
        return Err(format!("{path}: must be at least {min} character(s)"));
    }
    if let Some(max) = schema.get("maxLength").and_then(Json::as_u64)
        && length > max
    {
        return Err(format!("{path}: must be at most {max} character(s)"));
    }
    if let Some(pattern) = schema.get("pattern").and_then(Json::as_str) {
        // An uncompilable pattern was rejected by `validate_schema`; treat it as no constraint here.
        if let Ok(re) = regex::Regex::new(pattern)
            && !re.is_match(value)
        {
            return Err(format!("{path}: does not match pattern '{pattern}'"));
        }
    }
    Ok(())
}

fn validate_number(
    schema: &serde_json::Map<String, Json>,
    value: &Json,
    path: &str,
) -> Result<(), String> {
    let Some(n) = value.as_f64() else {
        return Ok(());
    };
    let bound = |key: &str| schema.get(key).and_then(Json::as_f64);
    if let Some(min) = bound("minimum")
        && n < min
    {
        return Err(format!("{path}: must be >= {min}"));
    }
    if let Some(max) = bound("maximum")
        && n > max
    {
        return Err(format!("{path}: must be <= {max}"));
    }
    if let Some(min) = bound("exclusiveMinimum")
        && n <= min
    {
        return Err(format!("{path}: must be > {min}"));
    }
    if let Some(max) = bound("exclusiveMaximum")
        && n >= max
    {
        return Err(format!("{path}: must be < {max}"));
    }
    Ok(())
}

fn matches_type(declared: &str, actual: &str, value: &Json) -> bool {
    match declared {
        "integer" => actual == "integer" || (actual == "number" && is_integral(value)),
        "number" => actual == "number" || actual == "integer",
        other => other == actual,
    }
}

fn is_integral(value: &Json) -> bool {
    value
        .as_f64()
        .is_some_and(|f| f.fract() == 0.0 && f.is_finite())
}

/// JSON Schema kind of a value (`integer` for whole numbers).
pub fn kind_of(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "boolean",
        Json::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Json::Number(_) => "number",
        Json::String(_) => "string",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scalar_types_follow_the_dotnet_rules() {
        assert!(ElemType::Int.matches(&json!(40)));
        assert!(!ElemType::Int.matches(&json!(40.5)));
        assert!(ElemType::Number.matches(&json!(40)));
        assert!(!ElemType::String.matches(&json!(1)));
        assert!(!ElemType::Json.matches(&Json::Null));
        assert_eq!(ValueType::one(ElemType::Int).port_type(), PortType::Value);
        assert_eq!(ValueType::list(ElemType::Int).port_type(), PortType::Json);
        assert_eq!(ValueType::one(ElemType::Json).port_type(), PortType::Json);
    }

    #[test]
    fn list_schemas_apply_to_each_element_with_an_index_path() {
        let schema = json!({ "type": "object", "required": ["serial", "protocol"] });
        let list = ValueType::list(ElemType::Json);
        assert!(
            list.check(Some(&schema), &json!([{"serial": "A", "protocol": "gige"}]))
                .is_ok()
        );
        let err = list
            .check(
                Some(&schema),
                &json!([{"serial": "A", "protocol": "gige"}, {"serial": "B"}]),
            )
            .unwrap_err();
        assert_eq!(err, "$[1]: missing required property 'protocol'");
        assert!(
            list.check(None, &json!([1, "two"])).is_ok(),
            "json elements may differ in shape"
        );
        let err = ValueType::list(ElemType::Int)
            .check(None, &json!([1, "two"]))
            .unwrap_err();
        assert!(err.starts_with("$[1]: expected int"), "{err}");
    }

    #[test]
    fn numeric_bounds_enum_const_pattern_and_lengths() {
        let int = ValueType::one(ElemType::Int);
        let bounds = json!({ "type": "integer", "minimum": 0, "maximum": 255 });
        assert!(int.check(Some(&bounds), &json!(40)).is_ok());
        assert_eq!(
            int.check(Some(&bounds), &json!(300)).unwrap_err(),
            "$: must be <= 255"
        );
        let s = ValueType::one(ElemType::String);
        let schema = json!({ "enum": ["gige", "usb"], "pattern": "^[a-z]+$", "maxLength": 4 });
        assert!(s.check(Some(&schema), &json!("gige")).is_ok());
        assert!(s.check(Some(&schema), &json!("GigE")).is_err());
        let schema = json!({ "pattern": "^[a-z]+$", "maxLength": 4 });
        assert_eq!(
            s.check(Some(&schema), &json!("abcde")).unwrap_err(),
            "$: must be at most 4 character(s)"
        );
        assert!(s.check(Some(&json!({"const": "x"})), &json!("y")).is_err());
        assert!(validate(Some(&json!(false)), &json!(1), "$").is_err());
    }

    #[test]
    fn objects_check_properties_and_additional_properties() {
        let schema = json!({
            "type": "object",
            "properties": { "port": { "type": "integer", "minimum": 1 } },
            "additionalProperties": false
        });
        let v = ValueType::one(ElemType::Json);
        assert!(v.check(Some(&schema), &json!({"port": 5})).is_ok());
        assert_eq!(
            v.check(Some(&schema), &json!({"port": 0})).unwrap_err(),
            "$.port: must be >= 1"
        );
        assert_eq!(
            v.check(Some(&schema), &json!({"port": 5, "x": 1}))
                .unwrap_err(),
            "$: property 'x' is not allowed"
        );
    }

    #[test]
    fn malformed_schemas_are_rejected_but_unknown_keywords_are_ignored() {
        assert!(validate_schema(&json!({"type": "decimal"})).is_err());
        assert!(validate_schema(&json!({"required": "serial"})).is_err());
        assert!(validate_schema(&json!({"pattern": "("})).is_err());
        assert!(validate_schema(&json!("object")).is_err());
        assert!(validate_schema(&json!({"$ref": "#/x", "allOf": []})).is_ok());
        assert!(validate_schema(&json!(true)).is_ok());
    }
}
