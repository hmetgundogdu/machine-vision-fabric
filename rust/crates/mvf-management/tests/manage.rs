//! Management part 1 (#33) through a real engine: topology, live deltas, events, logs and final
//! status over the local endpoint; reconnect and resync; slow and stuck clients; limits; OS
//! identity. `MVF_MANAGE_BENCH=1` adds the overhead benchmark.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_management::*;
use serde_json::{Value, json};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mvfm-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn pipeline(frames: u64, interval_ms: u64) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: managed }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 8, height: 8, frames: {frames}, intervalMs: {interval_ms} }} }}\n    - {{ id: pass, builtin: passthrough }}\n    - {{ id: sink, builtin: record }}\n    - {{ id: thr, primitive: value, config: {{ type: int, literal: 40 }} }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: pass.frame }}\n      - {{ from: pass.frame, to: sink.frame }}\n"
    )
}

struct Managed {
    feed: Arc<Feed>,
    server: Server,
    dir: PathBuf,
}

/// Builds the engine, its feed and the endpoint. `config` sizes the feed.
fn managed(
    name: &str,
    text: &str,
    config: FeedConfig,
    options: ServerOptions,
) -> (Engine, Managed, HostServices) {
    let feed = Feed::new(
        RuntimeInfo {
            pipeline: "managed".into(),
            version: "test".into(),
            mode: "serial".into(),
        },
        config,
    );
    let tap_feed = feed.clone();
    let services = HostServices {
        logs: Logs::streaming(move |e| tap_feed.log(e)),
        ..HostServices::default()
    };
    let resolver = HostResolver::new(repo()).with_module_root(repo().join("modules"));
    let graph = build_graph(text, &resolver).expect("graph");
    let engine = Engine::build(graph, &default_backends(), &services).expect("build");
    let backends: Vec<(String, String)> = engine
        .stats()
        .into_iter()
        .map(|s| (s.id, s.backend))
        .collect();
    feed.set_topology(&Topology::of(engine.graph(), &backends));
    let dir = temp(name);
    let server = Server::start(
        feed.clone(),
        ServerOptions {
            dir: Some(dir.clone()),
            ..options
        },
    )
    .expect("server");
    (engine, Managed { feed, server, dir }, services)
}

fn tap_for(engine: &Engine, feed: &Arc<Feed>) -> Tap {
    Tap::new(
        feed.clone(),
        engine.graph().nodes.iter().map(|n| n.id.clone()),
    )
}

/// Reads a subscription until the run's final status, returning every message.
fn until_stopped(sub: &mut Subscription) -> Vec<Value> {
    let mut out = Vec::new();
    for m in sub.by_ref() {
        let m = m.expect("message");
        let stopped = m["op"] == "entry"
            && m["entry"]["kind"] == "status"
            && m["entry"]["status"] == "stopped";
        out.push(m);
        if stopped {
            break;
        }
    }
    out
}

