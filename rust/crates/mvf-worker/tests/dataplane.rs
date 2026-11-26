//! CPU shared-memory data plane with real workers: zero-copy hand-off, copy accounting, fan-out,
//! bounded admission, crash cleanup, input-mutation detection and SDK layout interoperability.

use std::path::{Path, PathBuf};

use mvf_engine::builtins::SyntheticSource;
use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_memory::{ArenaConfig, MetricsSnapshot};
use mvf_worker::WorkerBackend;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("..")
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn services(slots: usize) -> HostServices {
    HostServices {
        arena_config: ArenaConfig {
            slot_size: 64 * 1024,
            slot_count: slots,
            dir: None,
        },
        ..HostServices::default()
    }
}

fn run_with(text: &str, services: &HostServices, backend: WorkerBackend, base: &Path) -> RunReport {
    let resolver = HostResolver::new(base).with_module_root(repo().join("modules"));
    let graph = build_graph(text, &resolver).expect("valid graph");
    let mut e = Engine::build(graph, &default_backends().with(backend), services).expect("build");
    e.run(&RunOptions::default(), &mut NullObserver)
}

fn run(text: &str, services: &HostServices) -> RunReport {
    run_with(text, services, WorkerBackend::from_env(), &fixtures())
}

fn arena(report: &RunReport) -> MetricsSnapshot {
    report.data_plane.arena.expect("arena was used")
}

fn inverted_checksum(sequence: u64, len: usize) -> String {
    let px: Vec<u8> = SyntheticSource::pixels(0, sequence, len)
        .iter()
        .map(|b| 255 - b)
        .collect();
    format!("{:016x}", fnv1a64(&px))
}

fn recorded_frames(services: &HostServices) -> Vec<(u64, String)> {
    services
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Frame {
                sequence, checksum, ..
            } => Some((sequence, checksum)),
            _ => None,
        })
        .collect()
}

const N: u64 = 10;
const LEN: usize = 64; // 8 × 8 × 1

/// cam (arena) → py invert → passthrough → py classifier, with the frame also recorded.
fn pass_through_pipeline(arena_source: bool) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: zero-copy }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 8, height: 8, arena: {arena_source} }} }}\n    - {{ id: inv, module: py.invert-transformer }}\n    - {{ id: pass, builtin: passthrough }}\n    - {{ id: cls, module: py.brightness-classifier }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: inv.frame }}\n      - {{ from: inv.frame, to: pass.frame }}\n      - {{ from: pass.frame, to: cls.frame }}\n      - {{ from: pass.frame, to: sink.frame }}\n      - {{ from: cls.class, to: sink.class }}\n  run: {{ maxCycles: {N} }}\n"
    )
}

#[test]
fn pass_through_path_moves_no_payload_bytes_between_nodes() {
    let s = services(8);
    let report = run(&pass_through_pipeline(true), &s);
    assert_eq!(report.end, EndReason::CycleLimit { limit: N });
    let m = arena(&report);
    // Producers wrote their own outputs (source + invert); nothing was copied anywhere.
    assert_eq!(m.copies_in, 0, "{m:?}");
    assert_eq!(m.copies_out, 0, "{m:?}");
    assert_eq!(m.transform_writes, 2 * N, "{m:?}");
    assert_eq!(m.transform_bytes, 2 * N * LEN as u64, "{m:?}");
    // invert and classifier each received an existing lease by handle.
    assert_eq!(m.zero_copy_handoffs, 2 * N, "{m:?}");
    assert_eq!(report.data_plane.heap_deliveries, 0);
    assert_eq!(report.data_plane.arena_deliveries, 4 * N);
    assert_eq!(m.in_use, 0, "every slot reclaimed after the run");

    let expected: Vec<_> = (1..=N).map(|n| (n, inverted_checksum(n, LEN))).collect();
    assert_eq!(recorded_frames(&s), expected);
}

