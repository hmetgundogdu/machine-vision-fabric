//! Worker backend against real Python processes using the existing MVF Python SDK.
//! Requires a Python 3 interpreter (`MVF_PYTHON`, default `python3`/`python`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_worker::WorkerBackend;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn registry() -> BackendRegistry {
    default_backends().with(WorkerBackend::from_env())
}

/// `cam → <node> → sink`, wiring the node's `out` port to the sink's `sink_port`.
fn pipeline(node_ref: &str, out: &str, sink_port: &str, cycles: u64) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: w }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 4, height: 2 }} }}\n    - {{ id: w, {node_ref} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: w.frame }}\n      - {{ from: w.{out}, to: sink.{sink_port} }}\n  run: {{ maxCycles: {cycles} }}\n"
    )
}

fn engine(text: &str, services: &HostServices) -> Result<Engine, BuildError> {
    let resolver = HostResolver::new(fixtures()).with_module_root(repo().join("modules"));
    let graph = build_graph(text, &resolver).expect("valid graph");
    Engine::build(graph, &registry(), services)
}

fn run(text: &str) -> (RunReport, HostServices) {
    let services = HostServices::default();
    let mut e = engine(text, &services).expect("build");
    let report = e.run(&RunOptions::default(), &mut NullObserver);
    (report, services)
}

fn classifier(contract: &str, cycles: u64) -> String {
    pipeline(&format!("contract: {contract}"), "class", "class", cycles)
}

fn pids(services: &HostServices) -> Vec<u32> {
    services
        .logs
        .events()
        .iter()
        .filter(|e| e.level == "supervisor")
        .filter_map(|e| e.message.strip_prefix("spawned pid="))
        .filter_map(|p| p.parse().ok())
        .collect()
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        // CSV quotes each field, so `"1234"` cannot match a memory column like `"12,345 K"`.
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
        .unwrap_or(false)
}

fn assert_no_orphans(services: &HostServices) {
    let pids = pids(services);
    assert!(!pids.is_empty(), "no worker was spawned");
    for pid in pids {
        assert!(!alive(pid), "worker pid {pid} outlived its handle");
    }
}

fn frames(services: &HostServices) -> Vec<(u64, u64, String)> {
    services
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Frame {
                sequence, checksum, ..
            } => Some((r.cycle, sequence, checksum)),
            _ => None,
        })
        .collect()
}