#[test]
fn a_client_sees_topology_live_deltas_events_and_the_final_status() {
    let (mut engine, m, _s) = managed(
        "live",
        &pipeline(300, 2),
        FeedConfig {
            publish_every: Duration::from_millis(20),
            ..FeedConfig::default()
        },
        ServerOptions::default(),
    );
    let control = engine.control();
    let mut client = Client::connect(m.server.endpoint(), "test").expect("connect");
    assert_eq!(client.runtime()["pipeline"], "managed");
    assert_eq!(client.runtime()["pid"], std::process::id());
    let (_, snap) = client.snapshot().expect("snapshot");
    assert_eq!(snap["status"], "starting");
    let ids: Vec<_> = snap["topology"]["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .map(|n| n["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(ids, ["cam", "pass", "sink", "thr"]);
    assert_eq!(snap["topology"]["edges"]["data"][0]["from"], "cam.frame");
    assert!(
        snap["topology"]["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .all(|n| n.get("config").is_none()),
        "config values never leave the runtime"
    );
    let mut sub = client.subscribe(None).expect("subscribe");
    let mut tap = tap_for(&engine, &m.feed);
    let report = std::thread::scope(|scope| {
        let run = scope.spawn(|| {
            let r = engine.run(&RunOptions::default(), &mut tap);
            tap.finish(&r);
            r
        });
        std::thread::sleep(Duration::from_millis(150));
        assert!(control.set_value("thr", json!(41)));
        let messages = until_stopped(&mut sub);
        (run.join().expect("run"), messages)
    });
    let (report, messages) = report;
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() }
    );
    assert_eq!(
        messages[0]["op"], "snapshot",
        "subscribe without `from` starts with a snapshot"
    );
    let entries: Vec<&Value> = messages.iter().filter(|m| m["op"] == "entry").collect();
    let seqs: Vec<u64> = entries.iter().filter_map(|e| e["seq"].as_u64()).collect();
    let first = messages[0]["seq"].as_u64().expect("seq") + 1;
    assert_eq!(
        seqs,
        (first..first + seqs.len() as u64).collect::<Vec<_>>(),
        "contiguous, in order"
    );
    let kinds = |k: &str| entries.iter().filter(|e| e["entry"]["kind"] == k).count();
    assert!(kinds("delta") >= 3, "{} deltas", kinds("delta"));
    assert!(
        entries.iter().any(|e| e["entry"]["kind"] == "event"
            && e["entry"]["event"] == "live"
            && e["entry"]["detail"]["kind"] == "value-set"
            && e["entry"]["detail"]["node"] == "thr"),
        "the live edit arrives as it happens"
    );
    let last = entries.last().expect("final");
    assert_eq!(last["entry"]["end"]["reason"], "source-exhausted");
    let (_, after) = Client::connect(m.server.endpoint(), "test")
        .expect("connect")
        .snapshot()
        .expect("snapshot");
    assert_eq!(after["status"], "stopped");
    for n in &report.nodes {
        assert_eq!(
            after["nodes"][&n.id]["executed"], n.executed,
            "{} matches the report",
            n.id
        );
    }
    assert_eq!(after["nodes"]["sink"]["executed"], 300);
    assert!(
        after["nodes"]["pass"]["latencyUs"]["p99"]
            .as_f64()
            .is_some()
    );
    assert_eq!(
        m.feed.deferred(),
        0,
        "nobody held the lock long enough to defer a publication"
    );
}

#[test]
fn a_reconnecting_client_continues_from_its_sequence_without_loss_or_duplicates() {
    let (mut engine, m, _s) = managed(
        "reconnect",
        &pipeline(400, 2),
        FeedConfig {
            publish_every: Duration::from_millis(10),
            ..FeedConfig::default()
        },
        ServerOptions::default(),
    );
    let mut tap = tap_for(&engine, &m.feed);
    let endpoint = m.server.endpoint().to_string();
    let (seen_first, seen_second) = std::thread::scope(|scope| {
        let run = scope.spawn(|| {
            let r = engine.run(&RunOptions::default(), &mut tap);
            tap.finish(&r);
        });
        let mut sub = Client::connect(&endpoint, "a")
            .expect("connect")
            .subscribe(Some(1))
            .expect("subscribe");
        let mut first = Vec::new();
        for msg in sub.by_ref() {
            let msg = msg.expect("msg");
            if msg["op"] == "entry" {
                first.push(msg["seq"].as_u64().expect("seq"));
                if first.len() == 5 {
                    break;
                }
            }
        }
        let resume = sub.last_seq().expect("last") + 1;
        drop(sub); // the client goes away mid-run
        std::thread::sleep(Duration::from_millis(100));
        let mut sub = Client::connect(&endpoint, "a")
            .expect("reconnect")
            .subscribe(Some(resume))
            .expect("resubscribe");
        let second: Vec<u64> = until_stopped(&mut sub)
            .iter()
            .filter(|m| m["op"] == "entry")
            .filter_map(|m| m["seq"].as_u64())
            .collect();
        run.join().expect("run");
        (first, second)
    });
    let all: Vec<u64> = seen_first.iter().chain(&seen_second).copied().collect();
    assert_eq!(
        all,
        (1..=all.len() as u64).collect::<Vec<_>>(),
        "every entry exactly once across the reconnect"
    );
}

