//! End-to-end CLI tests: `validate` and `run` on YAML pipelines with exact outputs and cycle ids.

use std::path::PathBuf;
use std::process::Command;

fn examples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn mvf() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mvf-rs"))
}

#[test]
fn validate_accepts_the_examples() {
    for example in ["basic", "gated", "python-worker"] {
        let out = mvf()
            .arg("validate")
            .arg(examples().join(example).join("pipeline.yaml"))
            .output()
            .expect("spawn");
        assert!(
            out.status.success(),
            "{example}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("valid"));
    }
}

#[test]
fn run_json_emits_ordered_records_with_cycle_ids_then_a_report() {
    let out = mvf()
        .args(["run", "--json"])
        .arg(examples().join("basic/pipeline.yaml"))
        .output()
        .expect("spawn");
    assert!(out.status.success());
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).expect("json line"))
        .collect();
    assert_eq!(lines.len(), 7, "5 records + 1 report + 1 metrics");
    assert!(lines[6].get("metrics").is_some());
    let expected_checksums = [
        "5da82418a8f85b75",
        "f19c29e807fbf7f5",
        "360f29bd75a304bd",
        "19205d66d9952905",
        "5f55c998fceda355",
    ];
    for (i, line) in lines[..5].iter().enumerate() {
        let r = &line["record"];
        assert_eq!(r["cycle"], i as u64 + 1);
        assert_eq!(r["value"]["sequence"], i as u64 + 1);
        assert_eq!(r["value"]["checksum"], expected_checksums[i]);
        assert_eq!(r["node"], "sink");
    }
    let report = &lines[5]["report"];
    assert_eq!(report["end"]["reason"], "cycle-limit");
    assert_eq!(report["cyclesCompleted"], 5);
}

#[test]
fn max_cycles_flag_bounds_the_run() {
    let out = mvf()
        .args(["run", "--json", "--max-cycles", "2"])
        .arg(examples().join("basic/pipeline.yaml"))
        .output()
        .expect("spawn");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).lines().count(),
        4,
        "2 records + report + metrics"
    );
}

#[test]
fn invalid_pipeline_exits_with_code_2_and_names_the_problem() {
    let dir = std::env::temp_dir().join(format!("mvf-cli-invalid-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let file = dir.join("pipeline.yaml");
    std::fs::write(
        &file,
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: bad }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source }\n    - { id: gate, builtin: brightness-gate }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: gate.frame }\n      - { from: gate.class, to: sink.frame }\n",
    )
    .expect("write");
    let out = mvf().arg("validate").arg(&file).output().expect("spawn");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("graph.edge.type-mismatch"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn node_failure_exits_with_code_1() {
    let dir = std::env::temp_dir().join(format!("mvf-cli-fail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let file = dir.join("pipeline.yaml");
    std::fs::write(
        &file,
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: f }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source }\n    - { id: f, builtin: fault, config: { failAtCycle: 2 } }\n  edges:\n    data:\n      - { from: cam.frame, to: f.frame }\n  run: { maxCycles: 5 }\n",
    )
    .expect("write");
    let out = mvf()
        .args(["run", "--json"])
        .arg(&file)
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(1));
    let report: serde_json::Value = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v.get("report").is_some())
        .expect("report");
    assert_eq!(report["report"]["end"]["reason"], "node-failed");
    assert_eq!(report["report"]["end"]["cycle"], 2);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn gate_a_demo_verifies_ordered_output_replacement_and_rollback() {
    let out = mvf()
        .args([
            "run",
            "--json",
            "--verify",
            "--expect-promotions",
            "2",
            "--expect-rollbacks",
            "1",
        ])
        .arg(examples().join("live-update/pipeline.yaml"))
        .arg("--updates")
        .arg(examples().join("live-update/updates.yaml"))
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "exit {:?}\n{}\n{}",
        out.status.code(),
        stdout.lines().last().unwrap_or_default(),
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect("json line"))
        .collect();
    let verify = &lines.last().expect("verify")["verify"];
    assert_eq!(verify["pass"], true, "{verify}");
    let report = &lines
        .iter()
        .find(|l| l.get("report").is_some())
        .expect("report")["report"];
    let kinds: Vec<&str> = report["replacements"]
        .as_array()
        .expect("events")
        .iter()
        .map(|e| e["kind"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        kinds,
        [
            "requested",
            "promoted",
            "committed",
            "requested",
            "promoted",
            "rolled-back"
        ]
    );
    let rolled = &report["replacements"][5];
    assert_eq!(rolled["cycle"], 280);
    assert_eq!(
        rolled["toGeneration"], 2,
        "rolled back to the Python worker generation"
    );
    assert_eq!(report["dataPlane"]["arena"]["copiesIn"], 0);
    assert_eq!(report["dataPlane"]["arena"]["copiesOut"], 0);
    assert_eq!(report["cyclesCompleted"], 400);
}

