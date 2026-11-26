//! Real OCI execution through a low-level runtime (runc/crun) on Linux — no Docker daemon.
//!
//! Needs: Linux, root (for cgroup limits and the user namespace), `runc` or `crun`, and
//! `MVF_OCI_FIXTURES` with `busybox.tar` and `python.tar` (`docker save` output, prepared once).
//! Enabled by `MVF_OCI_E2E=1`; `MVF_REQUIRE_OCI=1` turns a missing prerequisite into a failure.
//! `rust/scripts/oci-testhost.sh` runs this suite in a privileged Debian container that has
//! **no Python and no Docker daemon** inside.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

// ------------------------------------------------------------------------------- #23 network

const LAN_HOST: &str = "192.168.77.1";
const ALLOWED: &str = "192.168.77.10";
const DENIED: &str = "192.168.77.20";

fn ip(args: &[&str]) -> bool {
    Command::new("ip")
        .args(args)
        .status()
        .is_ok_and(|s| s.success())
}

/// A simulated factory LAN: bridge `mvflan0` (192.168.77.1/24) on the test host, and two client
/// namespaces, `ext1` (.10, the allowed camera/PLC) and `ext2` (.20, a stranger). Once per run.
fn factory_lan() -> bool {
    static LAN: OnceLock<bool> = OnceLock::new();
    *LAN.get_or_init(|| {
        let _ = ip(&["link", "del", "mvflan0"]);
        let mut ok = ip(&["link", "add", "mvflan0", "type", "bridge"])
            && ip(&["addr", "add", &format!("{LAN_HOST}/24"), "dev", "mvflan0"])
            && ip(&["link", "set", "mvflan0", "up"]);
        for (ns, addr) in [("ext1", ALLOWED), ("ext2", DENIED)] {
            let _ = ip(&["netns", "del", ns]);
            let (a, b) = (format!("{ns}a"), format!("{ns}b"));
            ok = ok
                && ip(&["netns", "add", ns])
                && ip(&["link", "add", &a, "type", "veth", "peer", "name", &b])
                && ip(&["link", "set", &a, "master", "mvflan0", "up"])
                && ip(&["link", "set", &b, "netns", ns])
                && ip(&["-n", ns, "addr", "add", &format!("{addr}/24"), "dev", &b])
                && ip(&["-n", ns, "link", "set", &b, "up"])
                && ip(&["-n", ns, "link", "set", "lo", "up"])
                && ip(&["-n", ns, "route", "add", "default", "via", LAN_HOST]);
        }
        ok
    })
}

macro_rules! need_net {
    () => {{
        let e = need_env!();
        if !e
            .backend
            .capabilities()
            .grants
            .network
            .contains(&mvf_contract::NetworkPolicy::Restricted)
        {
            assert!(
                std::env::var_os("MVF_REQUIRE_OCI").is_none(),
                "restricted networking unavailable (root, ip, nft)"
            );
            eprintln!("NOT RUN: restricted networking unavailable");
            return;
        }
        assert!(factory_lan(), "factory LAN setup");
        e
    }};
}

