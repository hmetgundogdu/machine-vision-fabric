//! Display Fabric end to end (#30): the Rust cube renderer under real input, resize and close; a
//! display host process that crashes and is replaced while the pipeline keeps running; native UI
//! gated on the session; and, where a window system exists (`MVF_DISPLAY_SMOKE=1`), real windows.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
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
    let dir = std::env::temp_dir().join(format!("mvf-host-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp");
    dir
}

/// A node package around one of this crate's binaries (contract entries are package-relative).
fn package(dir: &Path, id: &str, binary: &str, display: &str) -> PathBuf {
    let exe = format!("{binary}{}", std::env::consts::EXE_SUFFIX);
    let pkg = dir.join("modules").join(id);
    std::fs::create_dir_all(&pkg).expect("package");
    // CARGO_BIN_EXE_* names the binary built for this test run.
    let built = match binary {
        "mvf-cube" => env!("CARGO_BIN_EXE_mvf-cube"),
        _ => env!("CARGO_BIN_EXE_mvf-native-window"),
    };
    std::fs::copy(built, pkg.join(&exe)).expect("copy binary");
    std::fs::write(
        pkg.join("node.yaml"),
        format!(
            "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: {id}, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [ {{ name: frame, channel: data, type: frame }} ]\n    outputs: [ {{ name: view, channel: data, type: json }} ]\n  artifacts: [ {{ backend: worker, runtime: executable, entry: {exe}, protocol: 1 }} ]\n  isolation: {{ minimum: process }}\n  permissions:\n    display: {display}\n  presentation: {{ title: {id}, width: 320, height: 240 }}\n"
        ),
    )
    .expect("contract");
    pkg.join("node.yaml")
}

/// A pipeline `cam -> view -> sink` around the package at `contract`, and its module root.
fn pipeline(contract: &Path, frames: u64, interval_ms: u64) -> (String, PathBuf) {
    let pkg = contract.parent().expect("package dir");
    let id = pkg.file_name().expect("id").to_string_lossy().to_string();
    (
        format!(
            "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: display }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 64, height: 48, channels: 3, frames: {frames}, intervalMs: {interval_ms} }} }}\n    - {{ id: view, module: {id} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: view.frame }}\n      - {{ from: view.view, to: sink.json }}\n"
        ),
        pkg.parent().expect("module root").to_path_buf(),
    )
}

fn engine(
    (text, modules): &(String, PathBuf),
    s: &HostServices,
    worker: WorkerBackend,
) -> Result<Engine, String> {
    let resolver = HostResolver::new(repo().join("rust/crates/mvf-worker/tests/fixtures"))
        .with_module_root(modules)
        .with_module_root(repo().join("modules"));
    let graph = build_graph(text, &resolver).map_err(|e| e.to_string())?;
    Engine::build(graph, &default_backends().with(worker), s).map_err(|e| e.to_string())
}

fn values(s: &HostServices) -> Vec<serde_json::Value> {
    s.recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Json { value } => Some(value),
            _ => None,
        })
        .collect()
}

