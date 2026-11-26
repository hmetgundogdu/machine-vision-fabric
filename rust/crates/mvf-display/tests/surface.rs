//! Surface format v1: latest-frame semantics, integrity, ownership, input and status, plus the
//! Python SDK speaking the same format.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use mvf_display::surface::{self, modifiers};
use mvf_display::*;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mvf-display-test-{}-{name}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn pattern(width: u32, height: u32, seed: u8) -> Vec<u8> {
    (0..width * height * 4)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn frame(data: &[u8], width: u32, height: u32) -> Frame<'_> {
    Frame {
        width,
        height,
        stride: width * 4,
        format: PixelFormat::Bgra8,
        data,
    }
}

#[test]
fn the_host_sees_the_latest_frame_intact_and_the_writer_never_waits() {
    let dir = temp("latest");
    let path = dir.join("s.surface");
    surface::create(&path, 320, 240, 0).expect("create");
    let mut writer = SurfaceWriter::open(&path).expect("writer");

    // No host at all: presenting still succeeds every time and never waits.
    let t0 = Instant::now();
    let mut worst = Duration::ZERO;
    for i in 0..500u32 {
        let data = pattern(320, 240, i as u8);
        let t = Instant::now();
        let r = writer.present(&frame(&data, 320, 240)).expect("present");
        worst = worst.max(t.elapsed());
        assert_eq!(
            r,
            Presented::Shown {
                seq: u64::from(i) + 1
            }
        );
    }
    eprintln!(
        "500 presents of 320x240 BGRA without a host: {:?} total, worst {:?}",
        t0.elapsed(),
        worst
    );

    let mut host = SurfaceReader::open(&path).expect("host");
    let mut out = Vec::new();
    let info = host.take_latest(&mut out).expect("latest frame");
    assert_eq!(info.seq, 500, "only the newest frame is shown");
    assert_eq!((info.width, info.height), (320, 240));
    assert_eq!(out, pattern(320, 240, 499u32 as u8));
    assert!(host.take_latest(&mut out).is_none(), "nothing newer yet");

    // A slow host: the writer keeps publishing; the host jumps to whatever is newest.
    for i in 0..7u8 {
        writer
            .present(&frame(&pattern(160, 120, i), 160, 120))
            .expect("present");
    }
    let info = host.take_latest(&mut out).expect("newest");
    assert_eq!((info.seq, info.width), (507, 160));
    assert_eq!(out, pattern(160, 120, 6));
    assert_eq!(host.rejected, 0);
}

#[test]
fn a_torn_forged_or_truncated_surface_is_never_shown() {
    let dir = temp("forged");
    let path = dir.join("s.surface");
    let g = surface::create(&path, 64, 64, 0).expect("create");
    let mut writer = SurfaceWriter::open(&path).expect("writer");
    let mut host = SurfaceReader::open(&path).expect("host");
    writer
        .present(&frame(&pattern(64, 64, 1), 64, 64))
        .expect("present");

    // Corrupt one pixel of the published buffer behind the writer's back (a torn write). Patched
    // in place: Windows refuses to truncate a file that is mapped.
    let bytes = std::fs::read(&path).expect("read");
    let published = u64::from_le_bytes(bytes[256..264].try_into().expect("8 bytes"));
    let buffer = g.buffers_offset + (published & 3) as usize * g.capacity;
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open");
        f.seek(SeekFrom::Start((buffer + 100) as u64))
            .expect("seek");
        f.write_all(&[bytes[buffer + 100] ^ 0xFF]).expect("write");
    }
    let mut out = Vec::new();
    assert!(
        host.take_latest(&mut out).is_none(),
        "CRC mismatch is not shown"
    );
    assert!(host.rejected > 0);

    // The next good frame is shown.
    writer
        .present(&frame(&pattern(64, 64, 2), 64, 64))
        .expect("present");
    assert_eq!(host.take_latest(&mut out).map(|i| i.seq), Some(2));

    // A header that lies about its geometry, and a truncated file, are refused outright.
    let forged = dir.join("forged.surface");
    std::fs::copy(&path, &forged).expect("copy");
    let mut b = std::fs::read(&forged).expect("read");
    b[16..20].copy_from_slice(&100_000u32.to_le_bytes());
    std::fs::write(&forged, &b).expect("write");
    assert!(matches!(
        SurfaceReader::open(&forged),
        Err(SurfaceError::Invalid { .. })
    ));
    let short = dir.join("short.surface");
    std::fs::write(
        &short,
        &std::fs::read(&path).expect("read")[..g.buffers_offset],
    )
    .expect("write");
    assert!(matches!(
        SurfaceReader::open(&short),
        Err(SurfaceError::Invalid { .. })
    ));
}

