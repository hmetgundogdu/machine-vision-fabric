//! Worker hardening (#28): no orphans when the host is killed, warm spares for restarts, and
//! private arenas that keep other nodes' payloads out of a worker's reach. Real Python processes.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_worker::WorkerBackend;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn engine(text: &str, backend: WorkerBackend, services: &HostServices) -> Engine {
    let graph = build_graph(text, &HostResolver::new(fixtures())).expect("graph");
    Engine::build(graph, &default_backends().with(backend), services).expect("build")
}

fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks that the process exists and may be signalled.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: plain Win32 calls; the handle is checked and closed.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            ok != 0 && code == STILL_ACTIVE as u32
        }
    }
}

fn wait_gone(pid: u32, within: Duration) -> bool {
    let until = Instant::now() + within;
    while Instant::now() < until {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !alive(pid)
}

fn pids_from_logs(s: &HostServices, prefix: &str) -> Vec<u32> {
    s.logs
        .events()
        .into_iter()
        .filter_map(|e| {
            e.message
                .strip_prefix(prefix)
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|p| p.parse().ok())
        })
        .collect()
}

// ------------------------------------------------------------------------------------ orphans

/// Not a test on its own: the "host" that the orphan test kills. Starts a worker that never reads
/// stdin (so it would survive its host's death by itself), prints its pid, then waits to be killed.
#[test]
#[ignore = "helper process for a_killed_host_takes_its_workers_with_it"]
fn orphan_host_helper() {
    if std::env::var_os("MVF_ORPHAN_HELPER").is_none() {
        return;
    }
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: orphan }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { frames: 1 } }\n    - { id: deaf, contract: deaf.yaml }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: deaf.frame }\n      - { from: deaf.class, to: sink.class }\n";
    let s = HostServices::default();
    let mut e = engine(text, WorkerBackend::from_env(), &s);
    e.start(&CancellationToken::new()).expect("start");
    let pid = pids_from_logs(&s, "spawned pid=")
        .first()
        .copied()
        .expect("pid");
    println!("WORKER_PID={pid}");
    std::thread::sleep(Duration::from_secs(120));
}

#[test]
fn a_killed_host_takes_its_workers_with_it() {
    let mut host = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "orphan_host_helper",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("MVF_ORPHAN_HELPER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("helper");
    let stdout = host.stdout.take().expect("stdout");
    let pid = BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .find_map(|l| {
            l.split("WORKER_PID=")
                .nth(1)
                .and_then(|p| p.trim().parse::<u32>().ok())
        })
        .expect("the helper reported its worker");
    assert!(alive(pid), "worker {pid} running under the helper host");
    // SIGKILL on Unix, TerminateProcess on Windows: the host gets no chance to clean up.
    host.kill().expect("kill host");
    let _ = host.wait();
    let gone = wait_gone(pid, Duration::from_secs(5));
    if mvf_worker::spawner::KILLS_CHILDREN_WITH_HOST {
        assert!(gone, "worker {pid} outlived its killed host");
    } else {
        // macOS: no kernel mechanism; a worker that ignores stdin EOF is orphaned (documented).
        if !gone {
            // SAFETY: SIGKILL to the orphan this test created.
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        eprintln!(
            "orphan prevention is not available on this OS (worker survived: {})",
            !gone
        );
    }
}

// ------------------------------------------------------------------------------------ warm pool

fn crash_pipeline(camera: &str) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: warm }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: 8, sourceId: {camera} }} }}\n    - {{ id: pace, builtin: fault, config: {{ delayMs: 150 }} }}\n    - {{ id: w, contract: slow-crash.yaml, onError: {{ mode: restart, limit: 2, backoffMs: 1 }} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: pace.frame }}\n      - {{ from: pace.frame, to: w.frame }}\n      - {{ from: w.class, to: sink.class }}\n"
    )
}

/// Runs the crash pipeline; returns (report, services, time spent restarting in ms).
fn run_crash(spares: usize, tag: &str) -> (RunReport, HostServices) {
    let camera = format!("cam-{}-{tag}", std::process::id());
    let s = HostServices::default();
    let mut e = engine(
        &crash_pipeline(&camera),
        WorkerBackend::from_env().with_warm_spares(spares),
        &s,
    );
    let r = e.run(&RunOptions::default(), &mut NullObserver);
    drop(e);
    (r, s)
}

