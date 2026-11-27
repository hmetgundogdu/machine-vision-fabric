//! The C++ and .NET worker SDKs through the real engine and supervisor (#32 part 2): each speaks
//! protocol v2 when offered and v1 otherwise, keeps a stray stdout write off the v2 channel, and
//! carries checkpointed state across an engine restart.
//!
//! The modules are the SDKs' own examples, built outside cargo:
//!
//! ```text
//! cmake -S src/sdk/cpp -B rust/target/cpp-sdk -DCMAKE_BUILD_TYPE=Release && cmake --build rust/target/cpp-sdk --config Release
//! dotnet publish src/sdk/dotnet/examples/InvertTransformer -c Release -o rust/target/dotnet-sdk/InvertTransformer
//! dotnet publish src/sdk/dotnet/examples/MeanAnalyzer -c Release -o rust/target/dotnet-sdk/MeanAnalyzer
//! ```
//!
//! `MVF_CPP_SDK_DIR` / `MVF_DOTNET_SDK_DIR` point elsewhere (a Linux build for a container run).
//! A test whose module is not built reports NOT RUN, unless `MVF_REQUIRE_CPP` / `MVF_REQUIRE_DOTNET`
//! is set (CI), in which case it fails.

use std::path::{Path, PathBuf};

use mvf_engine::builtins::SyntheticSource;
use mvf_engine::*;
use mvf_graph::build_graph;
use mvf_worker::WorkerBackend;