fn wait_for_surfaces(root: &Path, n: usize) -> Vec<mvf_display::Discovered> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let found = discover(root);
        if found.len() >= n || Instant::now() > deadline {
            return found;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_cube_follows_input_renders_at_the_requested_size_and_survives_close() {
    let dir = temp("cube");
    let root = dir.join("display");
    let contract = package(
        &dir,
        "rs.cube",
        "mvf-cube",
        "{ mode: managed-surface, mouse: true, keyboard: true, maxWidth: 640, maxHeight: 480 }",
    );
    let s = HostServices {
        display: Arc::new(DisplayRegistry::new(&root)),
        ..HostServices::default()
    };
    let mut e = engine(&pipeline(&contract, 400, 4), &s, WorkerBackend::from_env()).expect("build");
    let report = std::thread::scope(|scope| {
        let run = scope.spawn(|| e.run(&RunOptions::default(), &mut NullObserver));
        let found = wait_for_surfaces(&root, 1);
        let mut host = SurfaceReader::open(&found[0].surface).expect("host");
        host.update_status(|st| {
            st.requested_width = 200;
            st.requested_height = 150;
        });
        let mut px = Vec::new();
        let mut sizes = Vec::new();
        let mut step = 0;
        let mut background_ok = 0;
        let mut centre_textured = 0;
        let mut beat = Instant::now();
        while !run.is_finished() {
            if beat.elapsed() > Duration::from_millis(200) {
                host.update_status(|_| {});
                beat = Instant::now();
            }
            if let Some(info) = host.take_latest(&mut px) {
                sizes.push((info.width, info.height));
                let at = |x: u32, y: u32| {
                    let o = (y * info.stride + x * 4) as usize;
                    [px[o], px[o + 1], px[o + 2]]
                };
                // Corner: background gradient (b > r). Centre at rest: the textured front face.
                let c = at(1, 1);
                if c[0] > c[2] {
                    background_ok += 1;
                }
                if at(info.width / 2, info.height / 2) != c {
                    centre_textured += 1;
                }
                step += 1;
                match step {
                    5 => {
                        // Drag 100 px to the right: +1.0 rad of yaw.
                        host.push(&InputEvent::pointer(InputKind::PointerDown, 50.0, 50.0));
                        host.push(&InputEvent::pointer(InputKind::PointerMove, 100.0, 50.0));
                        host.push(&InputEvent::pointer(InputKind::PointerMove, 150.0, 50.0));
                        host.push(&InputEvent::pointer(InputKind::PointerUp, 150.0, 50.0));
                    }
                    15 => {
                        let mut wheel = InputEvent::new(InputKind::Wheel);
                        wheel.dy = 2.0;
                        host.push(&wheel);
                    }
                    25 => {
                        // The window is resized; the node renders at the new size.
                        host.update_status(|st| {
                            st.requested_width = 320;
                            st.requested_height = 120;
                        });
                        host.push(&InputEvent::pointer(InputKind::Resize, 320.0, 120.0));
                    }
                    35 => {
                        host.push(&InputEvent::key(InputKind::KeyDown, "r"));
                    }
                    45 => {
                        // The user closes the window: the node goes headless, the graph goes on.
                        host.push(&InputEvent::new(InputKind::CloseRequested));
                        host.update_status(|st| st.flags |= mvf_display::surface::FLAG_CLOSED);
                    }
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        (
            run.join().expect("run"),
            sizes,
            background_ok,
            centre_textured,
            step,
        )
    });
    let (result, sizes, background_ok, centre_textured, shown) = report;
    assert_eq!(
        result.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        s.logs.events()
    );
    let v = values(&s);
    assert_eq!(v.len(), 400, "every frame processed, window or not");
    assert!(v.iter().all(|x| x["textured"] == true));
    let yaw_after_drag = v
        .iter()
        .find(|x| x["yaw"].as_f64().is_some_and(|y| y > 0.9))
        .expect("the drag turned the cube");
    assert!((yaw_after_drag["yaw"].as_f64().unwrap_or(0.0) - 1.0).abs() < 1e-3);
    assert!(
        v.iter()
            .any(|x| (x["zoom"].as_f64().unwrap_or(0.0) - 1.21).abs() < 1e-3),
        "wheel zoom"
    );
    assert!(
        v.iter().any(|x| x["size"] == serde_json::json!([320, 120])),
        "resize honoured"
    );
    let last = v.last().expect("last");
    assert_eq!(last["yaw"], 0.0, "`r` reset the view");
    assert_eq!(last["closed"], true);
    assert_eq!(
        last["presented"], false,
        "a closed window costs no rendering"
    );
    assert!(last["events"].as_u64().unwrap_or(0) >= 8);
    assert_eq!(sizes.first(), Some(&(200, 150)));
    assert!(sizes.contains(&(320, 120)));
    assert!(
        shown >= 45 && background_ok == shown && centre_textured == shown,
        "{shown} {background_ok} {centre_textured}"
    );
    let render: Vec<f64> = v
        .iter()
        .filter_map(|x| x["renderMs"].as_f64())
        .filter(|t| *t > 0.0)
        .collect();
    eprintln!(
        "cube: {} frames rendered (debug build), mean render {:.2} ms at 200x150/320x120; {shown} shown by the host",
        render.len(),
        render.iter().sum::<f64>() / render.len().max(1) as f64
    );
}

fn host_process(root: &Path, stats: &Path, extra: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_mvf-display-host"))
        .arg("--dir")
        .arg(root)
        .arg("--stats")
        .arg(stats)
        .args(extra)
        .spawn()
        .expect("spawn host")
}

fn read_stats(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap_or_default()).unwrap_or_default()
}

#[test]
fn a_display_host_crash_never_stops_the_pipeline_and_a_new_host_reattaches() {
    let dir = temp("crash");
    let root = dir.join("display");
    let s = HostServices {
        display: Arc::new(DisplayRegistry::new(&root)),
        ..HostServices::default()
    };
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: crash }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 64, height: 48, channels: 3, frames: 300, intervalMs: 10 } }\n    - { id: preview, module: py.camera-preview }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: preview.frame }\n      - { from: preview.status, to: sink.json }\n";
    let mut e = engine(
        &(text.to_string(), repo().join("modules")),
        &s,
        WorkerBackend::from_env(),
    )
    .expect("build");
    let (first_stats, second_stats) = (dir.join("first.json"), dir.join("second.json"));
    let t0 = Instant::now();
    let (result, crashed_at) = std::thread::scope(|scope| {
        let run = scope.spawn(|| e.run(&RunOptions::default(), &mut NullObserver));
        assert_eq!(wait_for_surfaces(&root, 2).len(), 2);
        let mut first = host_process(&root, &first_stats, &["--headless"]);
        std::thread::sleep(Duration::from_millis(700));
        first.kill().expect("kill the host"); // a crash: no clean detach, no stats
        let _ = first.wait();
        let crashed_at = t0.elapsed();
        std::thread::sleep(Duration::from_millis(300));
        let mut second = host_process(
            &root,
            &second_stats,
            &["--headless", "--exit-after-ms", "1500"],
        );
        let _ = second.wait();
        (run.join().expect("run"), crashed_at)
    });
    assert_eq!(
        result.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        s.logs.events()
    );
    let v = values(&s);
    assert_eq!(v.len(), 300, "the host crash cost the pipeline nothing");
    assert!(v.iter().all(|x| x["presented"] == true));
    assert!(!first_stats.exists(), "the first host really crashed");
    let second = read_stats(&second_stats);
    let surfaces = second["surfaces"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        surfaces.len(),
        2,
        "the new host took over both surfaces: {second}"
    );
    // It presents on both surfaces. A loaded CI runner managed 9 frames in 900 ms (p95 present
    // latency 204 ms), so this asserts the takeover, not a frame rate.
    assert!(
        surfaces
            .iter()
            .all(|x| x["frames"].as_u64().unwrap_or(0) >= 5 && x["rejected"] == 0),
        "{second}"
    );
    let attached = v.iter().filter(|x| x["hostAttached"] == true).count();
    eprintln!(
        "host crash at {crashed_at:?}: pipeline completed {} frames in {:?}; {attached} frames saw a host; new host showed {} frames (p50 {} ms)",
        v.len(),
        t0.elapsed(),
        surfaces[0]["frames"],
        surfaces[0]["latencyMs"]["p50"]
    );
}

