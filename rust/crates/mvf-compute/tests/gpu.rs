//! Real GPU execution (no CPU simulation). Skips with a visible `NOT RUN` when the host has no GPU
//! adapter; `MVF_REQUIRE_GPU=1` turns that into a failure (used for the recorded local run).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mvf_compute::*;
use mvf_engine::*;
use mvf_graph::build_graph;

fn gpu_with(config: &GpuConfig) -> Option<GpuContext> {
    match GpuContext::new(config) {
        Ok(g) => Some(g),
        Err(e) => {
            assert!(
                std::env::var_os("MVF_REQUIRE_GPU").is_none(),
                "MVF_REQUIRE_GPU is set but: {e}"
            );
            eprintln!("NOT RUN: {e}");
            None
        }
    }
}

fn gpu() -> Option<GpuContext> {
    gpu_with(&GpuConfig::default())
}

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/gpu")
}

fn bytes(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761).max(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x & 0xff) as u8
        })
        .collect()
}

#[test]
fn runs_on_real_gpu_hardware() {
    let Some(g) = gpu() else { return };
    let info = g.info();
    eprintln!(
        "GPU: {} | backend {:?} | type {:?} | driver {} {} | vendor {:04x} device {:04x}",
        info.name,
        info.backend,
        info.device_type,
        info.driver,
        info.driver_info,
        info.vendor,
        info.device
    );
    assert_ne!(
        info.device_type,
        wgpu::DeviceType::Cpu,
        "software adapter is not a GPU"
    );
}

#[test]
fn kernels_match_the_cpu_reference() {
    let Some(g) = gpu() else { return };
    for len in [1usize, 3, 4, 1001, 65_537] {
        let data = bytes(len, len as u32);
        let input = g.upload(&data).expect("upload");
        let inv = g.alloc(len).expect("alloc");
        g.dispatch(Kernel::InvertU8, &input, &inv, len.div_ceil(4) as u32, None)
            .expect("invert");
        assert_eq!(
            inv.read().expect("read"),
            data.iter().map(|b| 255 - b).collect::<Vec<_>>(),
            "invert len {len}"
        );

        let norm = g.alloc(len * 4).expect("alloc");
        g.dispatch(Kernel::NormalizeU8F32, &input, &norm, len as u32, None)
            .expect("normalize");
        let got: Vec<f32> = norm
            .read()
            .expect("read")
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let want: Vec<f32> = data.iter().map(|b| f32::from(*b) / 255.0).collect();
        // WGSL division may be off by up to 2.5 ULP (Metal is not correctly rounded), so compare
        // within that bound rather than bit for bit.
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(
                (g - w).abs() <= 3.0 * f32::EPSILON * w.abs().max(f32::MIN_POSITIVE),
                "normalize len {len} [{i}]: {g} vs {w}"
            );
        }

        let partial = g.alloc(64 * 4).expect("alloc");
        g.dispatch(Kernel::MeanF32, &norm, &partial, len as u32, Some(64))
            .expect("mean");
        let sum: f64 = partial
            .read()
            .expect("read")
            .chunks_exact(4)
            .map(|c| f64::from(f32::from_le_bytes([c[0], c[1], c[2], c[3]])))
            .sum();
        let cpu: f64 = want.iter().map(|v| f64::from(*v)).sum();
        assert!(
            (sum - cpu).abs() <= 1e-5 * cpu.max(1.0),
            "mean len {len}: gpu {sum} cpu {cpu}"
        );
    }
}

fn engine(pipeline: &str, registry: &BackendRegistry, services: &HostServices) -> Engine {
    let text = std::fs::read_to_string(examples().join(pipeline)).expect("pipeline");
    let graph = build_graph(&text, &HostResolver::new(examples())).expect("graph");
    Engine::build(graph, registry, services).expect("build")
}

