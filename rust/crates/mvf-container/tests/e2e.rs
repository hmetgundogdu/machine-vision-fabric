//! Real OCI execution through a low-level runtime (runc/crun) on Linux — no Docker daemon.
//!
//! Needs: Linux, root (for cgroup limits and the user namespace), `runc` or `crun`, and
//! `MVF_OCI_FIXTURES` with `busybox.tar` and `python.tar` (`docker save` output, prepared once).
//! Enabled by `MVF_OCI_E2E=1`; `MVF_REQUIRE_OCI=1` turns a missing prerequisite into a failure.
//! `rust/scripts/oci-testhost.sh` runs this suite in a privileged Debian container that has
//! **no Python and no Docker daemon** inside.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use mvf_container::package::{contract_text, oci_contract, python_node_image};
use mvf_container::{OciBackend, OciRuntime, WorkloadOutput, list_containers, run_workload};
use mvf_contract::{NodeContract, parse_contract};
use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_oci::{ContentStore, ImageRecord, Platform};
use mvf_worker::WorkerBackend;

struct Env {
    backend: OciBackend,
    store: ContentStore,
    busybox: ImageRecord,
    python: ImageRecord,
    work: PathBuf,
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn env() -> Option<&'static Env> {
    static ENV: OnceLock<Option<Env>> = OnceLock::new();
    ENV.get_or_init(|| {
        let required = std::env::var_os("MVF_REQUIRE_OCI").is_some();
        let skip = |why: &str| {
            assert!(!required, "MVF_REQUIRE_OCI is set but {why}");
            eprintln!("NOT RUN: {why}");
            None
        };
        if std::env::var_os("MVF_OCI_E2E").is_none() {
            return skip("MVF_OCI_E2E is not set");
        }
        let Some(fixtures) = std::env::var_os("MVF_OCI_FIXTURES").map(PathBuf::from) else {
            return skip("MVF_OCI_FIXTURES is not set");
        };
        let Some(runtime) = OciRuntime::detect() else {
            return skip("no runc/crun on PATH");
        };
        let work = std::env::temp_dir().join(format!("mvf-oci-e2e-{}", std::process::id()));
        let store = ContentStore::open(work.join("store")).expect("store");
        let platform = Platform::host_linux();
        let busybox = mvf_oci::import(
            &store,
            &fixtures.join("busybox.tar"),
            &platform,
            Some("busybox:e2e"),
        )
        .expect("busybox");
        let python = mvf_oci::import(
            &store,
            &fixtures.join("python.tar"),
            &platform,
            Some("python:e2e"),
        )
        .expect("python");
        let backend = OciBackend::new(runtime, store.clone(), work.join("state"));
        eprintln!(
            "runtime: {} {:?}",
            backend.runtime().version,
            backend.runtime()
        );
        Some(Env {
            backend,
            store,
            busybox,
            python,
            work,
        })
    })
    .as_ref()
}

macro_rules! need_env {
    () => {
        match env() {
            Some(e) => e,
            None => return,
        }
    };
}