const FRAMES: u64 = 20;
const LEN: usize = 64; // 8 × 8 × 1

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Where the examples were built: `MVF_CPP_SDK_DIR` / `MVF_DOTNET_SDK_DIR`, by default
/// `rust/target/cpp-sdk` and `rust/target/dotnet-sdk`.
fn build_dir(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(default)
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sdk {
    Cpp,
    Dotnet,
}

impl Sdk {
    fn require_var(self) -> &'static str {
        match self {
            Sdk::Cpp => "MVF_REQUIRE_CPP",
            Sdk::Dotnet => "MVF_REQUIRE_DOTNET",
        }
    }

    /// The built example, or `None` (NOT RUN) when it is not built and not required.
    fn module(self, example: Example) -> Option<PathBuf> {
        let exe = std::env::consts::EXE_SUFFIX;
        let candidates = match (self, example) {
            (Sdk::Cpp, _) => {
                let name = match example {
                    Example::Invert => "mvf_invert_transformer",
                    Example::Mean => "mvf_mean_analyzer",
                };
                let build = build_dir("MVF_CPP_SDK_DIR", "cpp-sdk");
                vec![
                    build.join(format!("{name}{exe}")),
                    build.join("Release").join(format!("{name}{exe}")),
                ]
            }
            (Sdk::Dotnet, Example::Invert) => vec![
                build_dir("MVF_DOTNET_SDK_DIR", "dotnet-sdk")
                    .join("InvertTransformer")
                    .join(format!("mvf-dotnet-invert{exe}")),
            ],
            (Sdk::Dotnet, Example::Mean) => vec![
                build_dir("MVF_DOTNET_SDK_DIR", "dotnet-sdk")
                    .join("MeanAnalyzer")
                    .join(format!("mvf-dotnet-mean{exe}")),
            ],
        };
        let found = candidates.into_iter().find(|p| p.is_file());
        if found.is_none() {
            assert!(
                std::env::var_os(self.require_var()).is_none(),
                "{} is set but the {self:?} SDK example {example:?} is not built",
                self.require_var()
            );
            eprintln!(
                "NOT RUN: the {self:?} SDK example {example:?} is not built (see the header of tests/sdks.rs)"
            );
        }
        found
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Example {
    Invert,
    Mean,
}

/// Writes a contract named `name` next to `binary` and returns its file name.
fn contract(binary: &Path, name: &str, example: Example) -> String {
    let entry = binary.file_name().and_then(|n| n.to_str()).expect("name");
    let (id, outputs, lifecycle) = match example {
        Example::Invert => (
            "sdk.invert",
            "[ { name: frame, channel: data, type: frame } ]",
            "",
        ),
        Example::Mean => (
            "sdk.mean",
            "[ { name: class, channel: data, type: classification }, { name: value, channel: data, type: json } ]",
            "\n  lifecycle: { replacement: stateful, state: { schemaVersion: 1 } }",
        ),
    };
    let file = format!("{name}.yaml");
    std::fs::write(
        binary.parent().expect("dir").join(&file),
        format!(
            "apiVersion: mvf.dev/v1alpha1\nkind: NodeContract\nmetadata: {{ id: {id}, version: 1.0.0 }}\nspec:\n  ports:\n    inputs: [ {{ name: frame, channel: data, type: frame }} ]\n    outputs: {outputs}\n  artifacts: [ {{ backend: worker, runtime: executable, entry: {entry}, protocol: 1 }} ]\n  isolation: {{ minimum: process }}{lifecycle}\n"
        ),
    )
    .expect("contract");
    file
}

fn pipeline(contract: &str, example: Example, frames: u64) -> String {
    let edges = match example {
        Example::Invert => {
            "      - { from: cam.frame, to: n.frame }\n      - { from: n.frame, to: sink.frame }\n"
        }
        Example::Mean => {
            "      - { from: cam.frame, to: n.frame }\n      - { from: n.class, to: sink.class }\n      - { from: n.value, to: sink.json }\n"
        }
    };
    format!(
        "apiVersion: mvf.dev/v1alpha1\nkind: Pipeline\nmetadata: {{ name: sdk }}\nspec:\n  nodes:\n    - {{ id: cam, builtin: synthetic-source, config: {{ width: 8, height: 8, frames: {frames}, arena: true }} }}\n    - {{ id: n, contract: {contract} }}\n    - {{ id: sink, builtin: record }}\n  edges:\n    data:\n{edges}"
    )
}

fn engine(text: &str, base: &Path, protocol: u64, services: &HostServices) -> Engine {
    let resolver = HostResolver::new(base).with_module_root(repo().join("modules"));
    let graph = build_graph(text, &resolver).expect("graph");
    let worker = WorkerBackend::from_env().with_protocol(protocol);
    Engine::build(graph, &default_backends().with(worker), services).expect("build")
}

fn spoke(services: &HostServices, protocol: u64) -> bool {
    services
        .logs
        .events()
        .iter()
        .any(|l| l.node == "n" && l.message.contains(&format!("protocol={protocol}")))
}

fn inverted(sequence: u64) -> String {
    let px: Vec<u8> = SyntheticSource::pixels(0, sequence, LEN)
        .iter()
        .map(|b| 255 - b)
        .collect();
    format!("{:016x}", fnv1a64(&px))
}

fn mean(sequence: u64) -> f64 {
    let px = SyntheticSource::pixels(0, sequence, LEN);
    px.iter().map(|&b| f64::from(b)).sum::<f64>() / px.len() as f64
}

fn transformer_speaks_both_protocols(sdk: Sdk) {
    let Some(binary) = sdk.module(Example::Invert) else {
        return;
    };
    let dir = binary.parent().expect("dir").to_path_buf();
    let file = contract(
        &binary,
        &format!("{sdk:?}-invert-protocols"),
        Example::Invert,
    );
    for protocol in [2, 1] {
        let services = HostServices::default();
        let report = engine(
            &pipeline(&file, Example::Invert, FRAMES),
            &dir,
            protocol,
            &services,
        )
        .run(&RunOptions::default(), &mut NullObserver);
        assert_eq!(
            report.end,
            EndReason::SourceExhausted { node: "cam".into() },
            "{sdk:?} v{protocol}: {:?}",
            services.logs.events()
        );
        assert!(
            spoke(&services, protocol),
            "{sdk:?} spoke v{protocol}: {:?}",
            services.logs.events()
        );
        let frames: Vec<_> = services
            .recorder
            .records()
            .into_iter()
            .filter_map(|r| match r.value {
                RecordedValue::Frame {
                    sequence, checksum, ..
                } => Some((sequence, checksum)),
                _ => None,
            })
            .collect();
        let expected: Vec<_> = (1..=FRAMES).map(|n| (n, inverted(n))).collect();
        assert_eq!(frames, expected, "{sdk:?} v{protocol} inverts every frame");
        let m = report.data_plane.arena.expect("arena");
        assert_eq!((m.copies_in, m.copies_out), (0, 0), "zero copy");
    }
}

/// `(cycle, label, measurement)` and `(cycle, frames)` from the mean analyzer's records.
type MeanRecords = (Vec<(u64, String, f64)>, Vec<(u64, u64)>);

fn mean_records(services: &HostServices) -> MeanRecords {
    let mut classes = Vec::new();
    let mut counts = Vec::new();
    for r in services.recorder.records() {
        match r.value {
            RecordedValue::Classification(c) => {
                classes.push((r.cycle, c.label, c.measurement.unwrap_or(-1.0)))
            }
            RecordedValue::Json { value } => counts.push((
                r.cycle,
                value.get("frames").and_then(|f| f.as_u64()).unwrap_or(0),
            )),
            _ => {}
        }
    }
    (classes, counts)
}

fn analyzer_keeps_stray_stdout_off_the_channel(sdk: Sdk) {
    let Some(binary) = sdk.module(Example::Mean) else {
        return;
    };
    let dir = binary.parent().expect("dir").to_path_buf();
    let file = contract(&binary, &format!("{sdk:?}-mean-stdout"), Example::Mean);
    let stray: &[&str] = match sdk {
        Sdk::Cpp => &["native stdout: first frame"],
        Sdk::Dotnet => &["managed stdout: first frame", "native stdout: first frame"],
    };
    for protocol in [2, 1] {
        let services = HostServices::default();
        let report = engine(
            &pipeline(&file, Example::Mean, FRAMES),
            &dir,
            protocol,
            &services,
        )
        .run(&RunOptions::default(), &mut NullObserver);
        let logs = services.logs.events();
        assert_eq!(
            report.end,
            EndReason::SourceExhausted { node: "cam".into() },
            "{sdk:?} v{protocol}: {logs:?}"
        );
        assert!(spoke(&services, protocol), "{logs:?}");
        let (classes, counts) = mean_records(&services);
        let expected: Vec<_> = (1..=FRAMES)
            .map(|n| {
                let m = mean(n);
                (n, if m < 128.0 { "dark" } else { "bright" }.to_string(), m)
            })
            .collect();
        assert_eq!(classes.len(), expected.len(), "{classes:?}");
        for (got, want) in classes.iter().zip(&expected) {
            assert_eq!((got.0, &got.1), (want.0, &want.1));
            assert!((got.2 - want.2).abs() < 1e-9, "{got:?} vs {want:?}");
        }
        assert_eq!(counts, (1..=FRAMES).map(|n| (n, n)).collect::<Vec<_>>());
        assert!(
            logs.iter()
                .any(|l| l.node == "n" && l.level == "info" && l.message == "analyzing"),
            "the SDK's own log line arrives: {logs:?}"
        );
        for line in stray {
            let landed = logs
                .iter()
                .find(|l| l.node == "n" && l.message.contains(line));
            let landed = landed.unwrap_or_else(|| panic!("{line:?} reached the logs: {logs:?}"));
            if protocol == 2 {
                // The SDK moved stdout onto stderr: the write is a log line, not channel noise.
                assert_eq!(landed.level, "stderr", "{landed:?}");
                assert_eq!(landed.message, *line);
            } else {
                // v1 shares stdout with the protocol; the host tolerates the line and says so.
                assert!(
                    landed.message.starts_with("non-protocol stdout"),
                    "{landed:?}"
                );
            }
        }
    }
}

struct CancelAfter(u64, CancellationToken);

impl Observer for CancelAfter {
    fn cycle_completed(&mut self, cycle: u64) {
        if cycle == self.0 {
            self.1.cancel();
        }
    }
}

fn analyzer_state_survives_an_engine_restart(sdk: Sdk) {
    let Some(binary) = sdk.module(Example::Mean) else {
        return;
    };
    let base = binary.parent().expect("dir").to_path_buf();
    let file = contract(&binary, &format!("{sdk:?}-mean-restart"), Example::Mean);
    let dir = std::env::temp_dir().join(format!("mvf-sdk-ckpt-{sdk:?}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let text = pipeline(&file, Example::Mean, FRAMES);
    let options = |cancel: CancellationToken| RunOptions {
        cancel,
        checkpoint_every: Some(5),
        checkpoint_dir: Some(dir.clone()),
        ..RunOptions::default()
    };
    let s1 = HostServices::default();
    let cancel = CancellationToken::new();
    let r1 =
        engine(&text, &base, 2, &s1).run(&options(cancel.clone()), &mut CancelAfter(12, cancel));
    assert_eq!(r1.end, EndReason::Cancelled, "{:?}", s1.logs.events());
    assert!(spoke(&s1, 2));
    assert!(
        dir.join("epoch-10/n.state").is_file(),
        "the module's own state was persisted"
    );

    let s2 = HostServices::default();
    let r2 =
        engine(&text, &base, 2, &s2).run(&options(CancellationToken::new()), &mut NullObserver);
    assert_eq!(r2.resumed_from_cycle, Some(10), "{:?}", s2.logs.events());
    let (_, counts) = mean_records(&s2);
    assert_eq!(
        counts,
        (11..=FRAMES).map(|n| (n, n)).collect::<Vec<_>>(),
        "the fresh process counts on from its restored state: {:?}",
        s2.logs.events()
    );
    assert!(!dir.exists());
}

#[test]
fn cpp_transformer_speaks_v2_when_offered_and_v1_otherwise() {
    transformer_speaks_both_protocols(Sdk::Cpp);
}

#[test]
fn dotnet_transformer_speaks_v2_when_offered_and_v1_otherwise() {
    transformer_speaks_both_protocols(Sdk::Dotnet);
}

#[test]
fn cpp_analyzer_keeps_a_native_stdout_write_off_the_v2_channel() {
    analyzer_keeps_stray_stdout_off_the_channel(Sdk::Cpp);
}

#[test]
fn dotnet_analyzer_keeps_managed_and_native_stdout_writes_off_the_v2_channel() {
    analyzer_keeps_stray_stdout_off_the_channel(Sdk::Dotnet);
}

#[test]
fn cpp_analyzer_state_survives_an_engine_restart_on_v2() {
    analyzer_state_survives_an_engine_restart(Sdk::Cpp);
}

#[test]
fn dotnet_analyzer_state_survives_an_engine_restart_on_v2() {
    analyzer_state_survives_an_engine_restart(Sdk::Dotnet);
}

/// The first byte a module writes when launched directly with `env`: `{` opens a v1 JSON hello,
/// `M` a v2 frame.
fn first_byte(program: &Path, args: &[PathBuf], env: &[(&str, &std::ffi::OsStr)]) -> u8 {
    use std::io::Read;
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let arena = std::env::temp_dir().join(format!(
        "mvf-sdk-negotiation-{}-{}.arena",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&arena, vec![0u8; 4096]).expect("arena");
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .env("MVF_ARENA_PATH", &arena)
        .env_remove("MVF_PROTOCOL")
        .env_remove("MVF_SESSION")
        .env_remove("MVF_GENERATION")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    for (k, v) in env {
        command.env(k, v);
    }
    let mut child = command.spawn().expect("spawn");
    let mut first = [0u8; 1];
    let read = child.stdout.take().expect("stdout").read_exact(&mut first);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&arena);
    read.expect("the module wrote a hello");
    first[0]
}

#[test]
fn an_offer_counts_only_with_its_session_token_in_every_sdk() {
    let python = std::env::var_os("MVF_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| (if cfg!(windows) { "python" } else { "python3" }).into());
    let sdk = repo().join("src/sdk/python");
    let mut modules = vec![(
        "python",
        python,
        vec![repo().join("modules/py-invert-transformer/transformer.py")],
        vec![("PYTHONPATH", sdk.into_os_string())],
    )];
    for (name, s) in [("c++", Sdk::Cpp), (".net", Sdk::Dotnet)] {
        if let Some(binary) = s.module(Example::Invert) {
            modules.push((name, binary, vec![], vec![]));
        }
    }
    let token = std::ffi::OsStr::new("0123456789abcdef");
    let (two, one) = (std::ffi::OsStr::new("2"), std::ffi::OsStr::new("1"));
    for (name, program, args, base) in &modules {
        let with = |extra: &[(&'static str, &std::ffi::OsStr)]| {
            let mut env: Vec<(&str, &std::ffi::OsStr)> =
                base.iter().map(|(k, v)| (*k, v.as_os_str())).collect();
            env.extend_from_slice(extra);
            first_byte(program, args, &env)
        };
        assert_eq!(with(&[]), b'{', "{name}: no offer → v1");
        assert_eq!(
            with(&[("MVF_PROTOCOL", two)]),
            b'{',
            "{name}: MVF_PROTOCOL=2 without a session token is not an offer"
        );
        assert_eq!(
            with(&[("MVF_PROTOCOL", one), ("MVF_SESSION", token)]),
            b'{',
            "{name}: MVF_PROTOCOL=1 → v1"
        );
        assert_eq!(
            with(&[("MVF_PROTOCOL", two), ("MVF_SESSION", token)]),
            b'M',
            "{name}: a full offer → v2"
        );
    }
}
