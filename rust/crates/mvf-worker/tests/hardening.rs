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