fn sandbox_contract(extra: &str) -> NodeContract {
    parse_contract(&format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: e2e.workload, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [ {{ name: frame, channel: data, type: frame }} ]\n  artifacts: [ {{ backend: oci, image: busybox, digest: \"sha256:{}\" }} ]\n  isolation: {{ minimum: sandbox }}\n{extra}",
        "0".repeat(64)
    ))
    .expect("contract")
}

fn sh(e: &Env, script: &str, extra: &str) -> WorkloadOutput {
    run_workload(
        &e.backend,
        &e.busybox,
        &sandbox_contract(extra),
        Some(vec!["sh".into(), "-c".into(), script.into()]),
        Duration::from_secs(30),
    )
    .expect("workload")
}

#[test]
fn host_has_no_python_and_no_docker_daemon() {
    let _ = need_env!();
    if std::env::var_os("MVF_REQUIRE_NO_HOST_RUNTIME").is_some() {
        for name in ["python3", "python", "dockerd", "containerd"] {
            let found = std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
                .unwrap_or(false);
            assert!(!found, "{name} is installed on the test host");
        }
        assert!(!Path::new("/var/run/docker.sock").exists());
    }
}

#[test]
fn image_entrypoint_and_command_run_as_a_generic_workload() {
    let e = need_env!();
    let out = run_workload(
        &e.backend,
        &e.busybox,
        &sandbox_contract(""),
        Some(vec!["echo".into(), "hello from busybox".into()]),
        Duration::from_secs(30),
    )
    .expect("run");
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(out.stdout, "hello from busybox\n");
    // The image's own default command (busybox: `sh`) with stdin closed exits cleanly.
    let default = run_workload(
        &e.backend,
        &e.busybox,
        &sandbox_contract(""),
        None,
        Duration::from_secs(30),
    )
    .expect("run");
    assert_eq!(default.exit_code, Some(0), "{default:?}");
    assert!(
        list_containers(&e.backend).is_empty(),
        "containers left behind"
    );
}

#[test]
fn sandbox_drops_privileges_and_isolates_the_process() {
    let e = need_env!();
    let out = sh(
        e,
        "grep -E '^(CapEff|CapBnd|NoNewPrivs|Seccomp):' /proc/self/status; echo uid_map: $(cat /proc/self/uid_map); echo pid1: $(cat /proc/1/comm); echo net: $(ls /sys/class/net); echo routes: $(tail -n +2 /proc/net/route | wc -l); touch /bin/nope 2>/dev/null && echo rootfs:writable || echo rootfs:readonly; touch /tmp/ok && echo tmp:writable; ls /dev",
        "",
    );
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    let s = &out.stdout;
    assert!(s.contains("CapEff:\t0000000000000000"), "{s}");
    assert!(s.contains("CapBnd:\t0000000000000000"), "{s}");
    assert!(s.contains("NoNewPrivs:\t1"), "{s}");
    assert!(s.contains("Seccomp:\t2"), "seccomp filter active: {s}");
    // No real interface (fallback tunnel devices some kernels create per namespace are down and
    // unrouted) and no route: the network namespace is empty.
    let nets = s
        .lines()
        .find_map(|l| l.strip_prefix("net: "))
        .unwrap_or_default();
    assert!(
        nets.split_whitespace().all(|n| [
            "lo",
            "bonding_masters",
            "tunl0",
            "sit0",
            "ip6tnl0",
            "ip_vti0",
            "ip6_vti0",
            "gre0",
            "gretap0",
            "erspan0",
            "ip6gre0"
        ]
        .contains(&n)),
        "unexpected interface: {nets}"
    );
    assert!(s.contains("routes: 0"), "routing table must be empty: {s}");
    assert!(
        s.contains("rootfs:readonly") && s.contains("tmp:writable"),
        "{s}"
    );
    assert!(s.contains("pid1: sh"), "own pid namespace: {s}");
    if e.backend.runtime().root {
        assert!(s.contains("uid_map: 0 100000 65536"), "user namespace: {s}");
    }
    for forbidden in ["sda", "vda", "nvme", "mem\n", "kmsg"] {
        assert!(!s.contains(forbidden), "/dev/{forbidden} visible: {s}");
    }
}

#[test]
fn network_and_denied_syscalls_fail() {
    let e = need_env!();
    let out = sh(
        e,
        "wget -q -T 3 -O- http://1.1.1.1 >/dev/null 2>&1; echo wget=$?; mkdir -p /tmp/m; mount -t tmpfs none /tmp/m 2>/dev/null; echo mount=$?; echo done",
        "",
    );
    assert_eq!(
        out.exit_code,
        Some(0),
        "the script itself must run: {out:?}"
    );
    assert!(out.stdout.contains("done"), "{out:?}");
    let code = |key: &str| {
        out.stdout
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.trim().parse::<i32>().ok())
    };
    assert!(
        matches!(code("wget="), Some(c) if c != 0),
        "network must be unreachable: {out:?}"
    );
    assert!(
        matches!(code("mount="), Some(c) if c != 0),
        "mount must be denied: {out:?}"
    );
}

#[test]
fn cgroup_limits_are_enforced() {
    let e = need_env!();
    if !e.backend.runtime().root {
        eprintln!("NOT RUN: limits need root/cgroup delegation");
        return;
    }
    let limits = sh(
        e,
        "cat /sys/fs/cgroup/memory.max /sys/fs/cgroup/pids.max /sys/fs/cgroup/cpu.max",
        "  resources: { memoryMib: 32, cpuMillis: 500, pids: 16, enforcement: required }\n",
    );
    assert_eq!(limits.stdout, "33554432\n16\n50000 100000\n", "{limits:?}");
    // 96 MiB through `tail -n 1`, which buffers a newline-free input entirely, under 32 MiB → OOM.
    let oom = sh(
        e,
        "head -c 100663296 /dev/zero | tail -n 1 >/dev/null; echo tail=$?",
        "  resources: { memoryMib: 32, enforcement: required }\n",
    );
    // The cgroup OOM killer picks its victim: `tail` (shell reports 137) or the whole sandbox.
    assert!(
        oom.stdout.contains("tail=137") || oom.exit_code == Some(137),
        "expected an OOM kill (128+9): {oom:?}"
    );
    let ok = sh(
        e,
        "head -c 1048576 /dev/zero | tail -n 1 >/dev/null; echo tail=$?",
        "  resources: { memoryMib: 32, enforcement: required }\n",
    );
    assert!(ok.stdout.contains("tail=0"), "{ok:?}");
    // 30 background sleeps under `pids: 8`: forks beyond the limit fail inside the shell.
    let pids = sh(
        e,
        "i=0; while [ $i -lt 30 ]; do sleep 3 & i=$((i+1)); done; echo loop-done",
        "  resources: { pids: 8, enforcement: required }\n",
    );
    let err = pids.stderr.to_lowercase();
    assert!(
        err.contains("fork") || err.contains("resource temporarily unavailable"),
        "pids limit not enforced: {pids:?}"
    );
}

fn node_image(e: &Env, name: &str, module: &Path, entry: &str) -> (ImageRecord, PathBuf) {
    let module_contract =
        parse_contract(&std::fs::read_to_string(module.join("node.yaml")).expect("node.yaml"))
            .expect("contract");
    let record = python_node_image(
        &e.store,
        &e.python,
        name,
        &repo().join("src/sdk/python/mvf_sdk"),
        module,
        entry,
        &contract_text(&module_contract),
    )
    .expect("package");
    let contract = oci_contract(&module_contract, name, record.manifest.digest.as_str());
    let dir = e.work.join("contracts");
    std::fs::create_dir_all(&dir).expect("dir");
    let file = dir.join(format!("{}.yaml", name.replace([':', '/'], "-")));
    std::fs::write(&file, contract_text(&contract)).expect("write");
    (record, file)
}

fn run_pipeline(e: &Env, proc_ref: &str, frames: u64, services: &HostServices) -> RunReport {
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: oci }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: {frames}, arena: true }} }}\n    - {{ id: proc, {proc_ref} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: proc.frame }}\n      - {{ from: proc.frame, to: sink.frame }}\n"
    );
    let resolver =
        HostResolver::new(e.work.join("contracts")).with_module_root(repo().join("modules"));
    let graph = build_graph(&text, &resolver).expect("graph");
    let registry = default_backends()
        .with(WorkerBackend::from_env())
        .with(e.backend.clone());
    let mut engine = Engine::build(graph, &registry, services).expect("build");
    engine.run(&RunOptions::default(), &mut NullObserver)
}