/// Runs this test binary as a network client/server inside namespace `ns`.
fn in_ns(ns: &str, op: &str) -> std::process::Child {
    Command::new("ip")
        .args(["netns", "exec", ns])
        .arg(std::env::current_exe().expect("exe"))
        .args([
            "net_helper",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("MVF_NET_HELPER", op)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("helper")
}

fn helper_says(mut child: std::process::Child) -> String {
    let mut out = String::new();
    if let Some(s) = child.stdout.as_mut() {
        let _ = std::io::Read::read_to_string(s, &mut out);
    }
    let _ = child.wait();
    out.lines()
        .filter_map(|l| l.split("NET:").nth(1))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Network helper (runs inside a client namespace): `tcp <ip> <port> <msg>`, `udp <ip> <port>
/// <msg>`, `serve <port> <count>` (TCP, replies `pong`).
#[test]
#[ignore = "helper process for the network tests"]
fn net_helper() {
    use std::io::{BufRead, Write};
    let Ok(op) = std::env::var("MVF_NET_HELPER") else {
        return;
    };
    let parts: Vec<&str> = op.split_whitespace().collect();
    let timeout = Duration::from_secs(3);
    match parts.as_slice() {
        ["tcp", ip, port, msg] => {
            let addr: std::net::SocketAddr = format!("{ip}:{port}").parse().expect("addr");
            for _ in 0..40 {
                match std::net::TcpStream::connect_timeout(&addr, timeout) {
                    Ok(mut s) => {
                        let _ = s.set_read_timeout(Some(timeout));
                        let _ = writeln!(s, "{msg}");
                        let mut line = String::new();
                        let _ = std::io::BufReader::new(&s).read_line(&mut line);
                        println!("NET: reply {}", line.trim());
                        return;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    Err(e) => {
                        println!("NET: error {:?}", e.kind());
                        return;
                    }
                }
            }
            println!("NET: error refused");
        }
        ["udp", ip, port, msg] => {
            let s = std::net::UdpSocket::bind("0.0.0.0:0").expect("udp");
            let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
            let mut buf = [0u8; 256];
            for _ in 0..12 {
                let _ = s.send_to(msg.as_bytes(), format!("{ip}:{port}"));
                if let Ok((n, _)) = s.recv_from(&mut buf) {
                    println!("NET: reply {}", String::from_utf8_lossy(&buf[..n]).trim());
                    return;
                }
            }
            println!("NET: error timeout");
        }
        ["ftp", ip, port, user, count, size] => {
            let files: Vec<Vec<u8>> = (0..count.parse::<usize>().unwrap_or(1))
                .map(|i| ftp_payload(i, size.parse().unwrap_or(1024)))
                .collect();
            match ftp_upload(&format!("{ip}:{port}"), user, &files) {
                Ok(t) => println!(
                    "NET: ok {}",
                    t.iter()
                        .map(|us| us.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                Err(e) => println!("NET: error {e}"),
            }
        }
        ["poll", ip, port, every_ms, total_ms] => {
            // Opens a new connection every `every_ms` and records the reply: `v1*12 v2*40 err*1`.
            let addr: std::net::SocketAddr = format!("{ip}:{port}").parse().expect("addr");
            let until =
                std::time::Instant::now() + Duration::from_millis(total_ms.parse().unwrap_or(1000));
            let every = Duration::from_millis(every_ms.parse().unwrap_or(20));
            let mut runs: Vec<(String, u32)> = Vec::new();
            while std::time::Instant::now() < until {
                let reply =
                    match std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
                        Ok(stream) => {
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                            let mut line = String::new();
                            match std::io::BufReader::new(&stream).read_line(&mut line) {
                                Ok(n) if n > 0 => line.trim().to_string(),
                                _ => "err".to_string(),
                            }
                        }
                        Err(_) => "err".to_string(),
                    };
                match runs.last_mut() {
                    Some((r, n)) if *r == reply => *n += 1,
                    _ => runs.push((reply, 1)),
                }
                std::thread::sleep(every);
            }
            println!(
                "NET: {}",
                runs.iter()
                    .map(|(r, n)| format!("{r}*{n}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        ["serve", port, count] => {
            let l = std::net::TcpListener::bind(format!("0.0.0.0:{port}")).expect("listen");
            println!("NET: listening");
            for _ in 0..count.parse::<u32>().unwrap_or(1) {
                if let Ok((mut s, peer)) = l.accept() {
                    let _ = writeln!(s, "pong {}", peer.ip());
                }
            }
        }
        _ => panic!("unknown helper op {op}"),
    }
}

fn python_net_contract(e: &Env, network: &str) -> NodeContract {
    let mut c = sandbox_contract(&format!("  permissions:\n    network:\n{network}"));
    c.spec.artifacts = vec![mvf_contract::Artifact::Oci {
        image: "python".into(),
        digest: e.python.manifest.digest.to_string(),
        targets: None,
    }];
    c
}

fn python(
    e: &Env,
    contract: NodeContract,
    script: &str,
    binding: Option<mvf_contract::NetworkBinding>,
) -> std::thread::JoinHandle<WorkloadOutput> {
    let backend = e.backend.clone();
    let record = e.python.clone();
    let script = script.to_string();
    std::thread::spawn(move || {
        mvf_container::run_workload_with(
            &backend,
            &record,
            &contract,
            Some(vec!["python3".into(), "-c".into(), script]),
            Duration::from_secs(60),
            binding.as_ref(),
        )
        .expect("workload")
    })
}

const SERVER: &str = "import socket\ns = socket.socket()\ns.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\ns.bind(('0.0.0.0', 2121))\ns.listen(4)\nu = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)\nu.bind(('0.0.0.0', 2122))\nu.settimeout(20)\ndata, peer = u.recvfrom(256)\nu.sendto(b'udp-hello ' + peer[0].encode(), peer)\nprint('udp from', peer[0], flush=True)\nfor _ in range(2):\n    c, peer = s.accept()\n    c.recv(64)\n    c.sendall(('hello ' + peer[0] + '\\n').encode())\n    c.close()\n    print('accepted', peer[0], flush=True)\nprint('done')\n";

#[test]
fn restricted_inbound_admits_only_the_declared_source_and_return_traffic_flows() {
    let e = need_net!();
    let contract = python_net_contract(
        e,
        &format!(
            "      mode: restricted\n      inbound:\n        - {{ protocol: tcp, port: 2121, source: {ALLOWED}/32 }}\n        - {{ protocol: udp, port: 2122, source: {ALLOWED}/32 }}\n      outbound: deny\n"
        ),
    );
    let server = python(e, contract, SERVER, None);
    // UDP first (the server waits for it), from the stranger and then from the allowed client.
    let stranger_udp = helper_says(in_ns("ext2", &format!("udp {LAN_HOST} 2122 hi")));
    let allowed_udp = helper_says(in_ns("ext1", &format!("udp {LAN_HOST} 2122 hi")));
    let stranger = helper_says(in_ns("ext2", &format!("tcp {LAN_HOST} 2121 ping")));
    let first = helper_says(in_ns("ext1", &format!("tcp {LAN_HOST} 2121 ping")));
    let second = helper_says(in_ns("ext1", &format!("tcp {LAN_HOST} 2121 ping")));
    let out = server.join().expect("server");
    eprintln!("server: {out:?}");
    assert_eq!(
        allowed_udp,
        format!("reply udp-hello {ALLOWED}"),
        "UDP from the allowed source, reply delivered"
    );
    assert!(
        stranger_udp.starts_with("error"),
        "UDP from a stranger is dropped: {stranger_udp}"
    );
    assert_eq!(
        first,
        format!("reply hello {ALLOWED}"),
        "the node sees the real client address; return traffic flows"
    );
    assert_eq!(second, format!("reply hello {ALLOWED}"));
    assert!(
        stranger.starts_with("error"),
        "TCP from a stranger is not admitted: {stranger}"
    );
    assert!(out.stdout.contains("done"), "{out:?}");
    assert_eq!(
        out.stdout.matches(&format!("accepted {ALLOWED}")).count(),
        2
    );
}

#[test]
fn restricted_outbound_reaches_only_the_declared_destination() {
    let e = need_net!();
    let plc = in_ns("ext1", "serve 5020 1");
    let plc_other_port = in_ns("ext1", "serve 5021 1");
    let other = in_ns("ext2", "serve 5020 1");
    std::thread::sleep(Duration::from_millis(500));
    let script = format!(
        "import socket\ndef probe(host, port):\n    try:\n        s = socket.create_connection((host, port), timeout=3)\n        return s.recv(64).decode().strip()\n    except Exception as e:\n        return 'blocked ' + type(e).__name__\nprint('plc', probe('{ALLOWED}', 5020), flush=True)\nprint('other', probe('{DENIED}', 5020), flush=True)\nprint('port', probe('{ALLOWED}', 5021), flush=True)\nprint('host', probe('{LAN_HOST}', 22), flush=True)\n"
    );
    let contract = python_net_contract(
        e,
        &format!(
            "      mode: restricted\n      outbound:\n        - {{ protocol: tcp, destination: {ALLOWED}/32, port: 5020 }}\n"
        ),
    );
    let out = python(e, contract, &script, None).join().expect("client");
    for mut c in [plc, plc_other_port, other] {
        let _ = c.kill();
        let _ = c.wait();
    }
    assert!(
        out.stdout.contains("port blocked"),
        "the allowed host on an undeclared port is not (a server does listen there): {out:?}"
    );
    assert!(
        out.stdout.contains("plc pong"),
        "the allowed PLC endpoint is reachable: {out:?}"
    );
    assert!(
        out.stdout.contains("other blocked"),
        "an undeclared destination is not: {out:?}"
    );
    assert!(
        out.stdout.contains("host blocked"),
        "the host itself is not: {out:?}"
    );
}

#[test]
fn a_host_port_is_published_once_and_every_resource_is_cleaned_up() {
    let e = need_net!();
    let net = |port: u16| {
        format!("      mode: restricted\n      inbound: [ {{ protocol: tcp, port: {port} }} ]\n")
    };
    let holder = python(
        e,
        python_net_contract(e, &net(2131)),
        "import time\ntime.sleep(4)\nprint('held')\n",
        None,
    );
    std::thread::sleep(Duration::from_millis(1500));
    let second = mvf_container::run_workload_with(
        &e.backend,
        &e.python,
        &python_net_contract(e, &net(2131)),
        Some(vec!["true".into()]),
        Duration::from_secs(30),
        None,
    );
    let err = second.expect_err("the port is taken");
    assert!(
        err.message
            .contains("already published by another MVF node"),
        "{err}"
    );
    // A port held by any other process on the host is detected too.
    let squatter = std::net::TcpListener::bind("0.0.0.0:2132").expect("squat");
    let third = mvf_container::run_workload_with(
        &e.backend,
        &e.python,
        &python_net_contract(e, &net(2132)),
        Some(vec!["true".into()]),
        Duration::from_secs(30),
        None,
    );
    assert!(
        third
            .expect_err("in use")
            .message
            .contains("in use by another process")
    );
    drop(squatter);
    assert!(holder.join().expect("holder").stdout.contains("held"));
    let netns = String::from_utf8_lossy(
        &Command::new("ip")
            .args(["netns", "list"])
            .output()
            .expect("ip")
            .stdout,
    )
    .to_string();
    let tables = String::from_utf8_lossy(
        &Command::new("nft")
            .args(["list", "tables"])
            .output()
            .expect("nft")
            .stdout,
    )
    .to_string();
    let links = String::from_utf8_lossy(
        &Command::new("ip")
            .args(["-o", "link"])
            .output()
            .expect("ip")
            .stdout,
    )
    .to_string();
    assert!(!netns.contains("mvf-"), "{netns}");
    assert!(!tables.contains("mvf_"), "{tables}");
    assert!(!links.contains("mvfh"), "{links}");
}

#[test]
fn networks_left_by_a_dead_host_are_swept_before_the_next_allocation() {
    let e = need_net!();
    let support = mvf_container::network::NetworkSupport::detect().expect("support");
    let state = e
        .work
        .join(format!("state-net-sweep-{}", std::process::id()));
    std::fs::create_dir_all(state.join("net/leases")).expect("dir");
    std::fs::create_dir_all(state.join("net/ports")).expect("dir");
    // A lease of a process that no longer exists, with its namespace and interface still there.
    std::fs::write(state.join("net/leases/9000"), "999999999 ghost").expect("lease");
    std::fs::write(state.join("net/ports/tcp-2141"), "999999999 ghost").expect("port lease");
    assert!(ip(&["netns", "add", "mvf-9000"]));
    assert!(ip(&["link", "add", "mvfh2328", "type", "dummy"]));
    let permission: mvf_contract::NetworkPermission =
        mvf_contract::NetworkPolicy::Restricted.into();
    let policy = mvf_container::network::Policy::resolve(&permission, None).expect("policy");
    let net = mvf_container::network::NodeNetwork::setup(&support, &state, "fresh", policy)
        .expect("setup");
    let netns = String::from_utf8_lossy(
        &Command::new("ip")
            .args(["netns", "list"])
            .output()
            .expect("ip")
            .stdout,
    )
    .to_string();
    assert!(
        !netns.contains("mvf-9000"),
        "stale namespace swept: {netns}"
    );
    assert!(!state.join("net/leases/9000").exists() && !state.join("net/ports/tcp-2141").exists());
    drop(net);
}

/// The deterministic content of the camera simulator's image `i`.
fn ftp_payload(i: usize, size: usize) -> Vec<u8> {
    (0..size).map(|j| ((i * 37 + j * 7) & 0xff) as u8).collect()
}

/// A minimal passive-mode FTP client (the camera simulator). Per-file round trips in microseconds.
fn ftp_upload(addr: &str, user: &str, files: &[Vec<u8>]) -> Result<Vec<u128>, String> {
    use std::io::{BufRead, BufReader, Write};
    let mut control = None;
    for _ in 0..80 {
        match std::net::TcpStream::connect_timeout(
            &addr.parse().map_err(|e| format!("{e}"))?,
            Duration::from_secs(3),
        ) {
            Ok(c) => {
                control = Some(c);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                std::thread::sleep(Duration::from_millis(250))
            }
            Err(e) => return Err(format!("connect {:?}", e.kind())),
        }
    }
    let control = control.ok_or("connect refused")?;
    control
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(control.try_clone().map_err(|e| e.to_string())?);
    let _ = control.set_nodelay(true);
    let mut w = control;
    let expect =
        |reader: &mut BufReader<std::net::TcpStream>, code: &str| -> Result<String, String> {
            let mut line = String::new();
            reader.read_line(&mut line).map_err(|e| e.to_string())?;
            if line.starts_with(code) {
                Ok(line)
            } else {
                Err(format!("expected {code}, got {line:?}"))
            }
        };
    expect(&mut reader, "220")?;
    for (cmd, code) in [
        (format!("USER {user}"), "331"),
        ("PASS secret".to_string(), "230"),
        ("TYPE I".to_string(), "200"),
    ] {
        w.write_all(format!("{cmd}\r\n").as_bytes())
            .map_err(|e| e.to_string())?;
        expect(&mut reader, code)?;
    }
    let mut timings = Vec::new();
    for (i, bytes) in files.iter().enumerate() {
        let t0 = std::time::Instant::now();
        w.write_all(b"PASV\r\n").map_err(|e| e.to_string())?;
        let line = expect(&mut reader, "227")?;
        let inner = line
            .split('(')
            .nth(1)
            .and_then(|r| r.split(')').next())
            .ok_or("bad 227")?;
        let n: Vec<u16> = inner
            .split(',')
            .filter_map(|x| x.trim().parse().ok())
            .collect();
        let [a, b, c, d, p1, p2] = n.as_slice() else {
            return Err(format!("bad 227: {line}"));
        };
        let mut data = std::net::TcpStream::connect(format!("{a}.{b}.{c}.{d}:{}", p1 * 256 + p2))
            .map_err(|e| format!("data connect: {e}"))?;
        w.write_all(format!("STOR image-{i}.bin\r\n").as_bytes())
            .map_err(|e| e.to_string())?;
        expect(&mut reader, "150")?;
        data.write_all(bytes).map_err(|e| e.to_string())?;
        drop(data);
        expect(&mut reader, "226")?;
        timings.push(t0.elapsed().as_micros());
    }
    let _ = w.write_all(b"QUIT\r\n");
    Ok(timings)
}

fn percentile(sorted: &[u128], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize] as f64 / 1000.0
}

#[test]
fn a_camera_uploads_over_ftp_into_a_restricted_oci_source_node() {
    let e = need_net!();
    let (_, contract_file) = node_image(
        e,
        "mvf-e2e/ftp-source:1",
        &repo().join("modules/py-ftp-source"),
        "ftp_source.py",
    );
    // The packager decides the enforced policy of the OCI node: the camera, and only the camera,
    // may reach the control port and the passive data range; no new outbound connections.
    let mut contract =
        parse_contract(&std::fs::read_to_string(&contract_file).expect("contract")).expect("parse");
    contract.spec.permissions.network =
        mvf_contract::NetworkPermission::Rules(mvf_contract::NetworkRules {
            mode: mvf_contract::NetworkPolicy::Restricted,
            inbound: vec![
                mvf_contract::InboundRule {
                    protocol: mvf_contract::Protocol::Tcp,
                    port: Some(2121),
                    port_range: None,
                    source: Some(format!("{ALLOWED}/32")),
                },
                mvf_contract::InboundRule {
                    protocol: mvf_contract::Protocol::Tcp,
                    port: None,
                    port_range: Some("50000-50009".into()),
                    source: Some(format!("{ALLOWED}/32")),
                },
            ],
            outbound: mvf_contract::Outbound::default(),
        });
    std::fs::write(&contract_file, contract_text(&contract)).expect("write");
    let file = contract_file
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default()
        .to_string();
    const N: usize = 20;
    const SIZE: usize = 256 * 1024;
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: ftp }}\nspec:\n  nodes:\n    - {{ id: ftp, contract: {file}, config: {{ port: 2121, passiveRange: 50000-50009, user: cognex-1, password: secret, maxFiles: {N} }}, network: {{ advertise: {LAN_HOST} }} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: ftp.frame, to: sink.frame }}\n"
    );
    let graph = build_graph(&text, &HostResolver::new(e.work.join("contracts"))).expect("graph");
    let mut services = HostServices::default();
    services.arena_config.slot_size = 512 * 1024;
    services.arena_config.slot_count = 8;
    let registry = default_backends().with(e.backend.clone());
    let mut engine = Engine::build(graph, &registry, &services).expect("build");
    let run = std::thread::spawn(move || engine.run(&RunOptions::default(), &mut NullObserver));
    let stranger = helper_says(in_ns("ext2", &format!("ftp {LAN_HOST} 2121 intruder 1 64")));
    let camera = helper_says(in_ns(
        "ext1",
        &format!("ftp {LAN_HOST} 2121 cognex-1 {N} {SIZE}"),
    ));
    let report = run.join().expect("run");
    let logs = services.logs.events();
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "ftp".into() },
        "{logs:?}"
    );
    assert!(
        stranger.starts_with("error"),
        "the stranger cannot reach the FTP node: {stranger}"
    );
    let mut timings: Vec<u128> = camera
        .strip_prefix("ok ")
        .unwrap_or_else(|| panic!("camera upload failed: {camera}"))
        .split(',')
        .filter_map(|t| t.parse().ok())
        .collect();
    let frames: Vec<(String, u64, String)> = services
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Frame {
                source,
                sequence,
                checksum,
                ..
            } => Some((source, sequence, checksum)),
            _ => None,
        })
        .collect();
    let expected: Vec<(String, u64, String)> = (0..N)
        .map(|i| {
            (
                "cognex-1".to_string(),
                i as u64 + 1,
                format!("{:016x}", fnv1a64(&ftp_payload(i, SIZE))),
            )
        })
        .collect();
    assert_eq!(
        frames, expected,
        "every image arrives in the graph, in order, byte for byte"
    );
    let arena = report.data_plane.arena.expect("arena");
    assert_eq!(
        arena.copies_in, 0,
        "the node writes each image straight into its output slot"
    );
    let policy = logs
        .iter()
        .find(|l| l.message.starts_with("network: restricted"))
        .expect("effective policy logged");
    assert!(
        !policy.message.contains("secret"),
        "no credentials in the effective policy"
    );
    let counters = logs
        .iter()
        .find(|l| l.message.starts_with("network counters"))
        .expect("counters")
        .message
        .clone();
    timings.sort_unstable();
    let total: u128 = timings.iter().sum();
    eprintln!(
        "FTP OCI restricted: {N} x {} KiB, upload round trip ms p50={:.2} p95={:.2} p99={:.2} max={:.2}, {:.1} MiB/s; {}; {}",
        SIZE / 1024,
        percentile(&timings, 0.5),
        percentile(&timings, 0.95),
        percentile(&timings, 0.99),
        percentile(&timings, 1.0),
        (N * SIZE) as f64 / (1024.0 * 1024.0) / (total as f64 / 1e6),
        policy.message,
        counters
    );
    let usage = logs
        .iter()
        .find(|l| l.message.starts_with("ftp: done"))
        .map(|l| l.message.clone())
        .unwrap_or_default();
    eprintln!("FTP OCI node usage: {usage}");
}

