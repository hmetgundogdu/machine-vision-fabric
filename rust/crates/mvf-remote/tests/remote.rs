//! Remote management through the relay (#33 part 2): mutual TLS, the access list, roles enforced
//! twice, audit under the real principal, refusal of foreign, unlisted, expired and revoked
//! certificates, a relay crash and network loss that leave the run untouched, a slow remote
//! client, and a Python client over TLS.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_management::{
    Control, Feed, FeedConfig, Role, RuntimeInfo, Server, ServerOptions, Tap, Topology,
};
use mvf_remote::{AccessList, Relay, RelayConfig, RemoteClient, pki};
use serde_json::{Value, json};
use time::OffsetDateTime;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mvfr-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

fn pipeline(frames: u64, interval_ms: u64) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: remote }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 8, height: 8, frames: {frames}, intervalMs: {interval_ms} }} }}\n    - {{ id: sink, builtin: record }}\n    - {{ id: thr, primitive: value, config: {{ type: int, literal: 40 }} }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: sink.frame }}\n"
    )
}

/// A PKI with alice (operator), bob (observer), carol (CA-signed, not admitted) and mallory
/// (admitted, but expired).
fn pki_dir(name: &str) -> PathBuf {
    let dir = temp(name).join("pki");
    pki::init(&dir, &["localhost".into(), "127.0.0.1".into()]).expect("init");
    let (from, to) = pki::validity(30);
    pki::issue(&dir, "alice", Some(Role::Operator), from, to).expect("alice");
    pki::issue(&dir, "bob", Some(Role::Observer), from, to).expect("bob");
    pki::issue(&dir, "carol", None, from, to).expect("carol");
    let past = OffsetDateTime::now_utc() - time::Duration::days(1);
    pki::issue(
        &dir,
        "mallory",
        Some(Role::Operator),
        past - time::Duration::days(30),
        past,
    )
    .expect("mallory");
    dir
}

fn tls_for(dir: &Path, who: &str) -> Arc<rustls::ClientConfig> {
    mvf_remote::tls::client(
        &dir.join("ca.pem"),
        &dir.join(format!("clients/{who}.pem")),
        &dir.join(format!("clients/{who}.key")),
    )
    .expect("client tls")
}

struct Runtime {
    engine: Engine,
    feed: Arc<Feed>,
    server: Server,
    cancel: CancellationToken,
    audit: PathBuf,
}

