//! What the TUI needs from management (#35):
//! - which node runs now, sampled off the engine thread;
//! - cumulative failures and restarts per node;
//! - the active contract and backend after a replacement;
//! - how each editable field is edited, without its value;
//! - a pause the feed shows while it lasts.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_management::*;
use serde_json::{Value, json};

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mvft-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

/// Builds the engine, the feed and a server; runs `during` against the endpoint while the
/// pipeline runs, then stops it (also when `during` panics).
fn with_runtime(
    name: &str,
    text: &str,
    pipelined: bool,
    setup: impl FnOnce(&PathBuf),
    during: impl FnOnce(&str),
) -> RunReport {
    let base = temp(name);
    setup(&base);
    let resolver = HostResolver::new(&base);
    let graph = build_graph(text, &resolver).expect("graph");
    let mut engine =
        Engine::build(graph, &default_backends(), &HostServices::default()).expect("build");
    let feed = Feed::new(
        RuntimeInfo {
            pipeline: name.into(),
            version: "test".into(),
            mode: "serial".into(),
        },
        FeedConfig::default(),
    );
    let backends: Vec<(String, String)> = engine
        .stats()
        .into_iter()
        .map(|s| (s.id, s.backend))
        .collect();
    feed.set_topology(&Topology::of(engine.graph(), &backends));
    let cancel = CancellationToken::new();
    let server = Server::start(
        feed.clone(),
        ServerOptions {
            dir: Some(base.join("run")),
            control: Some(Control {
                handle: engine.control(),
                cancel: cancel.clone(),
                resolver: Arc::new(resolver),
            }),
            ..ServerOptions::default()
        },
    )
    .expect("server");
    let mut tap = Tap::new(
        feed.clone(),
        engine.graph().nodes.iter().map(|n| n.id.clone()),
    );
    let options = RunOptions {
        cancel: cancel.clone(),
        pipelined,
        ..RunOptions::default()
    };
    let endpoint = server.endpoint().to_string();
    let stop = cancel.clone();
    std::thread::scope(|scope| {
        let h = scope.spawn(move || {
            let r = engine.run(&options, &mut tap);
            tap.finish(&r);
            r
        });
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| during(&endpoint)));
        stop.cancel();
        let report = h.join().expect("run");
        if let Err(p) = outcome {
            std::panic::resume_unwind(p);
        }
        report
    })
}

/// Reads the stream until `done` says so or `for_ms` passes.
fn follow(endpoint: &str, for_ms: u64, mut each: impl FnMut(&Value) -> bool) {
    let sub = Client::connect(endpoint, "test")
        .expect("connect")
        .subscribe(None)
        .expect("subscribe");
    let until = Instant::now() + Duration::from_millis(for_ms);
    for m in sub {
        let m = m.expect("message");
        if each(&m) || Instant::now() > until {
            return;
        }
    }
}

const SLOW: &str = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: slow }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 4, height: 4, frames: 60 } }\n    - { id: slow, builtin: fault, config: { delayMs: 20 } }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: slow.frame }\n      - { from: slow.frame, to: sink.frame }\n";

#[test]
fn the_node_seen_running_is_the_one_that_runs_and_nothing_runs_after_the_end() {
    for pipelined in [false, true] {
        let mut seen: std::collections::BTreeMap<String, u64> = Default::default();
        let mut after_end = Value::Null;
        with_runtime(
            if pipelined { "act-p" } else { "act-s" },
            SLOW,
            pipelined,
            |_| {},
            |endpoint| {
                // Activity is published on change: keep the current map and weigh it by time (one
                // count per message, and deltas come every 100 ms).
                let mut current: std::collections::BTreeMap<String, u64> = Default::default();
                follow(endpoint, 5000, |m| {
                    let e = &m["entry"];
                    if e["kind"] == "activity" {
                        for (id, n) in e["running"].as_object().into_iter().flatten() {
                            current.insert(id.clone(), n.as_u64().unwrap_or(0));
                        }
                    }
                    for (id, n) in &current {
                        if *n > 0 {
                            *seen.entry(id.clone()).or_default() += 1;
                        }
                    }
                    e["kind"] == "status" && e["status"] == "stopped"
                });
                let (_, snap) = Client::connect(endpoint, "test")
                    .expect("connect")
                    .snapshot()
                    .expect("snapshot");
                after_end = snap["activity"].clone();
            },
        );
        let slow = seen.get("slow").copied().unwrap_or(0);
        let others: u64 = seen
            .iter()
            .filter(|(k, _)| *k != "slow")
            .map(|(_, v)| *v)
            .sum();
        assert!(
            slow >= 5 && slow > others,
            "pipelined={pipelined}: {seen:?}"
        );
        assert!(
            after_end
                .as_object()
                .is_some_and(|m| m.values().all(|v| v == 0)),
            "pipelined={pipelined}: nothing runs after the end: {after_end}"
        );
    }
}

