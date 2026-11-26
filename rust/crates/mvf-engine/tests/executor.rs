//! Deterministic executor semantics: ordered cycles, exact outputs, exhaustion, failure, panic,
//! cancellation, bounded input, control gating and runtime output re-validation.

use mvf_contract::{Artifact, BackendCapabilities, BackendKind};
use mvf_engine::builtins::SyntheticSource;
use mvf_engine::*;
use mvf_graph::build_graph;

fn engine(text: &str, recorder: &Recorder) -> Engine {
    let graph = build_graph(text, &HostResolver::new(".")).expect("valid graph");
    Engine::build(
        graph,
        &default_backends(),
        &HostServices::with_recorder(recorder.clone()),
    )
    .expect("build")
}

fn linear(source_config: &str, middle: &str, max_cycles: Option<u64>) -> String {
    let run = max_cycles
        .map(|n| format!("  run: {{ maxCycles: {n} }}\n"))
        .unwrap_or_default();
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: t }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {source_config} }}\n    - {{ id: mid, {middle} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: mid.frame }}\n      - {{ from: mid.frame, to: sink.frame }}\n{run}"
    )
}

fn checksum_of_inverted(sequence: u64, len: usize) -> String {
    let inverted: Vec<u8> = SyntheticSource::pixels(0, sequence, len)
        .iter()
        .map(|b| 255 - b)
        .collect();
    format!("{:016x}", fnv1a64(&inverted))
}

fn frames(recorder: &Recorder) -> Vec<(u64, u64, String)> {
    recorder
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

#[test]
fn runs_n_cycles_in_order_with_exact_outputs() {
    let recorder = Recorder::new();
    let mut e = engine(
        &linear("{ width: 4, height: 2 }", "builtin: invert", Some(5)),
        &recorder,
    );
    let report = e.run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(report.end, EndReason::CycleLimit { limit: 5 });
    assert_eq!(report.cycles_completed, 5);

    let expected: Vec<_> = (1..=5)
        .map(|n| (n, n, checksum_of_inverted(n, 8)))
        .collect();
    assert_eq!(frames(&recorder), expected);
}

#[test]
fn node_order_within_a_cycle_is_deterministic() {
    #[derive(Default)]
    struct Trace(Vec<(u64, String)>);
    impl Observer for Trace {
        fn node(&mut self, cycle: u64, node: &str, _: u64, _: &NodeOutcome) {
            self.0.push((cycle, node.to_string()));
        }
    }
    let recorder = Recorder::new();
    let mut e = engine(&linear("{}", "builtin: passthrough", Some(2)), &recorder);
    let mut trace = Trace::default();
    e.run(&RunOptions::default(), &mut trace);
    let expected: Vec<(u64, String)> = [1, 2]
        .into_iter()
        .flat_map(|c| ["cam", "mid", "sink"].map(|n| (c, n.to_string())))
        .collect();
    assert_eq!(trace.0, expected);
}

#[test]
fn source_exhaustion_ends_the_run_cleanly() {
    let recorder = Recorder::new();
    let mut e = engine(&linear("{ frames: 3 }", "builtin: invert", None), &recorder);
    let report = e.run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() }
    );
    assert_eq!(report.cycles_completed, 3);
    assert_eq!(recorder.len(), 3);
}

#[test]
fn run_option_overrides_the_pipeline_bound() {
    let recorder = Recorder::new();
    let mut e = engine(&linear("{}", "builtin: invert", Some(50)), &recorder);
    let report = e.run(
        &RunOptions {
            max_cycles: Some(2),
            ..RunOptions::default()
        },
        &mut NullObserver,
    );
    assert_eq!(report.end, EndReason::CycleLimit { limit: 2 });
    assert_eq!(recorder.len(), 2);
}