fn runtime(name: &str, text: &str) -> Runtime {
    let dir = temp(&format!("{name}-rt"));
    let feed = Feed::new(
        RuntimeInfo {
            pipeline: "remote".into(),
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
    Runtime {
        engine,
        feed,
        server,
        cancel,
        audit,
    }
}

fn relay_for(pki: &Path, rt: &Runtime) -> Relay {
    let mut config = RelayConfig::from_pki(pki, "127.0.0.1:0".parse().expect("addr"));
    config.endpoint = Some(rt.server.endpoint().to_string());
    Relay::start(config).expect("relay")
}

fn addr(relay: &Relay) -> String {
    format!("localhost:{}", relay.local_addr().port())
}

fn run_while(rt: &mut Runtime, during: impl FnOnce() + Send) -> RunReport {
    let mut tap = Tap::new(
        rt.feed.clone(),
        rt.engine.graph().nodes.iter().map(|n| n.id.clone()),
    );
    let options = RunOptions {
        cancel: rt.cancel.clone(),
        ..RunOptions::default()
    };
    let cancel = rt.cancel.clone();
    let engine = &mut rt.engine;
    std::thread::scope(|scope| {
        let h = scope.spawn(move || {
            let r = engine.run(&options, &mut tap);
            tap.finish(&r);
            r
        });
        std::thread::sleep(Duration::from_millis(100));
        // A failing assertion must not leave a long run going: stop it, then fail.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(during));
        if outcome.is_err() {
            cancel.cancel();
        }
        let report = h.join().expect("run");
        if let Err(p) = outcome {
            std::panic::resume_unwind(p);
        }
        report
    })
}

fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[test]
fn an_operator_watches_and_commands_through_the_relay_under_its_own_name() {
    let pki = pki_dir("operator");
    let mut rt = runtime("operator", &pipeline(100_000, 2));
    let relay = relay_for(&pki, &rt);
    let at = addr(&relay);
    let report = run_while(&mut rt, || {
        let mut alice = RemoteClient::connect(&at, tls_for(&pki, "alice"), "test").expect("alice");
        let hello = alice.hello().clone();
        assert_eq!(
            (hello["principal"].as_str(), hello["role"].as_str()),
            (Some("alice"), Some("operator"))
        );
        assert_eq!(hello["features"], json!(["commands"]));
        let (_, snap) = alice.snapshot().expect("snapshot");
        assert_eq!(snap["topology"]["pipeline"], "remote");
        let sub = RemoteClient::connect(&at, tls_for(&pki, "alice"), "watch")
            .expect("connect")
            .subscribe(Some(1))
            .expect("subscribe");
        let watcher = std::thread::spawn(move || {
            sub.filter_map(Result::ok)
                .take_while(|m| m["entry"]["status"] != "stopped")
                .filter(|m| m["entry"]["kind"] == "event")
                .count()
        });
        let r = alice
            .command("a1", "set-value", json!({"node": "thr", "value": 41}), None)
            .expect("set");
        assert_eq!(r["status"], "applied", "{r}");
        let again = alice
            .command("a1", "set-value", json!({"node": "thr", "value": 41}), None)
            .expect("again");
        assert_eq!(again["duplicate"], true, "idempotent through the relay too");
        assert_eq!(
            alice.command("a2", "stop", json!({}), None).expect("stop")["status"],
            "applied"
        );
        assert!(
            watcher.join().expect("watcher") >= 1,
            "the watcher saw the live event"
        );
    });
    assert_eq!(report.end, EndReason::Cancelled);
    let audit = lines(&rt.audit);
    let alice: Vec<_> = audit.iter().filter(|a| a["principal"] == "alice").collect();
    assert_eq!(alice.len(), 3, "{audit:?}");
    assert!(alice.iter().all(|a| {
        a["role"] == "operator"
            && a["via"]
                .as_str()
                .is_some_and(|v| v.starts_with("mvf-remote 127.0.0.1:"))
    }));
    let relay_audit = lines(&pki.join("relay-audit.jsonl"));
    assert!(
        relay_audit
            .iter()
            .any(|a| a["event"] == "connection-admitted" && a["principal"] == "alice")
    );
}

#[test]
fn an_observer_sees_but_cannot_command() {
    let pki = pki_dir("observer");
    let mut rt = runtime("observer", &pipeline(100_000, 2));
    let relay = relay_for(&pki, &rt);
    let at = addr(&relay);
    run_while(&mut rt, || {
        let mut bob = RemoteClient::connect(&at, tls_for(&pki, "bob"), "test").expect("bob");
        assert_eq!(bob.hello()["role"], "observer");
        assert_eq!(bob.hello()["features"], json!([]));
        assert!(bob.snapshot().is_ok());
        let r = bob.command("b1", "stop", json!({}), None).expect("reply");
        assert_eq!(
            (r["status"].as_str(), r["code"].as_str()),
            (Some("refused"), Some("forbidden"))
        );
        let mut alice = RemoteClient::connect(&at, tls_for(&pki, "alice"), "test").expect("alice");
        alice.command("s", "stop", json!({}), None).expect("stop");
    });
    // Refused at the relay: it never reached the runtime.
    assert!(lines(&rt.audit).iter().all(|a| a["principal"] != "bob"));
    assert!(
        lines(&pki.join("relay-audit.jsonl"))
            .iter()
            .any(|a| a["event"] == "forbidden" && a["principal"] == "bob")
    );
}

#[test]
fn foreign_unlisted_expired_and_revoked_certificates_are_refused() {
    let pki = pki_dir("refused");
    let rt = runtime("refused", &pipeline(10, 0));
    let relay = relay_for(&pki, &rt);
    let at = addr(&relay);
    // Signed by the CA but not in the access list.
    let e = RemoteClient::connect(&at, tls_for(&pki, "carol"), "test").expect_err("carol");
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{e}");
    // Admitted, but expired: the TLS handshake itself fails.
    assert!(RemoteClient::connect(&at, tls_for(&pki, "mallory"), "test").is_err());
    // A certificate from another CA.
    let other = temp("refused-other").join("pki");
    pki::init(&other, &["localhost".into()]).expect("other ca");
    let (from, to) = pki::validity(30);
    pki::issue(&other, "eve", Some(Role::Operator), from, to).expect("eve");
    let eve = mvf_remote::tls::client(
        &pki.join("ca.pem"),
        &other.join("clients/eve.pem"),
        &other.join("clients/eve.key"),
    )
    .expect("tls");
    assert!(RemoteClient::connect(&at, eve, "test").is_err());
    // Revocation and rotation, effective on the next connection, no relay restart.
    assert!(RemoteClient::connect(&at, tls_for(&pki, "alice"), "test").is_ok());
    let access = pki.join("access.json");
    let mut list = AccessList::load(&access).expect("access");
    assert_eq!(list.revoke("alice"), 1);
    list.save(&access).expect("save");
    let e = RemoteClient::connect(&at, tls_for(&pki, "alice"), "test").expect_err("revoked");
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
    pki::issue(&pki, "alice", Some(Role::Operator), from, to).expect("rotated");
    assert!(
        RemoteClient::connect(&at, tls_for(&pki, "alice"), "test").is_ok(),
        "the new certificate works"
    );
    let reasons: Vec<String> = lines(&pki.join("relay-audit.jsonl"))
        .iter()
        .filter(|a| a["event"] == "connection-refused")
        .filter_map(|a| a["reason"].as_str().map(str::to_string))
        .collect();
    let has = |s: &str| reasons.iter().any(|r| r.contains(s));
    assert!(
        has("not in the access list") && has("revoked") && has("handshake"),
        "{reasons:?}"
    );
    assert_eq!(
        relay
            .counters()
            .refused
            .load(std::sync::atomic::Ordering::Relaxed),
        4
    );
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("port")
}

fn spawn_relay(pki: &Path, port: u16, endpoint: &str) -> std::process::Child {
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_mvf-remote"))
        .args([
            "serve",
            "--listen",
            &format!("127.0.0.1:{port}"),
            "--endpoint",
            endpoint,
        ])
        .arg("--pki")
        .arg(pki)
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("relay");
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))).is_err() {
        assert!(Instant::now() < deadline, "relay never listened");
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

#[test]
fn a_relay_crash_never_touches_the_run_and_a_new_relay_resumes_the_stream() {
    let pki = pki_dir("crash");
    // Long enough to outlive two relay start-ups even on a slow runner; stopped by a command.
    let mut rt = runtime("crash", &pipeline(1_000_000, 2));
    let port = free_port();
    let endpoint = rt.server.endpoint().to_string();
    let at = format!("localhost:{port}");
    let feed = rt.feed.clone();
    let mut seqs = Vec::new();
    let report = run_while(&mut rt, || {
        let mut relay = spawn_relay(&pki, port, &endpoint);
        let mut sub = RemoteClient::connect(&at, tls_for(&pki, "bob"), "test")
            .expect("bob")
            .subscribe(Some(1))
            .expect("sub");
        for m in sub.by_ref() {
            let m = m.expect("msg");
            if let Some(s) = m["seq"].as_u64().filter(|_| m["op"] == "entry") {
                seqs.push(s);
            }
            if seqs.len() == 5 {
                break;
            }
        }
        let resume = sub.last_seq().expect("last") + 1;
        let before = feed.last_seq();
        relay.kill().expect("kill"); // SIGKILL / TerminateProcess: no clean shutdown at all
        let _ = relay.wait();
        drop(sub);
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            feed.last_seq() > before,
            "the run kept publishing while the relay was dead"
        );
        let mut relay = spawn_relay(&pki, port, &endpoint);
        let mut sub = RemoteClient::connect(&at, tls_for(&pki, "bob"), "test")
            .expect("again")
            .subscribe(Some(resume))
            .expect("sub");
        let mut after_crash = 0;
        let mut stopping = false;
        for m in sub.by_ref() {
            let m = m.expect("msg");
            if m["op"] == "entry" {
                seqs.push(m["seq"].as_u64().expect("seq"));
                after_crash += 1;
                if m["entry"]["status"] == "stopped" {
                    break;
                }
            }
            // Past the crash point and well into what was published meanwhile: stop the run
            // through the new relay (an operator), and read on to the final status.
            if !stopping && after_crash >= 20 {
                stopping = true;
                let mut alice =
                    RemoteClient::connect(&at, tls_for(&pki, "alice"), "test").expect("alice");
                let r = alice
                    .command("crash-stop", "stop", json!({}), Some(10_000))
                    .expect("stop");
                assert_eq!(r["status"], "applied", "{r}");
            }
        }
        relay.kill().expect("kill");
        let _ = relay.wait();
    });
    assert_eq!(
        report.end,
        EndReason::Cancelled,
        "it ran until it was told to stop"
    );
    assert_eq!(
        seqs,
        (1..=seqs.len() as u64).collect::<Vec<_>>(),
        "no loss, no duplicate across the crash"
    );
}

