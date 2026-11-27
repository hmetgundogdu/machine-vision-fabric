//! `MVF_IPC_BENCH=1 cargo test --release -p mvf-ipc --test bench -- --nocapture --test-threads 1`
//!
//! The evidence behind the v2 decision (#32): control round trips of a minimal `execute` → `result`
//! through every combination of framing (v1 JSON lines, v2 binary frames), transport (stdio pipes,
//! a Unix socket pair at fd 3) and engine-side receive path (a reader thread handing messages over a
//! channel, as the v1 host does, or a direct read on the calling thread), with a Rust child and a
//! Python child. Reports RTT p50/p95/p99/max, throughput, jitter, engine and child CPU, context
//! switches and the child's peak RSS.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use mvf_ipc::frame::{Header, Kind, MAX_FRAME, read_frame};
use mvf_ipc::msg::{Execute, ResultMsg};

const N: usize = 20_000;
const WARMUP: usize = 1_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Proto {
    V1,
    V2,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Transport {
    Pipes,
    Socket,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Receive {
    Thread,
    Direct,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Child {
    Rust,
    Python,
}

fn rusage_self() -> libc::rusage {
    let mut r: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: valid out-pointer.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut r) };
    r
}

fn us(t: libc::timeval) -> f64 {
    t.tv_sec as f64 * 1e6 + t.tv_usec as f64
}

const PY_V1: &str = r#"
import json, sys
print(json.dumps({"type": "hello", "protocol": 1, "moduleId": "py.echo", "capability": "processor"}), flush=True)
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("type") == "shutdown":
        break
    if msg.get("type") == "execute":
        reply = {"type": "result", "id": msg["id"], "frame": {"shm": {"offset": msg["out"]["offset"]}}}
        sys.stdout.write(json.dumps(reply) + "\n")
        sys.stdout.flush()
"#;

const PY_V2: &str = r#"
import os, struct, sys, json
HDR = struct.Struct("<2sBBHHQII")
EXE = struct.Struct("<QQQQIHH")
RES = struct.Struct("<IIQQ")
inp = os.fdopen(os.dup(0), "rb", buffering=0)
out = os.fdopen(os.dup(1), "wb", buffering=0)
os.dup2(2, 1)
def send(kind, rid, gen, body=b""):
    out.write(HDR.pack(b"MV", 2, kind, 0, 0, rid, gen, len(body)) + body)
def read_exact(n):
    buf = b""
    while len(buf) < n:
        chunk = inp.read(n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf
send(1, 0, 1, json.dumps({"protocol": 2, "moduleId": "py.echo", "capability": "processor"}).encode())
while True:
    h = read_exact(24)
    if h is None:
        break
    magic, ver, kind, flags, _, rid, gen, length = HDR.unpack(h)
    body = read_exact(length) if length else b""
    if kind == 15:
        break
    if kind == 3:
        inp_off, seq, out_off, cap, deadline, cl, tl = EXE.unpack_from(body)
        send(4, rid, gen, RES.pack(1, 0, out_off, 0))
"#;

struct Run {
    rtt_us: Vec<f64>,
    wall: Duration,
    engine_cpu_us: f64,
    engine_csw: libc::c_long,
    child_cpu_us: f64,
    child_csw: libc::c_long,
    child_rss_mib: f64,
}

// The child is reaped with `wait4` (for its own resource usage), which clippy cannot see.
#[allow(clippy::zombie_processes)]
fn run(proto: Proto, transport: Transport, receive: Receive, child: Child) -> Run {
    let mut cmd = match child {
        Child::Rust => {
            let mut c = Command::new(env!("CARGO_BIN_EXE_mvf-ipc-echo"));
            c.args(["--protocol", if proto == Proto::V1 { "1" } else { "2" }]);
            // The bench drives raw fds itself: no stdio protection.
            c.env_remove("MVF_PROTOCOL");
            if transport == Transport::Socket {
                c.args(["--transport", "fd3"]);
            }
            c
        }
        Child::Python => {
            let mut c =
                Command::new(std::env::var("MVF_PYTHON").unwrap_or_else(|_| "python3".into()));
            c.args(["-u", "-c", if proto == Proto::V1 { PY_V1 } else { PY_V2 }]);
            c
        }
    };
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut sock_parent = None;
    if transport == Transport::Socket {
        let (parent, child_end) = std::os::unix::net::UnixStream::pair().expect("pair");
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        let fd = child_end.as_raw_fd();
        // SAFETY: dup2 in the forked child before exec only.
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        sock_parent = Some((parent, child_end));
    }
    let mut proc = cmd.spawn().expect("spawn");
    let pid = proc.id() as libc::pid_t;
    let (mut writer, reader): (Box<dyn Write + Send>, Box<dyn Read + Send>) =
        match sock_parent.take() {
            Some((parent, child_end)) => {
                drop(child_end);
                let w = parent.try_clone().expect("clone");
                (Box::new(w), Box::new(parent))
            }
            None => (
                Box::new(proc.stdin.take().expect("stdin")),
                Box::new(proc.stdout.take().expect("stdout")),
            ),
        };
    // Receive path: (kind, id) per message.
    enum Rx {
        Thread(mpsc::Receiver<u64>),
        DirectV1(BufReader<Box<dyn Read + Send>>),
        DirectV2(Box<dyn Read + Send>, Vec<u8>),
    }
    let mut rx = match (receive, proto) {
        (Receive::Thread, Proto::V1) => {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(reader).lines() {
                    let Ok(line) = line else { break };
                    let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
                    if v["type"] == "result" {
                        let _ = tx.send(v["id"].as_u64().unwrap_or(0));
                    }
                }
            });
            Rx::Thread(rx)
        }
        (Receive::Thread, Proto::V2) => {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let mut r = reader;
                let mut body = Vec::new();
                while let Ok(h) = read_frame(&mut r, MAX_FRAME, &mut body) {
                    if h.kind == Kind::Result {
                        let _ = ResultMsg::decode(&body);
                        let _ = tx.send(h.id);
                    }
                }
            });
            Rx::Thread(rx)
        }
        (Receive::Direct, Proto::V1) => Rx::DirectV1(BufReader::new(reader)),
        (Receive::Direct, Proto::V2) => Rx::DirectV2(reader, Vec::new()),
    };
    // Handshake.
    let mut line = String::new();
    match &mut rx {
        Rx::DirectV1(r) => {
            r.read_line(&mut line).expect("hello");
        }
        Rx::DirectV2(r, body) => {
            read_frame(r, MAX_FRAME, body).expect("hello");
        }
        Rx::Thread(_) => {}
    }
    let exec = |id: u64| -> Vec<u8> {
        match proto {
            Proto::V1 => format!(
                "{{\"type\":\"execute\",\"id\":{id},\"frame\":{{\"cameraId\":\"cam-1\",\"sequence\":{id},\"contentType\":\"image/raw\",\"shm\":{{\"offset\":4096}}}},\"out\":{{\"offset\":8392704,\"capacity\":8388416}}}}\n"
            )
            .into_bytes(),
            Proto::V2 => {
                let body = Execute {
                    input: Some(4096),
                    sequence: id,
                    out: Some((8_392_704, 8_388_416)),
                    deadline_us: 0,
                    camera: "cam-1".into(),
                    content_type: "image/raw".into(),
                }
                .encode();
                let mut b = Header {
                    length: body.len() as u32,
                    ..Header::new(Kind::Execute, id, 1)
                }
                .encode()
                .to_vec();
                b.extend_from_slice(&body);
                b
            }
        }
    };
    let mut recv = |rx: &mut Rx| -> u64 {
        match rx {
            Rx::Thread(r) => r.recv().expect("result"),
            Rx::DirectV1(r) => loop {
                line.clear();
                r.read_line(&mut line).expect("line");
                let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
                if v["type"] == "result" {
                    break v["id"].as_u64().unwrap_or(0);
                }
            },
            Rx::DirectV2(r, body) => loop {
                let h = read_frame(r, MAX_FRAME, body).expect("frame");
                if h.kind == Kind::Result {
                    let _ = ResultMsg::decode(body);
                    break h.id;
                }
            },
        }
    };
    for id in 1..=WARMUP as u64 {
        writer.write_all(&exec(id)).expect("write");
        writer.flush().expect("flush");
        assert_eq!(recv(&mut rx), id);
    }
    let before = rusage_self();
    let t0 = Instant::now();
    let mut rtt = Vec::with_capacity(N);
    for i in 0..N as u64 {
        let id = WARMUP as u64 + 1 + i;
        let msg = exec(id);
        let t = Instant::now();
        writer.write_all(&msg).expect("write");
        writer.flush().expect("flush");
        assert_eq!(recv(&mut rx), id);
        rtt.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let wall = t0.elapsed();
    let after = rusage_self();
    let shutdown: Vec<u8> = match proto {
        Proto::V1 => b"{\"type\":\"shutdown\"}\n".to_vec(),
        Proto::V2 => Header::new(Kind::Shutdown, 0, 1).encode().to_vec(),
    };
    let _ = writer.write_all(&shutdown);
    drop(writer);
    drop(rx);
    let mut status = 0;
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: waits for our own child and fills its resource usage.
    unsafe { libc::wait4(pid, &mut status, 0, &mut ru) };
    let rss_div = if cfg!(target_os = "macos") {
        1024.0 * 1024.0
    } else {
        1024.0
    };
    rtt.sort_by(f64::total_cmp);
    Run {
        rtt_us: rtt,
        wall,
        engine_cpu_us: us(after.ru_utime) + us(after.ru_stime)
            - us(before.ru_utime)
            - us(before.ru_stime),
        engine_csw: after.ru_nvcsw + after.ru_nivcsw - before.ru_nvcsw - before.ru_nivcsw,
        child_cpu_us: us(ru.ru_utime) + us(ru.ru_stime),
        child_csw: ru.ru_nvcsw + ru.ru_nivcsw,
        child_rss_mib: ru.ru_maxrss as f64 / rss_div,
    }
}

