//! Management part 3 (#33): opt-in previews (capped, audited, never retained) and node-package
//! upload (chunked, resumable, digest-checked, staged), activated with `replace` and undone with
//! `rollback`, all through a real engine.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvf_engine::builtins::SyntheticSource;
use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_management::*;
use serde_json::{Value, json};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mvf3-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

fn pipeline(width: u64, height: u64, channels: u64, interval_ms: u64) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: part3 }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: {width}, height: {height}, channels: {channels}, frames: 1000000, intervalMs: {interval_ms} }} }}\n    - {{ id: pass, builtin: passthrough }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: pass.frame }}\n      - {{ from: pass.frame, to: sink.frame }}\n"
    )
}

struct Rt {
    engine: Engine,
    feed: Arc<Feed>,
    server: Server,
    cancel: CancellationToken,
    audit: PathBuf,
    base: PathBuf,
}

fn runtime(name: &str, text: &str, previews: Option<PreviewConfig>) -> Rt {
    let base = temp(name);
    let feed = Feed::new(
        RuntimeInfo {
            pipeline: "part3".into(),
            version: "test".into(),
            mode: "serial".into(),
        },
        FeedConfig::default(),
    );
    if let Some(p) = previews {
        feed.enable_previews(p);
    }
    let resolver = HostResolver::new(&base).with_module_root(repo().join("modules"));
    let graph = build_graph(text, &resolver).expect("graph");
    let engine =
        Engine::build(graph, &default_backends(), &HostServices::default()).expect("build");
    feed.set_topology(&Topology::of(engine.graph(), &[]));
    let cancel = CancellationToken::new();
    let audit = base.join("audit.jsonl");
    let server = Server::start(
        feed.clone(),
        ServerOptions {
            dir: Some(base.join("run")),
            control: Some(Control {
                handle: engine.control(),
                cancel: cancel.clone(),
                resolver: Arc::new(resolver),
            }),
            audit: Some(audit.clone()),
            uploads: Some(UploadOptions::new(base.join(".mvf/staged"), ".mvf/staged")),
            ..ServerOptions::default()
        },
    )
    .expect("server");
    Rt {
        engine,
        feed,
        server,
        cancel,
        audit,
        base,
    }
}

fn run_while(rt: &mut Rt, during: impl FnOnce(&str)) -> RunReport {
    let mut tap = Tap::new(
        rt.feed.clone(),
        rt.engine.graph().nodes.iter().map(|n| n.id.clone()),
    );
    let options = RunOptions {
        cancel: rt.cancel.clone(),
        ..RunOptions::default()
    };
    let cancel = rt.cancel.clone();
    let endpoint = rt.server.endpoint().to_string();
    let engine = &mut rt.engine;
    std::thread::scope(|scope| {
        let h = scope.spawn(move || {
            let r = engine.run(&options, &mut tap);
            tap.finish(&r);
            r
        });
        std::thread::sleep(Duration::from_millis(100));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| during(&endpoint)));
        cancel.cancel();
        let report = h.join().expect("run");
        if let Err(p) = outcome {
            std::panic::resume_unwind(p);
        }
        report
    })
}

/// A raw line connection (optionally `onBehalfOf`).
struct Raw {
    w: Box<dyn Conn>,
    r: BufReader<Box<dyn Conn>>,
}

impl Raw {
    fn open(endpoint: &str, behalf: Option<(&str, &str)>) -> (Self, Value) {
        let conn = connect_raw(endpoint).expect("connect");
        let w = conn.duplicate().expect("dup");
        let mut raw = Self {
            w,
            r: BufReader::new(conn),
        };
        let mut hello = json!({"op": "hello", "api": [1]});
        if let Some((p, role)) = behalf {
            hello["onBehalfOf"] = json!({"principal": p, "role": role, "via": "test"});
        }
        let h = raw.ask(&hello);
        (raw, h)
    }

    fn send(&mut self, v: &Value) {
        self.w
            .write_all(format!("{v}\n").as_bytes())
            .expect("write");
        self.w.flush().expect("flush");
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.r.read_line(&mut line).expect("read");
        serde_json::from_str(&line).unwrap_or(Value::Null)
    }

    fn ask(&mut self, v: &Value) -> Value {
        self.send(v);
        self.read()
    }
}

