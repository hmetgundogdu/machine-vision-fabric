//! Package signatures (#33): with trusted signers configured, a package is staged only if one of
//! them signed exactly its name and files.

use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey};
use mvf_management::upload::{key_id, manifest_of, signing_bytes};
use mvf_management::{UploadOptions, Uploads, preview};
use serde_json::{Value, json};

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mvfs-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn id(k: &SigningKey) -> String {
    key_id(k.verifying_key().as_bytes())
}

fn sign(k: &SigningKey, package: &str, files: &serde_json::Map<String, Value>) -> Value {
    let sig = k.sign(&signing_bytes(package, files));
    json!({"key": id(k), "sig": preview::base64(&sig.to_bytes())})
}

/// A package directory: a contract and a model file.
fn package(dir: &Path, model: &[u8]) {
    std::fs::create_dir_all(dir.join("model")).expect("dir");
    std::fs::write(dir.join("node.yaml"), "apiVersion: mvf.dev/v1alpha1\n").expect("node.yaml");
    std::fs::write(dir.join("model/weights.bin"), model).expect("model");
}

/// Uploads every file of `dir` as `package`, one chunk each.
fn send(uploads: &Uploads, package: &str, dir: &Path) -> serde_json::Map<String, Value> {
    let files = manifest_of(dir).expect("manifest");
    for (path, digest) in &files {
        let bytes = std::fs::read(dir.join(path)).expect("file");
        let r = uploads.upload(&json!({"package": package, "path": path, "size": bytes.len(), "digest": digest, "offset": 0, "data": preview::base64(&bytes)}));
        assert_eq!(r["status"], "complete", "{r}");
    }
    files
}

fn uploads(name: &str, trusted: Vec<String>) -> (Uploads, PathBuf) {
    let staging = temp(name).join("staged");
    let u = Uploads::new(UploadOptions {
        trusted,
        ..UploadOptions::new(&staging, ".mvf/staged")
    })
    .expect("uploads");
    (u, staging)
}

#[test]
fn a_trusted_signature_stages_and_is_kept_beside_the_manifest() {
    let alice = key(1);
    let (u, staging) = uploads("trusted", vec![id(&alice)]);
    let src = temp("trusted-src");
    package(&src, b"weights v2");
    let files = send(&u, "inspect-v2", &src);
    let staged = u
        .stage(&json!({"package": "inspect-v2", "files": files, "signature": sign(&alice, "inspect-v2", &files)}))
        .expect("staged");
    assert_eq!(staged["signedBy"], id(&alice));
    let kept: Value = serde_json::from_slice(
        &std::fs::read(staging.join("inspect-v2/signature.json")).expect("sig"),
    )
    .expect("json");
    assert_eq!(kept["key"], id(&alice));
    // Re-staging is a no-op that still reports the signer.
    let again = u
        .stage(&json!({"package": "inspect-v2", "files": files, "signature": sign(&alice, "inspect-v2", &files)}))
        .expect("again");
    assert_eq!(
        (again["alreadyStaged"].as_bool(), again["signedBy"].as_str()),
        (Some(true), Some(id(&alice).as_str()))
    );
}

#[test]
fn unsigned_untrusted_tampered_renamed_and_garbage_signatures_are_refused() {
    let (alice, mallory) = (key(1), key(2));
    let (u, staging) = uploads("refused", vec![id(&alice)]);
    let src = temp("refused-src");
    package(&src, b"weights v2");
    let signed_files = manifest_of(&src).expect("manifest");
    let good = sign(&alice, "inspect-v2", &signed_files);
    // The model is swapped after signing: what was uploaded is not what was signed.
    package(&src, b"weights v2, quietly changed");
    let files = send(&u, "inspect-v2", &src);
    let stage = |signature: Value| {
        u.stage(&json!({"package": "inspect-v2", "files": files, "signature": signature}))
            .expect_err("refused")
            .0
    };
    assert_eq!(stage(Value::Null), "unsigned");
    assert_eq!(
        stage(sign(&mallory, "inspect-v2", &files)),
        "untrusted-signer"
    );
    assert_eq!(
        stage(good.clone()),
        "bad-signature",
        "the files changed after signing"
    );
    assert_eq!(
        stage(sign(&alice, "other-name", &files)),
        "bad-signature",
        "signed for another package"
    );
    let mut garbage = sign(&alice, "inspect-v2", &files);
    garbage["sig"] = json!(preview::base64(&[0u8; 64]));
    assert_eq!(stage(garbage.clone()), "bad-signature");
    garbage["sig"] = json!("short");
    assert_eq!(stage(garbage), "bad-signature");
    assert!(!staging.join("inspect-v2").exists(), "nothing was staged");
    // The uploaded files stay; signing what was actually uploaded stages it.
    let staged = u
        .stage(&json!({"package": "inspect-v2", "files": files, "signature": sign(&alice, "inspect-v2", &files)}))
        .expect("staged");
    assert_eq!(staged["signedBy"], id(&alice));
}

#[test]
fn without_trusted_signers_packages_stage_unsigned() {
    let (u, staging) = uploads("open", vec![]);
    let src = temp("open-src");
    package(&src, b"weights");
    let files = send(&u, "p", &src);
    let staged = u
        .stage(&json!({"package": "p", "files": files}))
        .expect("staged");
    assert!(staged["signedBy"].is_null());
    assert!(!staging.join("p/signature.json").exists());
}

#[test]
fn a_malformed_trusted_key_is_refused_at_start() {
    let staging = temp("badkey").join("staged");
    assert!(
        Uploads::new(UploadOptions {
            trusted: vec!["ed25519:not-hex".into()],
            ..UploadOptions::new(&staging, ".mvf/staged")
        })
        .is_err()
    );
}