#[test]
fn verify_fails_with_exit_code_3_when_expectations_are_not_met() {
    let out = mvf()
        .args(["run", "--json", "--verify", "--expect-rollbacks", "1"])
        .arg(examples().join("basic/pipeline.yaml"))
        .output()
        .expect("spawn");
    assert_eq!(out.status.code(), Some(3));
    let last: serde_json::Value = serde_json::from_str(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .last()
            .expect("verify"),
    )
    .expect("json");
    assert_eq!(last["verify"]["pass"], false);
}

#[test]
fn killed_engine_resumes_from_its_durable_checkpoint() {
    use std::io::{BufRead, BufReader};
    let dir = std::env::temp_dir().join(format!("mvf-cli-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let pipeline = dir.join("pipeline.yaml");
    std::fs::write(
        &pipeline,
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: crash }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { frames: 120, intervalMs: 5 } }\n    - { id: count, builtin: counter }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: count.frame }\n      - { from: count.count, to: sink.json }\n      - { from: cam.frame, to: sink.frame }\n",
    )
    .expect("pipeline");
    let ckpt = dir.join("ckpt");
    let args = |cmd: &mut Command| {
        cmd.args([
            "run",
            "--json",
            "--checkpoint-every",
            "10",
            "--checkpoint-dir",
        ])
        .arg(&ckpt)
        .arg(&pipeline);
    };

    // Run 1: SIGKILL (TerminateProcess on Windows) once cycle 35 has been recorded — no shutdown.
    let mut first = mvf();
    args(&mut first);
    let mut child = first
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    let stdout = child.stdout.take().expect("stdout");
    for line in BufReader::new(stdout).lines() {
        let v: serde_json::Value = serde_json::from_str(&line.expect("line")).expect("json");
        if v["record"]["cycle"].as_u64().is_some_and(|c| c >= 35) {
            break;
        }
    }
    child.kill().expect("kill");
    let _ = child.wait();
    let epoch: u64 = std::fs::read_to_string(ckpt.join("CURRENT"))
        .expect("a published epoch survived the kill")
        .trim()
        .parse()
        .expect("number");
    assert!(epoch >= 30 && epoch.is_multiple_of(10), "epoch {epoch}");

    // Run 2: resumes right after the published epoch and runs to the end.
    let mut second = mvf();
    args(&mut second);
    let out = second.output().expect("run 2");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).expect("json"))
        .collect();
    let counts: Vec<(u64, u64)> = lines
        .iter()
        .filter(|l| l["record"]["port"] == "json")
        .map(|l| {
            (
                l["record"]["cycle"].as_u64().unwrap_or(0),
                l["record"]["value"]["value"]["count"].as_u64().unwrap_or(0),
            )
        })
        .collect();
    assert_eq!(
        counts,
        ((epoch + 1)..=120).map(|c| (c, c)).collect::<Vec<_>>()
    );
    let report = lines
        .iter()
        .find(|l| l.get("report").is_some())
        .expect("report");
    assert_eq!(report["report"]["resumedFromCycle"], epoch);
    assert_eq!(report["report"]["end"]["reason"], "source-exhausted");
    assert!(!ckpt.exists(), "cleared after the fully consumed run");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn volumes_survive_runs_and_go_only_on_an_explicit_delete() {
    let dir = std::env::temp_dir().join(format!("mvf-cli-volumes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("modules/t.writer")).expect("dir");
    std::fs::write(
        dir.join("modules/t.writer/node.py"),
        "import os\nfrom mvf_sdk import analysis, run_analyzer\nfrom mvf_sdk.storage import volume, write_atomic\ndef analyze(p, m):\n    write_atomic(volume('out') / 'r-{}-{}.txt'.format(os.getpid(), m['sequence']), b'ok')\n    return analysis(value={})\nrun_analyzer('t.writer', analyze)\n",
    )
    .expect("py");
    std::fs::write(
        dir.join("modules/t.writer/node.yaml"),
        "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: { id: t.writer, version: 1.0.0 }\nspec:\n  ports:\n    inputs: [ { name: frame, channel: data, type: frame } ]\n    outputs: [ { name: out, channel: data, type: json } ]\n  artifacts: [ { backend: worker, runtime: python, entry: node.py, protocol: 1 } ]\n  storage:\n    mounts:\n      - { name: out, target: /out, access: read-write }\n",
    )
    .expect("yaml");
    std::fs::write(
        dir.join("pipeline.yaml"),
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: cli-plant }\nspec:\n  volumes:\n    - { name: results, type: persistent }\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { frames: 5 } }\n    - { id: w, module: t.writer, storage: { out: results } }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: w.frame }\n      - { from: w.out, to: sink.json }\n",
    )
    .expect("pipeline");
    let storage = dir.join("storage");
    let sdk = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../src/sdk/python");
    for _ in 0..2 {
        let out = mvf()
            .args(["run"])
            .arg(dir.join("pipeline.yaml"))
            .arg("--modules")
            .arg(dir.join("modules"))
            .env("MVF_STORAGE_DIR", &storage)
            .env("MVF_PYTHON_SDK", &sdk)
            .output()
            .expect("run");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let volume = |args: &[&str]| {
        mvf()
            .arg("volume")
            .arg("--storage")
            .arg(&storage)
            .args(args)
            .output()
            .expect("volume")
    };
    let list = volume(&["list", "--json"]);
    let listed: serde_json::Value = serde_json::from_slice(&list.stdout).expect("json");
    assert_eq!(listed[0]["scope"], "cli-plant");
    assert_eq!(listed[0]["name"], "results");
    assert_eq!(
        listed[0]["files"], 10,
        "two runs, five records each, all retained"
    );
    assert!(volume(&["inspect", "cli-plant/results"]).status.success());
    let refused = volume(&["delete", "cli-plant/results"]);
    assert!(!refused.status.success(), "deleting data needs --yes");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--yes"));
    assert!(
        volume(&["delete", "cli-plant/results", "--yes"])
            .status
            .success()
    );
    let after: serde_json::Value =
        serde_json::from_slice(&volume(&["list", "--json"]).stdout).expect("json");
    assert_eq!(after, serde_json::json!([]));
}