fn audit(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Decodes the PNG this crate writes (stored deflate): (width, height, channels, pixels).
fn decode_png(png: &[u8]) -> (usize, usize, usize, Vec<u8>) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let (mut at, mut w, mut h, mut c, mut z) = (8, 0, 0, 0, Vec::new());
    while at < png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().expect("len")) as usize;
        let kind = &png[at + 4..at + 8];
        let data = &png[at + 8..at + 8 + len];
        if kind == b"IHDR" {
            w = u32::from_be_bytes(data[0..4].try_into().expect("w")) as usize;
            h = u32::from_be_bytes(data[4..8].try_into().expect("h")) as usize;
            c = match data[9] {
                0 => 1,
                2 => 3,
                _ => 4,
            };
        } else if kind == b"IDAT" {
            z.extend_from_slice(data);
        }
        at += 12 + len;
    }
    let mut raw = Vec::new();
    let mut i = 2;
    loop {
        let last = z[i] & 1;
        let n = u16::from_le_bytes([z[i + 1], z[i + 2]]) as usize;
        raw.extend_from_slice(&z[i + 5..i + 5 + n]);
        i += 5 + n;
        if last == 1 {
            break;
        }
    }
    let pixels: Vec<u8> = raw
        .chunks(w * c + 1)
        .flat_map(|row| row[1..].to_vec())
        .collect();
    (w, h, c, pixels)
}

#[test]
fn previews_are_opt_in_downscaled_rate_capped_audited_and_never_retained() {
    let mut rt = runtime(
        "previews",
        &pipeline(320, 240, 3, 5),
        Some(PreviewConfig {
            nodes: vec!["cam".into()],
            max_fps: 5.0,
            max_pixels: 160 * 120,
            bytes_per_second: 4 << 20,
        }),
    );
    let feed = rt.feed.clone();
    let mut received = Vec::new();
    run_while(&mut rt, |endpoint| {
        // Not opted in.
        let (mut raw, _) = Raw::open(endpoint, None);
        let r = raw.ask(&json!({"op": "preview", "node": "pass"}));
        assert_eq!(r["code"], "not-enabled", "{r}");
        // An observer may not look at images; a previewer may.
        let (mut raw, _) = Raw::open(endpoint, Some(("olga", "observer")));
        assert_eq!(
            raw.ask(&json!({"op": "preview", "node": "cam"}))["code"],
            "forbidden"
        );
        let (mut raw, hello) = Raw::open(endpoint, Some(("pia", "previewer")));
        assert_eq!(hello["role"], "previewer");
        raw.send(&json!({"op": "preview", "node": "cam", "fps": 5}));
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(1500) {
            let m = raw.read();
            if m["op"] == "preview" {
                received.push((t0.elapsed(), m));
            }
        }
    });
    assert!(received.len() >= 3, "{} previews", received.len());
    // At most 5 per second (plus the first, which is immediate).
    assert!(received.len() <= 9, "{} previews in 1.5 s", received.len());
    for (_, p) in &received {
        assert_eq!(
            (
                p["format"].as_str(),
                p["width"].as_u64(),
                p["height"].as_u64()
            ),
            (Some("png"), Some(160), Some(120))
        );
        assert_eq!(
            (p["sourceWidth"].as_u64(), p["sourceHeight"].as_u64()),
            (Some(320), Some(240))
        );
        let png =
            mvf_management::preview::unbase64(p["data"].as_str().expect("data")).expect("base64");
        let (w, h, c, px) = decode_png(&png);
        assert_eq!((w, h, c), (160, 120, 3));
        // Nearest-neighbour: preview (y, x) is source (2y, 2x) of that cycle's frame.
        let cycle = p["cycle"].as_u64().expect("cycle");
        let source = SyntheticSource::pixels(0, cycle, 320 * 240 * 3);
        for (y, x) in [(0usize, 0usize), (10, 37), (119, 159)] {
            let s = ((2 * y) * 320 + 2 * x) * 3;
            assert_eq!(
                &px[(y * 160 + x) * 3..(y * 160 + x) * 3 + 3],
                &source[s..s + 3],
                "pixel ({y},{x}) of cycle {cycle}"
            );
        }
    }
    // Who looked, and at what: audited. Previews themselves: nowhere.
    let records = audit(&rt.audit);
    assert!(
        records.iter().any(|a| a["event"] == "preview-opened"
            && a["principal"] == "pia"
            && a["node"] == "cam")
    );
    assert!(
        !std::fs::read_to_string(&rt.audit)
            .unwrap_or_default()
            .contains("\"data\""),
        "no image data in the audit log"
    );
    let (_, snapshot) = feed.snapshot();
    assert!(
        !snapshot.to_string().contains("\"png\""),
        "no preview in snapshots"
    );
    let Read::Entries(entries) = feed.read_after(0, Duration::ZERO) else {
        panic!("entries");
    };
    assert!(
        entries.iter().all(|(_, l)| !l.contains("\"png\"")),
        "no preview in the feed's ring"
    );
}