#[test]
fn native_windows_need_an_interactive_session_and_fail_closed() {
    let dir = temp("native-gate");
    let headless = mvf_display::session::probe_with_override(Some("headless"));
    let required = package(&dir, "rs.native", "mvf-native-window", "{ mode: native }");
    let s = HostServices::default();
    let err = match engine(
        &pipeline(&required, 1, 0),
        &s,
        WorkerBackend::from_env().with_session(headless.clone()),
    ) {
        Ok(_) => panic!("a native node must not start without an interactive session"),
        Err(e) => e,
    };
    assert!(err.contains("negotiation.display-unavailable"), "{err}");

    let optional_dir = temp("native-optional");
    let optional = package(
        &optional_dir,
        "rs.native",
        "mvf-native-window",
        "{ mode: native, required: false }",
    );
    let s = HostServices::default();
    let mut e = engine(
        &pipeline(&optional, 3, 0),
        &s,
        WorkerBackend::from_env().with_session(headless),
    )
    .expect("headless fallback");
    let r = e.run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(r.end, EndReason::SourceExhausted { node: "cam".into() });
    assert!(
        s.logs
            .events()
            .iter()
            .any(|l| l.message.starts_with("display: headless")),
        "{:?}",
        s.logs.events()
    );
}

fn smoke() -> bool {
    let on = std::env::var_os("MVF_DISPLAY_SMOKE").is_some_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set MVF_DISPLAY_SMOKE=1 to open real windows");
    }
    on
}