/// `run --manage` registers a local endpoint; `manage list`, `status` and `watch` find and follow
/// it (#33 part 1).
#[test]
fn a_managed_run_is_listed_watched_and_reports_its_final_status() {
    let dir = std::env::temp_dir().join(format!("mvf-cli-manage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let pipeline = dir.join("pipeline.yaml");
    std::fs::write(
        &pipeline,
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: watched }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 8, height: 8, frames: 400, intervalMs: 5 } }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: sink.frame }\n",
    )
    .expect("pipeline");
    let runtime = dir.join("run");
    let mut run = mvf()
        .args(["run", "--manage"])
        .arg(&pipeline)
        .env("MVF_RUNTIME_DIR", &runtime)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run");
    // Wait for the registration.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let listed = loop {
        let out = mvf()
            .args(["manage", "list", "--json"])
            .env("MVF_RUNTIME_DIR", &runtime)
            .output()
            .expect("list");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
        if v.as_array().is_some_and(|a| !a.is_empty()) {
            break v;
        }
        assert!(std::time::Instant::now() < deadline, "never registered");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(listed[0]["pipeline"], "watched");
    assert_eq!(listed[0]["pid"], run.id());
    let watch = mvf()
        .args(["manage", "watch", "--until-stopped", "--json"])
        .env("MVF_RUNTIME_DIR", &runtime)
        .output()
        .expect("watch");
    assert!(
        watch.status.success(),
        "{}",
        String::from_utf8_lossy(&watch.stderr)
    );
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&watch.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    assert_eq!(lines[0]["op"], "snapshot");
    assert_eq!(lines[0]["snapshot"]["topology"]["pipeline"], "watched");
    assert!(lines.iter().any(|l| l["entry"]["kind"] == "delta"));
    let last = lines.last().expect("last");
    assert_eq!(last["entry"]["status"], "stopped");
    assert_eq!(last["entry"]["end"]["reason"], "source-exhausted");
    let status = run.wait().expect("wait");
    assert!(status.success());
    // The endpoint and its registration are gone with the run.
    let out = mvf()
        .args(["manage", "list", "--json"])
        .env("MVF_RUNTIME_DIR", &runtime)
        .output()
        .expect("list");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "[]");
    let out = mvf()
        .args(["manage", "status"])
        .env("MVF_RUNTIME_DIR", &runtime)
        .output()
        .expect("status");
    assert!(!out.status.success(), "no runtime left to report on");
}