#[test]
fn generations_share_one_sequence_and_one_host_presents() {
    let dir = temp("owners");
    let path = dir.join("s.surface");
    surface::create(&path, 32, 32, 0).expect("create");
    let data = pattern(32, 32, 0);

    // Two generations of the node (a live replacement): whoever presents continues the surface's
    // sequence, so the window follows the active one with no gap and no engine involvement.
    let mut old = SurfaceWriter::open(&path).expect("old");
    let mut new = SurfaceWriter::open(&path).expect("new");
    assert_eq!(
        old.present(&frame(&data, 32, 32)).expect("present"),
        Presented::Shown { seq: 1 }
    );
    assert_eq!(
        new.present(&frame(&data, 32, 32)).expect("present"),
        Presented::Shown { seq: 2 }
    );
    assert_eq!(
        old.present(&frame(&data, 32, 32)).expect("present"),
        Presented::Shown { seq: 3 }
    );

    // While another generation is in the middle of a present (holds the writer lock), a present
    // returns at once instead of tearing the frame or waiting.
    let busy = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(surface::writer_lock_path(&path))
        .expect("lock file");
    busy.try_lock().expect("take the writer lock");
    assert_eq!(
        new.present(&frame(&data, 32, 32)).expect("present"),
        Presented::Standby
    );
    let mut events = Vec::new();
    assert_eq!(new.poll_input(&mut events), 0);
    busy.unlock().expect("unlock");
    assert_eq!(
        new.present(&frame(&data, 32, 32)).expect("present"),
        Presented::Shown { seq: 4 }
    );

    let first = SurfaceReader::open(&path).expect("first host");
    assert!(matches!(
        SurfaceReader::open(&path),
        Err(SurfaceError::Busy(_))
    ));
    drop(first);
    let mut second = SurfaceReader::open(&path).expect("reattach after the host is gone");
    let mut out = Vec::new();
    assert_eq!(second.take_latest(&mut out).map(|i| i.seq), Some(4));
}

#[test]
fn input_is_ordered_bounded_and_never_blocks_the_host() {
    let dir = temp("input");
    let path = dir.join("s.surface");
    surface::create(&path, 32, 32, 0).expect("create");
    let mut node = SurfaceWriter::open(&path).expect("writer");
    let mut host = SurfaceReader::open(&path).expect("host");

    let mut accepted = 0;
    for i in 0..300 {
        let mut e = InputEvent::pointer(InputKind::PointerMove, i as f32, 2.0 * i as f32);
        e.modifiers = modifiers::SHIFT;
        if host.push(&e) {
            accepted += 1;
        }
    }
    assert_eq!(accepted, 256, "the ring holds 256 unread events");
    host.update_status(|_| {});
    assert_eq!(host.status().dropped_events, 44);

    let mut events = Vec::new();
    assert_eq!(node.poll_input(&mut events), 0);
    assert_eq!(events.len(), 256);
    assert!(
        events.iter().enumerate().all(|(i, e)| e.x == i as f32
            && e.y == 2.0 * i as f32
            && e.modifiers == modifiers::SHIFT)
    );
    assert!(host.push(&InputEvent::key(InputKind::KeyDown, "ArrowLeft")));
    events.clear();
    node.poll_input(&mut events);
    assert_eq!(
        events,
        vec![InputEvent::key(InputKind::KeyDown, "ArrowLeft")]
    );
}

