//! Storage Fabric with real Python workers (#31): a pinned model and persistent results through
//! ordinary file I/O, a shared producer/consumer volume, fail-closed isolation, and model v1 -> v2
//! live replacement with rollback to the generation's own pinned file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_storage::StorageService;
use mvf_worker::WorkerBackend;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn temp(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("mvf-worker-storage-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp");
    std::fs::canonicalize(&dir).expect("canonical")
}

/// A minimal ONNX `ModelProto` (an Identity graph): ir_version 8, producer, opset 13.
pub fn onnx(producer: &str) -> Vec<u8> {
    fn field(out: &mut Vec<u8>, number: u8, bytes: &[u8]) {
        out.push((number << 3) | 2);
        out.push(bytes.len() as u8);
        out.extend_from_slice(bytes);
    }
    let mut node = Vec::new();
    field(&mut node, 1, b"x");
    field(&mut node, 2, b"y");
    field(&mut node, 4, b"Identity");
    let mut graph = Vec::new();
    field(&mut graph, 1, &node);
    field(&mut graph, 2, b"g");
    let mut name = Vec::new();
    field(&mut name, 1, b"x");
    field(&mut graph, 11, &name);
    let mut out_name = Vec::new();
    field(&mut out_name, 1, b"y");
    field(&mut graph, 12, &out_name);
    let mut model = vec![0x08, 0x08];
    field(&mut model, 2, producer.as_bytes());
    field(&mut model, 7, &graph);
    field(&mut model, 8, &[0x10, 13]);
    model
}

fn digest(bytes: &[u8]) -> String {
    mvf_oci::Digest::of(bytes).to_string()
}

fn services(root: &Path, package: &Path) -> HostServices {
    HostServices {
        storage: Arc::new(StorageService::new(root)),
        package_dir: Some(package.to_path_buf()),
        ..HostServices::default()
    }
}

fn engine(text: &str, s: &HostServices, modules: &Path) -> Result<Engine, String> {
    let resolver = HostResolver::new(modules)
        .with_module_root(modules)
        .with_module_root(repo().join("modules"));
    let graph = build_graph(text, &resolver).map_err(|e| e.to_string())?;
    let worker = WorkerBackend::from_env().with_python_sdk(repo().join("src/sdk/python"));
    Engine::build(graph, &default_backends().with(worker), s).map_err(|e| e.to_string())
}

fn values(s: &HostServices, node: &str) -> Vec<serde_json::Value> {
    s.recorder
        .records()
        .into_iter()
        .filter(|r| r.node == node)
        .filter_map(|r| match r.value {
            RecordedValue::Json { value } => Some(value),
            _ => None,
        })
        .collect()
}

#[test]
fn a_worker_reads_a_pinned_model_and_publishes_retained_results() {
    let (root, package) = (temp("insp-root"), temp("insp-pkg"));
    std::fs::create_dir_all(package.join("models")).expect("models");
    let model = onnx("mvf-defect-v1");
    std::fs::write(package.join("models/defect-v1.onnx"), &model).expect("model");
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: plant }}\nspec:\n  volumes:\n    - {{ name: model-v1, type: artifact, file: models/defect-v1.onnx, digest: \"{}\" }}\n    - {{ name: results, type: persistent }}\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 32, height: 32, channels: 1, frames: 20 }} }}\n    - {{ id: insp, module: py.model-inspector, storage: {{ model: model-v1, results: results }} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: insp.frame }}\n      - {{ from: insp.result, to: sink.json }}\n",
        digest(&model)
    );
    let results = root.join("volumes/plant/results/data");
    for run in 1..=2 {
        let s = services(&root, &package);
        let mut e = engine(&text, &s, &package).expect("build");
        let r = e.run(&RunOptions::default(), &mut NullObserver);
        assert_eq!(
            r.end,
            EndReason::SourceExhausted { node: "cam".into() },
            "{:?}",
            s.logs.events()
        );
        let v = values(&s, "sink");
        assert_eq!(v.len(), 20);
        let m = &v[0]["model"];
        assert_eq!(m["sha256"], digest(&model).trim_start_matches("sha256:"));
        assert_eq!(
            (m["irVersion"].as_u64(), m["opset"].as_u64()),
            (Some(8), Some(13))
        );
        assert_eq!(m["producer"], "mvf-defect-v1");
        assert!(v.iter().all(|x| x["scratch"] == "ok"));
        let logs = s.logs.events();
        assert!(
            logs.iter().any(|l| l
                .message
                .starts_with("storage: model -> artifact `model-v1`")
                && l.message.contains("not enforced")),
            "{logs:?}"
        );
        let run_dir = std::fs::read_dir(root.join("runs"))
            .expect("runs")
            .flatten()
            .map(|e| e.path())
            .next()
            .expect("run dir");
        drop(e);
        drop(s);
        assert!(
            !run_dir.exists(),
            "scratch and model views go with the engine"
        );
        let files = std::fs::read_dir(&results).expect("results").count();
        assert_eq!(
            files,
            20 * run,
            "every record published, and every earlier run's retained"
        );
        eprintln!(
            "run {run}: model loaded in {} ms; {files} records retained in {}",
            m["loadMs"],
            results.display()
        );
    }
    // Each record is complete JSON: written to a temporary name, flushed, renamed.
    for f in std::fs::read_dir(&results).expect("results").flatten() {
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(f.path()).expect("read"))
                .expect("complete record");
        assert_eq!(record["producer"], "mvf-defect-v1");
    }
}

