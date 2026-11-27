//! End-to-end CLI tests: `validate` and `run` on YAML pipelines with exact outputs and cycle ids.

use std::path::PathBuf;
use std::process::Command;

fn examples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn mvf() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mvf"))
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

/// `mvf manage command stop` stops a managed run at the next boundary, and the command is
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

/// `--trust-signer`: an unsigned package is refused, a signed one stages; the flag needs
/// `--allow-uploads` (#33).
#[test]
fn a_runtime_that_trusts_a_signer_stages_only_its_packages() {
    use ed25519_dalek::{Signer, SigningKey};
    let dir = std::env::temp_dir().join(format!("mvf-cli-signed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("pkg")).expect("dir");
    let pipeline = dir.join("pipeline.yaml");
    std::fs::write(
        &pipeline,
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: signed }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 8, height: 8, frames: 1000000, intervalMs: 5 } }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: sink.frame }\n",
    )
    .expect("pipeline");
    std::fs::write(dir.join("pkg/node.yaml"), "x").expect("file");
    let key = SigningKey::from_bytes(&[9; 32]);
    let signer = mvf_management_key(key.verifying_key().as_bytes());
    std::fs::write(dir.join("signer.pub"), format!("{signer}\n")).expect("pub");
    // The flag alone is refused.
    let out = mvf()
        .args(["run", "--manage", "--trust-signer"])
        .arg(dir.join("signer.pub"))
        .arg(&pipeline)
        .output()
        .expect("run");
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runtime = dir.join("run");
    let mut run = mvf()
        .args(["run", "--manage", "--allow-uploads", "--trust-signer"])
        .arg(dir.join("signer.pub"))
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
    let pkg = dir.join("pkg");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let unsigned = loop {
        let out = manage(&[
            "upload",
            "--package",
            "p-unsigned",
            pkg.to_str().expect("path"),
        ]);
        if !out.stdout.is_empty() {
            break out;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let r: serde_json::Value = serde_json::from_slice(&unsigned.stdout).expect("reply");
    assert_eq!(
        (unsigned.status.success(), r["code"].as_str()),
        (false, Some("unsigned")),
        "{r}"
    );
    // Sign exactly what will be uploaded: the package name and every file's digest.
    // The package's only file is `node.yaml` = "x": its SHA-256.
    let digest = "sha256:2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881";
    let message = format!("mvf-package-signature-v1\npackage p-signed\nfile node.yaml {digest}\n");
    let sig = key.sign(message.as_bytes());
    let b64 = {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes = sig.to_bytes();
        let mut s = String::new();
        for c in bytes.chunks(3) {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            s.push(A[(n >> 18) as usize & 63] as char);
            s.push(A[(n >> 12) as usize & 63] as char);
            s.push(if c.len() > 1 {
                A[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            s.push(if c.len() > 2 {
                A[n as usize & 63] as char
            } else {
                '='
            });
        }
        s
    };
    std::fs::write(
        dir.join("p.sig.json"),
        serde_json::json!({"key": signer, "sig": b64}).to_string(),
    )
    .expect("sig");
    let signed = manage(&[
        "upload",
        "--package",
        "p-signed",
        "--signature",
        dir.join("p.sig.json").to_str().expect("path"),
        pkg.to_str().expect("path"),
    ]);
    let r: serde_json::Value = serde_json::from_slice(&signed.stdout).expect("reply");
    assert!(signed.status.success(), "{r}");
    assert_eq!(r["staged"]["signedBy"], signer.as_str());
    assert!(
        manage(&["command", "stop", "--id", "signed-stop"])
            .status
            .success()
    );
    assert!(run.wait().expect("wait").success());
}

fn mvf_management_key(public: &[u8]) -> String {
    let hex: String = public.iter().map(|b| format!("{b:02x}")).collect();
    format!("ed25519:{hex}")
}

#[test]
fn a_pipelined_managed_run_serves_previews_of_a_mid_graph_node() {
    let dir = std::env::temp_dir().join(format!("mvf-cli-pipelined-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let pipeline = dir.join("pipeline.yaml");
    std::fs::write(
        &pipeline,
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: pipelined }\nspec:\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 320, height: 240, channels: 3, frames: 1000000, intervalMs: 5 } }\n    - { id: pass, builtin: passthrough }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: pass.frame }\n      - { from: pass.frame, to: sink.frame }\n",
    )
    .expect("pipeline");
    let runtime = dir.join("run");
    let mut run = mvf()
        .args([
            "run",
            "--mode",
            "pipelined",
            "--manage",
            "--preview",
            "pass",
        ])
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
            "pass",
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
    let status = manage(&["status", "--json"]);
    let s: serde_json::Value = serde_json::from_slice(&status.stdout).expect("status");
    assert_eq!(s["runtime"]["mode"], "pipelined", "{s}");
    assert!(
        manage(&["command", "stop", "--id", "pipelined-stop"])
            .status
            .success()
    );
    assert!(run.wait().expect("wait").success());
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).expect("dir");
    for entry in std::fs::read_dir(from).expect("read").flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            if entry.file_name() != "out" && entry.file_name() != ".mvf" {
                copy_dir(&entry.path(), &target);
            }
        } else {
            std::fs::copy(entry.path(), &target).expect("copy");
        }
    }
}

