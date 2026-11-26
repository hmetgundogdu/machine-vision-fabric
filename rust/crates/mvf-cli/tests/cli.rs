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
    assert_eq!(lines.len(), 6, "5 records + 1 report");
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
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 3);
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
    let last: serde_json::Value = serde_json::from_str(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .last()
            .expect("report"),
    )
    .expect("json");
    assert_eq!(last["report"]["end"]["reason"], "node-failed");
    assert_eq!(last["report"]["end"]["cycle"], 2);
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
    let report = &lines[lines.len() - 2]["report"];
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