#[test]
fn host_status_reaches_the_node_and_detach_is_immediate() {
    let dir = temp("status");
    let path = dir.join("s.surface");
    surface::create(&path, 32, 32, 0).expect("create");
    let node = SurfaceWriter::open(&path).expect("writer");
    assert!(node.host().is_none(), "no host ever attached");
    let mut host = SurfaceReader::open(&path).expect("host");
    host.update_status(|s| {
        s.requested_width = 800;
        s.requested_height = 600;
        s.scale_milli = 2000;
        s.flags |= surface::FLAG_FOCUSED;
    });
    let status = node.host().expect("status");
    assert!(status.attached(Duration::from_secs(2)));
    assert!(status.focused());
    assert_eq!(
        (
            status.requested_width,
            status.requested_height,
            status.scale_milli
        ),
        (800, 600, 2000)
    );
    assert_eq!(status.host_pid, std::process::id());
    drop(host);
    assert!(
        !node
            .host()
            .expect("status")
            .attached(Duration::from_secs(2))
    );
}

#[test]
fn the_registry_allocates_per_node_and_cleans_up_dead_runs() {
    let root = temp("registry");
    let request = SurfaceRequest {
        title: "Preview".into(),
        width: 640,
        height: 360,
        max_width: 640,
        max_height: 360,
        windows: 2,
        grants: Grants {
            keyboard: false,
            mouse: true,
        },
    };
    let registry = DisplayRegistry::new(&root);
    let a = registry
        .surfaces("cam/preview", &request)
        .expect("surfaces");
    assert_eq!(a.len(), 2);
    assert_eq!(
        registry.surfaces("cam/preview", &request).expect("again"),
        a,
        "a later generation of the same node gets the same surfaces"
    );
    let found = discover(&root);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].descriptor.title, "Preview (1)");
    assert_eq!(found[1].descriptor.node, "cam/preview");
    assert!(found[0].descriptor.grants.mouse && !found[0].descriptor.grants.keyboard);
    assert!(found.iter().all(|d| d.surface.starts_with(&root)));

    // A run left behind by a dead engine is swept by the next one; a live run is not.
    let mut child = if cfg!(windows) {
        Command::new("cmd").args(["/C", "exit"]).spawn()
    } else {
        Command::new("true").spawn()
    }
    .expect("spawn");
    let dead_pid = child.id();
    child.wait().expect("wait");
    let stale = root.join(format!("run-{dead_pid}-000000001"));
    std::fs::create_dir_all(&stale).expect("stale run");
    std::fs::write(stale.join("owner"), dead_pid.to_string()).expect("owner");
    assert_eq!(sweep(&root), 1);
    assert!(!stale.exists());
    assert_eq!(discover(&root).len(), 2);

    let run = registry.run_dir().expect("run dir");
    drop(registry);
    assert!(
        !run.exists(),
        "the engine removes its run directory when it stops"
    );
    assert!(discover(&root).is_empty());
}

#[cfg(unix)]
#[test]
fn a_display_directory_others_can_write_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let root = temp("shared-root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).expect("chmod");
    let request = SurfaceRequest {
        title: "x".into(),
        width: 16,
        height: 16,
        max_width: 16,
        max_height: 16,
        windows: 1,
        grants: Grants::default(),
    };
    let err = DisplayRegistry::new(&root)
        .surfaces("n", &request)
        .expect_err("world-writable rendezvous directory");
    assert!(err.to_string().contains("not writable by others"), "{err}");
}

#[test]
fn the_session_probe_answers_on_every_platform() {
    let s = session::probe();
    eprintln!("session: {:?} {}", s.kind, s.detail);
    assert!(!s.detail.is_empty());
    assert_eq!(s.interactive(), s.kind == SessionKind::Interactive);
}

fn python() -> Option<std::ffi::OsString> {
    let p = std::env::var_os("MVF_PYTHON")
        .unwrap_or_else(|| if cfg!(windows) { "python" } else { "python3" }.into());
    Command::new(&p)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| p)
}

fn sdk() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../src/sdk/python")
}

