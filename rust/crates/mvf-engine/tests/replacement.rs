//! Live node replacement: lossless stateless cut-over, readiness and port rejection, rollback after
//! promotion, explicit policy errors, stateful migration and buffer hygiene.

use std::collections::BTreeMap;
use std::path::PathBuf;

use mvf_engine::builtins::SyntheticSource;
use mvf_engine::*;
use mvf_graph::build_graph;

/// A scripted command submitted at the boundary before its cycle.
type Step = (u64, Box<dyn Fn(&ControlHandle)>);

/// Records which generation of `proc` ran each cycle, and submits scripted commands.
struct Driver {
    control: ControlHandle,
    script: Vec<Step>,
    generations: BTreeMap<u64, Vec<u64>>,
}

impl Observer for Driver {
    fn node(&mut self, cycle: u64, node: &str, generation: u64, outcome: &NodeOutcome) {
        if node == "proc" && matches!(outcome, NodeOutcome::Executed { .. }) {
            self.generations.entry(cycle).or_default().push(generation);
        }
    }
    fn cycle_completed(&mut self, cycle: u64) {
        // A step "at cycle c" is submitted after cycle c-1, i.e. at the boundary before c.
        for (at, step) in &self.script {
            if *at == cycle + 1 {
                step(&self.control);
            }
        }
    }
}

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "mvf-replace-{}-{}",
        std::process::id(),
        std::thread::current()
            .name()
            .unwrap_or("t")
            .replace("::", "-")
    ));
    std::fs::create_dir_all(&d).expect("dir");
    d
}

fn pipeline(proc_ref: &str, source_extra: &str, frames: u64, sink_port: &str, out: &str) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: live }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: {frames}, intervalMs: 2{source_extra} }} }}\n    - {{ id: proc, {proc_ref} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: proc.frame }}\n      - {{ from: proc.{out}, to: sink.{sink_port} }}\n"
    )
}

struct Run {
    report: RunReport,
    services: HostServices,
    generations: BTreeMap<u64, Vec<u64>>,
}

fn run(text: &str, script: Vec<Step>) -> Run {
    let services = HostServices::default();
    let graph = build_graph(text, &HostResolver::new(dir())).expect("graph");
    let mut engine = Engine::build(graph, &default_backends(), &services).expect("build");
    let mut driver = Driver {
        control: engine.control(),
        script,
        generations: BTreeMap::new(),
    };
    let report = engine.run(&RunOptions::default(), &mut driver);
    Run {
        report,
        services,
        generations: driver.generations,
    }
}

fn request(
    node: &str,
    reference: NodeReference,
    config: &str,
    probation: u64,
) -> ReplacementRequest {
    let config: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(config).expect("config json");
    ReplacementRequest::resolve(
        &HostResolver::new(dir()),
        node,
        &reference,
        config,
        probation,
    )
    .expect("resolve")
}

fn replace_at(at: u64, reference: NodeReference, config: &'static str, probation: u64) -> Step {
    (
        at,
        Box::new(move |c: &ControlHandle| {
            assert!(c.replace(request("proc", reference.clone(), config, probation)));
        }),
    )
}

fn kinds(report: &RunReport) -> Vec<ReplacementEventKind> {
    report.replacements.iter().map(|e| e.kind).collect()
}