fn inverted(seq: u64) -> String {
    let px: Vec<u8> = mvf_engine::builtins::SyntheticSource::pixels(0, seq, 8)
        .iter()
        .map(|b| 255 - b)
        .collect();
    format!("{:016x}", fnv1a64(&px))
}

#[test]
fn python_module_runs_as_a_typed_sandboxed_oci_node_with_zero_copy() {
    let e = need_env!();
    let (_, contract) = node_image(
        e,
        "mvf/py-invert:e2e",
        &repo().join("modules/py-invert-transformer"),
        "transformer.py",
    );
    let services = HostServices::default();
    let report = run_pipeline(
        e,
        &format!(
            "contract: {}",
            contract.file_name().expect("f").to_string_lossy()
        ),
        20,
        &services,
    );
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        services.logs.events()
    );
    let proc = report.nodes.iter().find(|n| n.id == "proc").expect("proc");
    assert_eq!(proc.backend, "oci");
    let frames: Vec<(u64, String)> = services
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Frame {
                sequence, checksum, ..
            } => Some((sequence, checksum)),
            _ => None,
        })
        .collect();
    assert_eq!(
        frames,
        (1..=20).map(|s| (s, inverted(s))).collect::<Vec<_>>()
    );
    let arena = report.data_plane.arena.expect("arena");
    assert_eq!(
        (arena.copies_in, arena.copies_out, arena.in_use),
        (0, 0, 0),
        "{arena:?}"
    );
    assert_eq!(arena.zero_copy_handoffs, 20);
    assert!(list_containers(&e.backend).is_empty());
}