#[test]
fn failures_restarts_and_the_contract_after_a_replacement_are_counted_per_node() {
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: count }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 4, height: 4, frames: 100000, intervalMs: 5 } }\n    - { id: flaky, builtin: fault, onError: restart, config: { failAtCycle: 5, failGeneration: 1 } }\n    - { id: pass, builtin: passthrough }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: flaky.frame }\n      - { from: flaky.frame, to: pass.frame }\n      - { from: pass.frame, to: sink.frame }\n";
    let contract = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../mvf-engine/src/contracts/passthrough.yaml"),
    )
    .expect("contract")
    .replace("version: 1.0.0", "version: 2.0.0");
    let mut flaky = Value::Null;
    let mut pass = Value::Null;
    with_runtime(
        "count",
        text,
        false,
        |base| std::fs::write(base.join("pass-v2.yaml"), &contract).expect("v2"),
        |endpoint| {
            let mut c = Client::connect(endpoint, "test").expect("connect");
            std::thread::sleep(Duration::from_millis(300));
            let r = c
                .command(
                    "r1",
                    "replace",
                    json!({"node": "pass", "contract": "pass-v2.yaml"}),
                    Some(5000),
                )
                .expect("replace");
            assert_eq!(r["status"], "applied", "{r}");
            std::thread::sleep(Duration::from_millis(400));
            let (_, snap) = c.snapshot().expect("snapshot");
            flaky = snap["nodes"]["flaky"].clone();
            pass = snap["nodes"]["pass"].clone();
        },
    );
    assert_eq!(
        (flaky["failures"].as_u64(), flaky["restarts"].as_u64()),
        (Some(1), Some(1)),
        "{flaky}"
    );
    assert_eq!(pass["contract"], "mvf.passthrough@2.0.0", "{pass}");
    assert_eq!(pass["backend"], "in-process", "{pass}");
    assert_eq!(pass["generation"], 2, "{pass}");
}

#[test]
fn editable_fields_are_described_with_their_type_and_schema_but_never_their_value() {
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: fields }\nspec:\n  nodes:\n    - id: cam\n      builtin: synthetic-source\n      config: { width: 4, height: 4, frames: 3, intervalMs: 7 }\n      bindings:\n        intervalMs: { type: int, binding: cam.interval, schema: { type: integer, minimum: 0, maximum: 5000 } }\n    - { id: thr, primitive: value, config: { type: int, literal: 40417, schema: { type: integer, minimum: 0 } } }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: sink.frame }\n";
    let mut topology = Value::Null;
    with_runtime(
        "fields",
        text,
        false,
        |_| {},
        |endpoint| {
            let (_, snap) = Client::connect(endpoint, "test")
                .expect("connect")
                .snapshot()
                .expect("snapshot");
            topology = snap["topology"].clone();
        },
    );
    let node = |id: &str| {
        topology["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find(|n| n["id"] == id)
            .cloned()
            .expect("node")
    };
    assert_eq!(
        node("cam")["live"],
        json!([{"field": "intervalMs", "command": "configure", "type": "int", "shape": "one",
                "schema": {"type": "integer", "minimum": 0, "maximum": 5000}, "binding": "cam.interval"}])
    );
    assert_eq!(
        node("thr")["live"],
        json!([{"field": "value", "command": "set-value", "type": "int", "shape": "one", "schema": {"type": "integer", "minimum": 0}}])
    );
    let text = topology.to_string();
    assert!(
        !text.contains("40417") && !text.contains("\"7\"") && !text.contains("literal"),
        "no values: {text}"
    );
}

#[test]
fn a_pause_is_visible_while_it_lasts_and_its_command_is_applied() {
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: pause }\nspec:\n  nodes:\n    - { id: cycle, primitive: loop, config: { mode: forever } }\n    - { id: cam, builtin: synthetic-source, config: { width: 4, height: 4, frames: 20, intervalMs: 5 } }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: sink.frame }\n    loop:\n      - { from: sink, to: cycle }\n";
    with_runtime(
        "pause",
        text,
        false,
        |_| {},
        |endpoint| {
            let mut c = Client::connect(endpoint, "test").expect("connect");
            std::thread::sleep(Duration::from_millis(200));
            let t0 = Instant::now();
            let r = c
                .command("p1", "pause", json!({}), Some(3000))
                .expect("pause");
            assert_eq!(r["status"], "applied", "{r}");
            assert!(
                t0.elapsed() < Duration::from_millis(1500),
                "applied while paused, not at resume"
            );
            std::thread::sleep(Duration::from_millis(300));
            let (_, a) = c.snapshot().expect("snapshot");
            std::thread::sleep(Duration::from_millis(300));
            let (_, b) = c.snapshot().expect("snapshot");
            assert_eq!(a["status"], "paused");
            assert_eq!(a["cycle"], b["cycle"], "no cycle while paused");
            let r = c
                .command("p2", "resume", json!({}), Some(3000))
                .expect("resume");
            assert_eq!(r["status"], "applied", "{r}");
            std::thread::sleep(Duration::from_millis(300));
            let (_, d) = c.snapshot().expect("snapshot");
            assert_eq!(d["status"], "running");
            assert!(d["cycle"].as_u64() > b["cycle"].as_u64());
        },
    );
}