fn event(report: &RunReport, kind: ReplacementEventKind) -> &ReplacementEvent {
    report
        .replacements
        .iter()
        .find(|e| e.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} in {:?}", report.replacements))
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

fn checksum(bytes: &[u8]) -> String {
    format!("{:016x}", fnv1a64(bytes))
}

fn inverted(seq: u64) -> String {
    checksum(
        &SyntheticSource::pixels(0, seq, 8)
            .iter()
            .map(|b| 255 - b)
            .collect::<Vec<_>>(),
    )
}

fn raw(seq: u64) -> String {
    checksum(&SyntheticSource::pixels(0, seq, 8))
}

/// Every cycle 1..=n ran `proc` exactly once (after rollbacks) and reached the sink exactly once.
fn assert_lossless(run: &Run, n: u64) {
    let recorded = frames(&run.services);
    let cycles: Vec<u64> = recorded.iter().map(|(c, _, _)| *c).collect();
    assert_eq!(
        cycles,
        (1..=n).collect::<Vec<_>>(),
        "no lost or duplicated cycle"
    );
    assert!(
        recorded.iter().all(|(c, s, _)| c == s),
        "cycle id == frame sequence"
    );
    for c in 1..=n {
        assert_eq!(
            run.generations.get(&c).map(Vec::len),
            Some(1),
            "cycle {c} produced exactly one output on one generation"
        );
    }
}

#[test]
fn stateless_replacement_without_restart_is_lossless() {
    let run = run(
        &pipeline("builtin: invert", "", 40, "frame", "frame"),
        vec![replace_at(
            10,
            NodeReference::Builtin("passthrough".into()),
            "{}",
            5,
        )],
    );
    assert_eq!(
        run.report.end,
        EndReason::SourceExhausted { node: "cam".into() }
    );
    assert_lossless(&run, 40);
    assert_eq!(
        kinds(&run.report),
        [
            ReplacementEventKind::Requested,
            ReplacementEventKind::Promoted,
            ReplacementEventKind::Committed
        ]
    );
    let promoted = event(&run.report, ReplacementEventKind::Promoted);
    let c = promoted.cycle;
    assert!((10..20).contains(&c), "cut over at {c}");
    assert_eq!(
        (promoted.from_generation, promoted.to_generation),
        (1, Some(2))
    );
    assert!(promoted.preparation_ms.is_some() && promoted.cutover_us.is_some());
    // Generation fencing: gen 1 strictly before the cut-over cycle, gen 2 from it on.
    for (cycle, gens) in &run.generations {
        assert_eq!(gens, &vec![if *cycle < c { 1 } else { 2 }], "cycle {cycle}");
    }
    // And the outputs switch behaviour exactly there.
    for (cycle, seq, sum) in frames(&run.services) {
        let expected = if cycle < c { inverted(seq) } else { raw(seq) };
        assert_eq!(sum, expected, "cycle {cycle}");
    }
    let committed = event(&run.report, ReplacementEventKind::Committed);
    assert_eq!(
        committed.cycle,
        c + 5,
        "previous generation stopped after 5 probation cycles"
    );
    let proc = run
        .report
        .nodes
        .iter()
        .find(|n| n.id == "proc")
        .expect("proc");
    assert_eq!(proc.generation, 2);
}

#[test]
fn failed_candidate_readiness_keeps_v1_active() {
    let run = run(
        &pipeline("builtin: invert", "", 20, "frame", "frame"),
        vec![replace_at(
            5,
            NodeReference::Builtin("fault".into()),
            r#"{"failStart": true}"#,
            5,
        )],
    );
    assert_lossless(&run, 20);
    let rejected = event(&run.report, ReplacementEventKind::Rejected);
    assert_eq!(rejected.code, Some("replace.readiness-failed"));
    assert!(run.generations.values().all(|g| g == &vec![1]));
    assert!(
        frames(&run.services)
            .iter()
            .all(|(_, s, sum)| *sum == inverted(*s))
    );
}

#[test]
fn port_incompatible_candidate_is_rejected_before_staging() {
    let run = run(
        &pipeline("builtin: invert", "", 10, "frame", "frame"),
        vec![replace_at(
            3,
            NodeReference::Builtin("brightness-gate".into()),
            "{}",
            5,
        )],
    );
    assert_eq!(kinds(&run.report), [ReplacementEventKind::Rejected]);
    let rejected = event(&run.report, ReplacementEventKind::Rejected);
    assert_eq!(rejected.code, Some("replace.port-incompatible"));
    assert!(
        rejected
            .message
            .as_deref()
            .unwrap_or("")
            .contains("output `frame`")
    );
    assert_lossless(&run, 10);
}

#[test]
fn failure_after_promotion_rolls_back_and_replays_the_cycle_on_v1() {
    let run = run(
        &pipeline("builtin: invert", "", 40, "frame", "frame"),
        vec![replace_at(
            5,
            NodeReference::Builtin("fault".into()),
            r#"{"failAtCycle": 25}"#,
            100,
        )],
    );
    assert_eq!(
        run.report.end,
        EndReason::SourceExhausted { node: "cam".into() }
    );
    assert_lossless(&run, 40);
    let promoted = event(&run.report, ReplacementEventKind::Promoted).cycle;
    assert!(promoted < 25, "promoted at {promoted}");
    let rolled = event(&run.report, ReplacementEventKind::RolledBack);
    assert_eq!(rolled.code, Some("replace.candidate-failed"));
    assert_eq!((rolled.from_generation, rolled.to_generation), (2, Some(1)));
    assert!(rolled.message.as_deref().unwrap_or("").contains("cycle 25"));
    for (cycle, seq, sum) in frames(&run.services) {
        // v1 inverts; the candidate (fault) passes through until it fails at 25; cycle 25 itself is
        // replayed on v1, so it is inverted — no gap and no duplicate.
        let expected = if (promoted..25).contains(&cycle) {
            raw(seq)
        } else {
            inverted(seq)
        };
        assert_eq!(sum, expected, "cycle {cycle}");
    }
    assert_eq!(run.generations[&25], vec![1]);
    let proc = run
        .report
        .nodes
        .iter()
        .find(|n| n.id == "proc")
        .expect("proc");
    assert_eq!(proc.generation, 1);
}

#[test]
fn operator_rollback_and_commit_commands() {
    let rollback: Step = (
        12,
        Box::new(|c: &ControlHandle| assert!(c.rollback("proc"))),
    );
    let run1 = run(
        &pipeline("builtin: invert", "", 20, "frame", "frame"),
        vec![
            replace_at(3, NodeReference::Builtin("passthrough".into()), "{}", 100),
            rollback,
        ],
    );
    let rolled = event(&run1.report, ReplacementEventKind::RolledBack);
    assert_eq!(rolled.code, Some("replace.operator-rollback"));
    assert_eq!(rolled.cycle, 12);
    assert_lossless(&run1, 20);

    let nothing: Step = (4, Box::new(|c: &ControlHandle| assert!(c.rollback("proc"))));
    let run2 = run(
        &pipeline("builtin: invert", "", 8, "frame", "frame"),
        vec![nothing],
    );
    assert_eq!(
        event(&run2.report, ReplacementEventKind::Rejected).code,
        Some("replace.nothing-to-roll-back")
    );

    let commit: Step = (8, Box::new(|c: &ControlHandle| assert!(c.commit("proc"))));
    let run3 = run(
        &pipeline("builtin: invert", "", 20, "frame", "frame"),
        vec![
            replace_at(3, NodeReference::Builtin("passthrough".into()), "{}", 100),
            commit,
        ],
    );
    assert_eq!(
        event(&run3.report, ReplacementEventKind::Committed).cycle,
        8
    );
}

#[test]
fn unsupported_policies_fail_with_explicit_codes() {
    let d = dir();
    std::fs::write(
        d.join("sandboxed-invert.yaml"),
        "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: { id: sandboxed, version: 2.0.0 }\nspec:\n  ports:\n    inputs: [ { name: frame, channel: data, type: frame } ]\n    outputs: [ { name: frame, channel: data, type: frame } ]\n  artifacts: [ { backend: in-process, builtin: invert } ]\n  isolation: { minimum: sandbox }\n  lifecycle: { replacement: stateless }\n",
    )
    .expect("write");
    let cases: Vec<(&str, ReplacementRequest, &str)> = vec![
        (
            "source",
            request(
                "cam",
                NodeReference::Builtin("synthetic-source".into()),
                "{}",
                1,
            ),
            "replace.source-unsupported",
        ),
        (
            "sink",
            request("sink", NodeReference::Builtin("record".into()), "{}", 1),
            "replace.not-replaceable",
        ),
        (
            "unknown",
            request("ghost", NodeReference::Builtin("invert".into()), "{}", 1),
            "replace.unknown-node",
        ),
        (
            "mode",
            request("proc", NodeReference::Builtin("counter".into()), "{}", 1),
            "replace.mode-mismatch",
        ),
        (
            "negotiation",
            request(
                "proc",
                NodeReference::Contract("sandboxed-invert.yaml".into()),
                "{}",
                1,
            ),
            "replace.negotiation",
        ),
    ];
    for (name, req, code) in cases {
        let run = run(
            &pipeline("builtin: invert", "", 6, "frame", "frame"),
            vec![(
                3,
                Box::new(move |c: &ControlHandle| assert!(c.replace(req.clone()))),
            )],
        );
        let rejected = event(&run.report, ReplacementEventKind::Rejected);
        assert_eq!(rejected.code, Some(code), "{name}: {rejected:?}");
        assert_lossless(&run, 6);
    }

    // A second request while one is in probation.
    let run = run(
        &pipeline("builtin: invert", "", 20, "frame", "frame"),
        vec![
            replace_at(3, NodeReference::Builtin("passthrough".into()), "{}", 100),
            replace_at(10, NodeReference::Builtin("passthrough".into()), "{}", 100),
        ],
    );
    assert_eq!(
        event(&run.report, ReplacementEventKind::Rejected).code,
        Some("replace.in-progress")
    );
}

#[test]
fn stateful_counter_migrates_state_across_generations() {
    let d = dir();
    std::fs::write(
        d.join("counter-v2.yaml"),
        "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: { id: mvf.counter, version: 2.0.0 }\nspec:\n  ports:\n    inputs: [ { name: frame, channel: data, type: frame } ]\n    outputs: [ { name: count, channel: data, type: json } ]\n  artifacts: [ { backend: in-process, builtin: counter } ]\n  lifecycle: { replacement: stateful, state: { schemaVersion: 2, migratesFrom: [1] } }\n",
    )
    .expect("write");
    std::fs::write(
        d.join("counter-v3.yaml"),
        "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: { id: mvf.counter, version: 3.0.0 }\nspec:\n  ports:\n    inputs: [ { name: frame, channel: data, type: frame } ]\n    outputs: [ { name: count, channel: data, type: json } ]\n  artifacts: [ { backend: in-process, builtin: counter } ]\n  lifecycle: { replacement: stateful, state: { schemaVersion: 3, migratesFrom: [2] } }\n",
    )
    .expect("write");

    let text = pipeline("builtin: counter", "", 30, "json", "count");
    let run_v2 = run(
        &text,
        vec![replace_at(
            10,
            NodeReference::Contract("counter-v2.yaml".into()),
            r#"{"schemaVersion": 2}"#,
            3,
        )],
    );
    let promoted = event(&run_v2.report, ReplacementEventKind::Promoted);
    assert_eq!(promoted.to_version.as_deref(), Some("mvf.counter@2.0.0"));
    let counts: Vec<(u64, u64, u64)> = run_v2
        .services
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Json { value } => Some((
                r.cycle,
                value["count"].as_u64().unwrap_or(0),
                value["schema"].as_u64().unwrap_or(0),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(counts.len(), 30);
    for (cycle, count, schema) in counts {
        assert_eq!(
            count, cycle,
            "state carried over: no reset, no double count"
        );
        assert_eq!(schema, if cycle < promoted.cycle { 1 } else { 2 });
    }

    let run_v3 = run(
        &text,
        vec![replace_at(
            10,
            NodeReference::Contract("counter-v3.yaml".into()),
            r#"{"schemaVersion": 2}"#,
            3,
        )],
    );
    assert_eq!(
        event(&run_v3.report, ReplacementEventKind::Rejected).code,
        Some("replace.state-incompatible")
    );
}

#[test]
fn replacement_leaks_no_arena_buffers() {
    let run = run(
        &pipeline(
            "builtin: passthrough",
            ", arena: true",
            30,
            "frame",
            "frame",
        ),
        vec![
            replace_at(5, NodeReference::Builtin("invert".into()), "{}", 3),
            replace_at(15, NodeReference::Builtin("passthrough".into()), "{}", 3),
        ],
    );
    assert_lossless(&run, 30);
    let arena = run.report.data_plane.arena.expect("arena used");
    assert_eq!(arena.in_use, 0);
    assert_eq!(
        kinds(&run.report)
            .iter()
            .filter(|k| **k == ReplacementEventKind::Committed)
            .count(),
        2
    );
}

#[test]
fn update_plans_are_strict_and_resolved() {
    let resolver = HostResolver::new(dir());
    let plan = "apiVersion: mvf.dev/v1alpha1\nkind: UpdatePlan\nspec:\n  updates:\n    - { node: proc, atCycle: 20, builtin: passthrough, probationCycles: 4 }\n    - { node: proc, atCycle: 30, action: rollback }\n";
    let steps = parse_update_plan(plan, &resolver).expect("plan");
    assert_eq!(steps.len(), 2);
    assert!(matches!(&steps[0], (20, PlannedCommand::Replace(r)) if r.probation_cycles == 4));
    assert!(matches!(&steps[1], (30, PlannedCommand::Rollback(n)) if n == "proc"));

    for bad in [
        "apiVersion: mvf.dev/v1alpha1\nkind: UpdatePlan\nspec:\n  updates:\n    - { node: proc, atCycle: 2, builtin: passthrough, when: now }\n",
        "apiVersion: mvf.dev/v1alpha1\nkind: UpdatePlan\nspec:\n  updates:\n    - { node: proc, atCycle: 0, builtin: passthrough }\n",
        "apiVersion: mvf.dev/v1alpha1\nkind: UpdatePlan\nspec:\n  updates:\n    - { node: proc, atCycle: 2, action: rollback, builtin: invert }\n",
        "apiVersion: mvf.dev/v1alpha1\nkind: UpdatePlan\nspec:\n  updates:\n    - { node: proc, atCycle: 2, builtin: nope }\n",
    ] {
        assert!(parse_update_plan(bad, &resolver).is_err(), "{bad}");
    }
}
