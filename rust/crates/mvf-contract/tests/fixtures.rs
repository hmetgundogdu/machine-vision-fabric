//! Fixture-driven tests: every `valid/*.yaml` must load; every `invalid/*.yaml` must be rejected with
//! the code named on its first line (`# expect: <code>`).

use std::fs;
use std::path::{Path, PathBuf};

use mvf_contract::{ContractError, parse_contract};

fn fixtures(kind: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(kind);
    let mut files: Vec<_> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .collect();
    files.sort();
    files
}

#[test]
fn every_valid_fixture_loads() {
    let files = fixtures("valid");
    assert!(files.len() >= 5, "expected the valid fixture set");
    for file in files {
        let text = fs::read_to_string(&file).expect("read fixture");
        if let Err(e) = parse_contract(&text) {
            panic!("{} should be valid but failed: {e}", file.display());
        }
    }
}

#[test]
fn every_invalid_fixture_is_rejected_with_its_expected_code() {
    let files = fixtures("invalid");
    assert!(files.len() >= 30, "expected the invalid fixture set");
    for file in files {
        let text = fs::read_to_string(&file).expect("read fixture");
        let expected = text
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("# expect: "))
            .and_then(|l| l.split_whitespace().next())
            .unwrap_or_else(|| panic!("{} lacks a `# expect:` header", file.display()))
            .to_string();
        match parse_contract(&text) {
            Ok(_) => panic!("{} should be rejected with {expected}", file.display()),
            Err(e) => {
                let codes = e.codes();
                assert!(
                    codes.contains(&expected.as_str()),
                    "{}: expected {expected}, got {codes:?} ({e})",
                    file.display()
                );
            }
        }
    }
}

#[test]
fn schema_errors_carry_the_failing_field_path() {
    let text = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/invalid/unknown-port-field.yaml"),
    )
    .expect("read fixture");
    match parse_contract(&text) {
        Err(ContractError::Yaml(e)) => {
            assert_eq!(e.code, "yaml.schema");
            let path = e.path.expect("schema error has a path");
            assert!(path.starts_with("spec.ports.inputs"), "path was {path}");
            assert!(e.message.contains("colour"), "message was {}", e.message);
        }
        other => panic!("unexpected result {other:?}"),
    }
}

#[test]
fn semantic_validation_reports_all_issues_at_once() {
    let text = r#"
apiVersion: mvf.dev/v1alpha1
kind: NodeContract
metadata: { id: Bad.Id, version: nope }
spec:
  ports:
    inputs:
      - { name: frame, channel: control, type: frame }
      - { name: frame, channel: data, type: frame }
  artifacts: []
"#;
    let codes = parse_contract(text).expect_err("invalid").codes();
    for expected in [
        "contract.metadata.invalid-id",
        "contract.metadata.invalid-version",
        "contract.port.channel-type-mismatch",
        "contract.port.duplicate-name",
        "contract.artifact.none",
    ] {
        assert!(codes.contains(&expected), "missing {expected} in {codes:?}");
    }
}

#[test]
fn defaults_are_explicit_and_conservative() {
    let text = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/valid/in-process-invert.yaml"),
    )
    .expect("read fixture");
    let c = parse_contract(&text).expect("valid");
    use mvf_contract::*;
    assert_eq!(c.spec.execution.backend, BackendPreference::Auto);
    assert_eq!(c.spec.execution.placement.target, Target::Cpu);
    assert_eq!(c.spec.execution.placement.fallback, Fallback::Deny);
    assert_eq!(c.spec.isolation.minimum, IsolationLevel::None);
    assert_eq!(c.spec.memory.zero_copy, ZeroCopy::Preferred);
    assert_eq!(c.spec.lifecycle.activation, Activation::Resident);
    assert_eq!(c.spec.resources.enforcement, Enforcement::BestEffort);
    assert!(c.spec.ports.inputs[0].required);
}

#[test]
fn contract_round_trips_through_serialization() {
    for file in fixtures("valid") {
        let text = fs::read_to_string(&file).expect("read fixture");
        let c = parse_contract(&text).expect("valid");
        let json = serde_json::to_string(&c).expect("serialize");
        let back: mvf_contract::NodeContract = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(c, back, "{}", file.display());
    }
}