#[test]
fn managed_failure_names_node_and_cycle_and_stops_downstream() {
    let recorder = Recorder::new();
    let mut e = engine(
        &linear("{}", "builtin: fault, config: { failAtCycle: 3 }", Some(10)),
        &recorder,
    );
    let report = e.run(&RunOptions::default(), &mut NullObserver);
    match &report.end {
        EndReason::NodeFailed { node, cycle, error } => {
            assert_eq!(node, "mid");
            assert_eq!(*cycle, 3);
            assert_eq!(error.kind, NodeErrorKind::Failed);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(report.cycles_completed, 2);
    assert_eq!(recorder.len(), 2, "cycle 3 never reached the sink");
}

#[test]
fn panic_is_caught_and_classified_separately_from_managed_errors() {
    let recorder = Recorder::new();
    let mut e = engine(
        &linear(
            "{}",
            "builtin: fault, config: { panicAtCycle: 2 }",
            Some(10),
        ),
        &recorder,
    );
    let report = e.run(&RunOptions::default(), &mut NullObserver);
    match &report.end {
        EndReason::NodeFailed { error, cycle, .. } => {
            assert_eq!(error.kind, NodeErrorKind::Panicked);
            assert_eq!(*cycle, 2);
            assert!(error.message.contains("configured panic"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn cancellation_is_honoured_at_the_next_cycle_boundary() {
    struct CancelAt(u64, CancellationToken);
    impl Observer for CancelAt {
        fn cycle_completed(&mut self, cycle: u64) {
            if cycle == self.0 {
                self.1.cancel();
            }
        }
    }
    let recorder = Recorder::new();
    let mut e = engine(&linear("{}", "builtin: invert", None), &recorder);
    let cancel = CancellationToken::new();
    let report = e.run(
        &RunOptions {
            max_cycles: None,
            cancel: cancel.clone(),
            ..RunOptions::default()
        },
        &mut CancelAt(4, cancel),
    );
    assert_eq!(report.end, EndReason::Cancelled);
    assert_eq!(report.cycles_completed, 4);
    assert_eq!(
        recorder.len(),
        4,
        "cycles are atomic: no partial fifth cycle"
    );
}

#[test]
fn cooperative_cancellation_reaches_a_stalled_node() {
    let recorder = Recorder::new();
    let mut e = engine(
        &linear("{}", "builtin: fault, config: { delayMs: 60000 }", None),
        &recorder,
    );
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        trigger.cancel();
    });
    let started = std::time::Instant::now();
    let report = e.run(
        &RunOptions {
            max_cycles: None,
            cancel,
            ..RunOptions::default()
        },
        &mut NullObserver,
    );
    t.join().expect("join");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    match &report.end {
        EndReason::NodeFailed { error, .. } => assert_eq!(error.kind, NodeErrorKind::Cancelled),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn start_failure_is_reported_before_any_cycle() {
    let recorder = Recorder::new();
    let mut e = engine(
        &linear("{}", "builtin: fault, config: { failStart: true }", Some(3)),
        &recorder,
    );
    let report = e.run(&RunOptions::default(), &mut NullObserver);
    match &report.end {
        EndReason::StartFailed { node, error } => {
            assert_eq!(node, "mid");
            assert_eq!(error.kind, NodeErrorKind::StartFailed);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(report.cycles_completed, 0);
    assert!(recorder.is_empty());
}

#[test]
fn control_edges_gate_execution_and_absence_propagates() {
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: g }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 2, height: 2, frames: 40 } }\n    - { id: gate, builtin: brightness-gate, config: { threshold: 128 } }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: gate.frame }\n      - { from: cam.frame, to: sink.frame }\n    control:\n      - { from: gate.bright, to: sink.enable }\n";
    let recorder = Recorder::new();
    let mut e = engine(text, &recorder);
    let report = e.run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(report.cycles_completed, 40);

    let expected: Vec<u64> = (1..=40u64)
        .filter(|seq| {
            let px = SyntheticSource::pixels(0, *seq, 4);
            px.iter().map(|b| f64::from(*b)).sum::<f64>() / 4.0 >= 128.0
        })
        .collect();
    let recorded: Vec<u64> = frames(&recorder).into_iter().map(|(_, s, _)| s).collect();
    assert_eq!(recorded, expected);
    let sink = report.nodes.iter().find(|n| n.id == "sink").expect("sink");
    assert_eq!(sink.executed as usize, expected.len());
    assert_eq!(sink.skipped as usize, 40 - expected.len());
}

#[test]
fn unknown_config_fails_the_build_with_the_node_named() {
    let graph = build_graph(
        &linear("{ fps: 30 }", "builtin: invert", Some(1)),
        &HostResolver::new("."),
    )
    .expect("graph");
    let err = Engine::build(graph, &default_backends(), &HostServices::default())
        .expect_err("config rejected");
    assert_eq!(err.node, "cam");
    assert_eq!(err.error.kind, NodeErrorKind::InvalidConfig);
}

// ---- runtime re-validation of outputs with a deliberately misbehaving backend ----

#[derive(Debug)]
struct LyingBackend;

struct Liar;
impl NodeHandle for Liar {
    fn execute(&mut self, _: &ExecContext<'_>, _: &Inputs) -> Result<Execution, NodeError> {
        // Declared output `frame` is a frame; emit a classification instead.
        Ok(Execution::Produced(Outputs::new().with(
            "frame",
            Value::Classification(Classification {
                label: "x".into(),
                measurement: None,
                unit: None,
                details: None,
            }),
        )))
    }
}

impl ExecutionBackend for LyingBackend {
    fn capabilities(&self) -> BackendCapabilities {
        let mut caps = InProcessBackend.capabilities();
        caps.kind = BackendKind::Wasm;
        caps
    }
    fn instantiate(
        &self,
        _: &InstantiateRequest<'_>,
        _: &Artifact,
    ) -> Result<Box<dyn NodeHandle>, NodeError> {
        Ok(Box::new(Liar))
    }
}

#[test]
fn outputs_are_revalidated_against_the_declared_port_type() {
    let dir = std::env::temp_dir().join(format!("mvf-liar-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join("liar.yaml"),
        "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: { id: liar, version: 1.0.0 }\nspec:\n  ports:\n    inputs: [ { name: frame, channel: data, type: frame } ]\n    outputs: [ { name: frame, channel: data, type: frame } ]\n  artifacts: [ { backend: wasm, module: liar.wasm, digest: \"sha256:0000000000000000000000000000000000000000000000000000000000000000\" } ]\n",
    )
    .expect("write");
    let text = linear("{}", "contract: liar.yaml", Some(3));
    let graph = build_graph(&text, &HostResolver::new(&dir)).expect("graph");
    let registry = default_backends().with(LyingBackend);
    let mut e = Engine::build(graph, &registry, &HostServices::default()).expect("build");
    let report = e.run(&RunOptions::default(), &mut NullObserver);
    match &report.end {
        EndReason::NodeFailed { node, error, .. } => {
            assert_eq!(node, "mid");
            assert_eq!(error.kind, NodeErrorKind::InvalidOutput);
        }
        other => panic!("unexpected {other:?}"),
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn unsupported_backend_fails_closed_at_build() {
    let dir = std::env::temp_dir().join(format!("mvf-oci-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join("oci.yaml"),
        "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: { id: det, version: 1.0.0 }\nspec:\n  ports:\n    inputs: [ { name: frame, channel: data, type: frame } ]\n    outputs: [ { name: frame, channel: data, type: frame } ]\n  artifacts: [ { backend: oci, image: det:1, digest: \"sha256:0000000000000000000000000000000000000000000000000000000000000000\" } ]\n  isolation: { minimum: sandbox }\n",
    )
    .expect("write");
    let graph = build_graph(
        &linear("{}", "contract: oci.yaml", Some(1)),
        &HostResolver::new(&dir),
    )
    .expect("graph");
    let err = Engine::build(graph, &default_backends(), &HostServices::default())
        .expect_err("no OCI backend");
    assert_eq!(err.error.kind, NodeErrorKind::Unsupported);
    assert!(
        err.error
            .message
            .contains("negotiation.backend-unavailable")
    );
    let _ = std::fs::remove_dir_all(dir);
}
