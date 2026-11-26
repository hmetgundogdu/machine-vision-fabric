//! Live replacement across backends with real worker processes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_worker::WorkerBackend;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn resolver() -> HostResolver {
    HostResolver::new(fixtures()).with_module_root(repo().join("modules"))
}

struct Driver {
    control: ControlHandle,
    at: u64,
    request: Option<ReplacementRequest>,
    generations: BTreeMap<u64, u64>,
}

impl Observer for Driver {
    fn node(&mut self, cycle: u64, node: &str, generation: u64, outcome: &NodeOutcome) {
        if node == "proc" && matches!(outcome, NodeOutcome::Executed { .. }) {
            assert!(
                self.generations.insert(cycle, generation).is_none(),
                "cycle {cycle} executed twice"
            );
        }
    }
    fn cycle_completed(&mut self, cycle: u64) {
        if cycle + 1 == self.at
            && let Some(r) = self.request.take()
        {
            assert!(self.control.replace(r));
        }
    }
}

fn run(
    proc_ref: &str,
    at: u64,
    candidate: NodeReference,
    frames: u64,
) -> (RunReport, HostServices, BTreeMap<u64, u64>) {
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: live-worker }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: {frames}, intervalMs: 5, arena: true }} }}\n    - {{ id: proc, {proc_ref} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: proc.frame }}\n      - {{ from: proc.frame, to: sink.frame }}\n"
    );
    let services = HostServices::default();
    let graph = build_graph(&text, &resolver()).expect("graph");
    let mut engine = Engine::build(
        graph,
        &default_backends().with(WorkerBackend::from_env()),
        &services,
    )
    .expect("build");
    let request =
        ReplacementRequest::resolve(&resolver(), "proc", &candidate, Default::default(), 5)
            .expect("resolve");
    let mut driver = Driver {
        control: engine.control(),
        at,
        request: Some(request),
        generations: BTreeMap::new(),
    };
    let report = engine.run(&RunOptions::default(), &mut driver);
    (report, services, driver.generations)
}

fn event(report: &RunReport, kind: ReplacementEventKind) -> &ReplacementEvent {
    report
        .replacements
        .iter()
        .find(|e| e.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} in {:?}", report.replacements))
}

fn sink_sequences(services: &HostServices) -> Vec<(u64, u64, String)> {
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

fn spawned(services: &HostServices) -> Vec<u32> {
    services
        .logs
        .events()
        .iter()
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
        .is_ok_and(|s| s.success())
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
}

fn assert_lossless(report: &RunReport, services: &HostServices, n: u64) {
    let seqs = sink_sequences(services);
    assert_eq!(
        seqs.iter().map(|(c, s, _)| (*c, *s)).collect::<Vec<_>>(),
        (1..=n).map(|c| (c, c)).collect::<Vec<_>>(),
        "every cycle exactly once"
    );
    assert_eq!(
        report.data_plane.arena.map(|a| a.in_use),
        Some(0),
        "no leaked slot"
    );
    for pid in spawned(services) {
        assert!(!alive(pid), "worker {pid} outlived the run");
    }
}

#[test]
fn in_process_node_is_replaced_by_a_python_worker_while_running() {
    let (report, services, generations) = run(
        "builtin: invert",
        5,
        NodeReference::Module("py.invert-transformer".into()),
        40,
    );
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() }
    );
    assert_lossless(&report, &services, 40);
    let requested = event(&report, ReplacementEventKind::Requested).cycle;
    let promoted = event(&report, ReplacementEventKind::Promoted);
    // The worker was spawned and warmed while v1 kept serving cycles.
    assert!(
        promoted.cycle > requested,
        "cycles ran during preparation: requested {requested}, promoted {}",
        promoted.cycle
    );
    assert!(promoted.preparation_ms.unwrap_or(0.0) > 0.0);
    assert!(promoted.cutover_us.is_some());
    assert_eq!(
        promoted.to_version.as_deref(),
        Some("py.invert-transformer@1.0.0")
    );
    for (cycle, generation) in &generations {
        assert_eq!(*generation, if *cycle < promoted.cycle { 1 } else { 2 });
    }
    // Identical inversion in Rust and Python: checksums are continuous across the cut-over.
    let sums: Vec<String> = sink_sequences(&services)
        .into_iter()
        .map(|(_, _, s)| s)
        .collect();
    let reference: Vec<String> = (1..=40u64)
        .map(|s| {
            let px: Vec<u8> = mvf_engine::builtins::SyntheticSource::pixels(0, s, 8)
                .iter()
                .map(|b| 255 - b)
                .collect();
            format!("{:016x}", fnv1a64(&px))
        })
        .collect();
    assert_eq!(sums, reference);
    let proc = report.nodes.iter().find(|n| n.id == "proc").expect("proc");
    assert_eq!(proc.backend, "worker");
}

#[test]
fn worker_candidate_that_never_becomes_ready_is_rejected_and_reaped() {
    let (report, services, generations) = run(
        "builtin: invert",
        3,
        NodeReference::Contract("never-ready-processor.yaml".into()),
        200, // 1 s of cycles, longer than the candidate's 500 ms readiness budget
    );
    let rejected = event(&report, ReplacementEventKind::Rejected);
    assert_eq!(rejected.code, Some("replace.readiness-failed"));
    assert!(generations.values().all(|g| *g == 1));
    assert_lossless(&report, &services, 200);
    assert_eq!(
        spawned(&services).len(),
        1,
        "the candidate was spawned once and reaped"
    );
}

#[test]
fn crashing_worker_candidate_rolls_back_to_the_previous_worker() {
    let (report, services, generations) = run(
        "module: py.invert-transformer",
        4,
        NodeReference::Contract("crash-processor.yaml".into()),
        30,
    );
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() }
    );
    let promoted = event(&report, ReplacementEventKind::Promoted).cycle;
    let rolled = event(&report, ReplacementEventKind::RolledBack);
    assert_eq!(rolled.code, Some("replace.candidate-failed"));
    assert!(
        rolled
            .message
            .as_deref()
            .unwrap_or("")
            .contains("exited with code 9")
    );
    assert_eq!(
        rolled.cycle, promoted,
        "the first candidate cycle failed and was replayed"
    );
    assert!(
        generations.values().all(|g| *g == 1),
        "every cycle was served by v1"
    );
    assert_lossless(&report, &services, 30);
}