/// `mvf import` (#34): a legacy .NET package becomes a valid Rust pipeline next to it, the
/// converted pipeline runs, and what cannot be carried over is refused or named.
#[test]
fn import_converts_a_legacy_package_and_names_what_it_cannot_carry_over() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let modules = repo.join("modules");
    let root = std::env::temp_dir().join(format!("mvf-cli-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for demo in [
        "inspection-demo",
        "loop-demo",
        "multilang-demo",
        "py-invert-demo",
    ] {
        copy_dir(
            &repo.join("packages").join(demo),
            &root.join("packages").join(demo),
        );
    }
    let import = |args: &[&std::ffi::OsStr]| {
        mvf()
            .arg("import")
            .args(args)
            .arg("--modules")
            .arg(&modules)
            .output()
            .expect("import")
    };
    let text = |o: &std::process::Output| {
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    };

    // A 13-node package: ids renamed, modules mapped to builtins, Python modules kept, valid.
    let inspection = root.join("packages/inspection-demo");
    let out = import(&[inspection.as_os_str()]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    for line in [
        "id      saveInverted -> save-inverted",
        "builtin cam: mvf.folder-source -> folder-source",
        "13 nodes, 12 data edges, 0 control edges — valid",
        "py.brightness-classifier@1.0.0",
    ] {
        assert!(said.contains(line), "missing `{line}` in:\n{said}");
    }
    let yaml = std::fs::read_to_string(inspection.join("pipeline.yaml")).expect("pipeline.yaml");
    assert!(
        yaml.contains("onError: { sources: \"fail\", nodes: \"skip\" }"),
        "{yaml}"
    );
    assert!(
        yaml.contains("# save-inverted: "),
        "display names kept as comments"
    );
    // Not overwritten without --force.
    let again = import(&[inspection.as_os_str()]);
    assert_eq!(again.status.code(), Some(2), "{}", text(&again));
    assert!(text(&again).contains("--force"));

    // The converted loop demo runs (folder source rewound by the loop, frames written).
    let looped = root.join("packages/loop-demo");
    assert!(import(&[looped.as_os_str()]).status.success());
    let run = mvf()
        .args(["run", "--max-cycles", "6", "--json"])
        .arg(looped.join("pipeline.yaml"))
        .arg("--modules")
        .arg(&modules)
        .env("MVF_BINDING_CAM_INTERVAL", "0")
        .output()
        .expect("run");
    assert!(run.status.success(), "{}", text(&run));
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("\"cycle-limit\""),
        "{}",
        text(&run)
    );
    assert!(
        looped.join("out").is_dir(),
        "the dataset writer wrote frames"
    );

    // A .NET in-process camera has no contract here: written, named, exit 1.
    let cognex = root.join("packages/py-invert-demo");
    let out = import(&[cognex.as_os_str()]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("MISSING camera1: module `mvf.realworld-cognex-camera`"),
        "{}",
        text(&out)
    );

    // Refused outright, nothing written.
    for (doc, expect) in [
        (
            r#"{"name":"x","nodes":[{"id":"a","module":"mvf.folder-source","lifecycle":"resident"}],"edges":[]}"#,
            "field `lifecycle`",
        ),
        (
            r#"{"name":"x","nodes":[{"id":"a","module":"mvf.folder-source"},{"id":"b","module":"mvf.dataset-writer"}],"edges":[{"from":"a","to":"b"}]}"#,
            "must close into a `loop`",
        ),
        (
            r#"{"name":"x","nodes":[{"id":"saveOk","module":"mvf.dataset-writer"},{"id":"save-ok","module":"mvf.dataset-writer"}],"edges":[]}"#,
            "already uses",
        ),
        (
            r#"{"name":"x","nodes":[{"id":"a","primitive":"while"}],"edges":[]}"#,
            "unknown primitive",
        ),
    ] {
        let dir = root.join("bad");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("pipeline.json"), doc).expect("json");
        let out = import(&[dir.as_os_str()]);
        assert_eq!(out.status.code(), Some(2), "{}", text(&out));
        assert!(
            text(&out).contains(expect),
            "missing `{expect}` in {}",
            text(&out)
        );
        assert!(!dir.join("pipeline.yaml").exists());
    }
}

