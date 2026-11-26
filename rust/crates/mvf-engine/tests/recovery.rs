//! Failure policy (fail/skip/restart), backpressure (stall/drop) and epoch checkpoints.

use std::path::PathBuf;

use mvf_engine::*;
use mvf_graph::build_graph;

fn pipeline(cam: &str, mid: &str, run: &str) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: r }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, {cam} }}\n    - {{ id: mid, {mid} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: mid.frame }}\n      - {{ from: mid.frame, to: sink.frame }}\n  run: {{ {run} }}\n"
    )
}

fn run_with(text: &str, options: RunOptions, services: &HostServices) -> RunReport {
    let graph = build_graph(text, &HostResolver::new(".")).expect("graph");
    let mut e = Engine::build(graph, &default_backends(), services).expect("build");
    e.run(&options, &mut NullObserver)
}

fn run(text: &str) -> (RunReport, HostServices) {
    let s = HostServices::default();
    (run_with(text, RunOptions::default(), &s), s)
}

fn sequences(s: &HostServices) -> Vec<(u64, u64)> {
    s.recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Frame { sequence, .. } => Some((r.cycle, sequence)),
            _ => None,
        })
        .collect()
}

fn kinds(r: &RunReport) -> Vec<RecoveryKind> {
    r.recoveries
        .iter()
        .filter(|e| e.node.is_some())
        .map(|e| e.kind)
        .collect()
}

fn stats<'a>(r: &'a RunReport, id: &str) -> &'a NodeStats {
    r.nodes.iter().find(|n| n.id == id).expect("node")
}

#[test]
fn restart_retries_the_same_inputs_on_a_fresh_instance() {
    let (r, s) = run(&pipeline(
        "config: { frames: 10 }",
        "builtin: fault, config: { failAtCycle: 5, failGeneration: 1 }, onError: { mode: restart, limit: 2, backoffMs: 1 }",
        "",
    ));
    assert_eq!(r.end, EndReason::SourceExhausted { node: "cam".into() });
    assert_eq!(
        sequences(&s),
        (1..=10).map(|c| (c, c)).collect::<Vec<_>>(),
        "lossless: cycle 5 retried"
    );
    let mid = stats(&r, "mid");
    assert_eq!((mid.restarts, mid.generation), (1, 2));
    assert_eq!(kinds(&r), [RecoveryKind::Restarted]);
    assert_eq!(r.recoveries[0].cycle, 5);
}

#[test]
fn restart_budget_falls_through_to_fail() {
    let (r, _) = run(&pipeline(
        "config: { frames: 10 }",
        "builtin: fault, config: { failAtCycle: 5 }, onError: { mode: restart, limit: 2, backoffMs: 1 }",
        "",
    ));
    assert!(
        matches!(&r.end, EndReason::NodeFailed { node, cycle: 5, .. } if node == "mid"),
        "{:?}",
        r.end
    );
    assert_eq!(
        kinds(&r),
        [
            RecoveryKind::Restarted,
            RecoveryKind::Restarted,
            RecoveryKind::RestartsExhausted
        ]
    );
    assert_eq!(stats(&r, "mid").restarts, 2);
}

#[test]
fn backoff_is_exponential_and_capped() {
    let p = mvf_graph::FailurePolicy {
        mode: mvf_graph::FailureMode::Restart,
        limit: 0,
        backoff_ms: 100,
        max_backoff_ms: 450,
    };
    let ms: Vec<u128> = (1..=5).map(|a| p.backoff(a).as_millis()).collect();
    assert_eq!(ms, [100, 200, 400, 450, 450]);
    assert!(p.allows(1_000_000), "limit 0 = forever");
}

#[test]
fn skip_drops_only_the_failed_cycle() {
    let (r, s) = run(&pipeline(
        "config: { frames: 10 }",
        "builtin: fault, config: { failAtCycle: 5 }, onError: skip",
        "",
    ));
    assert_eq!(r.end, EndReason::SourceExhausted { node: "cam".into() });
    let cycles: Vec<u64> = sequences(&s).into_iter().map(|(c, _)| c).collect();
    assert_eq!(cycles, [1, 2, 3, 4, 6, 7, 8, 9, 10]);
    assert_eq!(stats(&r, "mid").failures_skipped, 1);
    assert_eq!(kinds(&r), [RecoveryKind::Skipped]);
}

#[test]
fn dotnet_role_defaults_end_on_source_failure_and_skip_mid_graph_failures() {
    let defaults = "onError: { sources: fail, nodes: skip }";
    let (r, _) = run(&pipeline(
        "config: { frames: 10 }",
        "builtin: fault, config: { failAtCycle: 3 }",
        defaults,
    ));
    assert_eq!(
        r.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "mid-graph failure skipped"
    );
    let (r, _) = run(&pipeline(
        "config: { frames: 10, failAtSequence: 3 }",
        "builtin: passthrough",
        defaults,
    ));
    assert!(
        matches!(&r.end, EndReason::NodeFailed { node, .. } if node == "cam"),
        "source failure ends the run"
    );
}

#[test]
fn restarted_source_resumes_its_cursor_only_with_checkpoints() {
    let cam = "config: { frames: 8, failAtSequence: 4, failGeneration: 1 }, onError: { mode: restart, backoffMs: 1 }";
    // Without a checkpoint a fresh source starts over (what a reconnecting live camera does).
    let (r, s) = run(&pipeline(cam, "builtin: passthrough", ""));
    assert_eq!(stats(&r, "cam").restarts, 1);
    let seqs: Vec<u64> = sequences(&s).into_iter().map(|(_, q)| q).collect();
    assert_eq!(
        &seqs[..4],
        &[1, 2, 3, 1],
        "fresh instance restarted at sequence 1"
    );
    // With a checkpoint every cycle the restarted source continues exactly.
    let (r, s) = run(&pipeline(
        cam,
        "builtin: passthrough",
        "checkpoint: { everyCycles: 1 }",
    ));
    assert_eq!(stats(&r, "cam").restarts, 1);
    assert_eq!(sequences(&s), (1..=8).map(|c| (c, c)).collect::<Vec<_>>());
    assert!(kinds(&r).contains(&RecoveryKind::Restored));
}

