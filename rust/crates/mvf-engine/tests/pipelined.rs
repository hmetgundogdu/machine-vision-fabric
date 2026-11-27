//! Pipelined executor — parity with `PipelinedGraphExecutorTests`, plus what the Rust executor adds
//! (primitives, loop, live operation and checkpoint resume in pipelined mode). The rule throughout:
//! a pipelined run produces exactly what the serial run produces.

use std::path::{Path, PathBuf};

use mvf_engine::*;
use mvf_graph::build_graph;
use serde_json::json;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn doc(nodes: &[&str], data: &[&str], control: &[&str], run: &str) -> String {
    let list = |items: &[&str]| -> String {
        if items.is_empty() {
            " []\n".into()
        } else {
            format!(
                "\n{}",
                items
                    .iter()
                    .map(|i| format!("      - {i}\n"))
                    .collect::<String>()
            )
        }
    };
    let nodes: String = nodes.iter().map(|n| format!("    - {n}\n")).collect();
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: p }}\nspec:\n  nodes:\n{nodes}  edges:\n    data:{}    control:{}  run: {{ {run} }}\n",
        list(data),
        list(control)
    )
}

fn engine(text: &str, s: &HostServices) -> Engine {
    let graph = build_graph(text, &HostResolver::new(fixtures())).expect("graph");
    Engine::build(graph, &default_backends(), s).expect("build")
}

fn pipelined() -> RunOptions {
    RunOptions {
        pipelined: true,
        ..RunOptions::default()
    }
}

/// (node, cycle, sequence, checksum) of every recorded frame, sorted.
fn frames(s: &HostServices) -> Vec<(String, u64, u64, String)> {
    let mut v: Vec<_> = s
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Frame {
                sequence, checksum, ..
            } => Some((r.node, r.cycle, sequence, checksum)),
            _ => None,
        })
        .collect();
    v.sort();
    v
}

/// Runs `text` serially and pipelined; returns both reports and recorders.
fn both(text: &str) -> ((RunReport, HostServices), (RunReport, HostServices)) {
    let a = HostServices::default();
    let ra = engine(text, &a).run(&RunOptions::default(), &mut NullObserver);
    let b = HostServices::default();
    let rb = engine(text, &b).run(&pipelined(), &mut NullObserver);
    ((ra, a), (rb, b))
}

const CAM: &str =
    "{ id: cam, builtin: synthetic-source, config: { width: 2, height: 2, frames: 40 } }";
const GATE: &str = "{ id: gate, builtin: brightness-gate, config: { threshold: 128 } }";

#[test]
fn pipelined_linear_graph_matches_serial_result() {
    let text = doc(
        &[
            CAM,
            "{ id: inv, builtin: invert }",
            "{ id: pass, builtin: passthrough }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: inv.frame }",
            "{ from: inv.frame, to: pass.frame }",
            "{ from: pass.frame, to: sink.frame }",
        ],
        &[],
        "",
    );
    let ((ra, a), (rb, b)) = both(&text);
    assert_eq!(ra.end, rb.end);
    assert_eq!(rb.cycles_completed, 40);
    assert_eq!(frames(&a), frames(&b));
}

#[test]
fn pipelined_preserves_source_order_at_the_sink() {
    let s = HostServices::default();
    let text = doc(
        &[
            CAM,
            "{ id: slow, builtin: fault, config: { delayMs: 1 } }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: slow.frame }",
            "{ from: slow.frame, to: sink.frame }",
        ],
        &[],
        "",
    );
    engine(&text, &s).run(&pipelined(), &mut NullObserver);
    let order: Vec<(u64, u64)> = s
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Frame { sequence, .. } => Some((r.cycle, sequence)),
            _ => None,
        })
        .collect();
    assert_eq!(
        order,
        (1..=40).map(|c| (c, c)).collect::<Vec<_>>(),
        "arrival order == source order"
    );
}