#[test]
fn a_real_window_presents_a_node_surface() {
    if !smoke() {
        return;
    }
    let session = mvf_display::session::probe();
    if !session.interactive() {
        eprintln!(
            "BLOCKED: {} (no window system for a real window)",
            session.detail
        );
        return;
    }
    let dir = temp("window");
    let root = dir.join("display");
    let registry = DisplayRegistry::new(&root);
    let request = mvf_display::SurfaceRequest {
        title: "Display smoke".into(),
        width: 320,
        height: 240,
        max_width: 640,
        max_height: 480,
        windows: 2,
        grants: mvf_display::Grants {
            keyboard: true,
            mouse: true,
        },
    };
    let paths = registry.surfaces("smoke", &request).expect("surfaces");
    let stats = dir.join("stats.json");
    let mut host = host_process(&root, &stats, &["--exit-after-ms", "2500"]);
    let mut writers: Vec<_> = paths
        .iter()
        .map(|p| mvf_display::SurfaceWriter::open(p).expect("writer"))
        .collect();
    let t0 = Instant::now();
    let mut n = 0u8;
    while t0.elapsed() < Duration::from_millis(2200) {
        for (i, w) in writers.iter_mut().enumerate() {
            let data: Vec<u8> = (0..320 * 240 * 4)
                .map(|k| (k as u8).wrapping_add(n).wrapping_mul(i as u8 + 1))
                .collect();
            w.present(&mvf_display::Frame {
                width: 320,
                height: 240,
                stride: 1280,
                format: mvf_display::PixelFormat::Bgra8,
                data: &data,
            })
            .expect("present");
        }
        n = n.wrapping_add(1);
        std::thread::sleep(Duration::from_millis(16));
    }
    let status = writers[0].host().expect("host status");
    let _ = host.wait();
    let st = read_stats(&stats);
    eprintln!("real window ({}): {st}", session.detail);
    assert_eq!(
        st["windowsOpened"].as_array().map(|a| a.len()),
        Some(2),
        "{st}"
    );
    // Every window showed the node's frames. Not a frame rate: a loaded runner presents a few
    // frames in the test's 2.5 s (7 at p95 436 ms on macOS CI), and the rate is not what this
    // test is about.
    let surfaces = st["surfaces"].as_array().cloned().unwrap_or_default();
    assert_eq!(surfaces.len(), 2, "{st}");
    assert!(
        surfaces
            .iter()
            .all(|s| s["frames"].as_u64().unwrap_or(0) >= 1
                && s["lastSeq"].as_u64().unwrap_or(0) >= 1),
        "{st}"
    );
    assert!(
        st["errors"].as_array().is_some_and(|e| e.is_empty()),
        "{st}"
    );
    assert!(
        status.requested_width > 0 && status.scale_milli >= 1000,
        "{status:?}"
    );
}

#[test]
fn a_native_ui_node_opens_its_own_window_in_an_interactive_session() {
    if !smoke() {
        return;
    }
    let session = mvf_display::session::probe();
    if !session.interactive() {
        eprintln!("BLOCKED: {}", session.detail);
        return;
    }
    let dir = temp("native");
    let contract = package(&dir, "rs.native", "mvf-native-window", "{ mode: native }");
    let s = HostServices::default();
    let mut e =
        engine(&pipeline(&contract, 120, 15), &s, WorkerBackend::from_env()).expect("build");
    let r = e.run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(
        r.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        s.logs.events()
    );
    let v = values(&s);
    let last = v.last().expect("value");
    eprintln!("native window ({}): {last}", session.detail);
    assert_eq!(last["windowOpen"], true, "{:?}", s.logs.events());
    assert!(last["framesDrawn"].as_u64().unwrap_or(0) > 10, "{last}");
    assert!(
        s.logs
            .events()
            .iter()
            .any(|l| l.message.starts_with("display: native windows"))
    );
}