/// A processor that also listens on TCP 2151 and answers its version: the "listening node" whose
/// live replacement must hand its published port over. `v3` crashes on its 20th frame.
fn listener_image(e: &Env, version: &str) -> PathBuf {
    let module = e.work.join(format!("listener-{version}"));
    std::fs::create_dir_all(&module).expect("dir");
    let script = format!(
        "import os, socket, threading\nfrom mvf_sdk import run_processor, blob\nVERSION = '{version}'\ndef serve():\n    s = socket.socket()\n    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)\n    s.bind(('0.0.0.0', 2151))\n    s.listen(64)\n    while True:\n        c, _ = s.accept()\n        try:\n            c.sendall((VERSION + '\\n').encode())\n        finally:\n            c.close()\nthreading.Thread(target=serve, daemon=True).start()\nseen = [0]\ndef transform(payload, meta):\n    seen[0] += 1\n    if VERSION == 'v3' and seen[0] >= 20:\n        os._exit(3)\n    return blob(bytes(payload.memory))\nrun_processor('e2e.listener', transform)\n"
    );
    std::fs::write(module.join("listener.py"), script).expect("py");
    std::fs::write(
        module.join("node.yaml"),
        std::fs::read_to_string(repo().join("modules/py-invert-transformer/node.yaml"))
            .expect("y")
            .replace("py.invert-transformer", "e2e.listener")
            .replace("transformer.py", "listener.py"),
    )
    .expect("yaml");
    let (_, file) = node_image(
        e,
        &format!("mvf-e2e/listener:{version}"),
        &module,
        "listener.py",
    );
    let mut contract =
        parse_contract(&std::fs::read_to_string(&file).expect("contract")).expect("parse");
    contract.spec.permissions.network =
        mvf_contract::NetworkPermission::Rules(mvf_contract::NetworkRules {
            mode: mvf_contract::NetworkPolicy::Restricted,
            inbound: vec![mvf_contract::InboundRule {
                protocol: mvf_contract::Protocol::Tcp,
                port: Some(2151),
                port_range: None,
                source: Some(format!("{ALLOWED}/32")),
            }],
            outbound: mvf_contract::Outbound::default(),
        });
    std::fs::write(&file, contract_text(&contract)).expect("write");
    file
}