/// cam (arena) feeds two in-process inverts; each needs an output slot, so a 2-slot arena can
/// serve only one of them per cycle.
fn two_branches(b_policy: &str) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: bp }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: 6, arena: true }} }}\n    - {{ id: a, builtin: invert }}\n    - {{ id: b, builtin: invert{b_policy} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: a.frame }}\n      - {{ from: cam.frame, to: b.frame }}\n      - {{ from: a.frame, to: sink.frame }}\n"
    )
}

fn small_arena() -> HostServices {
    HostServices {
        arena_config: mvf_memory::ArenaConfig {
            slot_size: 4096,
            slot_count: 2,
            dir: None,
        },
        ..HostServices::default()
    }
}

#[test]
fn full_arena_stalls_losslessly_by_default() {
    let s = small_arena();
    let r = run_with(&two_branches(""), RunOptions::default(), &s);
    match &r.end {
        EndReason::NodeFailed { node, cycle, error } => {
            assert_eq!((node.as_str(), *cycle), ("b", 1));
            assert_eq!(error.kind, NodeErrorKind::Backpressure);
            assert!(
                error.message.contains("backpressure: drop"),
                "actionable: {}",
                error.message
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn drop_policy_skips_the_starved_node_and_keeps_running() {
    let s = small_arena();
    let r = run_with(
        &two_branches(", backpressure: drop"),
        RunOptions::default(),
        &s,
    );
    assert_eq!(r.end, EndReason::SourceExhausted { node: "cam".into() });
    assert_eq!(stats(&r, "b").dropped, 6);
    assert_eq!(sequences(&s).len(), 6, "branch a is unaffected");
    assert_eq!(r.data_plane.arena.expect("arena").in_use, 0);
}

fn counter_pipeline(frames: u64) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: ckpt }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: {frames} }} }}\n    - {{ id: count, builtin: counter }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: count.frame }}\n      - {{ from: count.count, to: sink.json }}\n      - {{ from: cam.frame, to: sink.frame }}\n"
    )
}

fn counts(s: &HostServices) -> Vec<(u64, u64)> {
    s.recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Json { value } => Some((r.cycle, value["count"].as_u64().unwrap_or(0))),
            _ => None,
        })
        .collect()
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mvf-ckpt-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

struct CancelAfter(u64, CancellationToken);
impl Observer for CancelAfter {
    fn cycle_completed(&mut self, cycle: u64) {
        if cycle == self.0 {
            self.1.cancel();
        }
    }
}

#[test]
fn interrupted_run_resumes_after_the_last_epoch_and_clears_when_consumed() {
    let dir = tmp("resume");
    let text = counter_pipeline(40);
    let options = |cancel: CancellationToken| RunOptions {
        cancel,
        checkpoint_every: Some(10),
        checkpoint_dir: Some(dir.clone()),
        ..RunOptions::default()
    };

    // Run 1: interrupted after cycle 25 (cancellation stands in for an interruption).
    let s1 = HostServices::default();
    let graph = build_graph(&text, &HostResolver::new(".")).expect("graph");
    let mut e = Engine::build(graph, &default_backends(), &s1).expect("build");
    let cancel = CancellationToken::new();
    let r1 = e.run(&options(cancel.clone()), &mut CancelAfter(25, cancel));
    assert_eq!(r1.end, EndReason::Cancelled);
    assert_eq!(
        std::fs::read_to_string(dir.join("CURRENT"))
            .expect("published epoch")
            .trim(),
        "20"
    );
    assert!(dir.join("epoch-20/count.state").is_file() && dir.join("epoch-20/cam.state").is_file());
    assert!(!dir.join("epoch-10").exists(), "older epochs are removed");

    // Run 2: resumes after cycle 20 — numbering, source cursor and counter state all continue.
    let s2 = HostServices::default();
    let r2 = run_with(&text, options(CancellationToken::new()), &s2);
    assert_eq!(r2.resumed_from_cycle, Some(20));
    assert_eq!(r2.end, EndReason::SourceExhausted { node: "cam".into() });
    assert_eq!(
        sequences(&s2),
        (21..=40).map(|c| (c, c)).collect::<Vec<_>>()
    );
    assert_eq!(counts(&s2), (21..=40).map(|c| (c, c)).collect::<Vec<_>>());
    assert!(!dir.exists(), "a fully consumed run clears its checkpoint");
}

#[test]
fn memory_checkpoints_do_not_touch_disk_and_nothing_resumes_without_a_directory() {
    let s = HostServices::default();
    let r = run_with(
        &counter_pipeline(12),
        RunOptions {
            checkpoint_every: Some(5),
            ..RunOptions::default()
        },
        &s,
    );
    assert_eq!(r.resumed_from_cycle, None);
    let epochs: Vec<_> = r
        .recoveries
        .iter()
        .filter(|e| e.kind == RecoveryKind::Checkpointed)
        .map(|e| e.cycle)
        .collect();
    assert_eq!(epochs, [5, 10]);
    assert!(
        r.recoveries
            .iter()
            .all(|e| e.message.as_deref().unwrap_or("").contains("memory")
                || e.kind != RecoveryKind::Checkpointed)
    );
}