fn pct(v: &[f64], p: f64) -> f64 {
    v[((v.len() - 1) as f64 * p).round() as usize]
}

#[test]
fn control_round_trips_v1_vs_v2() {
    if std::env::var_os("MVF_IPC_BENCH").is_none_or(|v| v != "1") {
        eprintln!("skipped: set MVF_IPC_BENCH=1 (release build) to measure");
        return;
    }
    let python = Command::new(std::env::var("MVF_PYTHON").unwrap_or_else(|_| "python3".into()))
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    let mut cases = vec![
        (
            "rust  v1 json  pipes  thread",
            Proto::V1,
            Transport::Pipes,
            Receive::Thread,
            Child::Rust,
        ),
        (
            "rust  v1 json  pipes  direct",
            Proto::V1,
            Transport::Pipes,
            Receive::Direct,
            Child::Rust,
        ),
        (
            "rust  v2 bin   pipes  thread",
            Proto::V2,
            Transport::Pipes,
            Receive::Thread,
            Child::Rust,
        ),
        (
            "rust  v2 bin   pipes  direct",
            Proto::V2,
            Transport::Pipes,
            Receive::Direct,
            Child::Rust,
        ),
        (
            "rust  v2 bin   socket direct",
            Proto::V2,
            Transport::Socket,
            Receive::Direct,
            Child::Rust,
        ),
        (
            "rust  v2 bin   socket thread",
            Proto::V2,
            Transport::Socket,
            Receive::Thread,
            Child::Rust,
        ),
    ];
    if python {
        cases.push((
            "python v1 json pipes  thread",
            Proto::V1,
            Transport::Pipes,
            Receive::Thread,
            Child::Python,
        ));
        cases.push((
            "python v2 bin  pipes  thread",
            Proto::V2,
            Transport::Pipes,
            Receive::Thread,
            Child::Python,
        ));
        cases.push((
            "python v2 bin  pipes  direct",
            Proto::V2,
            Transport::Pipes,
            Receive::Direct,
            Child::Python,
        ));
    }
    eprintln!(
        "{:<30} {:>8} {:>8} {:>8} {:>8} {:>10} {:>10} {:>10} {:>9} {:>9} {:>8}",
        "case (N=20000)",
        "p50 us",
        "p95 us",
        "p99 us",
        "max us",
        "msg/s",
        "eng cpu/rt",
        "eng csw/rt",
        "child cpu",
        "child csw",
        "rss MiB"
    );
    for (name, p, t, r, c) in cases {
        let run = run(p, t, r, c);
        let v = &run.rtt_us;
        eprintln!(
            "{:<30} {:>8.1} {:>8.1} {:>8.1} {:>8.0} {:>10.0} {:>10.2} {:>10.2} {:>8.2}u {:>9.2} {:>8.1}",
            name,
            pct(v, 0.5),
            pct(v, 0.95),
            pct(v, 0.99),
            pct(v, 1.0),
            N as f64 / run.wall.as_secs_f64(),
            run.engine_cpu_us / N as f64,
            run.engine_csw as f64 / N as f64,
            run.child_cpu_us / (N + WARMUP) as f64,
            run.child_csw as f64 / (N + WARMUP) as f64,
            run.child_rss_mib
        );
    }
}

