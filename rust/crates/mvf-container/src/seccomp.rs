//! Seccomp profiles for sandboxed nodes.
//!
//! - **Denylist** (default): allow everything except [`crate::spec::DENIED_SYSCALLS`].
//! - **Allowlist** (`isolation.syscalls: allowlist`, or the host forces it): deny everything
//!   (`EPERM`) except the syscalls an unprivileged container needs. The list is Docker's default
//!   profile, vendored unchanged from moby `docker-v29.10.1`
//!   (`vendor/github.com/moby/profiles/seccomp/default.json`, git blob `6e5e47e2`, Apache-2.0).
//!   It is specialised here for a container with **no capabilities** on the host architecture:
//!   capability-gated entries are dropped and capability-excluded ones kept, entries for other
//!   architectures are dropped, argument filters and errno overrides are kept, and every name on the
//!   denylist is removed from the allowed set. The result is the stricter of the two policies.

use serde_json::{Value, json};

use crate::spec::DENIED_SYSCALLS;

/// The vendored Docker default profile.
const MOBY_DEFAULT: &str = include_str!("seccomp/moby-default.json");

/// Seccomp architecture names the filter applies to (as in the denylist profile).
const ARCHITECTURES: [&str; 4] = [
    "SCMP_ARCH_X86_64",
    "SCMP_ARCH_X86",
    "SCMP_ARCH_AARCH64",
    "SCMP_ARCH_ARM",
];

/// Docker's name for the architecture this binary runs on.
pub fn moby_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "x86",
        "arm" => "arm",
        "s390x" => "s390x",
        "powerpc64" => "ppc64le",
        "riscv64" => "riscv64",
        other => other,
    }
}

/// The denylist profile.
pub fn denylist() -> Value {
    json!({
        "defaultAction": "SCMP_ACT_ALLOW",
        "architectures": ARCHITECTURES,
        "syscalls": [{ "names": DENIED_SYSCALLS, "action": "SCMP_ACT_ERRNO", "errnoRet": 1 }],
    })
}

/// The allowlist profile for `arch` (a Docker architecture name, see [`moby_arch`]).
pub fn allowlist(arch: &str) -> Result<Value, String> {
    let profile: Value =
        serde_json::from_str(MOBY_DEFAULT).map_err(|e| format!("vendored profile: {e}"))?;
    let entries = profile["syscalls"]
        .as_array()
        .ok_or("vendored profile has no `syscalls`")?;
    let has = |v: &Value, key: &str, item: &str| {
        v.get(key)
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(item)))
    };
    let nonempty = |v: &Value, key: &str| {
        v.get(key)
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
    };
    let mut syscalls = Vec::new();
    for entry in entries {
        let includes = entry.get("includes").cloned().unwrap_or(Value::Null);
        let excludes = entry.get("excludes").cloned().unwrap_or(Value::Null);
        if nonempty(&includes, "caps") {
            continue; // needs a capability this container never has
        }
        if nonempty(&includes, "arches") && !has(&includes, "arches", arch) {
            continue;
        }
        if has(&excludes, "arches", arch) {
            continue;
        }
        let action = entry["action"].as_str().unwrap_or("SCMP_ACT_ERRNO");
        let names: Vec<&str> = entry["names"]
            .as_array()
            .map(|n| n.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|n| action != "SCMP_ACT_ALLOW" || !DENIED_SYSCALLS.contains(n))
            .collect();
        if names.is_empty() {
            continue;
        }
        let mut rule = json!({ "names": names, "action": action });
        for key in ["args", "errnoRet"] {
            if let Some(v) = entry.get(key) {
                rule[key] = v.clone();
            }
        }
        syscalls.push(rule);
    }
    Ok(json!({
        "defaultAction": "SCMP_ACT_ERRNO",
        "defaultErrnoRet": 1,
        "architectures": ARCHITECTURES,
        "syscalls": syscalls,
    }))
}

/// Every syscall name the allowlist allows (unconditionally or with argument filters).
pub fn allowed_names(profile: &Value) -> Vec<String> {
    let mut names: Vec<String> = profile["syscalls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["action"] == "SCMP_ACT_ALLOW")
        .flat_map(|r| r["names"].as_array().cloned().unwrap_or_default())
        .filter_map(|n| n.as_str().map(str::to_string))
        .collect();
    names.sort();
    names.dedup();
    names
}
