//! Machine-local bindings: values a pipeline cannot compute, kept outside the portable package.
//!
//! Same store and resolution order as the .NET engine (`FileValueBindingStore`,
//! `EnvironmentValueResolver`, `BindingPrePass`): `literal` → the binding store
//! (`.mvf/bindings.json`, a flat JSON object) → the environment variable `MVF_BINDING_<KEY>` →
//! `default`. An environment answer is transient (never written to the store, so a later change of
//! the variable is not shadowed). The Rust host is unattended: there is no terminal prompt, so an
//! unresolved value fails the run before cycle 1 with the binding name and where to set it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mvf_graph::{ValueConfig, ValueType};
use serde_json::Value as Json;

/// Prefix of the environment variables that answer bindings.
pub const ENV_PREFIX: &str = "MVF_BINDING_";

/// Where a resolved value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValueSource {
    /// `literal` in config.
    Literal,
    /// The binding store.
    Store,
    /// An environment variable.
    Environment,
    /// `default` in config.
    Default,
}

/// The binding store plus the environment.
#[derive(Debug, Default)]
pub struct Bindings {
    path: Option<PathBuf>,
    values: Mutex<BTreeMap<String, Json>>,
    /// When false the environment is not consulted (tests).
    no_env: bool,
}

impl Bindings {
    /// Bindings backed by a file. A missing or unreadable file is an empty store (the first run on a
    /// machine has no bindings yet).
    pub fn file(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let values = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Map<String, Json>>(&t).ok())
            .map(|m| m.into_iter().collect())
            .unwrap_or_default();
        Self {
            path: Some(path),
            values: Mutex::new(values),
            no_env: false,
        }
    }

    /// An in-memory store (nothing persisted), optionally pre-filled.
    pub fn memory(values: impl IntoIterator<Item = (String, Json)>) -> Self {
        Self {
            path: None,
            values: Mutex::new(values.into_iter().collect()),
            no_env: false,
        }
    }

    /// Stops consulting environment variables.
    pub fn without_env(mut self) -> Self {
        self.no_env = true;
        self
    }

    /// The store file, if any.
    pub fn location(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The stored value for `key`.
    pub fn stored(&self, key: &str) -> Option<Json> {
        self.values.lock().ok().and_then(|v| v.get(key).cloned())
    }

    /// A snapshot of the store.
    pub fn snapshot(&self) -> BTreeMap<String, Json> {
        self.values.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// The raw environment answer for `key`.
    pub fn environment(&self, key: &str) -> Option<(String, String)> {
        if self.no_env {
            return None;
        }
        let name = env_name(key);
        std::env::var(&name).ok().map(|v| (name, v))
    }

    /// Stores `value` under `key` and persists the whole store (atomic replace).
    pub fn store(&self, key: &str, value: Json) -> Result<(), String> {
        let snapshot = {
            let mut values = self
                .values
                .lock()
                .map_err(|_| "binding store poisoned".to_string())?;
            values.insert(key.to_string(), value);
            values.clone()
        };
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(&snapshot).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Looks `key` up in the store, then the environment, checking the answer against `ty`.
    pub fn lookup(
        &self,
        key: &str,
        ty: ValueType,
        schema: Option<&Json>,
    ) -> Result<Option<(Json, ValueSource)>, String> {
        if let Some(stored) = self.stored(key) {
            ty.check(schema, &stored).map_err(|e| {
                format!(
                    "binding `{key}` in {} is not a valid {}: {e}",
                    self.describe_location(),
                    ty.describe()
                )
            })?;
            return Ok(Some((stored, ValueSource::Store)));
        }
        if let Some((name, text)) = self.environment(key) {
            let value = parse_env(ty, &text).ok_or_else(|| {
                format!(
                    "{name} is set to '{text}', which is not a valid {}",
                    ty.describe()
                )
            })?;
            ty.check(schema, &value)
                .map_err(|e| format!("{name}: {e}"))?;
            return Ok(Some((value, ValueSource::Environment)));
        }
        Ok(None)
    }

    /// Resolves a `value` node: literal → store → environment → default.
    pub fn resolve_value(
        &self,
        node: &str,
        config: &ValueConfig,
    ) -> Result<(Json, ValueSource), String> {
        let ty = config.value_type();
        let schema = config.schema.as_ref();
        if let Some(literal) = &config.literal {
            ty.check(schema, literal)
                .map_err(|e| format!("value `{node}`: literal: {e}"))?;
            return Ok((literal.clone(), ValueSource::Literal));
        }
        if let Some(key) = &config.binding
            && let Some(found) = self
                .lookup(key, ty, schema)
                .map_err(|e| format!("value `{node}`: {e}"))?
        {
            return Ok(found);
        }
        if let Some(default) = &config.default {
            ty.check(schema, default)
                .map_err(|e| format!("value `{node}`: default: {e}"))?;
            return Ok((default.clone(), ValueSource::Default));
        }
        Err(match &config.binding {
            Some(key) => format!(
                "value `{node}`: binding `{key}` has no value; store one in {} or set {} (this host never prompts)",
                self.describe_location(),
                env_name(key)
            ),
            None => format!("value `{node}` has no literal, binding or default"),
        })
    }

    fn describe_location(&self) -> String {
        self.path
            .as_deref()
            .map_or_else(|| "the binding store".into(), |p| p.display().to_string())
    }
}

/// `camera.address` → `MVF_BINDING_CAMERA_ADDRESS`.
pub fn env_name(key: &str) -> String {
    format!(
        "{ENV_PREFIX}{}",
        key.replace(['.', '-'], "_").to_ascii_uppercase()
    )
}

/// A string-typed value is the raw text; everything else is parsed as JSON.
fn parse_env(ty: ValueType, text: &str) -> Option<Json> {
    match (ty.shape, ty.elem) {
        (mvf_graph::Shape::One, mvf_graph::ElemType::String) => Some(Json::String(text.into())),
        _ => serde_json::from_str(text).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvf_graph::{ElemType, Shape};
    use serde_json::json;

    fn value(cfg: Json) -> ValueConfig {
        serde_json::from_value(cfg).unwrap()
    }

    #[test]
    fn literal_store_default_order_and_actionable_failure() {
        let b = Bindings::memory([("t".to_string(), json!(55))]).without_env();
        let lit = value(json!({"type": "int", "literal": 40, "binding": "t", "default": 1}));
        assert_eq!(
            b.resolve_value("n", &lit).unwrap(),
            (json!(40), ValueSource::Literal)
        );
        let stored = value(json!({"type": "int", "binding": "t", "default": 1}));
        assert_eq!(
            b.resolve_value("n", &stored).unwrap(),
            (json!(55), ValueSource::Store)
        );
        let fallback = value(json!({"type": "int", "binding": "other", "default": 1}));
        assert_eq!(
            b.resolve_value("n", &fallback).unwrap(),
            (json!(1), ValueSource::Default)
        );
        let none = value(json!({"type": "int", "binding": "camera.address"}));
        let err = b.resolve_value("n", &none).unwrap_err();
        assert!(
            err.contains("camera.address") && err.contains("MVF_BINDING_CAMERA_ADDRESS"),
            "{err}"
        );
    }

    #[test]
    fn a_stored_binding_of_the_wrong_type_is_rejected() {
        let b = Bindings::memory([("t".to_string(), json!("forty"))]).without_env();
        let cfg = value(json!({"type": "int", "binding": "t", "default": 1}));
        assert!(
            b.resolve_value("n", &cfg)
                .unwrap_err()
                .contains("not a valid value:int")
        );
    }

    #[test]
    fn the_store_persists_atomically_and_reloads() {
        let dir = std::env::temp_dir().join(format!("mvf-bindings-{}", std::process::id()));
        let path = dir.join(".mvf/bindings.json");
        let b = Bindings::file(&path);
        assert!(b.stored("k").is_none());
        b.store("k", json!({"serial": "A"})).unwrap();
        assert_eq!(
            Bindings::file(&path).stored("k"),
            Some(json!({"serial": "A"}))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn environment_values_are_typed_and_named_like_dotnet() {
        assert_eq!(env_name("camera.address"), "MVF_BINDING_CAMERA_ADDRESS");
        assert_eq!(env_name("a-b.c"), "MVF_BINDING_A_B_C");
        let ty = ValueType {
            elem: ElemType::Int,
            shape: Shape::One,
        };
        assert_eq!(parse_env(ty, "42"), Some(json!(42)));
        assert_eq!(
            parse_env(ValueType::one(ElemType::String), "gige"),
            Some(json!("gige"))
        );
    }
}