#[test]
fn crashing_oci_node_is_classified_and_cleaned_up() {
    let e = need_env!();
    let module = e.work.join("crash-module");
    std::fs::create_dir_all(&module).expect("dir");
    std::fs::write(module.join("crash.py"), "import os\nfrom mvf_sdk import run_processor\nrun_processor('e2e.crash', lambda p, m: os._exit(5))\n").expect("py");
    std::fs::write(
        module.join("node.yaml"),
        std::fs::read_to_string(repo().join("modules/py-invert-transformer/node.yaml"))
            .expect("y")
            .replace("py.invert-transformer", "e2e.crash")
            .replace("transformer.py", "crash.py"),
    )
    .expect("yaml");
    let (_, contract) = node_image(e, "mvf/crash:e2e", &module, "crash.py");
    let services = HostServices::default();
    let report = run_pipeline(
        e,
        &format!(
            "contract: {}",
            contract.file_name().expect("f").to_string_lossy()
        ),
        5,
        &services,
    );
    match &report.end {
        EndReason::NodeFailed { error, .. } => {
            assert_eq!(error.kind, NodeErrorKind::ProcessExited);
            assert_eq!(error.exit_code, Some(5), "{error:?}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(
        list_containers(&e.backend).is_empty(),
        "container not deleted"
    );
    let bundles = std::fs::read_dir(e.work.join("state/bundles"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(bundles, 0, "bundle not removed");
}

#[test]
fn unavailable_devices_and_cdi_gpus_are_rejected_before_start() {
    let e = need_env!();
    let (_, contract) = node_image(
        e,
        "mvf/py-invert-dev:e2e",
        &repo().join("modules/py-invert-transformer"),
        "transformer.py",
    );
    for (field, value) in [
        ("devices", "[{\"path\": \"/dev/mvf-no-such-device\"}]"),
        ("cdiDevices", "[\"nvidia.com/gpu=0\"]"),
    ] {
        let text = std::fs::read_to_string(&contract).expect("contract");
        let mut v: serde_json::Value = serde_json::from_str(&text).expect("json");
        v["spec"]["permissions"][field] = serde_json::from_str(value).expect("value");
        let file = contract.with_file_name(format!("perm-{field}.yaml"));
        std::fs::write(&file, v.to_string()).expect("write");
        let text = format!(
            "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: p }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source }}\n    - {{ id: proc, contract: perm-{field}.yaml }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: proc.frame }}\n"
        );
        let graph =
            build_graph(&text, &HostResolver::new(e.work.join("contracts"))).expect("graph");
        let err = Engine::build(
            graph,
            &default_backends().with(e.backend.clone()),
            &HostServices::default(),
        )
        .expect_err("rejected");
        assert_eq!(err.error.kind, NodeErrorKind::Unsupported, "{field}: {err}");
    }
}

#[test]
fn oci_workload_is_live_replaced_and_rolled_back() {
    let e = need_env!();
    let (_, good) = node_image(
        e,
        "mvf/py-invert-live:e2e",
        &repo().join("modules/py-invert-transformer"),
        "transformer.py",
    );
    let module = e.work.join("crash-live");
    std::fs::create_dir_all(&module).expect("dir");
    std::fs::write(module.join("crash.py"), "import os\nfrom mvf_sdk import run_processor\nrun_processor('e2e.crash-live', lambda p, m: os._exit(6))\n").expect("py");
    std::fs::write(
        module.join("node.yaml"),
        std::fs::read_to_string(repo().join("modules/py-invert-transformer/node.yaml"))
            .expect("y")
            .replace("py.invert-transformer", "e2e.crash-live")
            .replace("transformer.py", "crash.py"),
    )
    .expect("yaml");
    let (_, bad) = node_image(e, "mvf/crash-live:e2e", &module, "crash.py");

    struct Driver {
        control: ControlHandle,
        steps: Vec<(u64, ReplacementRequest)>,
    }
    impl Observer for Driver {
        fn cycle_completed(&mut self, cycle: u64) {
            for (at, r) in &self.steps {
                if *at == cycle + 1 {
                    self.control.replace(r.clone());
                }
            }
        }
    }
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: live-oci }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { frames: 1500, intervalMs: 10, arena: true } }\n    - { id: proc, builtin: invert }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: proc.frame }\n      - { from: proc.frame, to: sink.frame }\n";
    let resolver = HostResolver::new(e.work.join("contracts"));
    let graph = build_graph(text, &resolver).expect("graph");
    let services = HostServices::default();
    let mut engine = Engine::build(
        graph,
        &default_backends().with(e.backend.clone()),
        &services,
    )
    .expect("build");
    let reference =
        |f: &Path| NodeReference::Contract(f.file_name().expect("f").to_string_lossy().to_string());
    let mut driver = Driver {
        control: engine.control(),
        steps: vec![
            (
                10,
                ReplacementRequest::resolve(
                    &resolver,
                    "proc",
                    &reference(&good),
                    Default::default(),
                    10,
                )
                .expect("good"),
            ),
            (
                700,
                ReplacementRequest::resolve(
                    &resolver,
                    "proc",
                    &reference(&bad),
                    Default::default(),
                    300,
                )
                .expect("bad"),
            ),
        ],
    };
    let report = engine.run(&RunOptions::default(), &mut driver);
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        report.replacements
    );
    let kinds: Vec<_> = report.replacements.iter().map(|r| r.kind).collect();
    use ReplacementEventKind::*;
    assert_eq!(
        kinds,
        [
            Requested, Promoted, Committed, Requested, Promoted, RolledBack
        ],
        "{:?}",
        report.replacements
    );
    let frames: Vec<(u64, u64, String)> = services
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Frame {
                sequence, checksum, ..
            } => Some((r.cycle, sequence, checksum)),
            _ => None,
        })
        .collect();
    assert_eq!(
        frames,
        (1..=1500).map(|s| (s, s, inverted(s))).collect::<Vec<_>>(),
        "lossless and identical across Rust and the OCI node"
    );
    for ev in &report.replacements {
        eprintln!(
            "REPLACEMENT {:?} cycle={} gen {}->{:?} prep_ms={:?} cutover_us={:?} code={:?}",
            ev.kind,
            ev.cycle,
            ev.from_generation,
            ev.to_generation,
            ev.preparation_ms,
            ev.cutover_us,
            ev.code
        );
    }
    assert!(list_containers(&e.backend).is_empty());
}

