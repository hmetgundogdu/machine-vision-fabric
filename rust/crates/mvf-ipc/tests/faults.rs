//! Fault injection through the real engine and worker supervisor (#32): a fixture worker
//! misbehaves on its 3rd request and the host must classify, terminate and clean up — never hang,
//! never resynchronise on a broken stream, never let logs block execution.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_worker::WorkerBackend;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mvf-ipc-faults-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp");
    dir
}

/// A node package around the fixture, misbehaving with `fault` (if any).
fn package(name: &str, fault: Option<&str>) -> PathBuf {
    let dir = temp(name);
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
    services: HostServices,
    elapsed: Duration,
}

fn run_with(dir: &Path, frames: u64, worker: WorkerBackend, services: HostServices) -> Outcome {
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: faults }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: {frames} }} }}\n    - {{ id: echo, module: t.echo }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: echo.frame }}\n      - {{ from: echo.value, to: sink.json }}\n"
    );
    let resolver = HostResolver::new(dir).with_module_root(dir.join("modules"));
    let graph = build_graph(&text, &resolver).expect("graph");
    let mut engine =
        Engine::build(graph, &default_backends().with(worker), &services).expect("build");
    let t0 = Instant::now();
    let report = engine.run(&RunOptions::default(), &mut NullObserver);
    let elapsed = t0.elapsed();
    drop(engine);
    Outcome {
        report,
        services,
        elapsed,
    }
}

fn run(fault: Option<&str>, frames: u64) -> Outcome {
    let dir = package(fault.unwrap_or("none"), fault);
    run_with(
        &dir,
        frames,
        WorkerBackend::from_env().with_protocol(2),
        HostServices::default(),
    )
}

fn failure(o: &Outcome) -> NodeError {
    match &o.report.end {
        EndReason::NodeFailed { error, .. } | EndReason::StartFailed { error, .. } => error.clone(),
        other => panic!(
            "expected a node failure, got {other:?}; logs {:?}",
            o.services.logs.events()
        ),
    }
}

fn pids(o: &Outcome) -> Vec<u32> {
    o.services
        .logs
        .events()
        .iter()
        .filter_map(|l| {
            l.message
                .strip_prefix("spawned pid=")
                .and_then(|p| p.parse().ok())
        })
        .collect()
}

fn assert_cleaned_up(o: &Outcome) {
    for pid in pids(o) {
        #[cfg(unix)]
        {
            // SAFETY: signal 0 only probes existence.
            let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
            assert!(!alive, "worker {pid} outlived its node");
        }
        #[cfg(not(unix))]
        let _ = pid;
    }
}

#[test]
fn a_healthy_v2_worker_runs_and_says_which_protocol_it_speaks() {
    let o = run(None, 50);
    assert_eq!(
        o.report.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        o.services.logs.events()
    );
    assert!(
        o.services
            .logs
            .events()
            .iter()
            .any(|l| l.message.contains("protocol=2")),
        "{:?}",
        o.services.logs.events()
    );
    let o1 = run_with(
        &package("v1-offer", None),
        50,
        WorkerBackend::from_env().with_protocol(1),
        HostServices::default(),
    );
    assert_eq!(
        o1.report.end,
        EndReason::SourceExhausted { node: "cam".into() }
    );
    assert!(
        o1.services
            .logs
            .events()
            .iter()
            .any(|l| l.message.contains("protocol=1")),
        "with v2 not offered the same child speaks v1"
    );
}

#[test]
fn protocol_violations_terminate_the_worker_at_once() {
    for (fault, needle) in [
        ("malformed", "malformed Result body"),
        ("oversized", "exceeds the limit"),
        ("bad-magic", "bad frame magic"),
        ("stale-generation", "stale generation"),
    ] {
        // A 10 s deadline: a violation must end the request long before it.
        let dir = package(fault, Some(fault));
        let yaml = dir.join("modules/t.echo/node.yaml");
        let contract = std::fs::read_to_string(&yaml).expect("yaml");
        std::fs::write(
            &yaml,
            contract.replace("executeTimeoutMs: 1500", "executeTimeoutMs: 10000"),
        )
        .expect("write");
        let o = run_with(
            &dir,
            10,
            WorkerBackend::from_env().with_protocol(2),
            HostServices::default(),
        );
        let e = failure(&o);
        assert_eq!(e.kind, NodeErrorKind::Protocol, "{fault}: {e:?}");
        assert!(e.message.contains(needle), "{fault}: {}", e.message);
        assert!(
            o.elapsed < Duration::from_secs(3),
            "{fault}: refused at once, not after the deadline ({:?})",
            o.elapsed
        );
        assert_cleaned_up(&o);
        eprintln!(
            "{fault:<17} -> {:?}: {} ({:?})",
            e.kind, e.message, o.elapsed
        );
    }
}