/// The same comparison through the real engine and worker supervisor: a Rust analyzer and the
/// Python invert transformer (the SDK's own v1 and v2 paths) on small frames, where control cost
/// dominates.
#[test]
fn engine_round_trips_v1_vs_v2() {
    use mvf_engine::*;
    if std::env::var_os("MVF_IPC_BENCH").is_none_or(|v| v != "1") {
        return;
    }
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let dir = std::env::temp_dir().join(format!("mvf-ipc-engine-bench-{}", std::process::id()));
    let module = dir.join("modules/t.echo");
    std::fs::create_dir_all(&module).expect("module");
    let exe = format!("mvf-ipc-echo{}", std::env::consts::EXE_SUFFIX);
    std::fs::copy(env!("CARGO_BIN_EXE_mvf-ipc-echo"), module.join(&exe)).expect("copy");
    std::fs::write(
        module.join("node.yaml"),
        format!("apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: t.echo, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [ {{ name: frame, channel: data, type: frame }} ]\n    outputs: [ {{ name: value, channel: data, type: json }} ]\n  artifacts: [ {{ backend: worker, runtime: executable, entry: {exe}, protocol: 1 }} ]\n"),
    )
    .expect("yaml");
    let frames = 5000;
    let cases = [
        ("rust analyzer", "t.echo", "value", "json"),
        (
            "python transformer",
            "py.invert-transformer",
            "frame",
            "frame",
        ),
    ];
    eprintln!(
        "{:<20} {:>3} {:>14} {:>14} {:>12} {:>10}",
        "node (5000 frames)", "v", "node us/frame", "max us", "run frames/s", "ready ms"
    );
    for (name, module_id, out, sink_port) in cases {
        // A warm-up run first, so neither protocol pays the cold process start.
        for (round, protocol) in [(0, 1u64), (1, 1), (1, 2)] {
            let text = format!(
                "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: bench }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 8, height: 8, channels: 1, frames: {frames}, arena: true }} }}\n    - {{ id: n, module: {module_id} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: n.frame }}\n      - {{ from: n.{out}, to: sink.{sink_port} }}\n"
            );
            let resolver = HostResolver::new(&dir)
                .with_module_root(dir.join("modules"))
                .with_module_root(repo.join("modules"));
            let graph = mvf_graph::build_graph(&text, &resolver).expect("graph");
            let services = HostServices::default();
            let worker = mvf_worker::WorkerBackend::from_env().with_protocol(protocol);
            let mut engine =
                Engine::build(graph, &default_backends().with(worker), &services).expect("build");
            let report = engine.run(&RunOptions::default(), &mut NullObserver);
            assert_eq!(
                report.end,
                EndReason::SourceExhausted { node: "cam".into() },
                "{:?}",
                services.logs.events()
            );
            let node = report.nodes.iter().find(|x| x.id == "n").expect("n");
            let ready_ms = services
                .logs
                .events()
                .iter()
                .find_map(|l| {
                    l.message
                        .strip_prefix("ready pid=")
                        .and_then(|r| r.split(" in ").nth(1))
                        .and_then(|r| r.trim_end_matches(" ms").parse::<u64>().ok())
                })
                .unwrap_or(0);
            let spoke = services
                .logs
                .events()
                .iter()
                .any(|l| l.message.contains(&format!("protocol={protocol}")));
            assert!(spoke, "the child spoke protocol {protocol}");
            if round == 0 {
                continue;
            }
            eprintln!(
                "{:<20} {:>3} {:>14.1} {:>14} {:>12.0} {:>10}",
                name,
                protocol,
                node.total_us as f64 / node.executed.max(1) as f64,
                node.max_us,
                frames as f64 / (report.cycles_ms / 1000.0),
                ready_ms
            );
        }
    }
}
