//! Display Fabric with real Python workers (#30): a camera preview presents into managed surfaces,
//! a click comes back as typed data, and the graph never waits for a display host.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvf_display::{DisplayRegistry, InputEvent, InputKind, SurfaceReader, discover};
use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_worker::WorkerBackend;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn temp(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("mvf-worker-display-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn preview_pipeline(frames: u64, interval_ms: u64) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: preview }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 64, height: 48, channels: 3, frames: {frames}, intervalMs: {interval_ms}, seed: 3 }} }}\n    - {{ id: preview, module: py.camera-preview }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: preview.frame }}\n      - {{ from: preview.status, to: sink.json }}\n"
    )
}

fn build(text: &str, services: &HostServices) -> Engine {
    let resolver = HostResolver::new(repo().join("rust/crates/mvf-worker/tests/fixtures"))
        .with_module_root(repo().join("modules"));
    let graph = build_graph(text, &resolver).expect("graph");
    Engine::build(
        graph,
        &default_backends().with(WorkerBackend::from_env()),
        services,
    )
    .expect("build")
}

fn statuses(s: &HostServices) -> Vec<serde_json::Value> {
    s.recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Json { value } => Some(value),
            _ => None,
        })
        .collect()
}

/// The synthetic camera's pattern: byte `i` of a frame is `(base + i) & 0xff`.
fn byte(base: u8, i: usize) -> u8 {
    base.wrapping_add(i as u8)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

#[test]
fn a_python_preview_presents_frames_and_a_click_becomes_typed_data() {
    let root = temp("preview");
    let s = HostServices {
        display: Arc::new(DisplayRegistry::new(&root)),
        ..HostServices::default()
    };
    let mut engine = build(&preview_pipeline(80, 25), &s);
    let (w, row) = (64usize, 64usize * 3);

    let report = std::thread::scope(|scope| {
        let run = scope.spawn(|| engine.run(&RunOptions::default(), &mut NullObserver));
        // The test is the display host: find the node's two surfaces and present them.
        let deadline = Instant::now() + Duration::from_secs(20);
        let found = loop {
            let found = discover(&root);
            if found.len() == 2 || Instant::now() > deadline {
                break found;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(found.len(), 2, "live and detail surfaces");
        assert_eq!(found[0].descriptor.title, "Camera preview (1)");
        assert!(found[0].descriptor.grants.mouse && !found[0].descriptor.grants.keyboard);
        let mut live = SurfaceReader::open(&found[0].surface).expect("live");
        let mut detail = SurfaceReader::open(&found[1].surface).expect("detail");
        let (mut px, mut dpx) = (Vec::new(), Vec::new());
        let (mut shown, mut clicked_at, mut marked, mut detail_ok) = (0, None, 0, 0);
        let mut latency_ms = Vec::new();
        let mut beat = Instant::now();
        while !run.is_finished() {
            if beat.elapsed() > Duration::from_millis(200) {
                live.update_status(|_| {});
                detail.update_status(|_| {});
                beat = Instant::now();
            }
            if let Some(info) = live.take_latest(&mut px) {
                let now_us = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_micros() as u64)
                    .unwrap_or(0);
                latency_ms.push(now_us.saturating_sub(info.timestamp_us) as f64 / 1000.0);
                shown += 1;
                assert_eq!((info.width, info.height), (64, 48));
                let base = px[0];
                // Away from the crosshair every pixel is the camera's.
                assert_eq!(px[30 * row + 40 * 3], byte(base, 30 * row + 40 * 3));
                match clicked_at {
                    None if shown >= 3 => {
                        assert!(live.push(&InputEvent::pointer(
                            InputKind::PointerDown,
                            10.0,
                            12.0
                        )));
                        clicked_at = Some(info.seq);
                    }
                    Some(at) if info.seq > at + 2 => {
                        let i = 12 * row + 10 * 3;
                        assert_eq!(px[i], 255 - byte(base, i), "crosshair at the click");
                        marked += 1;
                    }
                    _ => {}
                }
            }
            if let Some(info) = detail.take_latest(&mut dpx)
                && clicked_at.is_some_and(|at| info.seq > at + 2)
            {
                // A quarter-size crop centred on the click, rows taken from the frame.
                assert_eq!((info.width, info.height), (16, 12));
                assert_eq!(dpx[3], dpx[0].wrapping_add(3));
                assert_eq!(dpx[16 * 3], dpx[0].wrapping_add(row as u8));
                detail_ok += 1;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let result = run.join().expect("run thread");
        latency_ms.sort_by(f64::total_cmp);
        (result, shown, marked, detail_ok, latency_ms, live.rejected)
    });
    let (result, shown, marked, detail_ok, latency, rejected) = report;
    assert_eq!(
        result.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        s.logs.events()
    );
    let st = statuses(&s);
    assert_eq!(
        st.len(),
        80,
        "one status per frame: the preview never stalled the graph"
    );
    let last = st.last().expect("status");
    assert_eq!(last["presented"], true);
    assert_eq!(last["windows"], 2);
    assert_eq!(last["hostAttached"], true);
    assert_eq!(last["roi"], serde_json::json!([10, 12]));
    assert_eq!(last["clicks"], 1);
    assert!(
        shown >= 40 && marked >= 10 && detail_ok >= 10,
        "{shown} {marked} {detail_ok}"
    );
    assert_eq!(rejected, 0, "no frame failed its CRC");
    let _ = w;
    eprintln!(
        "Python preview 64x48 RGB @ 40 fps: {shown} frames shown; present -> host copy ms p50={:.2} p95={:.2} p99={:.2} (host polls every ~2 ms)",
        percentile(&latency, 0.5),
        percentile(&latency, 0.95),
        percentile(&latency, 0.99)
    );
    assert!(
        s.logs
            .events()
            .iter()
            .any(|l| l.message.starts_with("display: 2 managed surface(s)")),
        "the effective display grant is logged"
    );
    let run_dir = s.display.run_dir().expect("run dir");
    drop(engine);
    drop(s);
    assert!(
        !run_dir.exists(),
        "surfaces are removed when the engine stops"
    );
}

#[test]
fn the_graph_never_waits_for_a_missing_display_host() {
    let root = temp("headless");
    let s = HostServices {
        display: Arc::new(DisplayRegistry::new(&root)),
        ..HostServices::default()
    };
    let mut engine = build(&preview_pipeline(200, 0), &s);
    let t0 = Instant::now();
    let r = engine.run(&RunOptions::default(), &mut NullObserver);
    let elapsed = t0.elapsed();
    assert_eq!(r.end, EndReason::SourceExhausted { node: "cam".into() });
    let st = statuses(&s);
    assert_eq!(st.len(), 200);
    let odd: Vec<_> = st
        .iter()
        .filter(|v| v["presented"] != true || v["hostAttached"] != false)
        .collect();
    assert!(odd.is_empty(), "{odd:?}");
    eprintln!(
        "200 frames through the preview with no display host: {:?} ({:.2} ms/frame)",
        elapsed,
        elapsed.as_secs_f64() * 1000.0 / 200.0
    );
}