#[test]
fn pipelined_joins_two_inputs_from_the_same_cycle() {
    // `sink` joins a frame (via a slow path) with the classification of the same frame.
    let s = HostServices::default();
    let text = doc(
        &[
            CAM,
            GATE,
            "{ id: slow, builtin: fault, config: { delayMs: 1 } }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: gate.frame }",
            "{ from: cam.frame, to: slow.frame }",
            "{ from: slow.frame, to: sink.frame }",
            "{ from: gate.class, to: sink.class }",
        ],
        &[],
        "",
    );
    let r = engine(&text, &s).run(&pipelined(), &mut NullObserver);
    assert_eq!(r.cycles_completed, 40);
    let records = s.recorder.records();
    for cycle in 1..=40u64 {
        let here: Vec<_> = records.iter().filter(|r| r.cycle == cycle).collect();
        assert_eq!(here.len(), 2, "cycle {cycle}: frame + class");
        assert!(here.iter().any(
            |r| matches!(r.value, RecordedValue::Frame { sequence, .. } if sequence == cycle)
        ));
    }
}

#[test]
fn pipelined_untaken_branch_does_not_stall_the_join() {
    let text = doc(
        &[
            CAM,
            GATE,
            "{ id: route, primitive: switch }",
            "{ id: sink, builtin: record }",
            "{ id: other, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: gate.frame }",
            "{ from: cam.frame, to: route.frame }",
            "{ from: gate.class, to: route.class }",
            "{ from: route.bright, to: sink.frame }",
            "{ from: gate.class, to: sink.class }",
            "{ from: route.dark, to: other.frame }",
        ],
        &[],
        "",
    );
    let ((ra, a), (rb, b)) = both(&text);
    assert_eq!(
        rb.cycles_completed, 40,
        "void markers kept every edge in step"
    );
    assert_eq!(ra.end, rb.end);
    assert_eq!(frames(&a), frames(&b));
    assert_eq!(a.recorder.len(), b.recorder.len());
}

#[test]
fn pipelined_control_gating_matches_serial() {
    let text = doc(
        &[
            CAM,
            GATE,
            "{ id: when, primitive: if }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: gate.frame }",
            "{ from: cam.frame, to: when.frame }",
            "{ from: when.acceptedFrame, to: sink.frame }",
        ],
        &["{ from: gate.bright, to: when.productPresent }"],
        "",
    );
    let ((_, a), (rb, b)) = both(&text);
    assert_eq!(frames(&a), frames(&b));
    let when = rb.nodes.iter().find(|n| n.id == "when").expect("when");
    assert_eq!(when.executed + when.skipped, 40);
}

#[test]
fn pipelined_runs_a_loop_values_and_selects_exactly_like_serial() {
    let text = doc(
        &[
            "{ id: cycle, primitive: loop, config: { mode: forever } }",
            "{ id: cam, builtin: synthetic-source, config: { width: 1, height: 1, frames: 3 } }",
            r#"{ id: cams, primitive: value, config: { type: json, shape: list, literal: [ { s: a, p: x }, { s: b, p: y } ] } }"#,
            "{ id: want, primitive: value, config: { type: json, literal: y } }",
            "{ id: pick, primitive: select, config: { mode: many, by: p } }",
            "{ id: sink, builtin: record }",
            "{ id: out, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: sink.frame }",
            "{ from: cams.value, to: pick.items }",
            "{ from: want.value, to: pick.criterion }",
            "{ from: pick.selected, to: out.json }",
        ],
        &[],
        "maxCycles: 10",
    );
    let ((ra, a), (rb, b)) = both(&text);
    assert_eq!(rb.end, EndReason::CycleLimit { limit: 10 });
    assert_eq!(frames(&a), frames(&b));
    assert_eq!(
        ra.iteration.as_ref().map(|i| i.rewinds),
        rb.iteration.as_ref().map(|i| i.rewinds)
    );
    let selections = |s: &HostServices| -> Vec<serde_json::Value> {
        s.recorder
            .records()
            .into_iter()
            .filter(|r| r.node == "out")
            .filter_map(|r| match r.value {
                RecordedValue::Json { value } => Some(value),
                _ => None,
            })
            .collect()
    };
    assert_eq!(selections(&a), selections(&b));
    assert_eq!(selections(&b).len(), 10);
    assert_eq!(selections(&b)[0], json!([{"s": "b", "p": "y"}]));
}

#[test]
fn pipelined_counts_completed_cycles_not_sink_executions() {
    let text = doc(
        &[
            CAM,
            "{ id: a, builtin: record }",
            "{ id: b, builtin: record }",
            "{ id: c, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: a.frame }",
            "{ from: cam.frame, to: b.frame }",
            "{ from: cam.frame, to: c.frame }",
        ],
        &[],
        "",
    );
    let (_, (rb, _)) = both(&text);
    assert_eq!(rb.cycles_completed, 40);
}