fn sequences(s: &HostServices) -> Vec<u64> {
    s.recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Classification(c) => c.details.and_then(|d| d.parse().ok()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_restart_takes_a_warm_spare_instead_of_paying_the_cold_start() {
    let (cold, cs) = run_crash(0, "cold");
    let (warm, ws) = run_crash(1, "warm");
    for (r, s) in [(&cold, &cs), (&warm, &ws)] {
        assert_eq!(
            r.end,
            EndReason::SourceExhausted { node: "cam".into() },
            "{:?}",
            s.logs.events()
        );
        assert_eq!(
            sequences(s),
            (1..=8).collect::<Vec<_>>(),
            "lossless: cycle 5 retried"
        );
        assert_eq!(
            r.nodes.iter().find(|n| n.id == "w").map(|n| n.restarts),
            Some(1)
        );
    }
    let took = |s: &HostServices| {
        s.logs
            .events()
            .iter()
            .filter(|e| e.message.starts_with("took warm spare"))
            .count()
    };
    assert_eq!(took(&cs), 0);
    assert_eq!(took(&ws), 1, "{:?}", ws.logs.events());
    // The cold restart pays the 400 ms warmup again; the warm one does not.
    assert!(
        warm.cycles_ms + 250.0 < cold.cycles_ms,
        "warm {:.0} ms vs cold {:.0} ms",
        warm.cycles_ms,
        cold.cycles_ms
    );
    // No spare outlives the run.
    for pid in pids_from_logs(&ws, "warmed spare pid=") {
        assert!(wait_gone(pid, Duration::from_secs(5)), "spare {pid} leaked");
    }
}

// ------------------------------------------------------------------------------------ private arenas

fn snoop_pipeline(contract: &str, secret_dir: &Path) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: snoop }}\nspec:\n  nodes:\n    - {{ id: secret, builtin: folder-source, config: {{ sourceFolder: '{}', arena: true }} }}\n    - {{ id: vault, builtin: record }}\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: 4, arena: true }} }}\n    - {{ id: spy, contract: {contract} }}\n    - {{ id: out, builtin: record }}\n  edges:\n    data:\n      - {{ from: secret.frame, to: vault.frame }}\n      - {{ from: cam.frame, to: spy.frame }}\n      - {{ from: spy.class, to: out.class }}\n",
        secret_dir.display()
    )
}

fn seen(text: &str, backend: WorkerBackend) -> Vec<f64> {
    // Small slots: the spy reads its whole mapping, which faults in every page of the arena file.
    let s = HostServices {
        arena_config: mvf_memory::ArenaConfig {
            slot_size: 64 * 1024,
            slot_count: 16,
            dir: None,
        },
        ..HostServices::default()
    };
    let r = engine(text, backend, &s).run(&RunOptions::default(), &mut NullObserver);
    assert!(
        matches!(r.end, EndReason::SourceExhausted { .. }),
        "{:?} {:?}",
        r.end,
        s.logs.events()
    );
    s.recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Classification(c) => c.measurement,
            _ => None,
        })
        .collect()
}

