//! `MVF_IPC_BENCH=1 cargo test --release -p mvf-ipc --test engine_bench -- --nocapture`
//!
//! The v1 → v2 comparison through the real engine and worker supervisor (#32): a Rust analyzer and
//! the Python, C++ and .NET invert transformers, each on its SDK's own v1 and v2 paths, on 8×8
//! frames where control cost dominates. Portable, so CI measures Windows (reader-thread receive)
//! next to Linux and macOS. The C++ and .NET modules are used when built (see
//! crates/mvf-worker/tests/sdks.rs); otherwise their rows say NOT RUN.
//!
//! Columns: the node's mean and max cycle time, run throughput, readiness, and on Unix the
//! children's CPU per frame (a run of `FRAMES` minus a 1-frame run, so process start-up is not
//! counted).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mvf_engine::*;

const FRAMES: u64 = 50_000;

/// CPU time of all reaped child processes so far, in µs.
#[cfg(unix)]
fn children_cpu_us() -> Option<f64> {
    let mut r: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: valid out-pointer.
    unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut r) };
    let us = |t: libc::timeval| t.tv_sec as f64 * 1e6 + t.tv_usec as f64;
    Some(us(r.ru_utime) + us(r.ru_stime))
}

#[cfg(not(unix))]
fn children_cpu_us() -> Option<f64> {
    None
}

struct Case {
    name: &'static str,
    base: PathBuf,
    node: String,
    out: &'static str,
    sink: &'static str,
}

struct Measured {
    us_per_frame: f64,
    max_us: u64,
    frames_per_s: f64,
    ready_ms: u64,
    child_cpu_us: Option<f64>,
}

fn run(case: &Case, modules: &[PathBuf], protocol: u64, frames: u64) -> Measured {
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: bench }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 8, height: 8, channels: 1, frames: {frames}, arena: true }} }}\n    - {{ id: n, {} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: n.frame }}\n      - {{ from: n.{}, to: sink.{} }}\n",
        case.node, case.out, case.sink
    );
    let resolver = modules
        .iter()
        .fold(HostResolver::new(&case.base), |r, m| r.with_module_root(m));
    let graph = mvf_graph::build_graph(&text, &resolver).expect("graph");
    // The supervisor's `ready` line, caught as it arrives: a module that logs every frame (the
    // Python one does) pushes it out of the bounded log store long before the run ends.
    let ready = Arc::new(Mutex::new(None::<String>));
    let seen = ready.clone();
    let services = HostServices {
        logs: Logs::streaming(move |e| {
            if e.message.starts_with("ready pid=") {
                *seen.lock().expect("ready") = Some(e.message.clone());
            }
        }),
        ..HostServices::default()
    };
    let worker = mvf_worker::WorkerBackend::from_env().with_protocol(protocol);
    let cpu_before = children_cpu_us();
    let mut engine =
        Engine::build(graph, &default_backends().with(worker), &services).expect("build");
    let report = engine.run(&RunOptions::default(), &mut NullObserver);
    drop(engine); // reaps the child, so its CPU time is counted
    let child_cpu_us = cpu_before.zip(children_cpu_us()).map(|(a, b)| b - a);
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        services.logs.events()
    );
    let ready = ready.lock().expect("ready").clone().unwrap_or_default();
    assert!(
        ready.contains(&format!("protocol={protocol}")),
        "{} spoke protocol {protocol}: {ready:?}",
        case.name
    );
    let node = report.nodes.iter().find(|x| x.id == "n").expect("n");
    Measured {
        us_per_frame: node.total_us as f64 / node.executed.max(1) as f64,
        max_us: node.max_us,
        frames_per_s: frames as f64 / (report.cycles_ms / 1000.0),
        ready_ms: ready
            .split(" in ")
            .nth(1)
            .and_then(|r| r.trim_end_matches(" ms").parse::<u64>().ok())
            .unwrap_or(0),
        child_cpu_us,
    }
}