#[test]
fn a_dropped_connection_resumes_and_a_stuck_remote_client_never_slows_the_run() {
    let pki = pki_dir("loss");
    let mut rt = runtime("loss", &pipeline(1500, 1));
    let mut config = RelayConfig::from_pki(&pki, "127.0.0.1:0".parse().expect("addr"));
    config.endpoint = Some(rt.server.endpoint().to_string());
    config.write_timeout = Duration::from_millis(200);
    let relay = Relay::start(config).expect("relay");
    let at = addr(&relay);
    let t0 = Instant::now();
    let report = run_while(&mut rt, || {
        // A remote client that subscribes and then never reads.
        let stuck = RemoteClient::connect(&at, tls_for(&pki, "bob"), "stuck")
            .expect("stuck")
            .subscribe(Some(1))
            .expect("sub");
        // Another one loses its connection mid-stream and comes back.
        let mut seqs = Vec::new();
        let mut sub = RemoteClient::connect(&at, tls_for(&pki, "bob"), "a")
            .expect("a")
            .subscribe(Some(1))
            .expect("sub");
        for m in sub.by_ref().filter_map(Result::ok) {
            if m["op"] == "entry" {
                seqs.push(m["seq"].as_u64().expect("seq"));
                if seqs.len() == 10 {
                    break;
                }
            }
        }
        let resume = sub.last_seq().expect("last") + 1;
        drop(sub);
        let mut sub = RemoteClient::connect(&at, tls_for(&pki, "bob"), "a")
            .expect("again")
            .subscribe(Some(resume))
            .expect("sub");
        for m in sub.by_ref().filter_map(Result::ok) {
            if m["op"] == "entry" {
                seqs.push(m["seq"].as_u64().expect("seq"));
                if m["entry"]["status"] == "stopped" {
                    break;
                }
            }
        }
        assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
        drop(stuck);
    });
    assert_eq!(report.cycles_completed, 1500);
    // 1500 cycles paced at 1 ms: the stuck client did not stretch the run.
    assert!(t0.elapsed() < Duration::from_secs(15), "{:?}", t0.elapsed());
}