#[test]
fn granted_device_is_visible_with_only_the_granted_access() {
    let e = need_env!();
    if !Path::new("/dev/kmsg").exists() {
        eprintln!("NOT RUN: /dev/kmsg absent on this host");
        return;
    }
    let grant = "  permissions: { devices: [ { path: /dev/kmsg, access: r } ] }\n";
    let out = sh(
        e,
        "test -c /dev/kmsg && echo present; echo x 2>/dev/null > /dev/kmsg && echo writable || echo write-denied",
        grant,
    );
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(out.stdout.contains("present"), "{out:?}");
    assert!(
        out.stdout.contains("write-denied"),
        "read-only grant must not allow writes: {out:?}"
    );
    let without = sh(e, "test -e /dev/kmsg && echo present || echo absent", "");
    assert!(without.stdout.contains("absent"), "{without:?}");
}

// ------------------------------------------------------------------------------- #28 hardening

/// Calls `io_uring_setup` (425 on every architecture): allowed by the denylist, absent from
/// Docker's allowlist — a syscall that distinguishes the two profiles from inside the sandbox.
fn io_uring_probe(backend: &OciBackend, e: &Env) -> WorkloadOutput {
    let script = "import ctypes\nlibc = ctypes.CDLL(None, use_errno=True)\nparams = (ctypes.c_char * 120)()\nfd = libc.syscall(425, 1, params)\nprint('io_uring', fd, ctypes.get_errno())\nprint('done')\n";
    let mut contract = sandbox_contract("");
    contract.spec.artifacts = vec![mvf_contract::Artifact::Oci {
        image: "python".into(),
        digest: e.python.manifest.digest.to_string(),
        targets: None,
    }];
    run_workload(
        backend,
        &e.python,
        &contract,
        Some(vec!["python3".into(), "-c".into(), script.into()]),
        Duration::from_secs(60),
    )
    .expect("workload")
}