#[test]
fn a_private_arena_keeps_other_nodes_payloads_out_of_a_workers_reach() {
    let dir = std::env::temp_dir().join(format!("mvf-secret-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    for i in 0..4 {
        std::fs::write(
            dir.join(format!("{i}.bin")),
            b"TOPSECRET-OTHER-NODE payload of another node",
        )
        .expect("secret");
    }
    // Shared arena (the default): the other node's bytes are readable by the worker.
    let shared = seen(
        &snoop_pipeline("snoop.yaml", &dir),
        WorkerBackend::from_env(),
    );
    assert_eq!(shared.len(), 4);
    assert!(
        shared.contains(&1.0),
        "shared arena exposes other slots: {shared:?}"
    );
    // The contract asks for a private arena: nothing but its own inputs is ever in its mapping.
    let private = seen(
        &snoop_pipeline("snoop-private.yaml", &dir),
        WorkerBackend::from_env(),
    );
    assert_eq!(private, [0.0; 4]);
    // The host forces private arenas for every worker.
    let forced = seen(
        &snoop_pipeline("snoop.yaml", &dir),
        WorkerBackend::from_env().with_isolated_arenas(true),
    );
    assert_eq!(forced, [0.0; 4]);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_private_arena_still_runs_processors_and_hands_outputs_downstream() {
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: priv }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { frames: 6, arena: true } }\n    - { id: inv, module: py.invert-transformer }\n    - { id: cls, module: py.brightness-classifier }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: inv.frame }\n      - { from: inv.frame, to: cls.frame }\n      - { from: inv.frame, to: sink.frame }\n      - { from: cls.class, to: sink.class }\n";
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let run = |isolated: bool| {
        let s = HostServices::default();
        let resolver = HostResolver::new(fixtures()).with_module_root(repo.join("modules"));
        let graph = build_graph(text, &resolver).expect("graph");
        let mut e = Engine::build(
            graph,
            &default_backends().with(WorkerBackend::from_env().with_isolated_arenas(isolated)),
            &s,
        )
        .expect("build");
        let r = e.run(&RunOptions::default(), &mut NullObserver);
        assert_eq!(
            r.end,
            EndReason::SourceExhausted { node: "cam".into() },
            "{:?}",
            s.logs.events()
        );
        let mut records: Vec<_> = s
            .recorder
            .records()
            .into_iter()
            .map(|r| (r.cycle, format!("{:?}", r.value)))
            .collect();
        records.sort();
        let private_copies: u64 = s
            .logs
            .events()
            .iter()
            .filter_map(|e| e.message.strip_prefix("private arena: copiesIn="))
            .filter_map(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
            .sum();
        (
            records,
            r.data_plane.arena.map(|a| a.copies_in).unwrap_or(0),
            private_copies,
        )
    };
    let (shared, shared_copies, none) = run(false);
    let (private, _, private_copies) = run(true);
    assert_eq!(shared, private, "same results with isolated memory");
    assert_eq!(
        (shared_copies, none),
        (0, 0),
        "shared: every hand-off is by offset"
    );
    // Isolation costs one counted copy per worker input: 6 frames into `inv`, 6 outputs into `cls`.
    assert_eq!(private_copies, 12);
}

// ------------------------------------------------------------------------------------ sources

#[test]
fn a_python_source_worker_drives_the_graph_and_ends_when_exhausted() {
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: src }\nspec:\n  nodes:\n    - { id: src, contract: counting-source.yaml }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: src.frame, to: sink.frame }\n";
    let s = HostServices::default();
    let r =
        engine(text, WorkerBackend::from_env(), &s).run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(
        r.end,
        EndReason::SourceExhausted { node: "src".into() },
        "{:?}",
        s.logs.events()
    );
    assert_eq!(r.cycles_completed, 5);
    let frames: Vec<(String, u64, String)> = s
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Frame {
                source,
                sequence,
                checksum,
                ..
            } => Some((source, sequence, checksum)),
            _ => None,
        })
        .collect();
    let expected: Vec<(String, u64, String)> = (1..=5u8)
        .map(|n| {
            (
                "py-cam".to_string(),
                u64::from(n),
                format!("{:016x}", mvf_engine::fnv1a64(&[n; 16])),
            )
        })
        .collect();
    assert_eq!(frames, expected, "identity and bytes from the worker");
    let arena = r.data_plane.arena.expect("arena");
    assert_eq!(
        (arena.copies_in, r.data_plane.arena_deliveries),
        (0, 5),
        "written straight into shared memory"
    );
}

/// A minimal passive-mode FTP client (the camera simulator): logs in and STORes each file.
/// Returns the per-file round trip (PASV .. 226) in microseconds.
fn ftp_upload(addr: &str, user: &str, files: &[Vec<u8>]) -> Result<Vec<u128>, String> {
    use std::io::{BufRead, BufReader, Write};
    let control = std::net::TcpStream::connect(addr).map_err(|e| format!("connect: {e}"))?;
    control
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(control.try_clone().map_err(|e| e.to_string())?);
    let _ = control.set_nodelay(true);
    let mut w = control;
    let expect =
        |reader: &mut BufReader<std::net::TcpStream>, code: &str| -> Result<String, String> {
            let mut line = String::new();
            reader.read_line(&mut line).map_err(|e| e.to_string())?;
            if line.starts_with(code) {
                Ok(line)
            } else {
                Err(format!("expected {code}, got {line:?}"))
            }
        };
    expect(&mut reader, "220")?;
    for (cmd, code) in [
        (format!("USER {user}"), "331"),
        ("PASS secret".into(), "230"),
        ("TYPE I".into(), "200"),
    ] {
        w.write_all(format!("{cmd}\r\n").as_bytes())
            .map_err(|e| e.to_string())?;
        expect(&mut reader, code)?;
    }
    let mut timings = Vec::new();
    for (i, bytes) in files.iter().enumerate() {
        let t0 = Instant::now();
        w.write_all(b"PASV\r\n").map_err(|e| e.to_string())?;
        let line = expect(&mut reader, "227")?;
        let inner = line
            .split('(')
            .nth(1)
            .and_then(|r| r.split(')').next())
            .ok_or("bad 227")?;
        let n: Vec<u16> = inner
            .split(',')
            .filter_map(|x| x.trim().parse().ok())
            .collect();
        let [a, b, c, d, p1, p2] = n.as_slice() else {
            return Err(format!("bad 227: {line}"));
        };
        let mut data = std::net::TcpStream::connect(format!("{a}.{b}.{c}.{d}:{}", p1 * 256 + p2))
            .map_err(|e| format!("data: {e}"))?;
        w.write_all(format!("STOR image-{i}.bin\r\n").as_bytes())
            .map_err(|e| e.to_string())?;
        expect(&mut reader, "150")?;
        data.write_all(bytes).map_err(|e| e.to_string())?;
        drop(data);
        expect(&mut reader, "226")?;
        timings.push(t0.elapsed().as_micros());
    }
    let _ = w.write_all(b"QUIT\r\n");
    Ok(timings)
}