#[test]
fn the_python_client_speaks_mutual_tls_through_the_relay() {
    let python = std::env::var("MVF_PYTHON")
        .unwrap_or_else(|_| (if cfg!(windows) { "python" } else { "python3" }).into());
    let tls13 = std::process::Command::new(&python)
        .args([
            "-c",
            "import ssl, sys; sys.exit(0 if ssl.HAS_TLSv1_3 else 1)",
        ])
        .status()
        .is_ok_and(|s| s.success());
    if !tls13 {
        eprintln!("NOT RUN: {python}'s ssl module has no TLS 1.3 (the relay requires it)");
        return;
    }
    let pki = pki_dir("python");
    let mut rt = runtime("python", &pipeline(100_000, 2));
    let relay = relay_for(&pki, &rt);
    let at = format!("127.0.0.1:{}", relay.local_addr().port());
    let script = repo().join("clients/mvf-manage-py/mvf_manage.py");
    let py = |who: &str, args: &[&str]| {
        let out = std::process::Command::new(&python)
            .arg(&script)
            .args(["--remote", &at])
            .arg("--ca")
            .arg(pki.join("ca.pem"))
            .arg("--cert")
            .arg(pki.join(format!("clients/{who}.pem")))
            .arg("--key")
            .arg(pki.join(format!("clients/{who}.key")))
            .args(args)
            .output()
            .expect("python");
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };
    run_while(&mut rt, || {
        let (ok, out) = py("bob", &["status", "--json"]);
        assert!(ok, "{out}");
        let snap: Value = serde_json::from_str(out.trim()).expect("snapshot");
        assert_eq!(snap["topology"]["pipeline"], "remote");
        let (ok, out) = py("bob", &["command", "stop", "--id", "py-1"]);
        assert!(!ok && out.contains("forbidden"), "{out}");
        let (ok, out) = py("alice", &["command", "stop", "--id", "py-2"]);
        assert!(ok, "{out}");
    });
    assert!(
        lines(&rt.audit)
            .iter()
            .any(|a| a["principal"] == "alice" && a["id"] == "py-2")
    );
}

