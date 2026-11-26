//! The generated OCI runtime spec is default-deny. Portable: runs on every host.

use std::collections::BTreeMap;
use std::path::Path;

use mvf_container::package::{contract_text, oci_contract};
use mvf_container::spec::*;
use mvf_contract::{DeviceAccess, NetworkPolicy, NodeContract, parse_contract};
use mvf_oci::ExecConfig;

fn contract(extra: &str) -> NodeContract {
    parse_contract(&format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: t.node, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [ {{ name: frame, channel: data, type: frame }} ]\n    outputs: [ {{ name: frame, channel: data, type: frame }} ]\n  artifacts: [ {{ backend: oci, image: t:1, digest: \"sha256:{}\" }} ]\n  isolation: {{ minimum: sandbox }}\n{extra}",
        "0".repeat(64)
    ))
    .expect("contract")
}

fn image() -> ExecConfig {
    ExecConfig {
        entrypoint: Some(vec!["/bin/app".into()]),
        cmd: Some(vec!["--flag".into()]),
        env: Some(vec!["PATH=/bin".into(), "A=1".into()]),
        working_dir: Some("/work".into()),
        user: Some("1000:1000".into()),
        labels: Some(BTreeMap::new()),
    }
}

fn spec(
    c: &NodeContract,
    devices: Vec<DeviceNode>,
    userns: Option<IdMapping>,
) -> serde_json::Value {
    runtime_spec(&SpecInput {
        rootfs: Path::new("/state/rootfs/abc"),
        arena: Path::new("/tmp/mvf-arena-1.bin"),
        image: &image(),
        contract: c,
        args: None,
        extra_env: vec!["X=y".into()],
        devices,
        userns,
        hostname: "mvf-node".into(),
    })
    .expect("spec")
}

#[test]
fn process_is_unprivileged_and_runs_the_image_entrypoint() {
    let s = spec(&contract(""), vec![], None);
    let p = &s["process"];
    assert_eq!(p["args"], serde_json::json!(["/bin/app", "--flag"]));
    assert_eq!(p["cwd"], "/work");
    assert_eq!(p["user"], serde_json::json!({"uid": 1000, "gid": 1000}));
    assert_eq!(p["noNewPrivileges"], true);
    for set in [
        "bounding",
        "effective",
        "inheritable",
        "permitted",
        "ambient",
    ] {
        assert_eq!(p["capabilities"][set], serde_json::json!([]), "{set}");
    }
    let env: Vec<&str> = p["env"]
        .as_array()
        .expect("env")
        .iter()
        .filter_map(|e| e.as_str())
        .collect();
    assert!(
        env.contains(&"A=1") && env.contains(&"X=y") && env.contains(&"MVF_ARENA_PATH=/mvf/arena")
    );
    assert_eq!(s["root"]["readonly"], true);
}

#[test]
fn every_namespace_including_network_is_new() {
    let s = spec(&contract(""), vec![], None);
    let ns: Vec<&str> = s["linux"]["namespaces"]
        .as_array()
        .expect("ns")
        .iter()
        .filter_map(|n| n["type"].as_str())
        .collect();
    for want in ["pid", "ipc", "uts", "mount", "cgroup", "network"] {
        assert!(ns.contains(&want), "missing {want} namespace");
    }
    assert!(!ns.contains(&"user"), "no userns unless requested");
    let with_userns = spec(
        &contract(""),
        vec![],
        Some(IdMapping {
            host_id: 100_000,
            size: 65_536,
        }),
    );
    assert_eq!(
        with_userns["linux"]["uidMappings"],
        serde_json::json!([{"containerID": 0, "hostID": 100000, "size": 65536}])
    );
}

#[test]
fn only_the_arena_is_mounted_from_the_host() {
    let s = spec(&contract(""), vec![], None);
    let binds: Vec<&serde_json::Value> = s["mounts"]
        .as_array()
        .expect("mounts")
        .iter()
        .filter(|m| m["type"] == "bind")
        .collect();
    assert_eq!(binds.len(), 1);
    assert_eq!(binds[0]["destination"], CONTAINER_ARENA);
    assert_eq!(binds[0]["source"], "/tmp/mvf-arena-1.bin");
    let opts: Vec<&str> = binds[0]["options"]
        .as_array()
        .expect("o")
        .iter()
        .filter_map(|o| o.as_str())
        .collect();
    assert!(opts.contains(&"noexec") && opts.contains(&"nosuid") && opts.contains(&"nodev"));
}

#[test]
fn seccomp_denies_kernel_and_namespace_escape_syscalls() {
    let s = spec(&contract(""), vec![], None);
    let rule = &s["linux"]["seccomp"]["syscalls"][0];
    assert_eq!(rule["action"], "SCMP_ACT_ERRNO");
    let names: Vec<&str> = rule["names"]
        .as_array()
        .expect("names")
        .iter()
        .filter_map(|n| n.as_str())
        .collect();
    for syscall in [
        "mount",
        "unshare",
        "setns",
        "bpf",
        "ptrace",
        "init_module",
        "kexec_load",
        "keyctl",
        "pivot_root",
    ] {
        assert!(names.contains(&syscall), "{syscall} not denied");
    }
}

#[test]
fn devices_are_denied_unless_granted() {
    let s = spec(&contract(""), vec![], None);
    assert_eq!(
        s["linux"]["resources"]["devices"],
        serde_json::json!([{"allow": false, "access": "rwm"}])
    );
    assert!(s["linux"].get("devices").is_none());

    let granted = spec(
        &contract(""),
        vec![DeviceNode {
            path: "/dev/video0".into(),
            kind: 'c',
            major: 81,
            minor: 0,
            access: DeviceAccess::R,
        }],
        None,
    );
    let rules = granted["linux"]["resources"]["devices"]
        .as_array()
        .expect("rules");
    assert_eq!(rules.len(), 2);
    assert_eq!(
        rules[1],
        serde_json::json!({"allow": true, "type": "c", "major": 81, "minor": 0, "access": "r"})
    );
    assert_eq!(granted["linux"]["devices"][0]["path"], "/dev/video0");
}

#[test]
fn resource_limits_map_to_cgroups() {
    let s = spec(
        &contract(
            "  resources: { memoryMib: 64, cpuMillis: 500, pids: 32, enforcement: required }\n",
        ),
        vec![],
        None,
    );
    let r = &s["linux"]["resources"];
    assert_eq!(r["memory"]["limit"], 64 * 1024 * 1024);
    assert_eq!(
        r["cpu"],
        serde_json::json!({"quota": 50000, "period": 100000})
    );
    assert_eq!(r["pids"]["limit"], 32);
}

#[test]
fn unsupported_network_policy_fails_closed() {
    let mut c = contract("");
    c.spec.permissions.network = NetworkPolicy::Loopback;
    assert!(
        runtime_spec(&SpecInput {
            rootfs: Path::new("/r"),
            arena: Path::new("/a"),
            image: &image(),
            contract: &c,
            args: None,
            extra_env: vec![],
            devices: vec![],
            userns: None,
            hostname: "h".into(),
        })
        .is_err()
    );
}

#[test]
fn named_users_resolve_from_the_image_rootfs() {
    let dir = std::env::temp_dir().join(format!("mvf-users-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("etc")).expect("dir");
    std::fs::write(
        dir.join("etc/passwd"),
        "root:x:0:0::/root:/bin/sh\napp:x:1234:2345::/app:/bin/sh\n",
    )
    .expect("passwd");
    std::fs::write(dir.join("etc/group"), "root:x:0:\nvideo:x:44:\n").expect("group");
    assert_eq!(resolve_user(Some("app"), &dir), Ok((1234, 2345)));
    assert_eq!(resolve_user(Some("app:video"), &dir), Ok((1234, 44)));
    assert_eq!(resolve_user(Some("77:88"), &dir), Ok((77, 88)));
    assert_eq!(resolve_user(None, &dir), Ok((0, 0)));
    assert!(resolve_user(Some("ghost"), &dir).is_err());
}

#[test]
fn generated_oci_contract_parses_with_the_strict_loader() {
    let module = parse_contract(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../modules/py-invert-transformer/node.yaml"),
        )
        .expect("module contract"),
    )
    .expect("valid");
    let digest = format!("sha256:{}", "a".repeat(64));
    let c = oci_contract(&module, "mvf/py-invert:1", &digest);
    let back = parse_contract(&contract_text(&c)).expect("JSON is valid strict YAML");
    assert_eq!(back, c);
    assert_eq!(
        back.spec.isolation.minimum,
        mvf_contract::IsolationLevel::Sandbox
    );
}
