//! Packaging an MVF node as an **OCI-profile image**: a base image from the store plus one
//! deterministic layer holding the MVF Python SDK, the module and its contract, with the entrypoint
//! pointing at the module. No Dockerfile engine, no daemon; the result is a standard OCI image that
//! Docker can load and run (with an MVF protocol harness on stdin).

use std::collections::BTreeMap;
use std::path::Path;

use mvf_contract::{Artifact, BackendPreference, IsolationLevel, NodeContract};
use mvf_oci::{
    ContentStore, ExecConfig, ImageRecord, LayerBuilder, NODE_CONTRACT_LABEL, OciError,
    append_layer, read_config,
};

/// Where things live inside a packaged node image.
pub const SDK_DIR: &str = "/mvf/sdk";
/// Module directory inside the image.
pub const MODULE_DIR: &str = "/mvf/module";
/// Contract path inside the image (also the `dev.mvf.node.contract` label value).
pub const CONTRACT_PATH: &str = "/mvf/node.yaml";

fn files(dir: &Path) -> Result<Vec<(String, Vec<u8>)>, OciError> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)
        .map_err(|e| OciError::Malformed(format!("{}: {e}", dir.display())))?
        .flatten()
    {
        let p = e.path();
        if p.is_file() {
            let name = e.file_name().to_string_lossy().to_string();
            let bytes = std::fs::read(&p)
                .map_err(|e| OciError::Malformed(format!("{}: {e}", p.display())))?;
            out.push((name, bytes));
        }
    }
    out.sort();
    Ok(out)
}

/// Appends the SDK (`sdk_package_dir` = `src/sdk/python/mvf_sdk`), the module directory and
/// `contract_text` to `base`, sets the entrypoint to `python3 /mvf/module/<entry>`, and stores the
/// image as `name`.
pub fn python_node_image(
    store: &ContentStore,
    base: &ImageRecord,
    name: &str,
    sdk_package_dir: &Path,
    module_dir: &Path,
    entry: &str,
    contract_text: &str,
) -> Result<ImageRecord, OciError> {
    let mut layer = LayerBuilder::new()
        .dir("mvf")
        .dir("mvf/sdk")
        .dir("mvf/sdk/mvf_sdk")
        .dir("mvf/module");
    for (file, bytes) in files(sdk_package_dir)? {
        if file.ends_with(".py") {
            layer = layer.file(&format!("mvf/sdk/mvf_sdk/{file}"), &bytes, 0o644);
        }
    }
    for (file, bytes) in files(module_dir)? {
        layer = layer.file(&format!("mvf/module/{file}"), &bytes, 0o644);
    }
    layer = layer.file("mvf/node.yaml", contract_text.as_bytes(), 0o644);
    let blob = layer.gzip()?;

    let base_env = read_config(store, base)?
        .config
        .and_then(|c| c.env)
        .unwrap_or_default();
    let mut env: Vec<String> = base_env
        .into_iter()
        .filter(|e| !e.starts_with("PYTHONPATH="))
        .collect();
    env.push(format!("PYTHONPATH={SDK_DIR}"));
    let exec = ExecConfig {
        entrypoint: Some(vec!["python3".into(), format!("{MODULE_DIR}/{entry}")]),
        cmd: Some(vec![]),
        env: Some(env),
        working_dir: Some(MODULE_DIR.into()),
        labels: Some(BTreeMap::from([(
            NODE_CONTRACT_LABEL.to_string(),
            CONTRACT_PATH.to_string(),
        )])),
        user: None,
    };
    append_layer(store, base, name, &blob, Some(&exec))
}

/// The OCI-profile contract for a packaged module: same ports and lifecycle, an `oci` artifact
/// pinned to `digest`, `isolation.minimum: sandbox` and the backend pinned to `oci`.
pub fn oci_contract(module: &NodeContract, image: &str, digest: &str) -> NodeContract {
    let mut c = module.clone();
    c.metadata.id = format!("{}.oci", module.metadata.id);
    c.spec.artifacts = vec![Artifact::Oci {
        image: image.to_string(),
        digest: digest.to_string(),
        targets: None,
    }];
    c.spec.execution.backend = BackendPreference::Oci;
    c.spec.isolation.minimum = IsolationLevel::Sandbox;
    c
}

/// Serializes a contract as JSON, which the strict YAML loader accepts (JSON is YAML 1.2 flow).
pub fn contract_text(contract: &NodeContract) -> String {
    serde_json::to_string_pretty(contract).unwrap_or_default()
}