#[test]
fn pipelined_failure_ends_at_the_failing_cycle_after_earlier_cycles_complete() {
    let text = doc(
        &[
            CAM,
            "{ id: mid, builtin: fault, config: { failAtCycle: 17 } }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: mid.frame }",
            "{ from: mid.frame, to: sink.frame }",
        ],
        &[],
        "",
    );
    let ((ra, a), (rb, b)) = both(&text);
    assert!(
        matches!(&rb.end, EndReason::NodeFailed { node, cycle: 17, .. } if node == "mid"),
        "{:?}",
        rb.end
    );
    assert_eq!(ra.end, rb.end);
    assert_eq!(rb.cycles_completed, 16);
    assert_eq!(
        frames(&a),
        frames(&b),
        "cycles after the failure never reach the sink"
    );
}

#[test]
fn pipelined_restart_policy_is_lossless() {
    let text = doc(
        &[
            CAM,
            "{ id: mid, builtin: fault, config: { failAtCycle: 9, failGeneration: 1 }, onError: { mode: restart, limit: 2, backoffMs: 1 } }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: mid.frame }",
            "{ from: mid.frame, to: sink.frame }",
        ],
        &[],
        "",
    );
    let ((_, a), (rb, b)) = both(&text);
    assert_eq!(rb.end, EndReason::SourceExhausted { node: "cam".into() });
    assert_eq!(frames(&a), frames(&b));
    assert_eq!(
        rb.nodes.iter().find(|n| n.id == "mid").map(|n| n.restarts),
        Some(1)
    );
}

#[test]
fn pipelined_epoch_barrier_persists_and_resumes_exactly() {
    let dir = std::env::temp_dir().join(format!("mvf-pipe-ckpt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let text = doc(
        &[
            "{ id: cam, builtin: synthetic-source, config: { width: 1, height: 1, frames: 30 } }",
            "{ id: count, builtin: counter }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: count.frame }",
            "{ from: count.count, to: sink.json }",
            "{ from: cam.frame, to: sink.frame }",
        ],
        &[],
        &format!(
            "checkpoint: {{ everyCycles: 5, directory: {} }}",
            dir.display()
        ),
    );
    // First run: interrupted at cycle 12 (cancelled), so the last epoch is cycle 10.
    struct StopAt(CancellationToken, u64);
    impl Observer for StopAt {
        fn cycle_completed(&mut self, cycle: u64) {
            if cycle == self.1 {
                self.0.cancel();
            }
        }
    }
    let s1 = HostServices::default();
    let mut e = engine(&text, &s1);
    let opts = pipelined();
    let r1 = e.run(&opts, &mut StopAt(opts.cancel.clone(), 12));
    assert_eq!(r1.end, EndReason::Cancelled);
    let epochs: Vec<u64> = r1
        .recoveries
        .iter()
        .filter(|e| e.kind == RecoveryKind::Checkpointed)
        .map(|e| e.cycle)
        .collect();
    assert!(epochs.starts_with(&[5, 10]), "{epochs:?}");
    assert!(dir.join("CURRENT").exists());
    // Second run resumes after the last published epoch; the counter continues from its state.
    let s2 = HostServices::default();
    let r2 = engine(&text, &s2).run(&pipelined(), &mut NullObserver);
    assert_eq!(r2.end, EndReason::SourceExhausted { node: "cam".into() });
    let resumed = r2.resumed_from_cycle.expect("resumed");
    let counts: Vec<(u64, u64)> = s2
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Json { value } => Some((r.cycle, value["count"].as_u64().unwrap_or(0))),
            _ => None,
        })
        .collect();
    assert_eq!(
        counts.first(),
        Some(&(resumed + 1, resumed + 1)),
        "state captured with nothing in flight"
    );
    assert_eq!(counts.last(), Some(&(30, 30)));
    assert!(!dir.exists(), "cleared once the source ran out");
}