/// Linux/X11 only (`MVF_DISPLAY_XDOTOOL=1`, e.g. under Xvfb): real OS input through a real window.
#[test]
fn real_x11_input_reaches_only_the_node_that_is_granted_it() {
    if std::env::var_os("MVF_DISPLAY_XDOTOOL").is_none_or(|v| v != "1") {
        eprintln!("skipped: set MVF_DISPLAY_XDOTOOL=1 under an X server with xdotool");
        return;
    }
    let dir = temp("xdotool");
    let root = dir.join("display");
    let registry = DisplayRegistry::new(&root);
    let make = |node: &str, title: &str, granted: bool| {
        registry
            .surfaces(
                node,
                &mvf_display::SurfaceRequest {
                    title: title.into(),
                    width: 320,
                    height: 240,
                    max_width: 320,
                    max_height: 240,
                    windows: 1,
                    grants: mvf_display::Grants {
                        keyboard: granted,
                        mouse: granted,
                    },
                },
            )
            .expect("surface")
            .remove(0)
    };
    let granted = make("granted", "Granted", true);
    let denied = make("denied", "Denied", false);
    let stats = dir.join("stats.json");
    let mut host = host_process(&root, &stats, &["--exit-after-ms", "6000"]);
    let mut nodes =
        [granted, denied].map(|p| mvf_display::SurfaceWriter::open(&p).expect("writer"));
    let frame = vec![128u8; 320 * 240 * 4];
    let present = |nodes: &mut [mvf_display::SurfaceWriter; 2]| {
        for n in nodes.iter_mut() {
            n.present(&mvf_display::Frame {
                width: 320,
                height: 240,
                stride: 1280,
                format: mvf_display::PixelFormat::Bgra8,
                data: &frame,
            })
            .expect("present");
        }
    };
    present(&mut nodes);
    let xdo = |args: &[&str]| -> String {
        let out = Command::new("xdotool")
            .args(args)
            .output()
            .expect("xdotool");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let window = |title: &str| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let id = xdo(&["search", "--name", &format!("^{title} - MVF$")]);
            if !id.is_empty() || Instant::now() > deadline {
                return id.lines().next().unwrap_or_default().to_string();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let (g, d) = (window("Granted"), window("Denied"));
    assert!(!g.is_empty() && !d.is_empty(), "both windows exist");
    for (win, key) in [(&g, "k"), (&d, "j")] {
        present(&mut nodes);
        xdo(&["windowmove", win, "0", "0"]);
        xdo(&["windowfocus", "--sync", win]);
        std::thread::sleep(Duration::from_millis(200));
        xdo(&["mousemove", "--sync", "160", "120"]);
        xdo(&["click", "1"]);
        xdo(&["key", key]);
        xdo(&["windowsize", win, "400", "300"]);
        std::thread::sleep(Duration::from_millis(400));
        present(&mut nodes);
    }
    std::thread::sleep(Duration::from_millis(300));
    let mut got = [Vec::new(), Vec::new()];
    for (n, out) in nodes.iter_mut().zip(got.iter_mut()) {
        n.poll_input(out);
    }
    let kinds = |e: &[InputEvent]| e.iter().map(|x| x.kind).collect::<Vec<_>>();
    eprintln!(
        "granted: {:?}",
        got[0]
            .iter()
            .map(|e| (e.kind.as_str(), e.x, e.y, e.text.clone()))
            .collect::<Vec<_>>()
    );
    eprintln!("denied: {:?}", kinds(&got[1]));
    assert!(
        got[0].iter().any(|e| e.kind == InputKind::PointerDown),
        "click reached the granted node"
    );
    assert!(
        got[0]
            .iter()
            .any(|e| e.kind == InputKind::KeyDown && e.text == "k"),
        "key reached the granted node"
    );
    assert!(
        got[0].iter().any(|e| e.kind == InputKind::Resize),
        "resize reported"
    );
    assert!(
        got[1].iter().all(|e| e.kind.is_window()),
        "the ungranted node got no pointer or key input: {:?}",
        kinds(&got[1])
    );
    let _ = host.wait();
    let st = read_stats(&stats);
    let denied_count: u64 = st["surfaces"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| s["input"]["denied"].as_u64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0);
    eprintln!("host stats: {st}");
    assert!(
        denied_count > 0,
        "the host refused the ungranted input: {st}"
    );
}