#[test]
fn keys_are_private_and_the_access_list_names_each_certificate() {
    let pki = pki_dir("files");
    let list = AccessList::load(&pki.join("access.json")).expect("access");
    let names: Vec<_> = list
        .clients
        .iter()
        .map(|c| (c.name.as_str(), c.role.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("alice", "operator"),
            ("bob", "observer"),
            ("mallory", "operator")
        ]
    );
    assert!(
        list.clients
            .iter()
            .all(|c| c.fingerprint.starts_with("sha256:") && c.fingerprint.len() == 71)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for key in ["ca.key", "server.key", "clients/alice.key", "access.json"] {
            let mode = std::fs::metadata(pki.join(key))
                .expect(key)
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{key}");
        }
    }
    assert!(
        pki::init(&pki, &["localhost".into()]).is_err(),
        "an existing CA is never overwritten"
    );
}

/// `MVF_REMOTE_BENCH=1 cargo test --release -p mvf-remote --test remote bench -- --nocapture`
#[test]
fn bench_relay_overhead() {
    if std::env::var_os("MVF_REMOTE_BENCH").is_none_or(|v| v != "1") {
        return;
    }
    let pki = pki_dir("bench");
    let mut rt = runtime("bench", &pipeline(2_000, 2));
    let port = free_port();
    let endpoint = rt.server.endpoint().to_string();
    let at = format!("localhost:{port}");
    let pct = |v: &mut Vec<f64>, p: f64| {
        v.sort_by(|a, b| a.total_cmp(b));
        v[((v.len() - 1) as f64 * p).round() as usize]
    };
    #[cfg(unix)]
    let children_cpu = || {
        let mut r: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: valid out-pointer.
        unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut r) };
        let us = |t: libc::timeval| t.tv_sec as f64 * 1e6 + t.tv_usec as f64;
        us(r.ru_utime) + us(r.ru_stime)
    };
    #[cfg(not(unix))]
    let children_cpu = || 0.0;
    run_while(&mut rt, || {
        let mut relay = spawn_relay(&pki, port, &endpoint);
        let mut local = mvf_management::Client::connect(&endpoint, "bench").expect("local");
        let mut remote = RemoteClient::connect(&at, tls_for(&pki, "bob"), "bench").expect("remote");
        let rtt = |f: &mut dyn FnMut()| {
            let mut v: Vec<f64> = (0..2000)
                .map(|_| {
                    let t = Instant::now();
                    f();
                    t.elapsed().as_secs_f64() * 1e6
                })
                .collect();
            (pct(&mut v, 0.5), pct(&mut v, 0.99))
        };
        let l = rtt(&mut || {
            local.request(&json!({"op": "ping"})).expect("ping");
        });
        let r = rtt(&mut || {
            remote.request(&json!({"op": "ping"})).expect("ping");
        });
        eprintln!(
            "ping round trip p50 / p99: local {:.0} / {:.0} us, through the relay (TLS 1.3) {:.0} / {:.0} us",
            l.0, l.1, r.0, r.1
        );
        // A remote watcher for the rest of the run: the relay's own CPU and the bandwidth.
        let c0 = children_cpu();
        let t0 = Instant::now();
        let mut bytes = 0usize;
        let sub = RemoteClient::connect(&at, tls_for(&pki, "bob"), "w")
            .expect("w")
            .subscribe(None)
            .expect("sub");
        for m in sub.filter_map(Result::ok) {
            bytes += m.to_string().len() + 1;
            if m["entry"]["status"] == "stopped" {
                break;
            }
        }
        let secs = t0.elapsed().as_secs_f64();
        relay.kill().expect("kill");
        let _ = relay.wait();
        let cpu_s = (children_cpu() - c0) / 1e6;
        eprintln!(
            "relaying a live stream for {secs:.1} s: {:.1} KiB/s to the client, relay CPU {:.2} % of one core",
            bytes as f64 / 1024.0 / secs,
            cpu_s / secs * 100.0
        );
    });
}