fn means(services: &HostServices) -> Vec<(u64, f64)> {
    services
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Json { value } => {
                Some((r.cycle, value["mean"].as_f64().unwrap_or(f64::NAN)))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn device_resident_chain_matches_cpu_and_never_round_trips_through_the_host() {
    let Some(g) = gpu() else { return };
    let gpu_services = HostServices::default();
    let registry = default_backends().with(ComputeBackend::new(g.clone()));
    let report = engine("pipeline.yaml", &registry, &gpu_services)
        .run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() }
    );
    let n = report.cycles_completed;
    assert_eq!(n, 100);
    for node in ["invert", "normalize", "mean"] {
        assert_eq!(
            report
                .nodes
                .iter()
                .find(|s| s.id == node)
                .expect("node")
                .backend,
            "compute"
        );
    }

    let cpu_services = HostServices::default();
    let cpu = engine("pipeline-cpu.yaml", &default_backends(), &cpu_services)
        .run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(cpu.cycles_completed, n);
    let (gm, cm) = (means(&gpu_services), means(&cpu_services));
    assert_eq!(gm.len(), cm.len());
    for ((c, a), (_, b)) in gm.iter().zip(&cm) {
        assert!((a - b).abs() < 1e-5, "cycle {c}: gpu {a} cpu {b}");
    }

    let m = g.metrics();
    let frame = 640 * 480;
    assert_eq!(m.uploads, n, "exactly one host→device transfer per frame");
    assert_eq!(m.upload_bytes, n * frame);
    assert_eq!(
        m.device_handoffs,
        2 * n,
        "invert→normalize and normalize→mean by handle"
    );
    assert_eq!(m.downloads, n, "only the compact partial sums come back");
    assert_eq!(m.download_bytes, n * 64 * 4);
    assert_eq!(m.dispatches, 3 * n);
    assert_eq!(m.allocated, 0, "every device buffer was released");
    assert_eq!(report.data_plane.device_deliveries, 2 * n);
    eprintln!(
        "GPU chain: {n} frames of {frame} bytes in {:.1} ms (CPU chain {:.1} ms); peak device memory {} bytes; {m:?}",
        report.cycles_ms, cpu.cycles_ms, m.peak_allocated
    );
}

#[test]
fn a_host_consumer_forces_an_explicit_counted_download() {
    let Some(g) = gpu() else { return };
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: tap }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 8, height: 2, frames: 5 } }\n    - { id: invert, contract: nodes/gpu-invert.yaml }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: invert.frame }\n      - { from: invert.frame, to: sink.frame }\n";
    let graph = build_graph(text, &HostResolver::new(examples())).expect("graph");
    let services = HostServices::default();
    let mut e = Engine::build(
        graph,
        &default_backends().with(ComputeBackend::new(g.clone())),
        &services,
    )
    .expect("build");
    e.run(&RunOptions::default(), &mut NullObserver);
    let sums: Vec<String> = services
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Frame { checksum, .. } => Some(checksum),
            _ => None,
        })
        .collect();
    let want: Vec<String> = (1..=5u64)
        .map(|s| {
            format!(
                "{:016x}",
                fnv1a64(
                    &builtins::SyntheticSource::pixels(0, s, 16)
                        .iter()
                        .map(|b| 255 - b)
                        .collect::<Vec<_>>()
                )
            )
        })
        .collect();
    assert_eq!(sums, want);
    let m = g.metrics();
    assert_eq!(
        (m.uploads, m.downloads, m.download_bytes),
        (5, 5, 5 * 16),
        "{m:?}"
    );
}

#[test]
fn zero_copy_is_refused_across_devices_and_transfer_is_explicit_otherwise() {
    let (Some(a), Some(b)) = (gpu(), gpu()) else {
        return;
    };
    assert_ne!(a.id(), b.id(), "two logical devices");
    let data = bytes(1024, 7);
    let on_a: Arc<dyn DeviceBuffer> = Arc::new(a.upload(&data).expect("upload"));
    let payload = Payload::device(Descriptor::blob(1024), on_a).expect("payload");
    let mut inputs = Inputs::new();
    inputs.insert("frame", Value::Tensor(payload));
    let cancel = CancellationToken::new();
    let ctx = ExecContext {
        cycle: 1,
        node_id: "n",
        generation: 1,
        cancel: &cancel,
        deadline: None,
    };

    let mut strict = ComputeNode::new(b.clone(), Kernel::InvertU8, true);
    let err = strict.execute(&ctx, &inputs).expect_err("refused");
    assert_eq!(err.kind, NodeErrorKind::Unsupported);
    assert!(
        err.message.contains("zero copy required"),
        "{}",
        err.message
    );
    assert_eq!(b.metrics().uploads, 0, "nothing was transferred");

    let mut lenient = ComputeNode::new(b.clone(), Kernel::InvertU8, false);
    let out = lenient.execute(&ctx, &inputs).expect("transferred");
    let Execution::Produced(out) = out else {
        panic!("no output")
    };
    let Some(Value::Tensor(t)) = out.get("frame") else {
        panic!("no tensor")
    };
    assert_eq!(
        t.try_bytes().expect("download"),
        data.iter().map(|x| 255 - x).collect::<Vec<_>>().as_slice()
    );
    assert_eq!(a.metrics().downloads, 1, "explicit device A → host");
    assert_eq!(b.metrics().uploads, 1, "explicit host → device B");
}