#[test]
fn a_client_that_falls_behind_the_ring_gets_a_gap_and_a_fresh_snapshot() {
    let (mut engine, m, _s) = managed(
        "gap",
        &pipeline(300, 1),
        FeedConfig {
            capacity: 16,
            publish_every: Duration::from_millis(1),
            ..FeedConfig::default()
        },
        ServerOptions::default(),
    );
    let mut tap = tap_for(&engine, &m.feed);
    let report = engine.run(&RunOptions::default(), &mut tap);
    tap.finish(&report);
    assert!(m.feed.last_seq() > 40, "{}", m.feed.last_seq());
    let mut sub = Client::connect(m.server.endpoint(), "late")
        .expect("connect")
        .subscribe(Some(1))
        .expect("subscribe");
    let gap = sub.next().expect("gap").expect("gap");
    assert_eq!(gap["op"], "gap");
    assert_eq!(gap["from"], 1);
    let oldest = m.feed.last_seq() - 15;
    assert_eq!(
        gap["to"],
        oldest - 1,
        "exactly what the ring no longer holds"
    );
    let snap = sub.next().expect("snapshot").expect("snapshot");
    assert_eq!(snap["op"], "snapshot");
    assert_eq!(snap["snapshot"]["status"], "stopped");
    assert_eq!(snap["seq"], m.feed.last_seq());
}

#[test]
fn a_stuck_client_never_slows_the_pipeline() {
    let text = pipeline(1500, 1);
    let config = FeedConfig {
        publish_every: Duration::from_millis(1),
        ..FeedConfig::default()
    };
    let options = ServerOptions {
        write_timeout: Duration::from_millis(200),
        ..ServerOptions::default()
    };
    // Baseline: management on, nobody connected.
    let (mut engine, m, _s) = managed("stuck-base", &text, config.clone(), options.clone());
    let mut tap = tap_for(&engine, &m.feed);
    let base = engine.run(&RunOptions::default(), &mut tap);
    drop((engine, m));
    // A client subscribes and then never reads.
    let (mut engine, m, s) = managed("stuck", &text, config, options);
    let mut tap = tap_for(&engine, &m.feed);
    let stuck = Client::connect(m.server.endpoint(), "stuck")
        .expect("connect")
        .subscribe(Some(1))
        .expect("subscribe");
    // A log flood on top, from another thread.
    let flood = {
        let logs = s.logs.clone();
        std::thread::spawn(move || {
            for i in 0..20_000 {
                logs.push(LogEvent {
                    node: "cam".into(),
                    generation: 1,
                    level: "info".into(),
                    message: format!("flood {i} {}", "x".repeat(200)),
                });
            }
        })
    };
    let report = engine.run(&RunOptions::default(), &mut tap);
    flood.join().expect("flood");
    assert_eq!(
        report.end,
        EndReason::SourceExhausted { node: "cam".into() }
    );
    assert_eq!(report.cycles_completed, 1500);
    eprintln!(
        "cycles: baseline {:.0} ms, with a stuck client and a log flood {:.0} ms; publications deferred {}",
        base.cycles_ms,
        report.cycles_ms,
        m.feed.deferred()
    );
    assert!(
        report.cycles_ms < base.cycles_ms * 1.5 + 500.0,
        "baseline {:.0} ms, stuck {:.0} ms",
        base.cycles_ms,
        report.cycles_ms
    );
    #[cfg(unix)]
    {
        // The stuck client's writes blocked past the timeout: the server dropped it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while m.server.counters().2 == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(m.server.counters().2 >= 1, "{:?}", m.server.counters());
    }
    drop(stuck);
}

