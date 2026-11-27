//! Storage binding (#31): a node's logical mounts resolve against the pipeline's volumes, and no node
//! gets more access than the volume allows.

use mvf_contract::{ContractError, NodeContract, parse_contract};
use mvf_graph::{ContractResolver, GraphError, build_graph};

struct Contracts;

const DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

fn node(id: &str, mounts: &str) -> NodeContract {
    parse_contract(&format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: {id}, version: 1.0.0 }}\nspec:\n  ports:\n    outputs: [ {{ name: frame, channel: data, type: frame }} ]\n  artifacts: [ {{ backend: worker, runtime: python, entry: m.py, protocol: 1 }} ]\n  storage:\n    mounts:\n{mounts}"
    ))
    .expect("contract")
}

impl ContractResolver for Contracts {
    fn builtin(&self, name: &str) -> Option<NodeContract> {
        (name == "cam").then(|| {
            parse_contract("apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: { id: cam, version: 1.0.0 }\nspec:\n  ports:\n    outputs: [ { name: frame, channel: data, type: frame } ]\n  artifacts: [ { backend: in-process, builtin: cam } ]\n").expect("cam")
        })
    }
    fn file(&self, path: &str) -> Result<(String, NodeContract), ContractError> {
        let c = match path {
            "inspector.yaml" => node(
                "t.inspector",
                "      - { name: model, target: /models }\n      - { name: results, target: /results, access: read-write }\n      - { name: tmp, target: /scratch, kind: scratch, access: read-write, maxSize: 64MiB }\n",
            ),
            "writer.yaml" => node(
                "t.writer",
                "      - { name: out, target: /out, access: read-write }\n",
            ),
            "reader.yaml" => node("t.reader", "      - { name: in, target: /in }\n"),
            _ => panic!("unknown contract {path}"),
        };
        Ok((path.to_string(), c))
    }
}

fn pipeline(volumes: &str, nodes: &str) -> String {
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: plant }}\nspec:\n  volumes:\n{volumes}  nodes:\n    - {{ id: cam, builtin: cam }}\n{nodes}"
    )
}

fn codes(text: &str) -> Vec<&'static str> {
    match build_graph(text, &Contracts) {
        Ok(_) => Vec::new(),
        Err(GraphError::Invalid(issues)) => issues.iter().map(|i| i.code).collect(),
        Err(e) => panic!("{e}"),
    }
}

/// An operator-approved host directory, absolute on this platform (Windows paths need a drive).
#[cfg(windows)]
const CAMERA: &str = "'D:\\srv\\mvf-camera'";
#[cfg(not(windows))]
const CAMERA: &str = "/srv/mvf-camera";

const VOLUMES: &str = concat!(
    "    - { name: model-v1, type: artifact, file: models/v1.onnx, digest: \"sha256:0000000000000000000000000000000000000000000000000000000000000000\" }\n",
    "    - { name: results, type: persistent }\n",
);

fn volumes() -> String {
    format!("{VOLUMES}    - {{ name: camera, type: bind, hostPath: {CAMERA} }}\n")
}
const SHARED: &str = "    - { name: handoff, type: shared, writers: [w] }\n";