fn probe_result(out: &WorkloadOutput) -> (i64, i64) {
    let line = out
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("io_uring "))
        .unwrap_or_else(|| panic!("no probe output: {out:?}"));
    let mut it = line
        .split_whitespace()
        .map(|v| v.parse::<i64>().unwrap_or(i64::MIN));
    (it.next().unwrap_or(i64::MIN), it.next().unwrap_or(i64::MIN))
}

#[test]
fn the_syscall_allowlist_denies_what_the_denylist_allows() {
    let e = need_env!();
    let deny = io_uring_probe(&e.backend, e);
    assert!(deny.stdout.contains("done"), "{deny:?}");
    let (fd, _) = probe_result(&deny);
    assert!(fd >= 0, "denylist: io_uring_setup is allowed ({deny:?})");
    let strict = e.backend.clone().with_syscall_allowlist(true);
    let allow = io_uring_probe(&strict, e);
    assert!(
        allow.stdout.contains("done"),
        "python itself must run under the allowlist: {allow:?}"
    );
    assert_eq!(
        probe_result(&allow),
        (-1, 1),
        "allowlist: EPERM ({allow:?})"
    );
}

#[test]
fn a_python_node_runs_unchanged_under_the_allowlist() {
    let e = need_env!();
    let (_, contract) = node_image(
        e,
        "mvf-e2e/invert-allowlist:1",
        &repo().join("modules/py-invert-transformer"),
        "transformer.py",
    );
    let file = contract
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default()
        .to_string();
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: oci }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: 6, arena: true }} }}\n    - {{ id: proc, contract: {file} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: proc.frame }}\n      - {{ from: proc.frame, to: sink.frame }}\n"
    );
    let graph = build_graph(&text, &HostResolver::new(e.work.join("contracts"))).expect("graph");
    let services = HostServices::default();
    let registry = default_backends().with(e.backend.clone().with_syscall_allowlist(true));
    let r = Engine::build(graph, &registry, &services)
        .expect("build")
        .run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(
        r.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        services.logs.events()
    );
    let checksums: Vec<String> = services
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Frame { checksum, .. } => Some(checksum),
            _ => None,
        })
        .collect();
    assert_eq!(checksums, (1..=6).map(inverted).collect::<Vec<_>>());
}