#[test]
fn a_log_flood_is_throttled_and_counted() {
    let feed = Feed::new(
        RuntimeInfo {
            pipeline: "logs".into(),
            version: "test".into(),
            mode: "serial".into(),
        },
        FeedConfig {
            logs_per_second: 100.0,
            log_burst: 50.0,
            ..FeedConfig::default()
        },
    );
    let t0 = Instant::now();
    for i in 0..10_000 {
        feed.log(&LogEvent {
            node: "n".into(),
            generation: 1,
            level: "info".into(),
            message: format!("line {i}"),
        });
    }
    let elapsed = t0.elapsed().as_secs_f64();
    // One more after a pause reports how many were dropped.
    std::thread::sleep(Duration::from_millis(50));
    feed.log(&LogEvent {
        node: "n".into(),
        generation: 1,
        level: "info".into(),
        message: "after".into(),
    });
    let Read::Entries(entries) = feed.read_after(0, Duration::ZERO) else {
        panic!("no gap expected");
    };
    let values: Vec<Value> = entries
        .iter()
        .map(|(_, l)| serde_json::from_str::<Value>(l).expect("json"))
        .collect();
    let admitted = values
        .iter()
        .filter(|v| v["entry"]["kind"] == "log")
        .count();
    let dropped: u64 = values
        .iter()
        .filter(|v| v["entry"]["kind"] == "dropped")
        .filter_map(|v| v["entry"]["logs"].as_u64())
        .sum();
    assert!(
        (admitted as f64) <= 50.0 + 100.0 * (elapsed + 0.06) + 2.0,
        "{admitted} admitted in {elapsed:.3} s"
    );
    assert_eq!(
        admitted as u64 + dropped,
        10_001,
        "every line admitted or counted"
    );
}