#[test]
fn the_python_sdk_speaks_the_same_surface_format() {
    let Some(python) = python() else {
        assert!(
            std::env::var_os("CI").is_none(),
            "Python is required on CI for the SDK interop test"
        );
        eprintln!("BLOCKED: no Python interpreter");
        return;
    };
    let dir = temp("python");
    let path = dir.join("s.surface");
    surface::create(&path, 64, 48, 0).expect("create");
    let mut host = SurfaceReader::open(&path).expect("host");
    host.update_status(|s| {
        s.requested_width = 64;
        s.requested_height = 48;
    });
    assert!(host.push(&InputEvent::pointer(InputKind::PointerDown, 10.0, 20.0)));
    assert!(host.push(&InputEvent::key(InputKind::KeyDown, "ü")));

    let script = r#"
import json, sys
sys.path.insert(0, sys.argv[1])
from mvf_sdk import display
s = display.surfaces()[0]
status = s.host()
frames = []
for i in range(3):
    data = bytes(((x * 7 + i) & 0xFF) for x in range(64 * 48 * 3))
    frames.append(s.present(data, 64, 48, "rgb8"))
events = [(e.kind, e.x, e.y, e.text) for e in s.poll_input()]
print(json.dumps({"frames": frames, "attached": status.attached(), "requested": [status.requested_width, status.requested_height], "events": events}))
"#;
    let out = Command::new(&python)
        .args(["-I", "-c", script])
        .arg(sdk())
        .env("MVF_DISPLAY_SURFACES", &path)
        .output()
        .expect("run python");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).expect("python report");
    assert_eq!(report["frames"], serde_json::json!([true, true, true]));
    assert_eq!(report["attached"], true);
    assert_eq!(report["requested"], serde_json::json!([64, 48]));
    assert_eq!(
        report["events"],
        serde_json::json!([
            ["pointer-down", 10.0, 20.0, ""],
            ["key-down", 0.0, 0.0, "ü"]
        ])
    );

    let mut pixels = Vec::new();
    let info = host
        .take_latest(&mut pixels)
        .expect("python frame verifies");
    assert_eq!(info.seq, 3);
    assert_eq!(info.format, PixelFormat::Rgb8);
    let expected: Vec<u8> = (0..64 * 48 * 3)
        .map(|x: u32| ((x * 7 + 2) & 0xFF) as u8)
        .collect();
    assert_eq!(pixels, expected);
}

/// `MVF_DISPLAY_BENCH=1` (run with `--release`): per-frame cost of a 1920x1080 BGRA frame through
/// a surface — the Rust writer (copy + CRC), the host's verified copy-out, and the Python SDK.
#[test]
fn bench_full_hd_frames() {
    if std::env::var_os("MVF_DISPLAY_BENCH").is_none_or(|v| v != "1") {
        return;
    }
    let dir = temp("bench");
    let path = dir.join("s.surface");
    surface::create(&path, 1920, 1080, 0).expect("create");
    let mut writer = SurfaceWriter::open(&path).expect("writer");
    let mut host = SurfaceReader::open(&path).expect("host");
    let data = pattern(1920, 1080, 7);
    let mut out = Vec::new();
    let (mut w, mut r) = (Vec::new(), Vec::new());
    for _ in 0..200 {
        let t = Instant::now();
        writer.present(&frame(&data, 1920, 1080)).expect("present");
        w.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        host.take_latest(&mut out).expect("frame");
        r.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    w.sort_by(f64::total_cmp);
    r.sort_by(f64::total_cmp);
    let p = |v: &[f64], q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
    eprintln!(
        "1920x1080 BGRA (8.3 MB): Rust present ms p50={:.2} p99={:.2}; host verified copy ms p50={:.2} p99={:.2}",
        p(&w, 0.5),
        p(&w, 0.99),
        p(&r, 0.5),
        p(&r, 0.99)
    );
    drop(writer);
    let Some(python) = python() else { return };
    let script = r#"
import sys, time
sys.path.insert(0, sys.argv[1])
from mvf_sdk import display
s = display.surfaces()[0]
data = bytes(1920 * 1080 * 4)
t = []
for i in range(60):
    t0 = time.perf_counter(); s.present(data, 1920, 1080, "bgra8"); t.append((time.perf_counter() - t0) * 1000)
t.sort()
print("%.2f %.2f" % (t[len(t) // 2], t[-1]))
"#;
    let out = Command::new(&python)
        .args(["-I", "-c", script])
        .arg(sdk())
        .env("MVF_DISPLAY_SURFACES", &path)
        .output()
        .expect("python");
    eprintln!(
        "1920x1080 BGRA: Python present ms p50/max = {}",
        String::from_utf8_lossy(&out.stdout).trim()
    );
}
