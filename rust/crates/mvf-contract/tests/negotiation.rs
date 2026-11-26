//! Capability negotiation: pick the first satisfying candidate, reject with reasons, never weaken.

use std::fs;
use std::path::Path;

use mvf_contract::*;

fn load(name: &str) -> NodeContract {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/valid")
        .join(name);
    parse_contract(&fs::read_to_string(path).expect("read")).expect("valid fixture")
}

fn in_process() -> BackendCapabilities {
    BackendCapabilities {
        kind: BackendKind::InProcess,
        isolation: IsolationLevel::None,
        targets: vec![Target::Cpu],
        zero_copy_domains: vec![MemoryDomain::Host],
        cancellation: Cancellation::Cooperative,
        enforceable_termination: false,
        enforces_memory_limit: false,
        enforces_cpu_limit: false,
        enforces_pids_limit: false,
        replacement: vec![Replacement::Stateless, Replacement::Stateful],
        grants: Default::default(),
        private_arenas: false,
    }
}

fn worker() -> BackendCapabilities {
    BackendCapabilities {
        kind: BackendKind::Worker,
        isolation: IsolationLevel::Process,
        targets: vec![Target::Cpu],
        zero_copy_domains: vec![MemoryDomain::Host],
        cancellation: Cancellation::Enforceable,
        enforceable_termination: true,
        enforces_memory_limit: false,
        enforces_cpu_limit: false,
        enforces_pids_limit: false,
        replacement: vec![Replacement::Stateless],
        grants: Default::default(),
        private_arenas: false,
    }
}

fn compute() -> BackendCapabilities {
    BackendCapabilities {
        kind: BackendKind::Compute,
        isolation: IsolationLevel::None,
        targets: vec![Target::Gpu],
        zero_copy_domains: vec![MemoryDomain::Device],
        cancellation: Cancellation::Cooperative,
        enforceable_termination: false,
        enforces_memory_limit: false,
        enforces_cpu_limit: false,
        enforces_pids_limit: false,
        replacement: vec![Replacement::Stateless],
        grants: Default::default(),
        private_arenas: false,
    }
}

#[test]
fn trusted_builtin_lands_in_process_with_zero_copy() {
    let p = negotiate(&load("in-process-invert.yaml"), &[worker(), in_process()]).expect("placed");
    assert_eq!(p.backend, BackendKind::InProcess);
    assert_eq!(p.target, Target::Cpu);
    assert!(p.zero_copy);
    assert!(p.notes.is_empty());
}

#[test]
fn worker_classifier_requires_process_isolation_and_notes_advisory_limits() {
    let contract = load("python-worker-classifier.yaml");
    let p = negotiate(&contract, &[in_process(), worker()]).expect("placed");
    assert_eq!(p.backend, BackendKind::Worker);
    assert!(p.zero_copy);
    assert_eq!(p.notes.len(), 1);
    assert_eq!(p.notes[0].code, "placement.advisory-limits");
}

#[test]
fn missing_backend_is_a_structured_rejection() {
    let err = negotiate(&load("python-worker-classifier.yaml"), &[in_process()]).unwrap_err();
    assert_eq!(err.node, "py.brightness-classifier");
    assert_eq!(err.rejections.len(), 1);
    assert_eq!(
        err.rejections[0].reasons[0].code,
        "negotiation.backend-unavailable"
    );
}

#[test]
fn sandbox_requirement_fails_closed_without_a_sandbox_backend() {
    let mut fake_oci = worker();
    fake_oci.kind = BackendKind::Oci; // a process backend posing as OCI is still only `process`
    let err = negotiate(&load("oci-sandboxed-detector.yaml"), &[fake_oci]).unwrap_err();
    let codes: Vec<_> = err.rejections[0].reasons.iter().map(|r| r.code).collect();
    assert!(codes.contains(&"negotiation.isolation-insufficient"));
    assert!(codes.contains(&"negotiation.resource-limit-unenforceable"));
}