#[test]
fn a_viewer_over_its_byte_budget_gets_fewer_previews_not_a_queue() {
    let mut rt = runtime(
        "budget",
        &pipeline(320, 240, 3, 5),
        Some(PreviewConfig {
            nodes: vec!["cam".into()],
            max_fps: 10.0,
            max_pixels: 160 * 120,
            // About one 160x120 RGB preview (≈77 KB as base64) per second.
            bytes_per_second: 90_000,
        }),
    );
    let mut arrivals = Vec::new();
    run_while(&mut rt, |endpoint| {
        let (mut raw, _) = Raw::open(endpoint, None);
        raw.send(&json!({"op": "preview", "node": "cam", "fps": 10}));
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_millis(2500) {
            let m = raw.read();
            if m["op"] == "preview" {
                arrivals.push((t0.elapsed(), m.to_string().len() + 1));
            }
        }
    });
    // Previews are about 77 kB each: a full budget for the first, then about 1.2 per second.
    let window: Vec<_> = arrivals
        .iter()
        .filter(|(t, _)| *t < Duration::from_millis(2500))
        .collect();
    let bytes: usize = window.iter().map(|(_, b)| b).sum();
    assert!(
        (2..=4).contains(&window.len()),
        "{} previews in 2.5 s at 90 kB/s: {arrivals:?}",
        window.len()
    );
    assert!(bytes <= 90_000 + 225_000, "{bytes} bytes in 2.5 s");
}

/// The stage arguments for a package directory: every file with its digest.
fn staged_args(dir: &Path) -> Value {
    let files: serde_json::Map<String, Value> = ["node.yaml", "data/model.bin"]
        .iter()
        .map(|p| {
            (
                p.to_string(),
                json!(upload::digest(&std::fs::read(dir.join(p)).expect("file"))),
            )
        })
        .collect();
    json!({"package": "pass-v2", "files": files})
}