#[test]
fn a_second_node_image_on_the_same_base_unpacks_only_its_own_layer() {
    let e = need_env!();
    // A copy of the invert module with one changed byte: same base, a different module layer.
    let module = e.work.join("invert-copy");
    std::fs::create_dir_all(&module).expect("dir");
    for f in ["node.yaml", "transformer.py"] {
        std::fs::copy(
            repo().join("modules/py-invert-transformer").join(f),
            module.join(f),
        )
        .expect("copy");
    }
    let mut source = std::fs::read_to_string(module.join("transformer.py")).expect("read");
    source.push_str("\n# variant\n");
    std::fs::write(module.join("transformer.py"), source).expect("write");
    let (a, _) = node_image(
        e,
        "mvf-e2e/layer-a:1",
        &repo().join("modules/py-invert-transformer"),
        "transformer.py",
    );
    let (b, _) = node_image(e, "mvf-e2e/layer-b:1", &module, "transformer.py");
    assert_eq!(a.layers.len(), b.layers.len());
    // A fresh state directory, so nothing is cached yet.
    let backend = OciBackend::new(
        e.backend.runtime().clone(),
        e.store.clone(),
        e.work.join(format!("state-layers-{}", std::process::id())),
    );
    let contract = |record: &ImageRecord| {
        let mut c = sandbox_contract("");
        c.spec.artifacts = vec![mvf_contract::Artifact::Oci {
            image: "node".into(),
            digest: record.manifest.digest.to_string(),
            targets: None,
        }];
        c
    };
    let time = |record: &ImageRecord| {
        let t0 = std::time::Instant::now();
        let out = run_workload(
            &backend,
            record,
            &contract(record),
            Some(vec!["true".into()]),
            Duration::from_secs(60),
        )
        .expect("workload");
        assert_eq!(out.exit_code, Some(0), "{out:?}");
        (t0.elapsed(), backend.last_rootfs().expect("composed"))
    };
    let (cold, first) = time(&a);
    let (warm, second) = time(&b);
    eprintln!("rootfs: first image {cold:?} {first:?}");
    eprintln!("rootfs: second image {warm:?} {second:?}");
    assert_eq!(
        (first.layers_unpacked, first.layers_reused),
        (a.layers.len(), 0)
    );
    assert_eq!(
        (second.layers_unpacked, second.layers_reused),
        (1, b.layers.len() - 1),
        "only the module layer is new"
    );
    assert!(
        second.unpack.bytes * 20 < first.unpack.bytes,
        "the base was not unpacked again"
    );
    assert!(second.linked > 1000 && second.copied == 0, "{second:?}");
    assert!(
        warm < cold,
        "reusing the base is faster: {warm:?} vs {cold:?}"
    );
}

#[test]
fn a_contract_that_requires_a_security_module_fails_closed_without_one() {
    let e = need_env!();
    let mut c = sandbox_contract("");
    c.spec.isolation.lsm = mvf_contract::LsmRequirement::Required;
    let caps = e.backend.capabilities();
    let placed = mvf_contract::negotiate(&c, std::slice::from_ref(&caps));
    if caps.grants.lsm.is_empty() {
        let err = placed.expect_err("no LSM on this host");
        assert!(
            err.rejections
                .iter()
                .flat_map(|r| r.reasons.iter())
                .any(|r| r.code == "negotiation.lsm-unavailable"),
            "{err:?}"
        );
        eprintln!("LSM enforcement BLOCKED on this host: no AppArmor/SELinux profile available");
    } else {
        assert!(placed.is_ok(), "host offers {:?}", caps.grants.lsm);
    }
}