#[test]
fn a_listening_node_hands_its_port_over_on_live_replacement_and_rollback() {
    let e = need_net!();
    let v1 = listener_image(e, "v1");
    let v2 = listener_image(e, "v2");
    let v3 = listener_image(e, "v3");
    let name = |f: &Path| f.file_name().expect("f").to_string_lossy().to_string();
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
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: listener }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: 700, intervalMs: 10, arena: true }} }}\n    - {{ id: lst, contract: {} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: lst.frame }}\n      - {{ from: lst.frame, to: sink.frame }}\n",
        name(&v1)
    );
    let resolver = HostResolver::new(e.work.join("contracts"));
    let graph = build_graph(&text, &resolver).expect("graph");
    let services = HostServices::default();
    let mut engine = Engine::build(
        graph,
        &default_backends().with(e.backend.clone()),
        &services,
    )
    .expect("build");
    let request = |f: &Path, probation: u64| {
        ReplacementRequest::resolve(
            &resolver,
            "lst",
            &NodeReference::Contract(name(f)),
            Default::default(),
            probation,
        )
        .expect("request")
    };
    let mut driver = Driver {
        control: engine.control(),
        steps: vec![(150, request(&v2, 20)), (400, request(&v3, 200))],
    };
    let clients = in_ns("ext1", &format!("poll {LAN_HOST} 2151 15 9000"));
    let report = engine.run(&RunOptions::default(), &mut driver);
    let seen = helper_says(clients);
    let logs = services.logs.events();
    eprintln!("replies: {seen}");
    for r in &report.replacements {
        eprintln!(
            "  {:?} cycle {} {:?} cutover_us={:?}",
            r.kind, r.cycle, r.code, r.cutover_us
        );
    }
    for l in logs
        .iter()
        .filter(|l| l.message.contains("promoted") || l.message.contains("standby"))
    {
        eprintln!("  [{} g{}] {}", l.node, l.generation, l.message);
    }
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{logs:?}"
    );
    use mvf_engine::ReplacementEventKind as K;
    let kinds: Vec<K> = report.replacements.iter().map(|r| r.kind).collect();
    assert!(
        kinds.contains(&K::Promoted) && kinds.contains(&K::RolledBack),
        "{kinds:?}"
    );
    let runs: Vec<(&str, u32)> = seen
        .split_whitespace()
        .filter_map(|t| t.split_once('*').map(|(r, n)| (r, n.parse().unwrap_or(0))))
        .collect();
    let order: Vec<&str> = runs
        .iter()
        .map(|(r, _)| *r)
        .filter(|r| *r != "err")
        .collect();
    let first = |v: &str| order.iter().position(|r| *r == v);
    assert!(
        first("v1") < first("v2"),
        "v1 served first, then v2: {seen}"
    );
    // Promotion v1 -> v2 is seamless for new connections: no failure between them.
    let v1_last = runs.iter().rposition(|(r, _)| *r == "v1").expect("v1");
    let v2_first = runs.iter().position(|(r, _)| *r == "v2").expect("v2");
    assert!(
        runs[v1_last..v2_first].iter().all(|(r, _)| *r != "err"),
        "no refused connection at the v1 -> v2 handover: {seen}"
    );
    // After v3 crashed, the rollback gave the port back to v2, which serves until the end.
    assert_eq!(
        runs.iter()
            .rev()
            .find(|(r, _)| *r != "err")
            .map(|(r, _)| *r),
        Some("v2"),
        "{seen}"
    );
    // Nothing is left: one table/namespace per live generation, all gone after the run.
    let tables = String::from_utf8_lossy(
        &Command::new("nft")
            .args(["list", "tables"])
            .output()
            .expect("nft")
            .stdout,
    )
    .to_string();
    assert!(!tables.contains("mvf_"), "{tables}");
}

