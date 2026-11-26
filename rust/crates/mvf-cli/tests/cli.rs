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
    assert_eq!(rolled["cycle"], 70);
    assert_eq!(
        rolled["toGeneration"], 2,
        "rolled back to the Python worker generation"
    );
    assert_eq!(report["dataPlane"]["arena"]["copiesIn"], 0);
    assert_eq!(report["dataPlane"]["arena"]["copiesOut"], 0);
    assert_eq!(report["cyclesCompleted"], 120);
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