#[test]
fn enforcing_sandbox_backend_is_accepted() {
    let oci = BackendCapabilities {
        kind: BackendKind::Oci,
        isolation: IsolationLevel::Sandbox,
        targets: vec![Target::Cpu],
        zero_copy_domains: vec![MemoryDomain::Host],
        cancellation: Cancellation::Enforceable,
        enforceable_termination: true,
        enforces_memory_limit: true,
        enforces_cpu_limit: true,
        enforces_pids_limit: true,
        replacement: vec![Replacement::Stateless],
        grants: GrantSupport {
            network: vec![NetworkPolicy::None],
            devices: vec![],
            cdi_devices: vec![],
            lsm: vec![],
        },
        private_arenas: false,
    };
    let p = negotiate(&load("oci-sandboxed-detector.yaml"), &[oci]).expect("placed");
    assert_eq!(p.backend, BackendKind::Oci);
    assert!(p.notes.is_empty());
}

#[test]
fn gpu_node_prefers_device_and_falls_back_to_cpu_only_when_allowed() {
    let contract = load("gpu-compute-with-fallback.yaml");

    let on_gpu = negotiate(&contract, &[compute(), in_process()]).expect("gpu");
    assert_eq!(on_gpu.backend, BackendKind::Compute);
    assert_eq!(on_gpu.target, Target::Gpu);
    assert!(on_gpu.zero_copy);

    let on_cpu = negotiate(&contract, &[in_process()]).expect("fallback");
    assert_eq!(on_cpu.backend, BackendKind::InProcess);
    assert_eq!(on_cpu.target, Target::Cpu);
    assert_eq!(on_cpu.notes[0].code, "placement.fallback-to-cpu");

    let mut strict = contract.clone();
    strict.spec.execution.placement.fallback = Fallback::Deny;
    let err = negotiate(&strict, &[in_process()]).unwrap_err();
    assert_eq!(
        err.rejections[0].reasons[0].code,
        "negotiation.backend-unavailable"
    );
}

#[test]
fn required_zero_copy_rejects_a_copying_backend() {
    let contract = load("python-worker-classifier.yaml");
    let mut copying = worker();
    copying.zero_copy_domains.clear();
    let err = negotiate(&contract, &[copying]).unwrap_err();
    assert_eq!(
        err.rejections[0].reasons[0].code,
        "negotiation.zero-copy-unavailable"
    );
}

#[test]
fn stateful_replacement_requires_backend_support() {
    let contract = load("stateful-counter.yaml");
    let mut stateless_only = in_process();
    stateless_only.replacement = vec![Replacement::Stateless];
    let err = negotiate(&contract, &[stateless_only]).unwrap_err();
    assert_eq!(
        err.rejections[0].reasons[0].code,
        "negotiation.replacement-unsupported"
    );
    assert!(negotiate(&contract, &[in_process()]).is_ok());
}

#[test]
fn pinned_backend_is_never_substituted() {
    let mut contract = load("gpu-compute-with-fallback.yaml");
    contract.spec.execution.backend = BackendPreference::Compute;
    contract.spec.execution.placement.fallback = Fallback::Deny;
    // The in-process artifact would satisfy CPU, but the compute backend is pinned.
    let err = negotiate(&contract, &[in_process()]).unwrap_err();
    assert!(
        err.rejections
            .iter()
            .all(|r| r.backend == BackendKind::Compute)
    );
}

fn sandbox(grants: GrantSupport) -> BackendCapabilities {
    BackendCapabilities {
        kind: BackendKind::Oci,
        isolation: IsolationLevel::Sandbox,
        targets: vec![Target::Cpu],
        zero_copy_domains: vec![MemoryDomain::Host],
        cancellation: Cancellation::Enforceable,
        enforceable_termination: true,
        enforces_memory_limit: true,
        enforces_cpu_limit: true,
        enforces_pids_limit: true,
        replacement: vec![Replacement::Stateless],
        grants,
        private_arenas: false,
    }
}

