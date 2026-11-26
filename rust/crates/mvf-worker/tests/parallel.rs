//! Per-node parallelism with real worker processes (pipelined executor).

use std::path::{Path, PathBuf};

use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_worker::WorkerBackend;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn run(parallelism: u32, pipelined: bool) -> (RunReport, HostServices) {
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: par }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: 24 }} }}\n    - {{ id: w, contract: slow-pid.yaml, parallelism: {parallelism} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: w.frame }}\n      - {{ from: w.class, to: sink.class }}\n"
    );
    let graph = build_graph(&text, &HostResolver::new(fixtures())).expect("graph");
    let services = HostServices {
        arena_config: mvf_memory::ArenaConfig {
            slot_count: pipelined::arena_slots_for(&graph, pipelined::DEFAULT_QUEUE, 16),
            ..mvf_memory::ArenaConfig::default()
        },
        ..HostServices::default()
    };
    let mut e = Engine::build(
        graph,
        &default_backends().with(WorkerBackend::from_env()),
        &services,
    )
    .expect("build");
    let options = RunOptions {
        pipelined,
        ..RunOptions::default()
    };
    (e.run(&options, &mut NullObserver), services)
}

#[test]
fn parallel_instances_share_the_work_keep_order_and_raise_throughput() {
    let (serial, _) = run(1, false);
    let (parallel, s) = run(4, true);
    assert_eq!(
        parallel.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        s.logs.events()
    );
    let records = s.recorder.records();
    let cycles: Vec<u64> = records.iter().map(|r| r.cycle).collect();
    assert_eq!(
        cycles,
        (1..=24).collect::<Vec<_>>(),
        "results re-ordered by cycle"
    );
    let details: Vec<String> = records
        .iter()
        .filter_map(|r| match &r.value {
            RecordedValue::Classification(c) => c.details.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(
        details,
        (1..=24).map(|n| n.to_string()).collect::<Vec<_>>(),
        "each cycle got its own frame"
    );
    let mut pids: Vec<u64> = records
        .iter()
        .filter_map(|r| match &r.value {
            RecordedValue::Classification(c) => c.measurement.map(|m| m as u64),
            _ => None,
        })
        .collect();
    pids.sort_unstable();
    pids.dedup();
    assert_eq!(pids.len(), 4, "four processes shared the work: {pids:?}");
    assert!(
        parallel.cycles_ms < serial.cycles_ms * 0.6,
        "4 instances should beat 1: {:.0} ms vs {:.0} ms",
        parallel.cycles_ms,
        serial.cycles_ms
    );
}