#[test]
fn heap_source_costs_exactly_one_counted_copy_per_frame() {
    let s = services(8);
    let report = run(&pass_through_pipeline(false), &s);
    let m = arena(&report);
    assert_eq!(
        m.copies_in, N,
        "one heap→arena copy per frame at the first worker"
    );
    assert_eq!(m.bytes_copied_in, N * LEN as u64);
    assert_eq!(m.copies_out, 0);
    assert_eq!(
        m.transform_writes, N,
        "only the worker writes into the arena"
    );
    assert_eq!(
        m.zero_copy_handoffs, N,
        "the classifier still gets the invert output by handle"
    );
    assert_eq!(report.data_plane.heap_deliveries, N);
    assert_eq!(m.in_use, 0);
}

#[test]
fn fan_out_to_two_workers_shares_one_slot() {
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: fan }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 8, height: 8, arena: true }} }}\n    - {{ id: inv, module: py.invert-transformer }}\n    - {{ id: cls, module: py.brightness-classifier }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: inv.frame }}\n      - {{ from: cam.frame, to: cls.frame }}\n      - {{ from: inv.frame, to: sink.frame }}\n      - {{ from: cls.class, to: sink.class }}\n  run: {{ maxCycles: {N} }}\n"
    );
    let s = services(3); // source slot + invert output slot, one spare
    let report = run(&text, &s);
    assert_eq!(report.end, EndReason::CycleLimit { limit: N });
    let m = arena(&report);
    assert_eq!((m.copies_in, m.copies_out), (0, 0));
    assert_eq!(
        m.zero_copy_handoffs,
        2 * N,
        "both workers read the same slot"
    );
    assert_eq!(m.in_use, 0);
}

#[test]
fn full_arena_is_refused_explicitly_and_cleaned_up() {
    // The source holds the only slot; the invert worker cannot get an output slot.
    let s = services(1);
    let report = run(&pass_through_pipeline(true), &s);
    match &report.end {
        EndReason::NodeFailed { node, cycle, error } => {
            assert_eq!(node, "inv");
            assert_eq!(*cycle, 1);
            assert_eq!(error.kind, NodeErrorKind::Resource);
            assert!(error.message.contains("arena full"), "{}", error.message);
        }
        other => panic!("unexpected {other:?}"),
    }
    let m = arena(&report);
    assert!(m.admission_refusals >= 1);
    assert_eq!(m.in_use, 0);
}

#[test]
fn worker_crash_releases_its_input_and_output_slots() {
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: crash }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { arena: true } }\n    - { id: w, contract: crash-on-execute.yaml }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: w.frame }\n      - { from: w.class, to: sink.class }\n  run: { maxCycles: 5 }\n";
    let s = services(4);
    let report = run(text, &s);
    assert!(matches!(
        &report.end,
        EndReason::NodeFailed { error, .. } if error.kind == NodeErrorKind::ProcessExited
    ));
    assert_eq!(arena(&report).in_use, 0);
}