#[test]
fn sandbox_permissions_are_default_deny_and_unavailable_grants_fail_closed() {
    let mut contract = load("oci-sandboxed-detector.yaml");
    let none = GrantSupport {
        network: vec![NetworkPolicy::None],
        devices: vec!["/dev/video0".into()],
        cdi_devices: vec![],
        lsm: vec![],
    };
    assert!(
        negotiate(&contract, &[sandbox(none.clone())]).is_ok(),
        "no grants requested"
    );

    contract.spec.permissions.network = NetworkPolicy::Loopback;
    contract.spec.permissions.devices = vec![
        DeviceGrant {
            path: "/dev/video0".into(),
            access: DeviceAccess::R,
        },
        DeviceGrant {
            path: "/dev/ttyUSB0".into(),
            access: DeviceAccess::Rw,
        },
    ];
    contract.spec.permissions.cdi_devices = vec!["nvidia.com/gpu=0".into()];
    let err = negotiate(&contract, &[sandbox(none)]).unwrap_err();
    let codes: Vec<_> = err.rejections[0].reasons.iter().map(|r| r.code).collect();
    assert_eq!(
        codes,
        [
            "negotiation.permission-unsupported",
            "negotiation.device-unavailable",
            "negotiation.cdi-device-unavailable"
        ]
    );
}

#[test]
fn permissions_do_not_apply_to_non_restricting_backends() {
    let mut contract = load("python-worker-classifier.yaml");
    contract.spec.permissions.devices = vec![DeviceGrant {
        path: "/dev/video0".into(),
        access: DeviceAccess::Rw,
    }];
    assert!(negotiate(&contract, &[worker()]).is_ok());
}

fn contract_with(spec_extra: &str, backend: &str) -> Result<NodeContract, ContractError> {
    let artifact = match backend {
        "worker" => "{ backend: worker, runtime: python, entry: m.py, protocol: 1 }",
        _ => "{ backend: in-process, builtin: m }",
    };
    parse_contract(&format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: test.m, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [ {{ name: frame, channel: data, type: frame }} ]\n    outputs: [ {{ name: class, channel: data, type: classification }} ]\n  artifacts: [ {artifact} ]\n{spec_extra}"
    ))
}

#[test]
fn a_private_arena_is_negotiated_fail_closed() {
    let private = contract_with("  memory: { arena: private }\n", "in-process").expect("valid");
    let err = negotiate(&private, &[in_process()]).expect_err("in-process cannot isolate memory");
    assert!(
        err.rejections
            .iter()
            .flat_map(|r| r.reasons.iter())
            .any(|r| r.code == "negotiation.private-arena-unsupported")
    );
    let worker = contract_with("  memory: { arena: private }\n", "worker").expect("valid");
    let caps = BackendCapabilities {
        kind: BackendKind::Worker,
        isolation: IsolationLevel::Process,
        private_arenas: true,
        ..in_process()
    };
    assert!(negotiate(&worker, &[caps]).is_ok());
}

#[test]
fn contradictory_memory_and_parallelism_requests_are_invalid() {
    let codes = |extra: &str| match contract_with(extra, "worker") {
        Ok(_) => Vec::new(),
        Err(ContractError::Invalid(issues)) => issues.iter().map(|i| i.code).collect(),
        Err(e) => panic!("{e}"),
    };
    assert_eq!(
        codes("  memory: { arena: private, zeroCopy: required }\n"),
        ["contract.memory.private-arena-copies"]
    );
    assert_eq!(
        codes(
            "  execution: { maxParallelism: 4 }\n  lifecycle: { replacement: stateful, state: { schemaVersion: 1 } }\n"
        ),
        ["contract.execution.stateful-parallelism"]
    );
    assert_eq!(
        codes("  execution: { maxParallelism: 0 }\n"),
        ["contract.execution.invalid-max-parallelism"]
    );
    assert!(
        codes("  execution: { maxParallelism: 4 }\n  lifecycle: { replacement: stateless }\n")
            .is_empty()
    );
}