fn module(dir: &Path, id: &str, script: &str, mounts: &str, extra: &str) {
    let m = dir.join(id);
    std::fs::create_dir_all(&m).expect("dir");
    std::fs::write(m.join("node.py"), script).expect("py");
    std::fs::write(
        m.join("node.yaml"),
        format!(
            "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: {id}, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [ {{ name: frame, channel: data, type: frame }} ]\n    outputs: [ {{ name: out, channel: data, type: json }} ]\n  artifacts: [ {{ backend: worker, runtime: python, entry: node.py, protocol: 1 }} ]\n  lifecycle: {{ replacement: stateless, readinessTimeoutMs: 10000 }}\n{extra}  storage:\n    mounts:\n{mounts}"
        ),
    )
    .expect("yaml");
}

#[test]
fn a_shared_volume_hands_published_files_from_its_writer_to_a_reader() {
    let (root, modules) = (temp("shared-root"), temp("shared-modules"));
    module(
        &modules,
        "t.producer",
        "from mvf_sdk import analysis, run_analyzer\nfrom mvf_sdk.storage import volume, write_atomic\ndef analyze(payload, meta):\n    data = bytes(payload.memory)\n    write_atomic(volume('out') / 'frame-{:06d}.bin'.format(meta['sequence']), data)\n    return analysis(value={'published': len(data)})\nrun_analyzer('t.producer', analyze)\n",
        "      - { name: out, target: /out, access: read-write }\n",
        "",
    );
    module(
        &modules,
        "t.consumer",
        "import os\nfrom mvf_sdk import analysis, run_analyzer\nfrom mvf_sdk.storage import volume\ndef analyze(payload, meta):\n    d = volume('in')\n    path = d / 'frame-{:06d}.bin'.format(meta['sequence'])\n    intact = path.exists() and path.read_bytes() == bytes(payload.memory)\n    partial = [p.name for p in d.iterdir() if p.name.endswith('.tmp')]\n    try:\n        (d / 'reader-was-here').write_bytes(b'x')\n        write_denied = False\n    except OSError:\n        write_denied = True\n    return analysis(value={'intact': intact, 'partial': partial, 'writeDenied': write_denied})\nrun_analyzer('t.consumer', analyze)\n",
        "      - { name: in, target: /in }\n",
        "",
    );
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: line }\nspec:\n  volumes:\n    - { name: handoff, type: shared, writers: [producer] }\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { width: 64, height: 64, channels: 1, frames: 30 } }\n    - { id: producer, module: t.producer, storage: { out: handoff } }\n    - { id: consumer, module: t.consumer, storage: { in: handoff } }\n    - { id: gate, builtin: record }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: producer.frame }\n      - { from: cam.frame, to: consumer.frame }\n      - { from: producer.out, to: gate.json }\n      - { from: consumer.out, to: sink.json }\n";
    let s = services(&root, &modules);
    let mut e = engine(text, &s, &modules).expect("build");
    let r = e.run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(
        r.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        s.logs.events()
    );
    let v = values(&s, "sink");
    assert_eq!(v.len(), 30);
    assert!(
        v.iter().all(|x| x["intact"] == true),
        "every frame was published before it was read"
    );
    assert!(
        v.iter().all(|x| x["partial"] == serde_json::json!([])),
        "no partial file is ever visible"
    );
    // Honest label: on a worker the reader's read-only mount is not enforced by anything.
    eprintln!(
        "shared volume on a worker: 30/30 frames handed over intact; reader write denied = {} (not enforced for isolation `process`)",
        v[0]["writeDenied"]
    );
}