fn failure(report: &RunReport) -> (&str, &NodeError) {
    match &report.end {
        EndReason::NodeFailed { node, error, .. } | EndReason::StartFailed { node, error } => {
            (node.as_str(), error)
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn in_process_and_worker_nodes_share_one_graph_interface() {
    let (in_proc, a) = run(&pipeline("builtin: invert", "frame", "frame", 6));
    let (worker, b) = run(&pipeline(
        "module: py.invert-transformer",
        "frame",
        "frame",
        6,
    ));
    assert_eq!(in_proc.end, EndReason::CycleLimit { limit: 6 });
    assert_eq!(worker.end, EndReason::CycleLimit { limit: 6 });
    assert_eq!(frames(&a).len(), 6);
    assert_eq!(
        frames(&a),
        frames(&b),
        "same cycles, sequences and bytes from the Python worker"
    );
    let w = worker.nodes.iter().find(|n| n.id == "w").expect("w");
    assert_eq!(w.backend, "worker");
    assert_no_orphans(&b);
}

#[test]
fn one_process_serves_every_cycle_and_its_logs_reach_the_host() {
    let (report, services) = run(&classifier("pid-classifier.yaml", 5));
    assert_eq!(report.cycles_completed, 5);
    let spawned = pids(&services);
    assert_eq!(spawned.len(), 1, "spawned once, not per frame");
    let measured: Vec<f64> = services
        .recorder
        .records()
        .iter()
        .filter_map(|r| match &r.value {
            RecordedValue::Classification(c) => c.measurement,
            _ => None,
        })
        .collect();
    assert_eq!(measured, vec![f64::from(spawned[0]); 5]);
    let logs = services.logs.events();
    assert!(
        logs.iter()
            .any(|e| e.node == "w" && e.level == "info" && e.message == "frame 3 (8 bytes)"),
        "worker log() lines are forwarded: {logs:?}"
    );
    assert!(
        logs.iter().any(|e| e.message.contains("outcome=Graceful")),
        "graceful shutdown: {logs:?}"
    );
    assert_no_orphans(&services);
}

#[test]
fn existing_sdk_modules_run_unchanged() {
    let (report, services) = run(&pipeline(
        "module: py.brightness-classifier",
        "class",
        "class",
        3,
    ));
    assert_eq!(report.cycles_completed, 3);
    let labels: Vec<String> = services
        .recorder
        .records()
        .iter()
        .filter_map(|r| match &r.value {
            RecordedValue::Classification(c) => Some(c.label.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(labels, ["ok", "ok", "ok"]);

    // Analyzer: derived frame + classification + JSON metadata from one reply.
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: a }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source }\n    - { id: w, module: py.brightness-analyzer }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: w.frame }\n      - { from: w.frame, to: sink.frame }\n      - { from: w.class, to: sink.class }\n      - { from: w.meta, to: sink.json }\n  run: { maxCycles: 2 }\n";
    let (report, services) = run(text);
    assert_eq!(report.cycles_completed, 2, "{:?}", report.end);
    let ports: Vec<String> = services
        .recorder
        .records()
        .iter()
        .map(|r| r.port.clone())
        .collect();
    assert_eq!(ports, ["class", "frame", "json", "class", "frame", "json"]);
    assert_no_orphans(&services);
}

#[test]
fn asynchronous_warmup_is_awaited_within_the_readiness_budget() {
    let (report, services) = run(&classifier("slow-ready.yaml", 2));
    assert_eq!(report.cycles_completed, 2);
    let (report, _) = run(&pipeline(
        "module: py.warmup-classifier",
        "class",
        "class",
        2,
    ));
    assert_eq!(report.cycles_completed, 2);
    assert_no_orphans(&services);
}

#[test]
fn readiness_timeout_kills_the_worker() {
    let started = Instant::now();
    let (report, services) = run(&classifier("never-ready.yaml", 2));
    let (node, error) = failure(&report);
    assert_eq!(node, "w");
    assert_eq!(error.kind, NodeErrorKind::Timeout);
    assert!(matches!(report.end, EndReason::StartFailed { .. }));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_no_orphans(&services);
}

#[test]
fn exit_before_hello_reports_code_and_forwards_stderr() {
    let (report, services) = run(&classifier("exit-before-hello.yaml", 2));
    let (_, error) = failure(&report);
    assert_eq!(error.kind, NodeErrorKind::ProcessExited);
    assert_eq!(error.exit_code, Some(3));
    assert!(
        services
            .logs
            .events()
            .iter()
            .any(|e| e.level == "stderr" && e.message.contains("failing before hello"))
    );
}

#[test]
fn protocol_and_capability_mismatches_fail_closed() {
    let (report, services) = run(&classifier("protocol-v2.yaml", 1));
    assert_eq!(failure(&report).1.kind, NodeErrorKind::Protocol);
    assert_no_orphans(&services);

    let (report, services) = run(&pipeline(
        "contract: wrong-capability.yaml",
        "frame",
        "frame",
        1,
    ));
    let (_, error) = failure(&report);
    assert_eq!(error.kind, NodeErrorKind::StartFailed);
    assert!(error.message.contains("Classifier"), "{}", error.message);
    assert_no_orphans(&services);
}

#[test]
fn execute_deadline_terminates_a_hung_worker() {
    let started = Instant::now();
    let (report, services) = run(&classifier("hang.yaml", 3));
    let (_, error) = failure(&report);
    assert_eq!(error.kind, NodeErrorKind::Timeout);
    assert_eq!(report.cycles_completed, 0);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_no_orphans(&services);
}

#[test]
fn cancellation_is_enforced_by_terminating_the_process() {
    let services = HostServices::default();
    let mut e = engine(&classifier("hang-no-deadline.yaml", 3), &services).expect("build");
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        trigger.cancel();
    });
    let started = Instant::now();
    let report = e.run(
        &RunOptions {
            max_cycles: None,
            cancel,
        },
        &mut NullObserver,
    );
    t.join().expect("join");
    let (_, error) = failure(&report);
    assert_eq!(error.kind, NodeErrorKind::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_no_orphans(&services);
}

#[test]
fn child_exit_mid_run_is_a_classified_process_fault() {
    let (report, services) = run(&classifier("crash-on-execute.yaml", 5));
    match &report.end {
        EndReason::NodeFailed { cycle, error, .. } => {
            assert_eq!(*cycle, 2);
            assert_eq!(error.kind, NodeErrorKind::ProcessExited);
            assert_eq!(error.exit_code, Some(7));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(report.cycles_completed, 1);
    assert_no_orphans(&services);
}

#[cfg(unix)]
#[test]
fn native_fault_reports_the_signal() {
    let (report, services) = run(&classifier("segfault.yaml", 2));
    let (_, error) = failure(&report);
    assert_eq!(error.kind, NodeErrorKind::ProcessExited);
    assert_eq!(error.signal, Some(11));
    assert!(error.message.contains("SIGSEGV"));
    assert_no_orphans(&services);
}

#[test]
fn managed_worker_error_is_not_a_process_fault() {
    let (report, services) = run(&classifier("error-reply.yaml", 3));
    let (_, error) = failure(&report);
    assert_eq!(error.kind, NodeErrorKind::Failed);
    assert!(error.message.contains("odd frames are rejected"));
    assert_no_orphans(&services);
}

#[test]
fn stubborn_worker_is_forcibly_terminated_after_the_drain_budget() {
    let started = Instant::now();
    let (report, services) = run(&classifier("ignore-shutdown.yaml", 1));
    assert_eq!(report.end, EndReason::CycleLimit { limit: 1 });
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "bounded by the 200 ms drain budget"
    );
    let logs = services.logs.events();
    assert!(
        logs.iter().any(|e| e.message.contains("outcome=Forced")),
        "{logs:?}"
    );
    assert_no_orphans(&services);
}

#[test]
fn foreign_offsets_and_corrupt_headers_are_rejected() {
    let (report, services) = run(&pipeline("contract: bad-offset.yaml", "frame", "frame", 1));
    assert_eq!(failure(&report).1.kind, NodeErrorKind::Protocol);
    assert_no_orphans(&services);

    let (report, services) = run(&pipeline("contract: bad-header.yaml", "frame", "frame", 1));
    let (_, error) = failure(&report);
    assert_eq!(error.kind, NodeErrorKind::InvalidOutput);
    assert!(error.message.contains("magic"), "{}", error.message);
    let arena = services.arena_if_created().expect("arena");
    assert_eq!(arena.in_use(), 0, "rejected output slot was reclaimed");
    assert_no_orphans(&services);
}

#[test]
fn unsupported_security_and_limit_requests_fail_closed_at_build() {
    for contract in ["sandbox-required.yaml", "limits-required.yaml"] {
        let err = engine(&classifier(contract, 1), &HostServices::default())
            .expect_err("must not be placed on a process worker");
        assert_eq!(err.error.kind, NodeErrorKind::Unsupported, "{contract}");
    }
}

#[test]
fn dropping_a_started_engine_reaps_its_workers() {
    let services = HostServices::default();
    let mut e = engine(&classifier("pid-classifier.yaml", 1), &services).expect("build");
    e.start(&CancellationToken::new()).expect("start");
    let pid = pids(&services)[0];
    assert!(alive(pid));
    drop(e);
    assert!(!alive(pid), "worker {pid} survived engine drop");
}

#[test]
fn relative_contract_paths_resolve_the_entry_once() {
    // A pipeline whose package directory is given relative to the current directory.
    let here = std::env::current_dir().expect("cwd");
    let relative = pathdiff(&fixtures(), &here);
    let resolver = HostResolver::new(&relative);
    let graph = build_graph(&classifier("pid-classifier.yaml", 2), &resolver).expect("graph");
    let services = HostServices::default();
    let mut e = Engine::build(graph, &registry(), &services).expect("build");
    let report = e.run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(report.cycles_completed, 2, "{:?}", services.logs.events());
}

/// `target` relative to `base` (both absolute), via `..` segments.
fn pathdiff(target: &Path, base: &Path) -> PathBuf {
    let t: Vec<_> = target.components().collect();
    let b: Vec<_> = base.components().collect();
    let common = t.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c);
    }
    out
}