// ---- Display Fabric (#30 part 2): OCI nodes present through bind-mounted surfaces only ----

fn display_services(e: &Env, name: &str) -> (HostServices, PathBuf) {
    let root = e.work.join(format!("display-{name}"));
    let s = HostServices {
        display: std::sync::Arc::new(mvf_display::DisplayRegistry::new(&root)),
        ..HostServices::default()
    };
    (s, root)
}

fn display_engine(e: &Env, text: &str, s: &HostServices) -> Engine {
    let resolver = HostResolver::new(e.work.join("contracts"));
    let graph = build_graph(text, &resolver).expect("graph");
    Engine::build(graph, &default_backends().with(e.backend.clone()), s).expect("build")
}

fn json_values(s: &HostServices, node: &str) -> Vec<serde_json::Value> {
    s.recorder
        .records()
        .into_iter()
        .filter(|r| r.node == node)
        .filter_map(|r| match r.value {
            RecordedValue::Json { value } => Some(value),
            _ => None,
        })
        .collect()
}

fn wait_surfaces(root: &Path, n: usize) -> Vec<mvf_display::Discovered> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let found = mvf_display::discover(root);
        if found.len() >= n || std::time::Instant::now() > deadline {
            return found;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn an_oci_preview_presents_through_bind_mounted_surfaces_and_survives_a_host_disconnect() {
    let e = need_env!();
    let (_, contract) = node_image(
        e,
        "mvf-e2e/preview:1",
        &repo().join("modules/py-camera-preview"),
        "preview.py",
    );
    let (s, root) = display_services(e, "preview");
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: oci-preview }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 64, height: 48, channels: 3, frames: 90, intervalMs: 20, arena: true }} }}\n    - {{ id: prev, contract: {} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: prev.frame }}\n      - {{ from: prev.status, to: sink.json }}\n",
        contract.file_name().expect("f").to_string_lossy()
    );
    let mut engine = display_engine(e, &text, &s);
    let row = 64 * 3;
    let (report, shown, marked, latency) = std::thread::scope(|scope| {
        let run = scope.spawn(|| engine.run(&RunOptions::default(), &mut NullObserver));
        let found = wait_surfaces(&root, 2);
        assert_eq!(found.len(), 2, "live + detail surfaces");
        let open = || mvf_display::SurfaceReader::open(&found[0].surface).expect("host");
        let mut host = Some(open());
        let (mut px, mut shown, mut marked, mut clicked, mut latency) =
            (Vec::new(), 0u32, 0u32, None, Vec::new());
        let mut beat = std::time::Instant::now();
        while !run.is_finished() {
            match host.as_mut() {
                // Disconnect after 25 frames, reconnect 400 ms later: the node never notices.
                Some(_) if shown == 25 && clicked.is_some() => {
                    drop(host.take()); // the host goes away (clean detach)
                    std::thread::sleep(Duration::from_millis(400));
                    host = Some(open());
                    shown += 1;
                }
                Some(h) => {
                    if beat.elapsed() > Duration::from_millis(200) {
                        h.update_status(|_| {});
                        beat = std::time::Instant::now();
                    }
                    if let Some(info) = h.take_latest(&mut px) {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_micros() as u64)
                            .unwrap_or(0);
                        latency.push(now.saturating_sub(info.timestamp_us));
                        shown += 1;
                        let base = px[0];
                        match clicked {
                            None if shown >= 3 => {
                                h.push(&mvf_display::InputEvent::pointer(
                                    mvf_display::InputKind::PointerDown,
                                    10.0,
                                    12.0,
                                ));
                                clicked = Some(info.seq);
                            }
                            Some(at) if info.seq > at + 2 => {
                                let i = 12 * row + 10 * 3;
                                assert_eq!(px[i], 255 - base.wrapping_add(i as u8));
                                marked += 1;
                            }
                            _ => {}
                        }
                    }
                }
                None => {}
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        latency.sort_unstable();
        (run.join().expect("run"), shown, marked, latency)
    });
    let logs = s.logs.events();
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{logs:?}"
    );
    assert_eq!(
        report
            .nodes
            .iter()
            .find(|n| n.id == "prev")
            .map(|n| n.backend.as_str()),
        Some("oci")
    );
    let v = json_values(&s, "sink");
    assert_eq!(v.len(), 90, "the host disconnect cost the pipeline nothing");
    assert_eq!(v.last().expect("last")["roi"], serde_json::json!([10, 12]));
    assert!(
        v.iter().any(|x| x["hostAttached"] == false),
        "the node saw the disconnect"
    );
    assert_eq!(
        v.last().expect("last")["hostAttached"],
        true,
        "and the reconnect"
    );
    assert!(shown >= 50 && marked >= 10, "{shown} {marked}");
    assert!(
        logs.iter()
            .any(|l| l.message.contains("bind-mounted at /mvf/display")),
        "{logs:?}"
    );
    let p = |q: f64| latency[((latency.len() - 1) as f64 * q).round() as usize] as f64 / 1000.0;
    eprintln!(
        "OCI preview: {shown} frames shown through the bind-mounted surface; present -> host ms p50={:.2} p95={:.2} p99={:.2}",
        p(0.5),
        p(0.95),
        p(0.99)
    );
    let run_dir = s.display.run_dir().expect("run dir");
    drop(engine);
    drop(s);
    assert!(!run_dir.exists(), "surfaces removed with the engine");
    assert!(list_containers(&e.backend).is_empty());
}