#[test]
fn pipelined_full_arena_stalls_losslessly_instead_of_failing() {
    // Two slots, a slow consumer: serial would stop on backpressure; pipelined waits for a release.
    let text = doc(
        &[
            "{ id: cam, builtin: synthetic-source, config: { width: 4, height: 4, frames: 20, arena: true } }",
            "{ id: slow, builtin: fault, config: { delayMs: 1 } }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: slow.frame }",
            "{ from: slow.frame, to: sink.frame }",
        ],
        &[],
        "",
    );
    let s = HostServices {
        arena_config: mvf_memory::ArenaConfig {
            slot_size: 1024,
            slot_count: 2,
            dir: None,
        },
        ..HostServices::default()
    };
    let r = engine(&text, &s).run(&pipelined(), &mut NullObserver);
    assert_eq!(
        r.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        r.end
    );
    assert_eq!(frames(&s).len(), 20, "nothing dropped");
    assert_eq!(
        r.data_plane.arena.as_ref().map(|a| a.in_use),
        Some(0),
        "no leaked slot"
    );
}

#[test]
fn pipelined_live_value_lands_at_a_boundary() {
    struct SetAt(ControlHandle, u64);
    impl Observer for SetAt {
        fn cycle_completed(&mut self, cycle: u64) {
            if cycle == self.1 {
                self.0.set_value("want", json!("y"));
            }
        }
    }
    let text = doc(
        &[
            "{ id: cam, builtin: synthetic-source, config: { width: 1, height: 1, frames: 60 } }",
            r#"{ id: cams, primitive: value, config: { type: json, shape: list, literal: [ { s: a, p: x }, { s: b, p: y } ] } }"#,
            "{ id: want, primitive: value, config: { type: json, literal: x } }",
            "{ id: pick, primitive: select, config: { mode: one, by: p } }",
            "{ id: slow, builtin: fault, config: { delayMs: 1 } }",
            "{ id: out, builtin: record }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cams.value, to: pick.items }",
            "{ from: want.value, to: pick.criterion }",
            "{ from: pick.selected, to: out.json }",
            "{ from: cam.frame, to: slow.frame }",
            "{ from: slow.frame, to: sink.frame }",
        ],
        &[],
        "",
    );
    let s = HostServices::default();
    let mut e = engine(&text, &s);
    let mut obs = SetAt(e.control(), 10);
    let r = e.run(&pipelined(), &mut obs);
    assert_eq!(r.cycles_completed, 60);
    let picked: Vec<(u64, String)> = s
        .recorder
        .records()
        .into_iter()
        .filter(|r| r.node == "out")
        .filter_map(|r| match r.value {
            RecordedValue::Json { value } => {
                Some((r.cycle, value["s"].as_str().unwrap_or("").to_string()))
            }
            _ => None,
        })
        .collect();
    let switch = picked
        .iter()
        .position(|(_, s)| s == "b")
        .expect("the edit landed");
    assert!(picked[..switch].iter().all(|(_, s)| s == "a"));
    assert!(
        picked[switch..].iter().all(|(_, s)| s == "b"),
        "one clean switch at a boundary"
    );
    assert!(picked[switch].0 > 10);
    assert_eq!(
        r.live
            .iter()
            .filter(|e| e.kind == LiveEventKind::ValueSet)
            .count(),
        1
    );
}

#[test]
fn pipelined_two_edges_into_one_port_are_rejected_by_the_graph() {
    let text = doc(
        &[
            CAM,
            "{ id: cam2, builtin: synthetic-source }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: sink.frame }",
            "{ from: cam2.frame, to: sink.frame }",
        ],
        &[],
        "",
    );
    let err = build_graph(&text, &HostResolver::new(fixtures())).expect_err("implicit join");
    assert!(err.codes().contains(&"graph.input.multiple-edges"));
}

#[test]
fn pipelined_rejects_parallelism_above_the_contract_ceiling() {
    let text = doc(
        &[
            CAM,
            "{ id: inv, builtin: invert, parallelism: 2 }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: inv.frame }",
            "{ from: inv.frame, to: sink.frame }",
        ],
        &[],
        "",
    );
    let err = build_graph(&text, &HostResolver::new(fixtures())).expect_err("ceiling");
    assert_eq!(err.codes(), ["graph.node.invalid-parallelism"]);
}