#[test]
fn input_mutation_is_detected_when_verification_is_on() {
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: mutate }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { arena: true } }\n    - { id: w, contract: mutate-input.yaml }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: w.frame }\n      - { from: w.class, to: sink.class }\n  run: { maxCycles: 3 }\n";

    // Default: not detected. Workers map the arena read-write; this is the documented limit.
    let report = run_with(text, &services(4), WorkerBackend::from_env(), &fixtures());
    assert_eq!(report.end, EndReason::CycleLimit { limit: 3 });

    let report = run_with(
        text,
        &services(4),
        WorkerBackend::from_env().with_input_verification(true),
        &fixtures(),
    );
    match &report.end {
        EndReason::NodeFailed { error, .. } => {
            assert_eq!(error.kind, NodeErrorKind::InvalidOutput);
            assert!(error.message.contains("read-only input"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

fn python() -> String {
    std::env::var("MVF_PYTHON").unwrap_or_else(|_| {
        if cfg!(windows) {
            "python".into()
        } else {
            "python3".into()
        }
    })
}

#[test]
fn python_sdk_and_rust_agree_on_the_header_layout() {
    use mvf_memory::{Descriptor, HEADER_SIZE};
    // Python writes a header + payload; we compare bytes, then let Python parse a Rust header.
    let script = r#"
import sys, mvf_sdk
buf = bytearray(192 + 18)
mvf_sdk._write_descriptor(buf, 0, mvf_sdk.MediaType.IMAGE, mvf_sdk.ElementType.UINT8, (2, 3, 3), bytes(range(18)), 18)
print(buf.hex())
rust = bytes.fromhex(sys.argv[1])
print(mvf_sdk._read_descriptor(rust, 0))
"#;
    let d = Descriptor::image_u8(2, 3, 3);
    let mut header = [0u8; HEADER_SIZE];
    d.write_header(&mut header).expect("write");
    let hex: String = header.iter().map(|b| format!("{b:02x}")).collect();
    let out = std::process::Command::new(python())
        .args(["-c", script, &hex])
        .env("PYTHONPATH", repo().join("src/sdk/python"))
        .output()
        .expect("python");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    let py = lines.next().expect("python header");
    let mut expected = hex.clone();
    expected.extend((0..18u8).map(|b| format!("{b:02x}")));
    assert_eq!(py, expected, "Python SDK and Rust headers differ");
    assert_eq!(lines.next(), Some("(2, 0, (2, 3, 3), 18)"));
}

fn cpp_invert() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MVF_CPP_INVERT") {
        return Some(PathBuf::from(p));
    }
    let build = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/cpp-sdk");
    [
        build.join("mvf_invert_transformer"),
        build.join("Release/mvf_invert_transformer.exe"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

#[test]
fn cpp_sdk_worker_shares_the_arena_with_python_workers() {
    let Some(binary) = cpp_invert() else {
        assert!(
            std::env::var_os("MVF_REQUIRE_CPP").is_none(),
            "MVF_REQUIRE_CPP is set but the C++ SDK example was not built"
        );
        eprintln!(
            "NOT RUN: C++ SDK example not built (cmake -S src/sdk/cpp -B rust/target/cpp-sdk)"
        );
        return;
    };
    let dir = binary.parent().expect("dir");
    let entry = binary.file_name().and_then(|n| n.to_str()).expect("name");
    std::fs::write(
        dir.join("cpp-invert.yaml"),
        format!(
            "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: cpp.invert-transformer, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [ {{ name: frame, channel: data, type: frame }} ]\n    outputs: [ {{ name: frame, channel: data, type: frame }} ]\n  artifacts: [ {{ backend: worker, runtime: executable, entry: {entry}, protocol: 1 }} ]\n  isolation: {{ minimum: process }}\n"
        ),
    )
    .expect("contract");
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: cpp }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 8, height: 8, arena: true }} }}\n    - {{ id: inv, contract: cpp-invert.yaml }}\n    - {{ id: cls, module: py.brightness-classifier }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: inv.frame }}\n      - {{ from: inv.frame, to: cls.frame }}\n      - {{ from: inv.frame, to: sink.frame }}\n      - {{ from: cls.class, to: sink.class }}\n  run: {{ maxCycles: {N} }}\n"
    );
    let s = services(8);
    let report = run_with(&text, &s, WorkerBackend::from_env(), dir);
    assert_eq!(
        report.end,
        EndReason::CycleLimit { limit: N },
        "{:?}",
        s.logs.events()
    );
    let m = arena(&report);
    assert_eq!((m.copies_in, m.copies_out), (0, 0));
    assert_eq!(
        m.zero_copy_handoffs,
        2 * N,
        "C++ reads the source slot, Python reads C++'s"
    );
    let expected: Vec<_> = (1..=N).map(|n| (n, inverted_checksum(n, LEN))).collect();
    assert_eq!(
        recorded_frames(&s),
        expected,
        "C++ output matches the Rust/Python inversion"
    );
}