#[test]
fn device_memory_budget_is_enforced_and_released() {
    let Some(g) = gpu_with(&GpuConfig {
        memory_budget: 64 * 1024,
        ..GpuConfig::default()
    }) else {
        return;
    };
    let keep = g.upload(&bytes(48 * 1024, 1)).expect("fits");
    let err = g.upload(&bytes(32 * 1024, 2)).expect_err("over budget");
    assert!(err.contains("budget"), "{err}");
    drop(keep);
    assert_eq!(g.metrics().allocated, 0);
    assert!(
        g.upload(&bytes(32 * 1024, 3)).is_ok(),
        "released memory is reusable"
    );

    let mut node = ComputeNode::new(g.clone(), Kernel::NormalizeU8F32, true);
    let mut inputs = Inputs::new();
    inputs.insert(
        "frame",
        Value::Tensor(Payload::heap(Descriptor::blob(40 * 1024), bytes(40 * 1024, 4)).expect("p")),
    );
    let cancel = CancellationToken::new();
    let ctx = ExecContext {
        cycle: 1,
        node_id: "n",
        generation: 1,
        cancel: &cancel,
        deadline: None,
    };
    let err = node
        .execute(&ctx, &inputs)
        .expect_err("4x output exceeds the budget");
    assert_eq!(err.kind, NodeErrorKind::Resource);
    assert_eq!(g.metrics().allocated, 0, "nothing leaked on the error path");
}

#[test]
fn cancellation_is_checked_before_dispatch() {
    let Some(g) = gpu() else { return };
    let mut node = ComputeNode::new(g.clone(), Kernel::InvertU8, true);
    let mut inputs = Inputs::new();
    inputs.insert(
        "frame",
        Value::Tensor(Payload::heap(Descriptor::blob(4), vec![1, 2, 3, 4]).expect("p")),
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ctx = ExecContext {
        cycle: 1,
        node_id: "n",
        generation: 1,
        cancel: &cancel,
        deadline: None,
    };
    assert_eq!(
        node.execute(&ctx, &inputs).expect_err("cancelled").kind,
        NodeErrorKind::Cancelled
    );
    assert_eq!(g.metrics().dispatches, 0);
}

#[test]
fn device_contracts_are_rejected_without_a_gpu_and_fall_back_only_when_allowed() {
    let text = |node: &str| {
        format!(
            "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: n }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source }}\n    - {{ id: x, contract: nodes/{node} }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: x.frame }}\n"
        )
    };
    let cpu_only = default_backends();
    let graph =
        build_graph(&text("gpu-invert.yaml"), &HostResolver::new(examples())).expect("graph");
    let err =
        Engine::build(graph, &cpu_only, &HostServices::default()).expect_err("no GPU backend");
    assert_eq!(err.error.kind, NodeErrorKind::Unsupported);
    assert!(
        err.error
            .message
            .contains("negotiation.backend-unavailable"),
        "{}",
        err.error.message
    );

    let contract = mvf_contract::parse_contract(
        &std::fs::read_to_string(examples().join("nodes/invert-gpu-or-cpu.yaml")).expect("c"),
    )
    .expect("valid");
    let placement = cpu_only.negotiate(&contract).expect("falls back");
    assert_eq!(placement.backend, mvf_contract::BackendKind::InProcess);
    assert_eq!(placement.notes[0].code, "placement.fallback-to-cpu");
    if let Some(g) = gpu() {
        let with_gpu = default_backends().with(ComputeBackend::new(g));
        assert_eq!(
            with_gpu.negotiate(&contract).expect("gpu").backend,
            mvf_contract::BackendKind::Compute
        );
    }
}