/// The passthrough contract under a new version, as a prepared package.
fn package(dir: &Path) {
    let contract = std::fs::read_to_string(
        repo().join("rust/crates/mvf-engine/src/contracts/passthrough.yaml"),
    )
    .expect("contract")
    .replace("version: 1.0.0", "version: 2.0.0");
    std::fs::create_dir_all(dir.join("data")).expect("dir");
    std::fs::write(dir.join("node.yaml"), contract).expect("node.yaml");
    let blob: Vec<u8> = (0..300_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    std::fs::write(dir.join("data/model.bin"), blob).expect("model");
}

#[test]
fn a_package_is_uploaded_resumed_staged_activated_and_rolled_back() {
    let mut rt = runtime("upload", &pipeline(8, 8, 1, 2), None);
    let src = temp("upload-src");
    package(&src);
    let model = std::fs::read(src.join("data/model.bin")).expect("model");
    let model_digest = upload::digest(&model);
    let staged_root = rt.base.join(".mvf/staged");
    let report = run_while(&mut rt, |endpoint| {
        let (mut op, _) = Raw::open(endpoint, None);
        let chunk = |offset: usize, len: usize| json!({"op": "upload", "package": "pass-v2", "path": "data/model.bin", "size": model.len(), "digest": model_digest, "offset": offset, "data": preview::base64(&model[offset..offset + len])});
        // Two chunks, then the connection is lost.
        assert_eq!(op.ask(&chunk(0, 65536))["status"], "partial");
        assert_eq!(op.ask(&chunk(65536, 65536))["status"], "partial");
        drop(op);
        // A new connection: the runtime kept what it had.
        let (mut op, _) = Raw::open(endpoint, None);
        let s =
            op.ask(&json!({"op": "upload-status", "package": "pass-v2", "path": "data/model.bin"}));
        assert_eq!(
            (s["received"].as_u64(), s["complete"].as_bool()),
            (Some(131072), Some(false))
        );
        // A chunk it already has is acknowledged, not written twice; a gap is refused.
        let d = op.ask(&chunk(0, 65536));
        assert_eq!(
            (d["duplicate"].as_bool(), d["received"].as_u64()),
            (Some(true), Some(131072))
        );
        let g = op.ask(&chunk(200_000, 1000));
        assert_eq!(
            (g["code"].as_str(), g["received"].as_u64()),
            (Some("offset-mismatch"), Some(131072))
        );
        // Unsafe names and a wrong digest.
        let mut bad = chunk(0, 10);
        bad["path"] = json!("../escape");
        assert_eq!(op.ask(&bad)["code"], "invalid");
        bad["path"] = json!("x");
        bad["package"] = json!(".hidden");
        assert_eq!(op.ask(&bad)["code"], "invalid");
        let wrong = json!({"op": "upload", "package": "pass-v2", "path": "notes.txt", "size": 5, "digest": upload::digest(b"other"), "offset": 0, "data": preview::base64(b"hello")});
        let r = op.ask(&wrong);
        assert_eq!(
            (r["status"].as_str(), r["code"].as_str()),
            (Some("rejected"), Some("digest-mismatch")),
            "{r}"
        );
        assert_eq!(
            op.ask(&json!({"op": "upload-status", "package": "pass-v2", "path": "notes.txt"}))["received"],
            0,
            "discarded"
        );
        // An observer cannot upload.
        let (mut olga, _) = Raw::open(endpoint, Some(("olga", "observer")));
        assert_eq!(olga.ask(&chunk(0, 10))["code"], "forbidden");
        // The rest, resumed by the same helper the CLIs use, then staged.
        let mut request = |v: &Value| -> std::io::Result<Value> { Ok(op.ask(v)) };
        // Staging before every file is in is refused.
        let early = request(&json!({"op": "command", "id": "early", "command": "stage", "args": {"package": "pass-v2", "files": {"data/model.bin": model_digest}}})).expect("stage");
        assert_eq!(early["code"], "incomplete", "{early}");
        let staged = upload_dir(&mut request, "pass-v2", &src, 64 * 1024).expect("upload");
        assert_eq!(staged["status"], "applied", "{staged}");
        let contract = staged["staged"]["contract"]
            .as_str()
            .expect("contract")
            .to_string();
        assert_eq!(contract, ".mvf/staged/pass-v2/node.yaml");
        // Staging again is a no-op; the same name with other contents is refused.
        // Retrying the whole upload is harmless: every file is complete, and the stage command
        // carries the same id, so its first reply comes back.
        let again = upload_dir(&mut request, "pass-v2", &src, 64 * 1024).expect("again");
        assert_eq!(again["duplicate"], true, "{again}");
        let restage = request(&json!({"op": "command", "id": "restage", "command": "stage", "args": staged_args(&src)})).expect("restage");
        assert_eq!(restage["staged"]["alreadyStaged"], true, "{restage}");
        let other = request(&json!({"op": "command", "id": "other", "command": "stage", "args": {"package": "pass-v2", "files": {"node.yaml": upload::digest(b"x")}}})).expect("other");
        assert_eq!(other["code"], "exists");
        // Activation: an ordinary replace naming the staged contract, then a rollback.
        let r = request(&json!({"op": "command", "id": "act", "command": "replace", "args": {"node": "pass", "contract": contract, "probationCycles": 100000}})).expect("replace");
        assert_eq!(r["status"], "applied", "{r}");
        assert_eq!(
            (
                r["event"]["toGeneration"].as_u64(),
                r["event"]["toVersion"].as_str()
            ),
            (Some(2), Some("mvf.passthrough@2.0.0"))
        );
        let r = request(&json!({"op": "command", "id": "back", "command": "rollback", "args": {"node": "pass"}})).expect("rollback");
        assert_eq!(r["status"], "applied", "{r}");
        assert_eq!(r["event"]["toVersion"], "mvf.passthrough@1.0.0");
    });
    assert_eq!(report.end, EndReason::Cancelled);
    // The staged package is exactly what was sent, with its manifest; nothing partial is left.
    assert_eq!(
        std::fs::read(staged_root.join("pass-v2/data/model.bin")).expect("staged"),
        model
    );
    assert!(staged_root.join("pass-v2/manifest.json").is_file());
    assert!(!staged_root.join(".partial/pass-v2").exists());
    let records = audit(&rt.audit);
    let has = |pred: &dyn Fn(&Value) -> bool| records.iter().any(pred);
    assert!(has(&|a| a["event"] == "upload"
        && a["status"] == "complete"
        && a["path"] == "data/model.bin"));
    assert!(has(&|a| a["event"] == "upload"
        && a["status"] == "rejected"
        && a["code"] == "digest-mismatch"));
    assert!(has(&|a| a["command"] == "stage" && a["status"] == "applied"));
    assert!(has(
        &|a| a["command"] == "replace" && a["status"] == "applied"
    ));
    assert!(has(
        &|a| a["command"] == "rollback" && a["status"] == "applied"
    ));
}

#[test]
fn upload_limits_hold() {
    let base = temp("limits");
    let uploads = Uploads::new(UploadOptions {
        max_file: 1000,
        max_package: 1500,
        max_files: 2,
        ..UploadOptions::new(base.join("staged"), ".mvf/staged")
    })
    .expect("uploads");
    let up = |path: &str, size: u64| {
        uploads.upload(&json!({"package": "p", "path": path, "size": size, "digest": upload::digest(b""), "offset": 0, "data": ""}))
    };
    assert_eq!(up("big", 2000)["code"], "too-large");
    assert_eq!(up("a", 900)["status"], "partial");
    assert_eq!(up("b", 900)["code"], "too-large", "package over 1500 bytes");
    assert_eq!(up("b", 500)["status"], "partial");
    assert_eq!(up("c", 10)["code"], "too-large", "more than 2 files");
    assert_eq!(up("a/b/c/d/e", 1)["code"], "invalid", "too deep");
}

/// `MVF_MANAGE_BENCH=1 cargo test --release -p mvf-management --test part3 bench -- --nocapture`
#[test]
fn bench_previews_and_uploads() {
    if std::env::var_os("MVF_MANAGE_BENCH").is_none_or(|v| v != "1") {
        return;
    }
    // Engine cost of an opted-in node: 50k cycles of 64x48 RGB, previews off vs on (2 fps).
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: bench }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 64, height: 48, channels: 3, frames: 50000 } }\n    - { id: pass, builtin: passthrough }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: pass.frame }\n      - { from: pass.frame, to: sink.frame }\n";
    for previews in [false, true, false, true] {
        let mut rt = runtime(
            "bench-preview",
            text,
            previews.then(|| PreviewConfig {
                nodes: vec!["cam".into()],
                ..Default::default()
            }),
        );
        let mut tap = Tap::new(
            rt.feed.clone(),
            rt.engine.graph().nodes.iter().map(|n| n.id.clone()),
        );
        let r = rt.engine.run(&RunOptions::default(), &mut tap);
        eprintln!(
            "previews {:<5} {:.3} us/cycle over {} cycles",
            previews,
            r.cycles_ms * 1e3 / r.cycles_completed as f64,
            r.cycles_completed
        );
    }
    // One preview of a 1280x960 RGB frame, and its size.
    let mut rt = runtime(
        "bench-render",
        &pipeline(1280, 960, 3, 50),
        Some(PreviewConfig {
            nodes: vec!["cam".into()],
            bytes_per_second: 64 << 20,
            ..Default::default()
        }),
    );
    let mut sample = Value::Null;
    run_while(&mut rt, |endpoint| {
        let (mut raw, _) = Raw::open(endpoint, None);
        sample = raw.ask(&json!({"op": "preview", "node": "cam"}));
    });
    eprintln!(
        "preview of 1280x960 RGB: {}x{} in {} us render, {} bytes on the wire",
        sample["width"],
        sample["height"],
        sample["renderUs"],
        sample.to_string().len() + 1
    );
    // Local upload throughput: a 32 MiB file in 512 KiB chunks, then staging.
    let mut rt = runtime("bench-upload", &pipeline(8, 8, 1, 50), None);
    let src = temp("bench-upload-src");
    std::fs::write(src.join("node.yaml"), "x").expect("file");
    std::fs::write(src.join("blob.bin"), vec![3u8; 32 << 20]).expect("blob");
    run_while(&mut rt, |endpoint| {
        let (mut op, _) = Raw::open(endpoint, None);
        let mut request = |v: &Value| -> std::io::Result<Value> { Ok(op.ask(v)) };
        let t0 = Instant::now();
        let r = upload_dir(&mut request, "big", &src, 512 * 1024).expect("upload");
        let s = t0.elapsed().as_secs_f64();
        assert_eq!(r["status"], "applied", "{r}");
        eprintln!(
            "upload of 32 MiB over the local endpoint (base64 chunks, digest checks, staging): {:.1} MiB/s",
            32.0 / s
        );
    });
}