/// `mvf-rs manage command stop` stops a managed run at the next boundary, and the command is
/// audited (#33 part 2).
#[test]
fn a_managed_run_stops_on_a_command_and_audits_it() {
    let dir = std::env::temp_dir().join(format!("mvf-cli-command-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let pipeline = dir.join("pipeline.yaml");
    std::fs::write(
        &pipeline,
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: stoppable }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 8, height: 8, frames: 1000000, intervalMs: 5 } }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: sink.frame }\n",
    )
    .expect("pipeline");
    let runtime = dir.join("run");
    let audit = dir.join("audit.jsonl");
    let mut run = mvf()
        .args(["run", "--manage", "--audit"])
        .arg(&audit)
        .arg(&pipeline)
        .env("MVF_RUNTIME_DIR", &runtime)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let stop = loop {
        let out = mvf()
            .args(["manage", "command", "stop", "--id", "cli-stop-1"])
            .env("MVF_RUNTIME_DIR", &runtime)
            .output()
            .expect("command");
        if out.status.success() {
            break out;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let reply: serde_json::Value = serde_json::from_slice(&stop.stdout).expect("reply");
    assert_eq!(reply["status"], "applied", "{reply}");
    assert!(
        run.wait().expect("wait").success(),
        "a commanded stop is a clean end"
    );
    let records: Vec<serde_json::Value> = std::fs::read_to_string(&audit)
        .expect("audit")
        .lines()
        .map(|l| serde_json::from_str(l).expect("json"))
        .collect();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["command"], "stop");
    assert_eq!(records[0]["principal"], "local");
    assert_eq!(records[0]["status"], "applied");
}

/// `run --manage --preview cam --allow-uploads`: `manage preview` saves PNGs, `manage upload`
/// stages a package, `manage command replace` activates it (#33 part 3).
#[test]
fn a_managed_run_serves_previews_and_activates_an_uploaded_package() {
    let dir = std::env::temp_dir().join(format!("mvf-cli-part3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("pkg")).expect("dir");
    let pipeline = dir.join("pipeline.yaml");
    std::fs::write(
        &pipeline,
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: part3 }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 320, height: 240, channels: 3, frames: 1000000, intervalMs: 5 } }\n    - { id: pass, builtin: passthrough }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: pass.frame }\n      - { from: pass.frame, to: sink.frame }\n",
    )
    .expect("pipeline");
    let contract = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../mvf-engine/src/contracts/passthrough.yaml"),
    )
    .expect("contract")
    .replace("version: 1.0.0", "version: 2.0.0");
    std::fs::write(dir.join("pkg/node.yaml"), contract).expect("node.yaml");
    std::fs::write(dir.join("pkg/weights.bin"), vec![7u8; 700_000]).expect("weights");
    let runtime = dir.join("run");
    let mut run = mvf()
        .args(["run", "--manage", "--preview", "cam", "--allow-uploads"])
        .arg(&pipeline)
        .env("MVF_RUNTIME_DIR", &runtime)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run");
    let manage = |args: &[&str]| {
        mvf()
            .arg("manage")
            .args(args)
            .env("MVF_RUNTIME_DIR", &runtime)
            .output()
            .expect("manage")
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let shots = dir.join("shots");
    std::fs::create_dir_all(&shots).expect("shots");
    loop {
        let out = manage(&[
            "preview",
            "--node",
            "cam",
            "--count",
            "2",
            "--out",
            shots.to_str().expect("path"),
        ]);
        if out.status.success() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let pngs: Vec<_> = std::fs::read_dir(&shots)
        .expect("shots")
        .flatten()
        .collect();
    assert_eq!(pngs.len(), 2);
    for p in &pngs {
        assert_eq!(
            &std::fs::read(p.path()).expect("png")[..8],
            b"\x89PNG\r\n\x1a\n"
        );
    }
    let not_enabled = manage(&["preview", "--node", "pass"]);
    assert!(!not_enabled.status.success());
    let up = manage(&[
        "upload",
        "--package",
        "pass-v2",
        dir.join("pkg").to_str().expect("path"),
    ]);
    assert!(
        up.status.success(),
        "{}{}",
        String::from_utf8_lossy(&up.stdout),
        String::from_utf8_lossy(&up.stderr)
    );
    let staged: serde_json::Value = serde_json::from_slice(&up.stdout).expect("staged");
    let contract = staged["staged"]["contract"]
        .as_str()
        .expect("contract")
        .to_string();
    let replace = manage(&[
        "command",
        "replace",
        "--node",
        "pass",
        "--contract",
        &contract,
        "--id",
        "p3-replace",
    ]);
    assert!(
        replace.status.success(),
        "{}",
        String::from_utf8_lossy(&replace.stdout)
    );
    let r: serde_json::Value = serde_json::from_slice(&replace.stdout).expect("reply");
    assert_eq!(r["event"]["toVersion"], "mvf.passthrough@2.0.0");
    assert!(
        manage(&["command", "stop", "--id", "p3-stop"])
            .status
            .success()
    );
    assert!(run.wait().expect("wait").success());
    assert_eq!(
        std::fs::read(dir.join(".mvf/staged/pass-v2/weights.bin"))
            .expect("staged")
            .len(),
        700_000
    );
}
