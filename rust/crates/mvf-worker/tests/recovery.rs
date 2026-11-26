//! Worker state recovery over the protocol's `checkpoint`/`restore` messages, with real Python.

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

fn pipeline(node: &str, frames: u64, run: &str) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: wr }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: {frames}, arena: true }} }}\n    - {{ id: w, {node} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: w.frame }}\n      - {{ from: w.class, to: sink.class }}\n      - {{ from: cam.frame, to: sink.frame }}\n  run: {{ {run} }}\n"
    )
}

fn engine(text: &str, services: &HostServices) -> Engine {
    let resolver = HostResolver::new(fixtures()).with_module_root(repo().join("modules"));
    let graph = build_graph(text, &resolver).expect("graph");
    Engine::build(
        graph,
        &default_backends().with(WorkerBackend::from_env()),
        services,
    )
    .expect("build")
}

fn measurements(s: &HostServices) -> Vec<(u64, u64)> {
    s.recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Classification(c) => {
                Some((r.cycle, c.measurement.unwrap_or(-1.0) as u64))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn crashed_stateful_worker_restarts_with_its_checkpointed_state() {
    let s = HostServices::default();
    let text = pipeline(
        "contract: counting-crash.yaml, onError: { mode: restart, limit: 3, backoffMs: 1 }",
        12,
        "checkpoint: { everyCycles: 1 }",
    );
    let r = engine(&text, &s).run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(
        r.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        s.logs.events()
    );
    // The process died at count 7; the fresh process got count 6 back and redid cycle 7.
    assert_eq!(
        measurements(&s),
        (1..=12).map(|c| (c, c)).collect::<Vec<_>>()
    );
    let w = r.nodes.iter().find(|n| n.id == "w").expect("w");
    assert_eq!((w.restarts, w.generation), (1, 2));
    let kinds: Vec<_> = r
        .recoveries
        .iter()
        .filter(|e| e.node.as_deref() == Some("w"))
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, [RecoveryKind::Restored, RecoveryKind::Restarted]);
    assert!(
        r.recoveries
            .iter()
            .any(|e| e.kind == RecoveryKind::Restarted && e.cycle == 7)
    );
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
fn unchanged_sdk_module_state_survives_an_engine_restart() {
    let dir = std::env::temp_dir().join(format!("mvf-worker-ckpt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let text = pipeline("module: py.frame-counter", 20, "");
    let options = |cancel: CancellationToken| RunOptions {
        cancel,
        checkpoint_every: Some(5),
        checkpoint_dir: Some(dir.clone()),
        ..RunOptions::default()
    };
    let s1 = HostServices::default();
    let cancel = CancellationToken::new();
    let r1 = engine(&text, &s1).run(&options(cancel.clone()), &mut CancelAfter(12, cancel));
    assert_eq!(r1.end, EndReason::Cancelled);
    assert!(
        dir.join("epoch-10/w.state").is_file(),
        "the worker's own state was persisted"
    );

    let s2 = HostServices::default();
    let r2 = engine(&text, &s2).run(&options(CancellationToken::new()), &mut NullObserver);
    assert_eq!(r2.resumed_from_cycle, Some(10));
    assert_eq!(
        measurements(&s2),
        (11..=20).map(|c| (c, c)).collect::<Vec<_>>(),
        "{:?}",
        s2.logs.events()
    );
    assert!(!dir.exists());
}

#[test]
fn stateful_worker_is_live_replaced_without_losing_its_count() {
    struct Driver(ControlHandle, Option<ReplacementRequest>);
    impl Observer for Driver {
        fn cycle_completed(&mut self, cycle: u64) {
            if cycle == 9
                && let Some(r) = self.1.take()
            {
                assert!(self.0.replace(r));
            }
        }
    }
    let s = HostServices::default();
    let text = pipeline("module: py.frame-counter", 40, "")
        .replace("arena: true", "arena: true, intervalMs: 10");
    let mut e = engine(&text, &s);
    let resolver = HostResolver::new(fixtures());
    let request = ReplacementRequest::resolve(
        &resolver,
        "w",
        &NodeReference::Contract("counter-v2/node.yaml".into()),
        Default::default(),
        5,
    )
    .expect("v2");
    let mut d = Driver(e.control(), Some(request));
    let r = e.run(&RunOptions::default(), &mut d);
    let promoted = r
        .replacements
        .iter()
        .find(|ev| ev.kind == ReplacementEventKind::Promoted)
        .unwrap_or_else(|| panic!("{:?}", r.replacements));
    assert_eq!(
        promoted.to_version.as_deref(),
        Some("py.frame-counter@2.0.0")
    );
    assert_eq!(
        measurements(&s),
        (1..=40).map(|c| (c, c)).collect::<Vec<_>>(),
        "count continued across processes"
    );
}