fn probe_module(e: &Env, name: &str, display: &str, host_root: &Path) -> PathBuf {
    let module = e.work.join(name);
    std::fs::create_dir_all(&module).expect("dir");
    std::fs::write(
        module.join("probe.py"),
        format!(
            "import os\nfrom mvf_sdk import analysis, run_analyzer\nfrom mvf_sdk import display\ndef analyze(payload, meta):\n    d = '/mvf/display'\n    return analysis(value={{\n        'surfacesEnv': os.environ.get('MVF_DISPLAY_SURFACES'),\n        'displayDir': sorted(os.listdir(d)) if os.path.isdir(d) else None,\n        'displayEnv': [k for k in ('DISPLAY', 'WAYLAND_DISPLAY', 'XAUTHORITY', 'WAYLAND_SOCKET') if k in os.environ],\n        'hostRootVisible': os.path.exists({root:?}),\n        'x11Socket': os.path.exists('/tmp/.X11-unix'),\n        'presented': [s.present(bytes([77]) * 256, 16, 16, 'gray8') for s in display.surfaces()],\n    }})\nrun_analyzer('e2e.probe', analyze)\n",
            root = host_root.display().to_string()
        ),
    )
    .expect("py");
    std::fs::write(
        module.join("node.yaml"),
        format!(
            "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: e2e.{name}, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [ {{ name: frame, channel: data, type: frame }} ]\n    outputs: [ {{ name: status, channel: data, type: json }} ]\n  artifacts: [ {{ backend: worker, runtime: python, entry: probe.py, protocol: 1 }} ]\n  lifecycle: {{ readinessTimeoutMs: 20000 }}\n  permissions:\n    display: {display}\n"
        ),
    )
    .expect("yaml");
    let (_, contract) = node_image(e, &format!("mvf-e2e/{name}:1"), &module, "probe.py");
    contract
}