#[test]
fn a_peer_without_the_launch_token_is_refused() {
    let o = run(Some("wrong-session"), 10);
    let e = failure(&o);
    assert!(
        matches!(e.kind, NodeErrorKind::Protocol | NodeErrorKind::StartFailed),
        "{e:?}"
    );
    assert!(
        o.services
            .logs
            .events()
            .iter()
            .any(|l| l.message.contains("unauthorized peer")),
        "{:?}",
        o.services.logs.events()
    );
    assert_cleaned_up(&o);
}

#[test]
fn hangs_partial_frames_and_crashes_are_bounded_and_classified() {
    let o = run(Some("hang"), 10);
    let e = failure(&o);
    assert_eq!(e.kind, NodeErrorKind::Timeout);
    assert!(o.elapsed < Duration::from_secs(4), "{:?}", o.elapsed);
    assert_cleaned_up(&o);

    let o = run(Some("partial"), 10);
    let e = failure(&o);
    assert_eq!(
        e.kind,
        NodeErrorKind::Timeout,
        "a frame left half-written is bounded by the request's deadline"
    );
    assert_cleaned_up(&o);

    let o = run(Some("crash-mid-frame"), 10);
    let e = failure(&o);
    assert_eq!(
        (e.kind, e.exit_code),
        (NodeErrorKind::ProcessExited, Some(3))
    );
    assert_cleaned_up(&o);
}

#[test]
fn a_stray_print_cannot_corrupt_the_channel() {
    let o = run(Some("print"), 10);
    assert_eq!(
        o.report.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        o.services.logs.events()
    );
    assert!(
        o.services
            .logs
            .events()
            .iter()
            .any(|l| l.message.contains("careless library")),
        "the print landed in the logs"
    );
}

#[test]
fn a_slow_log_consumer_drops_lines_instead_of_stalling_execution() {
    let dir = package("flood", Some("log-flood"));
    let slow = HostServices {
        logs: Logs::streaming(|_| std::thread::sleep(Duration::from_micros(200))),
        ..HostServices::default()
    };
    let o = run_with(&dir, 6, WorkerBackend::from_env().with_protocol(2), slow);
    assert_eq!(
        o.report.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        o.report.end
    );
    let echo = o
        .report
        .nodes
        .iter()
        .find(|n| n.id == "echo")
        .expect("echo");
    // The delivery thread keeps feeding the slow consumer after the run: wait for its count.
    let counted = |o: &Outcome| -> u64 {
        o.services
            .logs
            .events()
            .iter()
            .filter_map(|l| {
                l.message
                    .strip_prefix("dropped ")
                    .and_then(|r| r.split_whitespace().next())
                    .and_then(|n| n.parse::<u64>().ok())
            })
            .sum()
    };
    let until = Instant::now() + Duration::from_secs(15);
    while counted(&o) == 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
    let dropped = counted(&o);
    eprintln!(
        "20000 log lines against a 200 us/line consumer: run {:?}, slowest echo cycle {} us, {dropped} lines dropped and counted",
        o.elapsed, echo.max_us
    );
    // Delivering 20000 lines would take 4 s; the cycle that produced them did not wait for it.
    assert!(echo.max_us < 1_000_000, "{}", echo.max_us);
    assert!(dropped > 0);
}

#[test]
fn cancellation_ends_an_in_flight_request() {
    let dir = package("cancel", Some("hang"));
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: cancel }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { frames: 10 } }\n    - { id: echo, module: t.echo }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: echo.frame }\n      - { from: echo.value, to: sink.json }\n";
    let resolver = HostResolver::new(&dir).with_module_root(dir.join("modules"));
    // Cancel 200 ms into the hung 3rd request, well inside its deadline.
    let yaml = dir.join("modules/t.echo/node.yaml");
    let contract = std::fs::read_to_string(&yaml).expect("yaml");
    std::fs::write(
        &yaml,
        contract.replace("executeTimeoutMs: 1500", "executeTimeoutMs: 10000"),
    )
    .expect("write");
    let services = HostServices::default();
    let mut engine = Engine::build(
        build_graph(text, &resolver).expect("graph"),
        &default_backends().with(WorkerBackend::from_env().with_protocol(2)),
        &services,
    )
    .expect("build");
    let options = RunOptions::default();
    let cancel = options.cancel.clone();
    let logs = services.logs.clone();
    let cancelled_at = std::sync::Arc::new(std::sync::Mutex::new(None));
    let at = cancelled_at.clone();
    std::thread::spawn(move || {
        while !logs
            .events()
            .iter()
            .any(|l| l.message.starts_with("ready pid="))
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(200));
        *at.lock().expect("lock") = Some(Instant::now());
        cancel.cancel();
    });
    let report = engine.run(&options, &mut NullObserver);
    let after_cancel = cancelled_at
        .lock()
        .expect("lock")
        .map(|t| t.elapsed())
        .expect("cancelled");
    assert!(
        after_cancel < Duration::from_millis(500),
        "the in-flight request ended {after_cancel:?} after cancellation, not at its 10 s deadline"
    );
    assert!(
        matches!(
            report.end,
            EndReason::Cancelled | EndReason::NodeFailed { .. }
        ),
        "{:?}",
        report.end
    );
}
