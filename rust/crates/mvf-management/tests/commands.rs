//! Management commands (#33 part 2) through a real engine: the engine's own verdicts, idempotency,
//! roles, deadlines and the audit log.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_management::*;
use serde_json::{Value, json};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn pipeline(frames: u64, interval_ms: u64) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: commanded }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 8, height: 8, frames: {frames}, intervalMs: {interval_ms} }} }}\n    - {{ id: pass, builtin: passthrough }}\n    - {{ id: sink, builtin: record }}\n    - {{ id: thr, primitive: value, config: {{ type: int, literal: 40 }} }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: pass.frame }}\n      - {{ from: pass.frame, to: sink.frame }}\n"
    )
}

struct Run {
    engine: Engine,
    feed: Arc<Feed>,
    server: Server,
    cancel: CancellationToken,
    audit: PathBuf,
}

fn setup(name: &str, text: &str) -> Run {
    let dir = std::env::temp_dir().join(format!("mvfc-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let feed = Feed::new(
        RuntimeInfo {
            pipeline: "commanded".into(),
            version: "test".into(),
            mode: "serial".into(),
        },
        FeedConfig {
            publish_every: Duration::from_millis(10),
            ..FeedConfig::default()
        },
    );
    let resolver = HostResolver::new(repo()).with_module_root(repo().join("modules"));
    let graph = build_graph(text, &resolver).expect("graph");
    let engine =
        Engine::build(graph, &default_backends(), &HostServices::default()).expect("build");
    feed.set_topology(&Topology::of(engine.graph(), &[]));
    let cancel = CancellationToken::new();
    let audit = dir.join("audit.jsonl");
    let server = Server::start(
        feed.clone(),
        ServerOptions {
            dir: Some(dir.join("run")),
            control: Some(Control {
                handle: engine.control(),
                cancel: cancel.clone(),
                resolver: Arc::new(resolver),
            }),
            audit: Some(audit.clone()),
            ..ServerOptions::default()
        },
    )
    .expect("server");
    Run {
        engine,
        feed,
        server,
        cancel,
        audit,
    }
}

/// Runs the engine on a thread while `during` talks to it; returns the report.
fn with_run(run: &mut Run, during: impl FnOnce(&str) + Send) -> RunReport {
    let mut tap = Tap::new(
        run.feed.clone(),
        run.engine.graph().nodes.iter().map(|n| n.id.clone()),
    );
    let endpoint = run.server.endpoint().to_string();
    let options = RunOptions {
        cancel: run.cancel.clone(),
        ..RunOptions::default()
    };
    let engine = &mut run.engine;
    std::thread::scope(|scope| {
        let handle = scope.spawn(move || {
            let r = engine.run(&options, &mut tap);
            tap.finish(&r);
            r
        });
        // Let the run start before commanding it.
        std::thread::sleep(Duration::from_millis(100));
        during(&endpoint);
        handle.join().expect("run")
    })
}

fn audit_records(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).expect("audit json"))
        .collect()
}