#[test]
fn an_oci_node_sees_only_its_own_surfaces_and_no_display_server() {
    let e = need_env!();
    let (s, root) = display_services(e, "probe");
    let granted = probe_module(
        e,
        "probe-granted",
        "{ mode: managed-surface, maxWidth: 64, maxHeight: 64 }",
        &root,
    );
    let denied = probe_module(e, "probe-denied", "{ mode: none }", &root);
    let name = |f: &Path| f.file_name().expect("f").to_string_lossy().to_string();
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: oci-probe }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: 3, arena: true }} }}\n    - {{ id: granted, contract: {} }}\n    - {{ id: denied, contract: {} }}\n    - {{ id: sa, builtin: record }}\n    - {{ id: sb, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: granted.frame }}\n      - {{ from: cam.frame, to: denied.frame }}\n      - {{ from: granted.status, to: sa.json }}\n      - {{ from: denied.status, to: sb.json }}\n",
        name(&granted),
        name(&denied)
    );
    let mut engine = display_engine(e, &text, &s);
    let report = engine.run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        s.logs.events()
    );
    let a = json_values(&s, "sa").pop().expect("granted");
    let b = json_values(&s, "sb").pop().expect("denied");
    eprintln!("granted node sees: {a}\ndenied node sees: {b}");
    assert_eq!(a["surfacesEnv"], "/mvf/display/0.surface");
    assert_eq!(
        a["displayDir"],
        serde_json::json!(["0.surface", "0.surface.writer.lock"]),
        "its own surface and writer lock: no host lock, no other node's surface"
    );
    assert_eq!(a["presented"], serde_json::json!([true]));
    assert_eq!(b["surfacesEnv"], serde_json::Value::Null);
    assert_eq!(
        b["displayDir"],
        serde_json::json!([]),
        "nothing mounted without a grant"
    );
    for v in [&a, &b] {
        assert_eq!(v["displayEnv"], serde_json::json!([]));
        assert_eq!(
            v["hostRootVisible"], false,
            "the rendezvous directory is not reachable"
        );
        assert_eq!(v["x11Socket"], false);
    }
    // What the granted container presented reached the host intact.
    let found = mvf_display::discover(&root);
    assert_eq!(found.len(), 1, "only the granted node has a surface");
    let mut host = mvf_display::SurfaceReader::open(&found[0].surface).expect("host");
    let mut px = Vec::new();
    let info = host.take_latest(&mut px).expect("frame from the container");
    assert_eq!((info.width, info.height, info.seq), (16, 16, 3));
    assert!(px.iter().all(|&g| g == 77));
}