#[test]
fn mounts_resolve_to_volumes_with_the_access_the_node_asked_for() {
    let text = pipeline(
        &format!("{}{SHARED}", volumes()),
        "    - { id: insp, contract: inspector.yaml, storage: { model: model-v1, results: results } }\n    - { id: w, contract: writer.yaml, storage: { out: handoff } }\n    - { id: r, contract: reader.yaml, storage: { in: handoff } }\n",
    );
    let g = build_graph(&text, &Contracts).expect("valid");
    let insp = g.nodes.iter().find(|n| n.id == "insp").expect("insp");
    let names: Vec<(&str, Option<&str>)> = insp
        .storage
        .iter()
        .map(|b| {
            (
                b.mount.name.as_str(),
                b.volume.as_ref().map(|v| v.name.as_str()),
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("model", Some("model-v1")),
            ("results", Some("results")),
            ("tmp", None)
        ]
    );
    assert!(insp.storage.iter().all(|b| b.scope == "plant"));
    assert_eq!(g.volumes.len(), 4);
    let r = g.nodes.iter().find(|n| n.id == "r").expect("r");
    assert_eq!(
        r.storage[0].access,
        mvf_contract::MountAccess::ReadOnly,
        "a reader of a shared volume"
    );
    let _ = DIGEST;
}

#[test]
fn no_node_gets_more_access_than_the_volume_allows() {
    let c = |nodes: &str| codes(&pipeline(&volumes(), nodes));
    assert_eq!(
        c("    - { id: w, contract: writer.yaml, storage: { out: model-v1 } }\n"),
        ["graph.storage.read-only-volume"],
        "artifacts are immutable"
    );
    assert_eq!(
        c("    - { id: w, contract: writer.yaml, storage: { out: camera } }\n"),
        ["graph.storage.read-only-volume"],
        "a host bind is read-only unless the operator says otherwise"
    );
    assert_eq!(
        codes(&pipeline(
            &format!("{}{SHARED}", volumes()),
            "    - { id: w, contract: reader.yaml, storage: { in: handoff } }\n    - { id: x, contract: writer.yaml, storage: { out: handoff } }\n"
        )),
        ["graph.storage.not-a-writer"],
        "only listed writers write a shared volume"
    );
    assert_eq!(
        c(
            "    - { id: a, contract: writer.yaml, storage: { out: results } }\n    - { id: b, contract: writer.yaml, storage: { out: results } }\n"
        ),
        ["graph.storage.volume-not-shared"]
    );
}

#[test]
fn bindings_and_volumes_are_strict() {
    let c = |volumes: &str, nodes: &str| codes(&pipeline(volumes, nodes));
    assert_eq!(
        c(&volumes(), "    - { id: r, contract: reader.yaml }\n"),
        ["graph.storage.unbound-mount"]
    );
    assert_eq!(
        c(
            &volumes(),
            "    - { id: r, contract: reader.yaml, storage: { in: nope } }\n"
        ),
        ["graph.storage.unknown-volume"]
    );
    assert_eq!(
        c(
            &volumes(),
            "    - { id: r, contract: reader.yaml, storage: { in: results, extra: results } }\n"
        ),
        ["graph.storage.unknown-mount"]
    );
    assert_eq!(
        c(
            &volumes(),
            "    - { id: i, contract: inspector.yaml, storage: { model: model-v1, results: results, tmp: results } }\n"
        ),
        ["graph.storage.scratch-is-provisioned"]
    );
    let reader = "    - { id: r, contract: reader.yaml, storage: { in: v } }\n";
    for (volume, code) in [
        (
            "    - { name: v, type: bind, hostPath: / }\n",
            "graph.storage.unsafe-host-path",
        ),
        (
            "    - { name: v, type: bind, hostPath: /etc/ssl }\n",
            "graph.storage.unsafe-host-path",
        ),
        (
            "    - { name: v, type: bind, hostPath: relative/dir }\n",
            "graph.storage.unsafe-host-path",
        ),
        (
            "    - { name: v, type: bind, hostPath: /srv/../etc }\n",
            "graph.storage.unsafe-host-path",
        ),
        (
            "    - { name: v, type: artifact, file: ../outside.onnx, digest: \"sha256:0000000000000000000000000000000000000000000000000000000000000000\" }\n",
            "graph.storage.unsafe-artifact-path",
        ),
        (
            "    - { name: v, type: artifact, file: m.onnx, digest: sha256:abc }\n",
            "graph.storage.invalid-digest",
        ),
        (
            "    - { name: v, type: persistent, hostPath: /srv/x }\n",
            "graph.storage.invalid-volume",
        ),
        (
            "    - { name: v, type: shared, writers: [ghost] }\n",
            "graph.storage.unknown-writer",
        ),
        (
            "    - { name: V, type: persistent }\n",
            "graph.storage.invalid-volume-name",
        ),
    ] {
        assert!(
            c(volume, reader).contains(&code),
            "{volume} -> {:?}",
            c(volume, reader)
        );
    }
}