#[test]
fn required_storage_isolation_fails_closed_on_a_worker() {
    let (root, modules) = (temp("iso-root"), temp("iso-modules"));
    module(
        &modules,
        "t.strict",
        "from mvf_sdk import analysis, run_analyzer\nrun_analyzer('t.strict', lambda p, m: analysis(value={}))\n",
        "      - { name: in, target: /in }\n",
        "  storage_isolation_placeholder: {}\n",
    );
    // Rewrite the contract with `isolation: required` (the helper writes `mounts` only).
    let yaml = modules.join("t.strict/node.yaml");
    let text = std::fs::read_to_string(&yaml)
        .expect("yaml")
        .replace("  storage_isolation_placeholder: {}\n", "")
        .replace(
            "  storage:\n    mounts:",
            "  storage:\n    isolation: required\n    mounts:",
        );
    std::fs::write(&yaml, text).expect("write");
    let pipeline = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: strict }\nspec:\n  volumes:\n    - { name: data, type: persistent }\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { frames: 1 } }\n    - { id: strict, module: t.strict, storage: { in: data } }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: strict.frame }\n      - { from: strict.out, to: sink.json }\n";
    let s = services(&root, &modules);
    let err = engine(pipeline, &s, &modules).expect_err("a worker cannot confine the filesystem");
    assert!(err.contains("negotiation.storage-not-enforceable"), "{err}");
}