#[test]
fn an_ftp_source_turns_each_uploaded_image_into_a_typed_frame() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: ftp }\nspec:\n  nodes:\n    - { id: ftp, module: py.ftp-source, config: { port: 2181, passiveRange: 50180-50189, maxFiles: 24 } }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: ftp.frame, to: sink.frame }\n";
    let resolver = HostResolver::new(fixtures()).with_module_root(repo.join("modules"));
    let graph = build_graph(text, &resolver).expect("graph");
    let s = HostServices::default();
    let mut e = Engine::build(
        graph,
        &default_backends().with(WorkerBackend::from_env()),
        &s,
    )
    .expect("build");
    // Four small checked files, then a measured batch of 20 x 256 KiB in the same session.
    let files: Vec<Vec<u8>> = (0..4u8)
        .map(|i| vec![i.wrapping_mul(37); 4096 + usize::from(i)])
        .chain((0..20u8).map(|i| vec![i; 256 * 1024]))
        .collect();
    let uploads = files.clone();
    let camera = std::thread::spawn(move || {
        for _ in 0..80 {
            match ftp_upload("127.0.0.1:2181", "cognex-1", &uploads) {
                Ok(t) => return Ok(t),
                Err(e) if e.starts_with("connect") => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                Err(e) => return Err(e),
            }
        }
        Err("server never came up".to_string())
    });
    let r = e.run(&RunOptions::default(), &mut NullObserver);
    let timings = camera.join().expect("camera").expect("uploads");
    assert_eq!(
        r.end,
        EndReason::SourceExhausted { node: "ftp".into() },
        "{:?}",
        s.logs.events()
    );
    let frames: Vec<(String, u64, usize, String)> = s
        .recorder
        .records()
        .into_iter()
        .filter_map(|r| match r.value {
            RecordedValue::Frame {
                source,
                sequence,
                bytes,
                checksum,
            } => Some((source, sequence, bytes, checksum)),
            _ => None,
        })
        .collect();
    let expected: Vec<(String, u64, usize, String)> = files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            (
                "cognex-1".to_string(),
                i as u64 + 1,
                f.len(),
                format!("{:016x}", mvf_engine::fnv1a64(f)),
            )
        })
        .collect();
    assert_eq!(
        frames, expected,
        "every upload, in order, byte for byte; camera = FTP user"
    );
    assert_eq!(timings.len(), 24);
    let mut measured: Vec<u128> = timings[4..].to_vec();
    measured.sort_unstable();
    let pct = |p: f64| measured[((measured.len() - 1) as f64 * p).round() as usize] as f64 / 1000.0;
    let total: u128 = measured.iter().sum();
    let usage = s
        .logs
        .events()
        .iter()
        .find(|l| l.message.starts_with("ftp: done"))
        .map(|l| l.message.clone())
        .unwrap_or_default();
    eprintln!(
        "FTP native worker (host network, loopback): 20 x 256 KiB, upload round trip ms p50={:.2} p95={:.2} p99={:.2} max={:.2}, {:.1} MiB/s; {usage}",
        pct(0.5),
        pct(0.95),
        pct(0.99),
        pct(1.0),
        5.0 / (total as f64 / 1e6)
    );
    let network_note = s
        .logs
        .events()
        .iter()
        .any(|e| e.message.starts_with("network: shares the host network"));
    assert!(
        network_note,
        "a native worker says it is not network-isolated"
    );
}