#[test]
fn an_oci_presenter_that_crashes_restarts_into_the_same_window_and_nothing_is_left() {
    let e = need_env!();
    let module = e.work.join("crash-presenter");
    std::fs::create_dir_all(&module).expect("dir");
    // Crashes once, when the surface (shared by every generation) reaches frame 30.
    std::fs::write(
        module.join("presenter.py"),
        "import os\nfrom mvf_sdk import analysis, run_analyzer\nfrom mvf_sdk import display\ndef analyze(payload, meta):\n    s = display.surfaces()[0]\n    shown = s.present(bytes([90]) * (32 * 24), 32, 24, 'gray8')\n    if s.sequence == 30:\n        os._exit(3)\n    return analysis(value={'shown': shown, 'pid': os.getpid(), 'sequence': s.sequence})\nrun_analyzer('e2e.crash-presenter', analyze)\n",
    )
    .expect("py");
    std::fs::write(
        module.join("node.yaml"),
        "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: { id: e2e.crash-presenter, version: 1.0.0 }\nspec:\n  ports:\n    inputs: [ { name: frame, channel: data, type: frame } ]\n    outputs: [ { name: status, channel: data, type: json } ]\n  artifacts: [ { backend: worker, runtime: python, entry: presenter.py, protocol: 1 } ]\n  lifecycle: { readinessTimeoutMs: 20000 }\n  permissions:\n    display: { mode: managed-surface, maxWidth: 64, maxHeight: 64 }\n",
    )
    .expect("yaml");
    let (_, contract) = node_image(e, "mvf-e2e/crash-presenter:1", &module, "presenter.py");
    let (s, root) = display_services(e, "crash");
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: oci-crash }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: 80, intervalMs: 15, arena: true }} }}\n    - {{ id: show, contract: {}, onError: {{ mode: restart, limit: 2, backoffMs: 10 }} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: show.frame }}\n      - {{ from: show.status, to: sink.json }}\n",
        contract.file_name().expect("f").to_string_lossy()
    );
    let mut engine = display_engine(e, &text, &s);
    let (report, seqs) = std::thread::scope(|scope| {
        let run = scope.spawn(|| engine.run(&RunOptions::default(), &mut NullObserver));
        let found = wait_surfaces(&root, 1);
        // One reader for the whole run: the window is never reopened across the restart.
        let mut host = mvf_display::SurfaceReader::open(&found[0].surface).expect("host");
        let (mut px, mut seqs) = (Vec::new(), Vec::new());
        while !run.is_finished() {
            if let Some(info) = host.take_latest(&mut px) {
                seqs.push(info.seq);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(host.rejected, 0);
        (run.join().expect("run"), seqs)
    });
    let logs = s.logs.events();
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{logs:?}"
    );
    let show = report.nodes.iter().find(|n| n.id == "show").expect("show");
    assert_eq!(show.restarts, 1, "{logs:?}");
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "one sequence across the restart: {seqs:?}"
    );
    assert!(
        seqs.iter().any(|&q| q < 30) && seqs.iter().any(|&q| q > 31),
        "{seqs:?}"
    );
    // The writer pid is namespace-local (1 in every container); the engine's log shows the second
    // container that took over the same surface.
    let containers = logs
        .iter()
        .filter(|l| l.node == "show" && l.message.starts_with("ready pid="))
        .count();
    assert_eq!(
        containers, 2,
        "a new container took over the same surface: {logs:?}"
    );
    let v = json_values(&s, "sink");
    assert!(v.iter().all(|x| x["shown"] == true));
    eprintln!(
        "OCI presenter crash at frame 30: restarts={}, window kept {} frames in one sequence (last {:?}), {containers} containers, {} statuses",
        show.restarts,
        seqs.len(),
        seqs.last(),
        v.len()
    );
    let run_dir = s.display.run_dir().expect("run dir");
    drop(engine);
    drop(s);
    assert!(!run_dir.exists(), "the surface is removed with the engine");
    assert!(
        list_containers(&e.backend).is_empty(),
        "no container is left"
    );
    let bundles: Vec<String> = std::fs::read_dir(e.work.join("state/bundles"))
        .map(|d| {
            d.flatten()
                .map(|x| x.file_name().to_string_lossy().to_string())
                .filter(|n| n.contains("-show-g"))
                .collect()
        })
        .unwrap_or_default();
    assert!(bundles.is_empty(), "no bundle is left: {bundles:?}");
}