#[test]
fn a_model_is_swapped_live_and_a_failed_candidate_rolls_back_to_its_pinned_file() {
    let (root, modules) = (temp("swap-root"), temp("swap-modules"));
    std::fs::create_dir_all(modules.join("models")).expect("models");
    for (name, content) in [("v1", "model-v1"), ("v2", "model-v2"), ("v3", "bad")] {
        std::fs::write(modules.join(format!("models/{name}.bin")), content).expect("model");
    }
    // Reads its model once; a candidate given the `bad` model fails on its 5th frame.
    module(
        &modules,
        "t.scorer",
        "import os\nfrom mvf_sdk import analysis, run_analyzer\nfrom mvf_sdk.storage import volume\nstate = {'model': None, 'n': 0}\ndef analyze(payload, meta):\n    if state['model'] is None:\n        state['model'] = sorted(volume('model').iterdir())[0].read_text()\n    state['n'] += 1\n    if state['model'] == 'bad' and state['n'] >= 5:\n        os._exit(3)\n    return analysis(value={'model': state['model']})\nrun_analyzer('t.scorer', analyze)\n",
        "      - { name: model, target: /models }\n",
        "",
    );
    let volume = |name: &str, content: &str| {
        format!(
            "    - {{ name: model-{name}, type: artifact, file: models/{name}.bin, digest: \"{}\" }}\n",
            digest(content.as_bytes())
        )
    };
    let text = format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: swap }}\nspec:\n  volumes:\n{}{}{}  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ frames: 150, intervalMs: 4 }} }}\n    - {{ id: score, module: t.scorer, storage: {{ model: model-v1 }} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n      - {{ from: cam.frame, to: score.frame }}\n      - {{ from: score.out, to: sink.json }}\n",
        volume("v1", "model-v1"),
        volume("v2", "model-v2"),
        volume("v3", "bad")
    );
    let s = services(&root, &modules);
    let mut e = engine(&text, &s, &modules).expect("build");
    let resolver = HostResolver::new(&modules).with_module_root(&modules);
    let request = |model: &str, probation: u64| {
        ReplacementRequest::resolve(
            &resolver,
            "score",
            &NodeReference::Module("t.scorer".into()),
            Default::default(),
            probation,
        )
        .expect("request")
        .with_storage(BTreeMap::from([("model".to_string(), model.to_string())]))
    };
    struct Driver {
        control: ControlHandle,
        steps: Vec<(u64, ReplacementRequest)>,
    }
    impl Observer for Driver {
        fn cycle_completed(&mut self, cycle: u64) {
            for (at, r) in &self.steps {
                if *at == cycle + 1 {
                    self.control.replace(r.clone());
                }
            }
        }
    }
    let mut driver = Driver {
        control: e.control(),
        steps: vec![
            (30, request("model-v2", 10)),
            (70, request("model-v3", 100)),
            (110, request("nope", 10)),
        ],
    };
    let r = e.run(&RunOptions::default(), &mut driver);
    assert_eq!(
        r.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        s.logs.events()
    );
    use mvf_engine::ReplacementEventKind as K;
    let events: Vec<(K, Option<&str>)> = r.replacements.iter().map(|x| (x.kind, x.code)).collect();
    assert!(
        events.contains(&(K::RolledBack, Some("replace.candidate-failed"))),
        "{events:?}"
    );
    assert!(
        events.contains(&(K::Rejected, Some("replace.storage-invalid"))),
        "rebinding to an undeclared volume is refused: {events:?}"
    );
    let mut order: Vec<String> = values(&s, "sink")
        .iter()
        .map(|v| v["model"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        order.len(),
        150,
        "the rolled-back cycle was re-run: nothing lost or doubled"
    );
    order.dedup();
    assert_eq!(
        order,
        ["model-v1", "model-v2", "bad", "model-v2"],
        "each generation read its own pinned file"
    );
    eprintln!("model swap: {order:?}; events {events:?}");
}

/// `MVF_STORAGE_BENCH=1`: file I/O of a worker through its resolved host paths (the OCI e2e runs
/// the same script in a sandbox), and the node's peak memory.
#[test]
fn bench_worker_file_io() {
    if std::env::var_os("MVF_STORAGE_BENCH").is_none_or(|v| v != "1") {
        return;
    }
    let (root, modules) = (temp("bench-root"), temp("bench-modules"));
    module(
        &modules,
        "t.io-bench",
        "import os, sys, time\nfrom mvf_sdk import analysis, run_analyzer\nfrom mvf_sdk.storage import volume\nMIB = 64\nblock = os.urandom(1 << 20)\ndef bench(d):\n    p = d / 'bench.bin'\n    t0 = time.perf_counter()\n    with open(p, 'wb') as f:\n        for _ in range(MIB):\n            f.write(block)\n        f.flush(); os.fsync(f.fileno())\n    w = time.perf_counter() - t0\n    t0 = time.perf_counter()\n    with open(p, 'rb') as f:\n        while f.read(1 << 20):\n            pass\n    r = time.perf_counter() - t0\n    os.remove(p)\n    return {'writeMiBps': round(MIB / w, 1), 'readMiBps': round(MIB / r, 1)}\nresult = {}\ndef analyze(payload, meta):\n    if not result:\n        result['persistent'] = bench(volume('results'))\n        result['scratch'] = bench(volume('tmp'))\n        try:\n            import resource\n            rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss\n            result['peakRssMiB'] = round(rss / (1 << 20 if sys.platform == 'darwin' else 1 << 10), 1)\n        except ImportError:\n            pass\n    return analysis(value=result)\nrun_analyzer('t.io-bench', analyze)\n",
        "      - { name: results, target: /results, access: read-write }\n      - { name: tmp, target: /scratch, kind: scratch, access: read-write, maxSize: 128MiB }\n",
        "",
    );
    let text = "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: { name: bench }\nspec:\n  volumes:\n    - { name: results, type: persistent }\n  nodes:\n    - { id: cam, builtin: synthetic-source, config: { frames: 1 } }\n    - { id: b, module: t.io-bench, storage: { results: results } }\n    - { id: sink, builtin: record }\n  edges:\n    data:\n      - { from: cam.frame, to: b.frame }\n      - { from: b.out, to: sink.json }\n";
    let s = services(&root, &modules);
    let mut e = engine(text, &s, &modules).expect("build");
    let r = e.run(&RunOptions::default(), &mut NullObserver);
    assert_eq!(
        r.end,
        EndReason::SourceExhausted { node: "cam".into() },
        "{:?}",
        s.logs.events()
    );
    eprintln!(
        "worker file I/O, 64 MiB with fsync: {}",
        values(&s, "sink")[0]
    );
}