#[test]
fn requests_are_bounded_and_errors_are_explicit() {
    let (_engine, m, _s) = managed(
        "limits",
        &pipeline(10, 0),
        FeedConfig::default(),
        ServerOptions {
            max_clients: 1,
            ..ServerOptions::default()
        },
    );
    let raw = |lines: &[&str]| -> Vec<Value> {
        use std::io::{BufRead, BufReader, Write};
        // One client at a time: wait until the previous probe's server thread has gone, or this
        // one is told `busy` instead of what it is testing.
        let idle = Instant::now() + Duration::from_secs(2);
        while m.server.clients() > 0 && Instant::now() < idle {
            std::thread::sleep(Duration::from_millis(10));
        }
        let conn = mvf_management::connect_raw(m.server.endpoint()).expect("connect");
        let mut w = conn.duplicate().expect("dup");
        let mut r = BufReader::new(conn);
        let mut out = Vec::new();
        for l in lines {
            let _ = w.write_all(l.as_bytes());
            let _ = w.write_all(b"\n");
            let _ = w.flush();
            let mut reply = String::new();
            if r.read_line(&mut reply).unwrap_or(0) == 0 {
                break;
            }
            out.push(serde_json::from_str(&reply).expect("json"));
        }
        out
    };
    assert_eq!(raw(&[r#"{"op":"snapshot"}"#])[0]["code"], "hello-first");
    assert_eq!(
        raw(&[r#"{"op":"hello","api":[99]}"#])[0]["code"],
        "api-unsupported"
    );
    let r = raw(&[
        r#"{"op":"hello","api":[1]}"#,
        r#"{"op":"reboot"}"#,
        r#"{"op":"ping"}"#,
    ]);
    assert_eq!(
        (r[1]["code"].as_str(), r[2]["op"].as_str()),
        (Some("unknown-op"), Some("pong"))
    );
    assert_eq!(raw(&["not json"])[0]["code"], "malformed");
    let big = format!(r#"{{"op":"hello","pad":"{}"}}"#, "x".repeat(70 * 1024));
    assert_eq!(raw(&[&big])[0]["code"], "too-large");
    // One client at a time here: a second one is told so.
    let first = Client::connect(m.server.endpoint(), "first").expect("first");
    let deadline = Instant::now() + Duration::from_secs(2);
    while m.server.clients() < 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let second = Client::connect(m.server.endpoint(), "second").expect_err("busy");
    assert!(second.to_string().contains("busy"), "{second}");
    drop(first);
}

#[test]
fn the_endpoint_is_registered_privately_and_removed_on_shutdown() {
    let (_engine, m, _s) = managed(
        "registry",
        &pipeline(10, 0),
        FeedConfig::default(),
        ServerOptions::default(),
    );
    let listed = list(&m.dir);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].pid, std::process::id());
    assert_eq!(listed[0].pipeline, "managed");
    assert_eq!(listed[0].endpoint, m.server.endpoint());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&m.dir), 0o700);
        assert_eq!(mode(Path::new(m.server.endpoint())), 0o600);
        assert_eq!(mode(&listed[0].registry), 0o600);
    }
    let endpoint = m.server.endpoint().to_string();
    let dir = m.dir.clone();
    drop(m);
    assert!(list(&dir).is_empty(), "registration removed");
    assert!(
        Client::connect(&endpoint, "late").is_err(),
        "endpoint closed"
    );
}

/// Another OS user is refused even when the filesystem would let it reach the socket (the peer
/// check), and cannot reach it at all through the private directory. Needs root to switch users.
#[cfg(unix)]
#[test]
fn another_os_user_is_refused() {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("NOT RUN: needs root to act as another user");
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;
    let (_engine, m, _s) = managed(
        "peer",
        &pipeline(10, 0),
        FeedConfig::default(),
        ServerOptions::default(),
    );
    let probe = r#"
import json, socket, sys
s = socket.socket(socket.AF_UNIX)
try:
    s.connect(sys.argv[1])
except OSError as e:
    print(json.dumps({"connect": e.errno})); sys.exit(0)
s.sendall(b'{"op":"hello","api":[1]}\n')
print(s.makefile().readline().strip())
"#;
    let as_nobody = |endpoint: &str| {
        let out = std::process::Command::new("python3")
            .args(["-c", probe, endpoint])
            .uid(65534)
            .gid(65534)
            .output()
            .expect("python3");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    // Through the private directory: not even reachable.
    let denied = as_nobody(m.server.endpoint());
    assert!(denied.contains("\"connect\": 13"), "{denied}");
    // Open the filesystem up (a misconfiguration): the peer check still refuses.
    std::fs::set_permissions(&m.dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    std::fs::set_permissions(m.server.endpoint(), std::fs::Permissions::from_mode(0o666))
        .expect("chmod");
    let refused = as_nobody(m.server.endpoint());
    assert!(refused.contains("unauthorized"), "{refused}");
    assert!(m.server.counters().1 >= 1);
}

#[test]
fn an_independent_python_client_reads_the_same_api() {
    let python = std::env::var("MVF_PYTHON")
        .unwrap_or_else(|_| (if cfg!(windows) { "python" } else { "python3" }).into());
    let (mut engine, m, _s) = managed(
        "python",
        &pipeline(200, 2),
        FeedConfig {
            publish_every: Duration::from_millis(20),
            ..FeedConfig::default()
        },
        ServerOptions::default(),
    );
    let script = repo().join("clients/mvf-manage-py/mvf_manage.py");
    let mut tap = tap_for(&engine, &m.feed);
    let endpoint = m.server.endpoint().to_string();
    let out = std::thread::scope(|scope| {
        let watch = scope.spawn(|| {
            std::process::Command::new(&python)
                .arg(&script)
                .args([
                    "--endpoint",
                    &endpoint,
                    "watch",
                    "--until-stopped",
                    "--json",
                ])
                .output()
                .expect("python")
        });
        std::thread::sleep(Duration::from_millis(300));
        let r = engine.run(&RunOptions::default(), &mut tap);
        tap.finish(&r);
        watch.join().expect("watch")
    });
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).expect("json line"))
        .collect();
    assert_eq!(lines[0]["op"], "snapshot");
    assert!(lines.iter().any(|l| l["entry"]["kind"] == "delta"));
    assert_eq!(lines.last().expect("last")["entry"]["status"], "stopped");
    let status = std::process::Command::new(&python)
        .arg(&script)
        .args(["--endpoint", &endpoint, "status", "--json"])
        .output()
        .expect("python");
    let snap: Value = serde_json::from_slice(&status.stdout).expect("snapshot");
    assert_eq!(snap["nodes"]["sink"]["executed"], 200);
}

/// `MVF_MANAGE_BENCH=1 cargo test --release -p mvf-management --test manage bench -- --nocapture`
#[test]
fn bench_management_overhead() {
    if std::env::var_os("MVF_MANAGE_BENCH").is_none_or(|v| v != "1") {
        return;
    }
    const CYCLES: u64 = 200_000;
    let text = pipeline(CYCLES, 0);
    #[cfg(unix)]
    let cpu = || {
        let mut r: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: valid out-pointer.
        unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut r) };
        let us = |t: libc::timeval| t.tv_sec as f64 * 1e6 + t.tv_usec as f64;
        us(r.ru_utime) + us(r.ru_stime)
    };
    #[cfg(not(unix))]
    let cpu = || 0.0;
    eprintln!(
        "{:<34} {:>12} {:>12} {:>14} {:>12}",
        "200k cycles (3 builtins)", "us/cycle", "cycles/s", "cpu us/cycle", "client KiB/s"
    );
    // One unmeasured run warms caches and the allocator; then three runs per case, median shown.
    let cases = [
        "warm-up",
        "management off",
        "on, no client",
        "on, 2 clients",
        "on, 1 stuck client",
    ];
    for case in cases {
        let mut runs = Vec::new();
        for _ in 0..if case == "warm-up" { 1 } else { 3 } {
            let (mut engine, m, _s) = managed(
                &format!("bench-{}", case.len()),
                &text,
                FeedConfig::default(),
                ServerOptions::default(),
            );
            let mut tap = tap_for(&engine, &m.feed);
            let readers: Vec<_> = (0..if case == "on, 2 clients" { 2 } else { 0 })
                .map(|_| {
                    let sub = Client::connect(m.server.endpoint(), "bench")
                        .expect("connect")
                        .subscribe(None)
                        .expect("sub");
                    std::thread::spawn(move || {
                        let mut bytes = 0usize;
                        for msg in sub {
                            let Ok(msg) = msg else { break };
                            bytes += msg.to_string().len() + 1;
                            if msg["entry"]["status"] == "stopped" {
                                break;
                            }
                        }
                        bytes
                    })
                })
                .collect();
            let stuck = (case == "on, 1 stuck client").then(|| {
                Client::connect(m.server.endpoint(), "stuck")
                    .expect("connect")
                    .subscribe(Some(1))
                    .expect("sub")
            });
            let c0 = cpu();
            let t0 = Instant::now();
            let report = if case == "management off" || case == "warm-up" {
                engine.run(&RunOptions::default(), &mut NullObserver)
            } else {
                let r = engine.run(&RunOptions::default(), &mut tap);
                tap.finish(&r);
                r
            };
            let wall = t0.elapsed().as_secs_f64();
            let c1 = cpu();
            let clients = readers.len().max(1);
            let bytes: usize = readers.into_iter().map(|r| r.join().unwrap_or(0)).sum();
            drop(stuck);
            runs.push((
                report.cycles_ms * 1e3 / CYCLES as f64,
                (c1 - c0) / CYCLES as f64,
                bytes as f64 / 1024.0 / wall / clients as f64,
            ));
        }
        if case == "warm-up" {
            continue;
        }
        runs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (us, cpu_us, kib) = runs[runs.len() / 2];
        eprintln!(
            "{:<34} {:>12.3} {:>12.0} {:>14.3} {:>12.1}",
            case,
            us,
            1e6 / us,
            cpu_us,
            kib
        );
    }
}