/// Part 3 through the relay: a previewer sees previews, an observer may not, and an operator
/// uploads, stages and activates a package over mutual TLS.
#[test]
fn previews_and_uploads_go_through_the_relay_by_role() {
    let pki = pki_dir("part3");
    let (from, to) = pki::validity(30);
    pki::issue(&pki, "pia", Some(Role::Previewer), from, to).expect("pia");
    let base = temp("part3-base");
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: remote3 }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 64, height: 48, channels: 3, frames: 1000000, intervalMs: 5 } }\n    - { id: pass, builtin: passthrough }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: pass.frame }\n      - { from: pass.frame, to: sink.frame }\n";
    let feed = Feed::new(
        RuntimeInfo {
            pipeline: "remote3".into(),
            version: "test".into(),
            mode: "serial".into(),
        },
        FeedConfig::default(),
    );
    feed.enable_previews(mvf_management::PreviewConfig {
        nodes: vec!["cam".into()],
        ..Default::default()
    });
    let resolver = HostResolver::new(&base).with_module_root(repo().join("modules"));
    let graph = build_graph(text, &resolver).expect("graph");
    let engine =
        Engine::build(graph, &default_backends(), &HostServices::default()).expect("build");
    feed.set_topology(&Topology::of(engine.graph(), &[]));
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
            audit: Some(base.join("audit.jsonl")),
            uploads: Some(mvf_management::UploadOptions::new(
                base.join(".mvf/staged"),
                ".mvf/staged",
            )),
            ..ServerOptions::default()
        },
    )
    .expect("server");
    let stop = cancel.clone();
    let mut rt = Runtime {
        engine,
        feed,
        server,
        cancel,
        audit: base.join("audit.jsonl"),
    };
    let relay = relay_for(&pki, &rt);
    let at = addr(&relay);
    let src = temp("part3-pkg");
    std::fs::write(
        src.join("node.yaml"),
        std::fs::read_to_string(
            repo().join("rust/crates/mvf-engine/src/contracts/passthrough.yaml"),
        )
        .expect("contract")
        .replace("version: 1.0.0", "version: 3.0.0"),
    )
    .expect("node.yaml");
    run_while(&mut rt, || {
        // Previews: the previewer gets one; the observer does not.
        let mut pia = RemoteClient::connect(&at, tls_for(&pki, "pia"), "test").expect("pia");
        assert_eq!(pia.hello()["role"], "previewer");
        let first = pia
            .request(&json!({"op": "preview", "node": "cam", "fps": 2}))
            .expect("preview");
        assert_eq!(
            (first["op"].as_str(), first["format"].as_str()),
            (Some("preview"), Some("png")),
            "{first}"
        );
        let mut bob = RemoteClient::connect(&at, tls_for(&pki, "bob"), "test").expect("bob");
        assert_eq!(
            bob.request(&json!({"op": "preview", "node": "cam"}))
                .expect("reply")["code"],
            "forbidden"
        );
        assert_eq!(
            bob.request(&json!({"op": "upload-status", "package": "p", "path": "x"}))
                .expect("reply")["code"],
            "forbidden"
        );
        // The operator uploads, stages and activates.
        let mut alice = RemoteClient::connect(&at, tls_for(&pki, "alice"), "test").expect("alice");
        let mut request = |v: &Value| alice.request(v);
        let staged =
            mvf_management::upload_dir(&mut request, "pass-v3", &src, 512 * 1024).expect("upload");
        assert_eq!(staged["status"], "applied", "{staged}");
        let contract = staged["staged"]["contract"]
            .as_str()
            .expect("contract")
            .to_string();
        let r = request(&json!({"op": "command", "id": "act3", "command": "replace", "args": {"node": "pass", "contract": contract}})).expect("replace");
        assert_eq!(
            (r["status"].as_str(), r["event"]["toVersion"].as_str()),
            (Some("applied"), Some("mvf.passthrough@3.0.0")),
            "{r}"
        );
        stop.cancel();
    });
    let records = lines(&rt.audit);
    assert!(
        records
            .iter()
            .any(|a| a["event"] == "preview-opened" && a["principal"] == "pia")
    );
    assert!(
        records.iter().any(|a| a["event"] == "upload"
            && a["principal"] == "alice"
            && a["status"] == "complete")
    );
    assert!(records.iter().any(|a| a["command"] == "replace"
        && a["principal"] == "alice"
        && a["status"] == "applied"));
    assert!(
        lines(&pki.join("relay-audit.jsonl"))
            .iter()
            .any(|a| a["event"] == "forbidden" && a["principal"] == "bob" && a["op"] == "preview")
    );
}