const TUI_PIPELINE: &str = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: tui-smoke }\nspec:\n  nodes:\n    - { id: cycle, primitive: loop, config: { mode: forever } }\n    - { id: cam, builtin: synthetic-source, config: { width: 8, height: 8, frames: 50, intervalMs: 5 } }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: sink.frame }\n    loop:\n      - { from: sink, to: cycle }\n";

/// `mvf manage tui` and `mvf run --tui` without a terminal (#35): refused, never a broken screen.
#[test]
fn the_tui_refuses_a_terminal_that_is_not_interactive() {
    let dir = std::env::temp_dir().join(format!("mvf-cli-tui-nontty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("p.yaml"), TUI_PIPELINE).expect("pipeline");
    for args in [vec!["manage", "tui"], vec!["run", "--tui", "p.yaml"]] {
        let out = mvf()
            .args(&args)
            .current_dir(&dir)
            .env("MVF_RUNTIME_DIR", dir.join("run"))
            .stdin(std::process::Stdio::null())
            .output()
            .expect("mvf");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("interactive terminal") && err.contains("manage watch"),
            "{err}"
        );
    }
}

/// Runs `command` in a pseudo-terminal (120x36) through `script`, typing `keys` (after a delay
/// in ms each), and returns the exit status and everything the terminal received.
#[cfg(unix)]
fn in_terminal(
    command: &str,
    keys: &[(u64, &str)],
    runtime: &std::path::Path,
    out: &std::path::Path,
) -> Option<(std::process::ExitStatus, Vec<u8>)> {
    use std::io::Write;
    let shell = format!("stty cols 120 rows 36; exec {command}");
    let mut c = Command::new("script");
    if cfg!(target_os = "macos") {
        c.arg("-q").arg(out).args(["sh", "-c", &shell]);
    } else {
        c.args(["-q", "-e", "-c", &shell]).arg(out);
    }
    let mut child = c
        .env("MVF_RUNTIME_DIR", runtime)
        .env("TERM", "xterm-256color")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    for (ms, k) in keys {
        std::thread::sleep(std::time::Duration::from_millis(*ms));
        stdin.write_all(k.as_bytes()).ok()?;
        stdin.flush().ok()?;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        if let Some(s) = child.try_wait().ok()? {
            break s;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("`{command}` did not exit in the terminal");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    drop(stdin);
    Some((status, std::fs::read(out).ok()?))
}

/// The TUI in a real (pseudo-)terminal (#35): it draws the graph, help and node details, leaves
/// the terminal as it found it, and detaching never touches the runtime. `run --tui`: `q` stops the
/// run, and the report follows the restored screen.
#[cfg(unix)]
#[test]
fn the_tui_draws_in_a_real_terminal_and_detaches_without_touching_the_runtime() {
    if Command::new("script").arg("-V").output().is_err()
        && Command::new("which")
            .arg("script")
            .output()
            .map_or(true, |o| !o.status.success())
    {
        eprintln!("skipped: no `script` to make a pseudo-terminal");
        return;
    }
    let dir = std::env::temp_dir().join(format!("mvf-cli-tui-pty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("p.yaml"), TUI_PIPELINE).expect("pipeline");
    let runtime = dir.join("run");
    let mvf_path = env!("CARGO_BIN_EXE_mvf");
    let mut run = mvf()
        .args(["run", "--manage", "p.yaml"])
        .current_dir(&dir)
        .env("MVF_RUNTIME_DIR", &runtime)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run");
    let status = |runtime: &std::path::Path| -> Option<serde_json::Value> {
        let out = mvf()
            .args(["manage", "status", "--json"])
            .env("MVF_RUNTIME_DIR", runtime)
            .output()
            .ok()?;
        serde_json::from_slice(&out.stdout).ok()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while status(&runtime).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the runtime never came up"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // help, close it, next node, details, back, quit
    let keys = [
        (1500, "?"),
        (400, "x"),
        (300, "\x1b[C"),
        (300, "\r"),
        (600, "\x1b"),
        (400, "q"),
    ];
    let (exit, screen) = in_terminal(
        &format!("'{mvf_path}' manage tui"),
        &keys,
        &runtime,
        &dir.join("tui.out"),
    )
    .expect("script");
    assert!(exit.success(), "{exit:?}");
    let text = String::from_utf8_lossy(&screen);
    for want in [
        "tui-smoke",
        "RUNNING",
        "cam",
        "sink",
        "keys",
        "# NODE",
        "\x1b[?1049h",
        "\x1b[?1049l",
    ] {
        assert!(
            text.contains(want),
            "missing {want:?} in the terminal output"
        );
    }
    assert!(
        text.rfind("\x1b[?1049l") > text.find("\x1b[?1049h"),
        "the alternate screen is left at the end"
    );
    // The runtime did not notice: still running, still counting.
    let a = status(&runtime).expect("status")["cycle"]
        .as_u64()
        .unwrap_or(0);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let b = status(&runtime).expect("status");
    assert_eq!(b["status"], "running");
    assert!(b["cycle"].as_u64().unwrap_or(0) > a);
    assert!(
        mvf()
            .args(["manage", "command", "stop", "--id", "tui-smoke-stop"])
            .env("MVF_RUNTIME_DIR", &runtime)
            .status()
            .expect("stop")
            .success()
    );
    assert!(run.wait().expect("wait").success());

    // `run --tui`: q stops the run; the report comes after the restored screen.
    let (exit, screen) = in_terminal(
        &format!("'{mvf_path}' run --tui '{}'", dir.join("p.yaml").display()),
        &[(2000, "q")],
        &dir.join("run2"),
        &dir.join("run.out"),
    )
    .expect("script");
    assert!(exit.success(), "{exit:?}");
    let text = String::from_utf8_lossy(&screen);
    let left = text
        .rfind("\x1b[?1049l")
        .expect("left the alternate screen");
    assert!(
        text[left..].contains("cancelled"),
        "the report follows the dashboard"
    );
}

/// `mvf manage tui --print` (#35): one frame of the dashboard as text, through the same feed,
/// model, layout and drawing as the TUI. Runs on every platform, Windows included (named pipe, no
/// terminal needed).
#[test]
fn manage_tui_print_renders_a_running_pipeline_without_a_terminal() {
    let dir = std::env::temp_dir().join(format!("mvf-cli-tui-print-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("p.yaml"), TUI_PIPELINE).expect("pipeline");
    let runtime = dir.join("run");
    let mut run = mvf()
        .args(["run", "--manage", "p.yaml"])
        .current_dir(&dir)
        .env("MVF_RUNTIME_DIR", &runtime)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let screen = loop {
        let out = mvf()
            .args(["manage", "tui", "--print", "120x30"])
            .env("MVF_RUNTIME_DIR", &runtime)
            .output()
            .expect("print");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if out.status.success() && text.contains("RUNNING") {
            break text;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    for want in [
        "MVF",
        "tui-smoke",
        "cam",
        "sink",
        "cycle",
        "-frame",
        "enter details",
    ] {
        assert!(screen.contains(want), "missing {want:?}:\n{screen}");
    }
    assert!(screen.is_ascii(), "{screen}");
    assert!(screen.lines().all(|l| l.len() <= 120));
    let bad = mvf()
        .args(["manage", "tui", "--print", "wide"])
        .env("MVF_RUNTIME_DIR", &runtime)
        .output()
        .expect("print");
    assert!(!bad.status.success());
    assert!(
        mvf()
            .args(["manage", "command", "stop", "--id", "print-stop"])
            .env("MVF_RUNTIME_DIR", &runtime)
            .status()
            .expect("stop")
            .success()
    );
    assert!(run.wait().expect("wait").success());
}