#[test]
fn pipelined_parallel_instances_keep_order_and_match_serial() {
    let text = doc(
        &[
            CAM,
            "{ id: inv, contract: parallel-invert.yaml, parallelism: 3 }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: inv.frame }",
            "{ from: inv.frame, to: sink.frame }",
        ],
        &[],
        "",
    );
    let ((_, a), (rb, b)) = both(&text);
    assert_eq!(rb.cycles_completed, 40);
    assert_eq!(frames(&a), frames(&b));
    let order: Vec<u64> = b.recorder.records().into_iter().map(|r| r.cycle).collect();
    assert_eq!(
        order,
        (1..=40).collect::<Vec<_>>(),
        "re-ordered by cycle before routing"
    );
}

#[test]
fn arena_sizing_follows_the_graph() {
    let text = doc(
        &[
            "{ id: cam, builtin: synthetic-source, config: { arena: true } }",
            "{ id: a, builtin: record }",
            "{ id: b, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: a.frame }",
            "{ from: cam.frame, to: b.frame }",
        ],
        &[],
        "",
    );
    let g = build_graph(&text, &HostResolver::new(fixtures())).expect("graph");
    assert_eq!(pipelined::arena_slots_for(&g, 2, 8), 8);
    assert_eq!(pipelined::arena_slots_for(&g, 8, 8), 1 + 8 * 2 + 2);
}

/// Wants every output of the named nodes and keeps (node, cycle) → payload checksum.
#[derive(Default)]
struct Samples {
    want: Vec<&'static str>,
    asked: std::collections::BTreeMap<String, u64>,
    got: Vec<(String, u64, u64)>,
}

impl Observer for Samples {
    fn wants_outputs(&mut self, node: &str) -> bool {
        let yes = self.want.contains(&node);
        if yes {
            *self.asked.entry(node.to_string()).or_default() += 1;
        }
        yes
    }
    fn outputs(
        &mut self,
        cycle: u64,
        node: &str,
        outputs: &std::collections::BTreeMap<String, Value>,
    ) {
        let sum = outputs["frame"].payload().expect("frame").checksum();
        self.got.push((node.to_string(), cycle, sum));
    }
}

#[test]
fn pipelined_hands_sampled_outputs_to_the_observer_exactly_as_serial_produces_them() {
    let text = doc(
        &[
            CAM,
            "{ id: inv, builtin: invert }",
            "{ id: par, contract: parallel-invert.yaml, parallelism: 3 }",
            "{ id: sink, builtin: record }",
        ],
        &[
            "{ from: cam.frame, to: inv.frame }",
            "{ from: inv.frame, to: par.frame }",
            "{ from: par.frame, to: sink.frame }",
        ],
        &[],
        "",
    );
    let want = vec!["cam", "inv", "par"];
    let mut serial = Samples {
        want: want.clone(),
        ..Samples::default()
    };
    engine(&text, &HostServices::default()).run(&RunOptions::default(), &mut serial);
    let truth: std::collections::BTreeMap<(String, u64), u64> = serial
        .got
        .iter()
        .map(|(n, c, s)| ((n.clone(), *c), *s))
        .collect();
    assert_eq!(
        truth.len(),
        3 * 40,
        "serial: every executed node, every cycle"
    );

    let mut sampled = Samples {
        want,
        ..Samples::default()
    };
    let report = engine(&text, &HostServices::default()).run(&pipelined(), &mut sampled);
    assert_eq!(report.cycles_completed, 40);
    for (n, c, s) in &sampled.got {
        assert_eq!(
            truth.get(&(n.clone(), *c)),
            Some(s),
            "{n} at cycle {c}: what serial produced"
        );
    }
    for node in ["cam", "inv", "par"] {
        let cycles: Vec<u64> = sampled
            .got
            .iter()
            .filter(|(n, _, _)| n == node)
            .map(|(_, c, _)| *c)
            .collect();
        assert!(!cycles.is_empty(), "{node} was sampled");
        assert!(
            cycles.windows(2).all(|w| w[0] < w[1]),
            "{node}: in cycle order"
        );
        // Every `true` is answered by one sample, except at most one still armed at the end.
        let asked = sampled.asked[node];
        assert!(
            asked - cycles.len() as u64 <= 1,
            "{node}: asked {asked}, sampled {}",
            cycles.len()
        );
    }
    // The source runs on the driver thread and is sampled like the serial executor does it.
    assert_eq!(
        sampled.got.iter().filter(|(n, _, _)| n == "cam").count(),
        40
    );
}