/// A package signed by `mvf-remote sign` (ring) is verified by the runtime (ed25519-dalek), through
/// the relay; an unsigned one is refused when the runtime trusts signers.
#[test]
fn a_package_signed_by_mvf_remote_is_verified_by_the_runtime_through_the_relay() {
    let pki = pki_dir("signed");
    let keys = temp("signed-keys");
    let bin = env!("CARGO_BIN_EXE_mvf-remote");
    let out = std::process::Command::new(bin)
        .args(["sign", "keygen", "--dir"])
        .arg(&keys)
        .output()
        .expect("keygen");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let signer = std::fs::read_to_string(keys.join("signer.pub"))
        .expect("pub")
        .trim()
        .to_string();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), signer);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(keys.join("signer.pk8"))
            .expect("pk8")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    let src = temp("signed-pkg");
    std::fs::write(
        src.join("node.yaml"),
        std::fs::read_to_string(
            repo().join("rust/crates/mvf-engine/src/contracts/passthrough.yaml"),
        )
        .expect("contract")
        .replace("version: 1.0.0", "version: 4.0.0"),
    )
    .expect("node.yaml");
    let sig_file = keys.join("pass-v4.sig.json");
    let out = std::process::Command::new(bin)
        .args(["sign", "package", "--package", "pass-v4", "--key"])
        .arg(keys.join("signer.pk8"))
        .arg("--out")
        .arg(&sig_file)
        .arg(&src)
        .output()
        .expect("sign");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let signature: Value =
        serde_json::from_slice(&std::fs::read(&sig_file).expect("sig")).expect("json");

    let base = temp("signed-base");
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: signed }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 8, height: 8, frames: 1000000, intervalMs: 5 } }\n    - { id: pass, builtin: passthrough }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: pass.frame }\n      - { from: pass.frame, to: sink.frame }\n";
    let feed = Feed::new(
        RuntimeInfo {
            pipeline: "signed".into(),
            version: "test".into(),
            mode: "serial".into(),
        },
        FeedConfig::default(),
    );
    let resolver = HostResolver::new(&base).with_module_root(repo().join("modules"));
    let graph = build_graph(text, &resolver).expect("graph");
    let engine =
        Engine::build(graph, &default_backends(), &HostServices::default()).expect("build");
    feed.set_topology(&Topology::of(engine.graph(), &[]));
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let server = Server::start(
        feed.clone(),
        ServerOptions {
            dir: Some(base.join("run")),
            control: Some(Control {
                handle: engine.control(),
                cancel: cancel.clone(),
                resolver: Arc::new(resolver),
            }),
            audit: Some(base.join("audit.jsonl")),
            uploads: Some(mvf_management::UploadOptions {
                trusted: vec![signer.clone()],
                ..mvf_management::UploadOptions::new(base.join(".mvf/staged"), ".mvf/staged")
            }),
            ..ServerOptions::default()
        },
    )
    .expect("server");
    let mut rt = Runtime {
        engine,
        feed,
        server,
        cancel,
        audit: base.join("audit.jsonl"),
    };
    let relay = relay_for(&pki, &rt);
    let at = addr(&relay);
    run_while(&mut rt, || {
        let mut alice = RemoteClient::connect(&at, tls_for(&pki, "alice"), "test").expect("alice");
        let mut request = |v: &Value| alice.request(v);
        // Unsigned: refused by the policy, nothing staged.
        let unsigned = mvf_management::upload_dir(&mut request, "pass-unsigned", &src, 512 * 1024)
            .expect("upload");
        assert_eq!(
            (unsigned["status"].as_str(), unsigned["code"].as_str()),
            (Some("refused"), Some("unsigned")),
            "{unsigned}"
        );
        // Signed by the trusted key: staged, then activated.
        let staged = mvf_management::upload_dir_signed(
            &mut request,
            "pass-v4",
            &src,
            512 * 1024,
            Some(&signature),
        )
        .expect("upload");
        assert_eq!(
            (
                staged["status"].as_str(),
                staged["staged"]["signedBy"].as_str()
            ),
            (Some("applied"), Some(signer.as_str())),
            "{staged}"
        );
        let contract = staged["staged"]["contract"]
            .as_str()
            .expect("contract")
            .to_string();
        let r = request(&json!({"op": "command", "id": "act4", "command": "replace", "args": {"node": "pass", "contract": contract}})).expect("replace");
        assert_eq!(
            (r["status"].as_str(), r["event"]["toVersion"].as_str()),
            (Some("applied"), Some("mvf.passthrough@4.0.0")),
            "{r}"
        );
        stop.cancel();
    });
    let records = lines(&rt.audit);
    assert!(
        records.iter().any(|a| a["command"] == "stage"
            && a["status"] == "refused"
            && a["code"] == "unsigned")
    );
    assert!(
        records.iter().any(|a| a["command"] == "stage"
            && a["status"] == "applied"
            && a["principal"] == "alice")
    );
}
