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

/// A presenter version `v` (gray level `v × 60`); v3 crashes on its 20th frame.
fn presenter(base: &Path, version: u8) -> String {
    let dir = base.join(format!("v{version}"));
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(
        dir.join("presenter.py"),
        format!(
            "import os\nfrom mvf_sdk import analysis, run_analyzer\nfrom mvf_sdk import display\nVERSION = {version}\nseen = [0]\ndef analyze(payload, meta):\n    seen[0] += 1\n    if VERSION == 3 and seen[0] >= 20:\n        os._exit(3)\n    shown = [s.present(bytes([VERSION * 60]) * (32 * 24), 32, 24, 'gray8') for s in display.surfaces()]\n    return analysis(value={{'version': VERSION, 'shown': shown}})\nrun_analyzer('t.presenter', analyze)\n"
        ),
    )
    .expect("py");
    std::fs::write(
        dir.join("node.yaml"),
        format!(
            "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: t.presenter, version: {version}.0.0 }}\nspec:\n  ports:\n    inputs: [ {{ name: frame, channel: data, type: frame }} ]\n    outputs: [ {{ name: status, channel: data, type: json }} ]\n  artifacts: [ {{ backend: worker, runtime: python, entry: presenter.py, protocol: 1 }} ]\n  isolation: {{ minimum: process }}\n  lifecycle: {{ replacement: stateless, readinessTimeoutMs: 10000 }}\n  permissions:\n    display: {{ mode: managed-surface, maxWidth: 64, maxHeight: 64 }}\n  presentation: {{ title: Presenter }}\n"
        ),
    )
    .expect("yaml");
    format!("v{version}/node.yaml")
}

#[test]
fn the_window_follows_live_replacement_and_rollback() {
    let base = temp("replace");
    std::fs::create_dir_all(&base).expect("base");
    let root = base.join("display");
    let (v1, v2, v3) = (
        presenter(&base, 1),
        presenter(&base, 2),
        presenter(&base, 3),
    );
    let s = HostServices {
        display: Arc::new(DisplayRegistry::new(&root)),
        ..HostServices::default()
    };
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: replace }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 8, height: 8, channels: 1, frames: 460, intervalMs: 10 }} }}\n    - {{ id: show, contract: {v1} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: show.frame }}\n      - {{ from: show.status, to: sink.json }}\n"
    );
    let resolver = HostResolver::new(&base);
    let graph = build_graph(&text, &resolver).expect("graph");
    let worker = WorkerBackend::from_env().with_python_sdk(repo().join("src/sdk/python"));
    let mut engine = Engine::build(graph, &default_backends().with(worker), &s).expect("build");
    struct Driver {
        control: ControlHandle,
        steps: Vec<(u64, ReplacementRequest)>,
    }
    impl Observer for Driver {
        fn cycle_completed(&mut self, cycle: u64) {
            for (at, r) in &self.steps {
                if *at == cycle + 1 {
                    self.control.replace(r.clone());
                }
            }
        }
    }
    let request = |c: &str, probation: u64| {
        ReplacementRequest::resolve(
            &resolver,
            "show",
            &NodeReference::Contract(c.to_string()),
            Default::default(),
            probation,
        )
        .expect("request")
    };
    let mut driver = Driver {
        control: engine.control(),
        // 210 cycles of 10 ms between the steps: room for a cold Python start on a slow runner.
        steps: vec![(40, request(&v2, 20)), (250, request(&v3, 200))],
    };
    let (report, shown) = std::thread::scope(|scope| {
        let run = scope.spawn(|| engine.run(&RunOptions::default(), &mut driver));
        let found = loop {
            let found = discover(&root);
            if !found.is_empty() {
                break found;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // One host reader for the whole run: the window is never reopened.
        let mut host = SurfaceReader::open(&found[0].surface).expect("host");
        let (mut px, mut shown) = (Vec::new(), Vec::<(u64, u8)>::new());
        while !run.is_finished() {
            if let Some(info) = host.take_latest(&mut px) {
                shown.push((info.seq, px[0]));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(host.rejected, 0);
        (run.join().expect("run"), shown)
    });
    use mvf_engine::ReplacementEventKind as K;
    let kinds: Vec<K> = report.replacements.iter().map(|r| r.kind).collect();
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        s.logs.events()
    );
    assert!(
        kinds.windows(2).any(|w| w == [K::Promoted, K::Committed])
            && kinds.contains(&K::RolledBack),
        "{kinds:?}"
    );
    // The versions the window showed, in order, collapsed: v1 -> v2 -> v3 -> back to v2.
    let mut order: Vec<u8> = shown.iter().map(|(_, g)| *g).collect();
    order.dedup();
    assert_eq!(order, vec![60, 120, 180, 120], "{shown:?}");
    assert!(
        shown.windows(2).all(|w| w[0].0 < w[1].0),
        "one sequence across generations"
    );
    let values = statuses(&s);
    assert_eq!(
        values.len(),
        460,
        "rollback re-ran the failed cycle: none lost, none doubled"
    );
    assert!(
        values
            .iter()
            .all(|v| v["shown"] == serde_json::json!([true]))
    );
    let cutovers: Vec<String> = report
        .replacements
        .iter()
        .filter(|r| matches!(r.kind, K::Promoted | K::RolledBack))
        .map(|r| format!("{:?} {:?}us", r.kind, r.cutover_us))
        .collect();
    eprintln!(
        "window across replacement: {} frames shown, versions {order:?}, {cutovers:?}",
        shown.len()
    );
}
