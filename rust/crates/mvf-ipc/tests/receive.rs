//! Both v2 receive paths through the real engine and supervisor (#32): the direct path (Unix:
//! `poll` on the stdout pipe; Windows: overlapped reads on a named pipe) and the reader thread
//! (`MVF_WORKER_RECEIVE=thread`, the rollback switch). Each path runs the same cases. One test per
//! binary, because the receive path is a host-wide setting.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_worker::WorkerBackend;
use mvf_worker::process::{ReceivePath, set_receive_path};

fn package(name: &str, fault: Option<&str>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mvf-ipc-receive-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let module = dir.join("modules/t.echo");
    std::fs::create_dir_all(&module).expect("module");
    let exe = format!("mvf-ipc-echo{}", std::env::consts::EXE_SUFFIX);
    std::fs::copy(env!("CARGO_BIN_EXE_mvf-ipc-echo"), module.join(&exe)).expect("copy");
    if let Some(f) = fault {
        std::fs::write(module.join("mvf-ipc-echo.fault"), f).expect("fault");
    }
    std::fs::write(
        module.join("node.yaml"),
        format!(
            "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: t.echo, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [ {{ name: frame, channel: data, type: frame }} ]\n    outputs: [ {{ name: value, channel: data, type: json }} ]\n  artifacts: [ {{ backend: worker, runtime: executable, entry: {exe}, protocol: 1 }} ]\n  lifecycle: {{ readinessTimeoutMs: 5000, executeTimeoutMs: 1500 }}\n"
        ),
    )
    .expect("yaml");
    dir
}

struct Outcome {
    report: RunReport,
    logs: Vec<LogEvent>,
    records: usize,
    elapsed: Duration,
}

fn run(dir: &Path, frames: u64) -> Outcome {
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: receive }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: {frames} }} }}\n    - {{ id: echo, module: t.echo }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: echo.frame }}\n      - {{ from: echo.value, to: sink.json }}\n"
    );
    let resolver = HostResolver::new(dir).with_module_root(dir.join("modules"));
    let graph = build_graph(&text, &resolver).expect("graph");
    let services = HostServices::default();
    let backends = default_backends().with(WorkerBackend::from_env().with_protocol(2));
    let mut engine = Engine::build(graph, &backends, &services).expect("build");
    let t0 = Instant::now();
    let report = engine.run(&RunOptions::default(), &mut NullObserver);
    let elapsed = t0.elapsed();
    drop(engine);
    // Let the bounded log delivery catch up before reading the logs.
    std::thread::sleep(Duration::from_millis(100));
    Outcome {
        report,
        logs: services.logs.events(),
        records: services.recorder.records().len(),
        elapsed,
    }
}

fn failure(o: &Outcome) -> NodeError {
    match &o.report.end {
        EndReason::NodeFailed { error, .. } | EndReason::StartFailed { error, .. } => error.clone(),
        other => panic!("expected a node failure, got {other:?}; logs {:?}", o.logs),
    }
}

fn spoke(o: &Outcome, protocol: u64) -> bool {
    o.logs
        .iter()
        .any(|l| l.message.contains(&format!("protocol={protocol}")))
}

#[test]
fn both_receive_paths_run_detect_and_bound_faults_alike() {
    for path in [ReceivePath::Direct, ReceivePath::Thread] {
        set_receive_path(path);

        let o = run(&package(&format!("{path:?}-healthy"), None), 50);
        assert_eq!(
            o.report.end,
            EndReason::SourceExhausted { node: "cam".into() },
            "{path:?}: {:?}",
            o.logs
        );
        assert!(spoke(&o, 2), "{path:?}: {:?}", o.logs);
        assert_eq!(o.records, 50, "{path:?}");

        // A child that answers a v2 offer in v1: detected by its first byte, then read as lines.
        let o = run(&package(&format!("{path:?}-v1"), Some("v1-only")), 50);
        assert_eq!(
            o.report.end,
            EndReason::SourceExhausted { node: "cam".into() },
            "{path:?}: {:?}",
            o.logs
        );
        assert!(spoke(&o, 1), "{path:?}: {:?}", o.logs);
        assert_eq!(o.records, 50, "{path:?}");

        // A hung request ends at its deadline (1.5 s), not later.
        let o = run(&package(&format!("{path:?}-hang"), Some("hang")), 10);
        assert_eq!(failure(&o).kind, NodeErrorKind::Timeout, "{path:?}");
        assert!(
            o.elapsed < Duration::from_secs(8),
            "{path:?}: {:?}",
            o.elapsed
        );

        // A frame left half-written is bounded by the deadline too.
        let o = run(&package(&format!("{path:?}-partial"), Some("partial")), 10);
        assert_eq!(failure(&o).kind, NodeErrorKind::Timeout, "{path:?}");

        // A crash inside a frame is the process exit, with its code.
        let o = run(
            &package(&format!("{path:?}-crash"), Some("crash-mid-frame")),
            10,
        );
        let e = failure(&o);
        assert_eq!(
            (e.kind, e.exit_code),
            (NodeErrorKind::ProcessExited, Some(3)),
            "{path:?}: {e:?}"
        );

        // Garbage on the channel is a protocol violation.
        let o = run(&package(&format!("{path:?}-magic"), Some("bad-magic")), 10);
        assert_eq!(failure(&o).kind, NodeErrorKind::Protocol, "{path:?}");

        // A stray print lands in the logs; the channel is untouched.
        let o = run(&package(&format!("{path:?}-print"), Some("print")), 10);
        assert_eq!(
            o.report.end,
            EndReason::SourceExhausted { node: "cam".into() },
            "{path:?}: {:?}",
            o.logs
        );
        assert!(
            o.logs
                .iter()
                .any(|l| l.message.contains("hello from a careless library")),
            "{path:?}: {:?}",
            o.logs
        );
    }
    set_receive_path(ReceivePath::Direct);
}
