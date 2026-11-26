//! GPU vs CPU on the same typed graph (invert → normalize → mean), matched synthetic frames.
//!   cargo run --release -p mvf-compute --example gpu_bench [frames]
//! Prints per-cycle latency percentiles, throughput and transfer accounting per resolution.

use std::path::Path;
use std::time::Instant;

use mvf_compute::{ComputeBackend, GpuConfig, GpuContext};
use mvf_engine::*;
use mvf_graph::build_graph;

struct Timer {
    last: Instant,
    cycles: Vec<f64>,
}

impl Observer for Timer {
    fn cycle_completed(&mut self, _: u64) {
        let now = Instant::now();
        self.cycles.push((now - self.last).as_secs_f64() * 1e3);
        self.last = now;
    }
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

fn run(
    file: &str,
    w: u32,
    h: u32,
    frames: u64,
    registry: &BackendRegistry,
) -> (Vec<f64>, RunReport) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/gpu");
    let text = std::fs::read_to_string(dir.join(file))
        .expect("pipeline")
        .replace(
            "width: 640, height: 480, frames: 100",
            &format!("width: {w}, height: {h}, frames: {frames}"),
        );
    let graph = build_graph(&text, &HostResolver::new(&dir)).expect("graph");
    let services = HostServices::default();
    let mut engine = Engine::build(graph, registry, &services).expect("build");
    let mut timer = Timer {
        last: Instant::now(),
        cycles: Vec::new(),
    };
    engine.start(&CancellationToken::new()).expect("start");
    timer.last = Instant::now();
    let report = engine.run(&RunOptions::default(), &mut timer);
    let mut c = timer.cycles;
    c.remove(0); // first cycle includes pipeline/shader compilation
    c.sort_by(|a, b| a.total_cmp(b));
    (c, report)
}

fn main() {
    let frames: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(200);
    let gpu = GpuContext::new(&GpuConfig {
        memory_budget: 2 << 30,
        ..GpuConfig::default()
    })
    .expect("GPU");
    let info = gpu.info();
    println!(
        "GPU: {} ({:?}), {frames} frames per row, release build",
        info.name, info.backend
    );
    println!(
        "{:<10} {:<5} {:>9} {:>9} {:>9} {:>10} {:>12} {:>14}",
        "frame", "path", "p50 ms", "p95 ms", "p99 ms", "fps", "host->dev", "dev->host"
    );
    for (w, h, label) in [
        (640, 480, "640x480"),
        (1920, 1080, "1920x1080"),
        (3840, 2160, "3840x2160"),
    ] {
        let cpu_reg = default_backends();
        let (c, cr) = run("pipeline-cpu.yaml", w, h, frames, &cpu_reg);
        let before = gpu.metrics();
        let gpu_reg = default_backends().with(ComputeBackend::new(gpu.clone()));
        let (g, gr) = run("pipeline.yaml", w, h, frames, &gpu_reg);
        let after = gpu.metrics();
        for (path, s, r, up, down) in [
            ("cpu", &c, &cr, 0, 0),
            (
                "gpu",
                &g,
                &gr,
                after.upload_bytes - before.upload_bytes,
                after.download_bytes - before.download_bytes,
            ),
        ] {
            println!(
                "{label:<10} {path:<5} {:>9.3} {:>9.3} {:>9.3} {:>10.1} {:>12} {:>14}",
                pct(s, 0.5),
                pct(s, 0.95),
                pct(s, 0.99),
                r.cycles_completed as f64 / (r.cycles_ms / 1e3),
                up,
                down,
            );
        }
    }
}