#[test]
fn commands_go_through_the_engine_and_report_its_verdict() {
    let mut run = setup("verdicts", &pipeline(100_000, 2));
    let mut replies = Vec::new();
    let report = with_run(&mut run, |endpoint| {
        let mut c = Client::connect(endpoint, "test").expect("connect");
        assert_eq!(c.runtime()["pipeline"], "commanded");
        let mut cmd = |id: &str, name: &str, args: Value| {
            let r = c.command(id, name, args, Some(5000)).expect("command");
            replies.push(r.clone());
            r
        };
        let r = cmd("v1", "set-value", json!({"node": "thr", "value": 41}));
        assert_eq!(r["status"], "applied", "{r}");
        assert_eq!(r["event"]["kind"], "value-set");
        // The engine type-checks the value: refused by the engine, nothing changed.
        let r = cmd("v2", "set-value", json!({"node": "thr", "value": "forty"}));
        assert_eq!(r["status"], "rejected", "{r}");
        // No loop in this graph: the engine refuses to pause it.
        let r = cmd("p1", "pause", json!({}));
        assert_eq!(r["status"], "rejected", "{r}");
        // A live replacement of a node, then its rollback within probation.
        let r = cmd(
            "r1",
            "replace",
            json!({"node": "pass", "builtin": "passthrough", "probationCycles": 100000}),
        );
        assert_eq!(r["status"], "applied", "{r}");
        assert_eq!(r["event"]["kind"], "promoted");
        assert_eq!(r["event"]["toGeneration"], 2);
        let r = cmd("r2", "rollback", json!({"node": "pass"}));
        assert_eq!(r["status"], "applied", "{r}");
        assert_eq!(r["event"]["kind"], "rolled-back");
        // Not sent at all.
        let r = cmd("x1", "reboot", json!({}));
        assert_eq!(
            (r["status"].as_str(), r["code"].as_str()),
            (Some("refused"), Some("unknown-command"))
        );
        let r = cmd("", "stop", json!({}));
        assert_eq!(r["code"], "id-required");
        let r = cmd(
            "bad",
            "replace",
            json!({"node": "pass", "contract": "../../etc/x.yaml"}),
        );
        assert_eq!(
            (r["status"].as_str(), r["code"].as_str()),
            (Some("refused"), Some("invalid")),
            "{r}"
        );
        // A clean stop at the next boundary.
        let r = cmd("s1", "stop", json!({}));
        assert_eq!(r["status"], "applied", "{r}");
        assert_eq!(r["event"]["status"], "stopped");
    });
    assert_eq!(report.end, EndReason::Cancelled);
    let kinds: Vec<_> = report.replacements.iter().map(|e| e.kind).collect();
    assert!(
        kinds.contains(&ReplacementEventKind::Promoted)
            && kinds.contains(&ReplacementEventKind::RolledBack)
    );
    let audit = audit_records(&run.audit);
    assert_eq!(
        audit.len(),
        replies.len(),
        "every command, refused ones too, is audited"
    );
    for (a, r) in audit.iter().zip(&replies) {
        assert_eq!(
            (&a["command"], &a["status"], &a["principal"]),
            (&r["command"], &r["status"], &json!("local"))
        );
        assert!(a["atMs"].as_u64().is_some());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&run.audit)
            .expect("audit")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn a_repeated_command_id_is_applied_once() {
    let mut run = setup("idempotent", &pipeline(100_000, 2));
    let report = with_run(&mut run, |endpoint| {
        let mut c = Client::connect(endpoint, "test").expect("connect");
        let first = c
            .command(
                "same",
                "set-value",
                json!({"node": "thr", "value": 7}),
                None,
            )
            .expect("first");
        assert_eq!(first["status"], "applied");
        // A retry after, say, a lost reply: same id, from a fresh connection.
        let mut again = Client::connect(endpoint, "test").expect("connect");
        let second = again
            .command(
                "same",
                "set-value",
                json!({"node": "thr", "value": 7}),
                None,
            )
            .expect("second");
        assert_eq!(second["duplicate"], true);
        assert_eq!(second["status"], first["status"]);
        assert_eq!(second["event"], first["event"]);
        assert_eq!(
            c.command("stop", "stop", json!({}), None).expect("stop")["status"],
            "applied"
        );
    });
    let sets = report
        .live
        .iter()
        .filter(|e| e.kind == LiveEventKind::ValueSet && e.node.as_deref() == Some("thr"))
        .count();
    assert_eq!(
        sets, 1,
        "the engine saw the command once: {:?}",
        report.live
    );
    let audit = audit_records(&run.audit);
    assert_eq!(
        audit.iter().filter(|a| a["duplicate"] == true).count(),
        1,
        "the retry is audited as a duplicate"
    );
}

#[test]
fn an_observer_cannot_run_commands_and_the_attempt_is_audited() {
    let mut run = setup("roles", &pipeline(100_000, 2));
    let report = with_run(&mut run, |endpoint| {
        // A relay speaks for a remote observer.
        let conn = connect_raw(endpoint).expect("connect");
        let mut w = conn.duplicate().expect("dup");
        let mut r = BufReader::new(conn);
        let mut ask = |v: Value| -> Value {
            w.write_all(format!("{v}\n").as_bytes()).expect("write");
            let mut line = String::new();
            r.read_line(&mut line).expect("read");
            serde_json::from_str(&line).expect("json")
        };
        let hello = ask(
            json!({"op": "hello", "api": [1], "onBehalfOf": {"principal": "alice", "role": "observer", "via": "relay test"}}),
        );
        assert_eq!(
            (hello["principal"].as_str(), hello["role"].as_str()),
            (Some("alice"), Some("observer"))
        );
        assert_eq!(
            hello["features"],
            json!([]),
            "no commands offered to an observer"
        );
        let refused = ask(json!({"op": "command", "id": "a1", "command": "stop", "args": {}}));
        assert_eq!(
            (refused["status"].as_str(), refused["code"].as_str()),
            (Some("refused"), Some("forbidden"))
        );
        assert_eq!(
            ask(json!({"op": "snapshot"}))["op"],
            "snapshot",
            "observing is still fine"
        );
        // An unknown role is not an escalation path.
        let conn = connect_raw(endpoint).expect("connect");
        let mut w2 = conn.duplicate().expect("dup");
        let mut r2 = BufReader::new(conn);
        w2.write_all(b"{\"op\":\"hello\",\"api\":[1],\"onBehalfOf\":{\"principal\":\"mallory\",\"role\":\"root\"}}\n").expect("write");
        let mut line = String::new();
        r2.read_line(&mut line).expect("read");
        assert!(line.contains("\"invalid\""), "{line}");
        let mut owner = Client::connect(endpoint, "owner").expect("connect");
        assert_eq!(
            owner.command("s", "stop", json!({}), None).expect("stop")["status"],
            "applied"
        );
    });
    assert_eq!(report.end, EndReason::Cancelled);
    let audit = audit_records(&run.audit);
    let alice: Vec<_> = audit.iter().filter(|a| a["principal"] == "alice").collect();
    assert_eq!(alice.len(), 1);
    assert_eq!(
        (
            alice[0]["role"].as_str(),
            alice[0]["status"].as_str(),
            alice[0]["via"].as_str()
        ),
        (Some("observer"), Some("refused"), Some("relay test"))
    );
}

#[test]
fn a_command_past_its_deadline_is_pending_and_its_outcome_follows_in_the_stream() {
    // Slow cycles: the engine applies the command at the next boundary, after the deadline.
    let mut run = setup("deadline", &pipeline(100_000, 300));
    let report = with_run(&mut run, |endpoint| {
        let mut c = Client::connect(endpoint, "test").expect("connect");
        let r = c
            .command(
                "late",
                "set-value",
                json!({"node": "thr", "value": 3}),
                Some(1),
            )
            .expect("command");
        assert_eq!(r["status"], "pending", "{r}");
        let after = r["afterSeq"].as_u64().expect("afterSeq");
        let mut sub = Client::connect(endpoint, "test")
            .expect("connect")
            .subscribe(Some(after + 1))
            .expect("sub");
        let found = sub.by_ref().filter_map(Result::ok).find(|m| {
            m["entry"]["kind"] == "event"
                && m["entry"]["detail"]["kind"] == "value-set"
                && m["entry"]["detail"]["node"] == "thr"
        });
        assert!(
            found.is_some(),
            "the outcome arrives in the stream after afterSeq"
        );
        let mut c = Client::connect(endpoint, "test").expect("connect");
        assert_eq!(
            c.command("stop", "stop", json!({}), Some(10_000))
                .expect("stop")["status"],
            "applied"
        );
    });
    assert_eq!(report.end, EndReason::Cancelled);
}

#[test]
fn an_observation_only_endpoint_takes_no_commands() {
    let dir = std::env::temp_dir().join(format!("mvfc-{}-readonly", std::process::id()));
    let feed = Feed::new(
        RuntimeInfo {
            pipeline: "ro".into(),
            version: "test".into(),
            mode: "serial".into(),
        },
        FeedConfig::default(),
    );
    let server = Server::start(
        feed,
        ServerOptions {
            dir: Some(dir),
            ..ServerOptions::default()
        },
    )
    .expect("server");
    let mut c = Client::connect(server.endpoint(), "test").expect("connect");
    let r = c.command("x", "stop", json!({}), None).expect("command");
    assert_eq!(
        (r["status"].as_str(), r["code"].as_str()),
        (Some("refused"), Some("unavailable"))
    );
}