/// Writes a transformer contract next to an SDK example and returns its case, if it is built.
fn sdk_case(name: &'static str, candidates: &[PathBuf]) -> Option<Case> {
    let Some(binary) = candidates.iter().find(|p| p.is_file()) else {
        eprintln!("NOT RUN: {name} ({} is not built)", candidates[0].display());
        return None;
    };
    let base = binary.parent().expect("dir").to_path_buf();
    let entry = binary.file_name().and_then(|n| n.to_str()).expect("name");
    std::fs::write(
        base.join("bench-invert.yaml"),
        format!("apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: bench.invert, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [ {{ name: frame, channel: data, type: frame }} ]\n    outputs: [ {{ name: frame, channel: data, type: frame }} ]\n  artifacts: [ {{ backend: worker, runtime: executable, entry: {entry}, protocol: 1 }} ]\n"),
    )
    .expect("contract");
    Some(Case {
        name,
        base,
        node: "contract: bench-invert.yaml".into(),
        out: "frame",
        sink: "frame",
    })
}

#[test]
fn engine_round_trips_v1_vs_v2() {
    if std::env::var_os("MVF_IPC_BENCH").is_none_or(|v| v != "1") {
        return;
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let dir = std::env::temp_dir().join(format!("mvf-ipc-engine-bench-{}", std::process::id()));
    let module = dir.join("modules/t.echo");
    std::fs::create_dir_all(&module).expect("module");
    let exe = std::env::consts::EXE_SUFFIX;
    let echo = format!("mvf-ipc-echo{exe}");
    std::fs::copy(env!("CARGO_BIN_EXE_mvf-ipc-echo"), module.join(&echo)).expect("copy");
    std::fs::write(
        module.join("node.yaml"),
        format!("apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: t.echo, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [ {{ name: frame, channel: data, type: frame }} ]\n    outputs: [ {{ name: value, channel: data, type: json }} ]\n  artifacts: [ {{ backend: worker, runtime: executable, entry: {echo}, protocol: 1 }} ]\n"),
    )
    .expect("yaml");
    let modules = [dir.join("modules"), repo.join("modules")];
    let build = |var: &str, default: &str| {
        std::env::var_os(var)
            .map(PathBuf::from)
            .unwrap_or_else(|| repo.join("rust/target").join(default))
    };
    let cpp = build("MVF_CPP_SDK_DIR", "cpp-sdk");
    let dotnet = build("MVF_DOTNET_SDK_DIR", "dotnet-sdk");
    let mut cases = vec![
        Case {
            name: "rust analyzer",
            base: dir.clone(),
            node: "module: t.echo".into(),
            out: "value",
            sink: "json",
        },
        Case {
            name: "python transformer",
            base: dir.clone(),
            node: "module: py.invert-transformer".into(),
            out: "frame",
            sink: "frame",
        },
    ];
    cases.extend(sdk_case(
        "c++ transformer",
        &[
            cpp.join(format!("mvf_invert_transformer{exe}")),
            cpp.join(format!("Release/mvf_invert_transformer{exe}")),
        ],
    ));
    cases.extend(sdk_case(
        ".net transformer",
        &[dotnet.join(format!("InvertTransformer/mvf-dotnet-invert{exe}"))],
    ));

    eprintln!(
        "{:<20} {:>3} {:>14} {:>10} {:>12} {:>9} {:>16}",
        "node (50k frames)",
        "v",
        "node us/frame",
        "max us",
        "run frames/s",
        "ready ms",
        "child cpu us/fr"
    );
    for case in &cases {
        for protocol in [1, 2] {
            // A 1-frame run first: it warms caches and gives the start-up CPU to subtract.
            let start = run(case, &modules, protocol, 1);
            let m = run(case, &modules, protocol, FRAMES);
            let cpu = m
                .child_cpu_us
                .zip(start.child_cpu_us)
                .map(|(big, small)| format!("{:.1}", (big - small) / (FRAMES - 1) as f64))
                .unwrap_or_else(|| "-".into());
            eprintln!(
                "{:<20} {:>3} {:>14.1} {:>10} {:>12.0} {:>9} {:>16}",
                case.name, protocol, m.us_per_frame, m.max_us, m.frames_per_s, m.ready_ms, cpu
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
