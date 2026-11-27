//! `mvf-rs` — native CLI for the MVF Rust runtime.
//!
//! Exit codes: 0 = run ended normally (cycle limit, source exhausted, cancelled) or validation passed;
//! 1 = a node failed or could not start; 2 = invalid input (YAML, contract, graph, negotiation).
//!
//! Output: sink records go to stdout (text or `--json` lines); node logs and the run summary go to
//! stderr, so stdout stays machine-readable.

mod image;
mod manage;
mod volume;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use mvf_engine::{
    BackendRegistry, CancellationToken, ControlHandle, EndReason, Engine, HostResolver,
    HostServices, LogEvent, Logs, Observer, PlannedCommand, Record, Recorder, RunOptions,
    RunReport, default_backends, parse_update_plan,
};
use mvf_worker::WorkerBackend;

#[derive(Debug, Parser)]
#[command(
    name = "mvf-rs",
    version,
    about = "MVF Rust runtime (research build, issue #10)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Args)]
struct PipelineArgs {
    /// Pipeline YAML file.
    pipeline: PathBuf,
    /// Module root searched for `<dir>/node.yaml` (repeatable). Default: the nearest `modules/`
    /// directory above the pipeline, plus `MVF_MODULE_PATH`.
    #[arg(long = "modules")]
    modules: Vec<PathBuf>,
    /// Shared-memory arena slot size in bytes, header included (default 4 MiB).
    #[arg(long)]
    arena_slot_size: Option<usize>,
    /// Number of shared-memory arena slots (default 16).
    #[arg(long)]
    arena_slots: Option<usize>,
    /// Machine-local binding store (`value`/`select`/module bindings). Default:
    /// `<pipeline dir>/.mvf/bindings.json`. Environment `MVF_BINDING_<KEY>` answers too.
    #[arg(long)]
    bindings: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate a node contract and show how this host would place it.
    Contract {
        /// Contract YAML file.
        file: PathBuf,
    },
    /// Validate a pipeline: YAML, contracts, typed edges and backend negotiation.
    Validate {
        #[command(flatten)]
        args: PipelineArgs,
    },
    /// Run a pipeline until a limit, source exhaustion, failure or Ctrl+C.
    Run {
        #[command(flatten)]
        args: PipelineArgs,
        /// Override `spec.run.maxCycles`.
        #[arg(long)]
        max_cycles: Option<u64>,
        /// Emit JSON lines (`{"record":…}` per sink record, then `{"report":…}`).
        #[arg(long)]
        json: bool,
        /// Live-update plan (`kind: UpdatePlan`) applied while the pipeline runs.
        #[arg(long)]
        updates: Option<PathBuf>,
        /// Verify the run: normal end, every cycle delivered exactly once and in order to each
        /// recording sink (frame sequence == cycle), no leaked arena slot. Exit code 3 on failure.
        #[arg(long)]
        verify: bool,
        /// With --verify: required number of promoted replacements.
        #[arg(long)]
        expect_promotions: Option<usize>,
        /// With --verify: required number of rollbacks.
        #[arg(long)]
        expect_rollbacks: Option<usize>,
        /// With --verify: largest acceptable cut-over pause of any promotion or rollback.
        #[arg(long)]
        max_cutover_us: Option<u64>,
        /// Fail unless this host provides the GPU compute backend.
        #[arg(long)]
        require_gpu: bool,
        /// Fail unless this host provides the daemonless OCI backend.
        #[arg(long)]
        require_oci: bool,
        /// Capture node state every N completed cycles (overrides `run.checkpoint.everyCycles`).
        #[arg(long)]
        checkpoint_every: Option<u64>,
        /// Durable checkpoint directory; a run that finds one resumes after its last epoch.
        #[arg(long)]
        checkpoint_dir: Option<PathBuf>,
        /// `serial` (one cycle at a time) or `pipelined` (stage-parallel, bounded queues).
        #[arg(long, value_enum, default_value_t = Mode::Serial)]
        mode: Mode,
        /// Pipelined: depth of each inter-stage queue (default 2).
        #[arg(long)]
        queue: Option<usize>,
        /// Serve live observation and commands on a local endpoint only this OS user can reach
        /// (#33); see `mvf-rs manage`.
        #[arg(long)]
        manage: bool,
        /// With --manage: the audit log of commands (default `<pipeline dir>/.mvf/audit.jsonl`).
        #[arg(long)]
        audit: Option<PathBuf>,
        /// With --manage: allow capped image previews of this node's output (repeatable). Nothing
        /// is previewed unless named here.
        #[arg(long = "preview")]
        preview: Vec<String>,
        /// With --manage: let operators upload node packages and stage them in
        /// `<pipeline dir>/.mvf/staged` (activated with the `replace` command).
        #[arg(long)]
        allow_uploads: bool,
    },
    /// List the builtins and backends this host provides.
    Host,
    /// Offline OCI image store: import, verify, export, unpack (no Docker daemon, no network).
    Image {
        /// Store directory (default: `MVF_STORE` or `~/.mvf/store`).
        #[arg(long, global = true)]
        store: Option<PathBuf>,
        #[command(subcommand)]
        command: image::ImageCommand,
    },
    /// Observe a running `mvf-rs run --manage` (#33): list, status, watch.
    Manage {
        #[command(subcommand)]
        command: manage::ManageCommand,
    },
    /// Retained volumes and pinned artifacts (#31): list, inspect, delete explicitly.
    Volume {
        /// Storage root (default: `MVF_STORAGE_DIR` or the per-user default).
        #[arg(long, global = true)]
        storage: Option<std::path::PathBuf>,
        #[command(subcommand)]
        command: volume::VolumeCommand,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Contract { file } => contract(&file),
        Command::Validate { args } => validate(&args),
        Command::Run {
            args,
            max_cycles,
            json,
            updates,
            verify,
            expect_promotions,
            expect_rollbacks,
            max_cutover_us,
            require_gpu,
            require_oci,
            checkpoint_every,
            checkpoint_dir,
            mode,
            queue,
            manage,
            audit,
            preview,
            allow_uploads,
        } => {
            if require_gpu && gpu_context().is_none() {
                return fail(
                    2,
                    "--require-gpu: this host provides no GPU compute backend",
                );
            }
            if require_oci
                && !host_backends()
                    .capabilities()
                    .iter()
                    .any(|c| c.kind == mvf_contract::BackendKind::Oci)
            {
                return fail(
                    2,
                    "--require-oci: this host provides no OCI backend (Linux + runc/crun)",
                );
            }
            run(
                &args,
                RunOptions {
                    max_cycles,
                    checkpoint_every,
                    checkpoint_dir,
                    pipelined: mode == Mode::Pipelined,
                    queue,
                    ..RunOptions::default()
                },
                json,
                updates.as_deref(),
                verify.then_some(Expectations {
                    promotions: expect_promotions,
                    rollbacks: expect_rollbacks,
                    max_cutover_us,
                }),
                manage.then_some(ManageOptions {
                    audit,
                    preview,
                    uploads: allow_uploads,
                }),
            )
        }
        Command::Manage { command } => manage::run(command),
        Command::Host => host(),
        Command::Image { store, command } => {
            image::run(&store.unwrap_or_else(image::default_store), command)
        }
        Command::Volume { storage, command } => volume::run(
            &storage
                .unwrap_or_else(|| mvf_storage::StorageService::from_env().root().to_path_buf()),
            command,
        ),
    }
}

/// The host GPU, opened once (`None` without a hardware adapter or with `MVF_DISABLE_GPU=1`).
fn gpu_context() -> Option<mvf_compute::GpuContext> {
    static GPU: std::sync::OnceLock<Option<mvf_compute::GpuContext>> = std::sync::OnceLock::new();
    GPU.get_or_init(|| {
        if std::env::var_os("MVF_DISABLE_GPU").is_some() {
            return None;
        }
        mvf_compute::GpuContext::new(&mvf_compute::GpuConfig::default()).ok()
    })
    .clone()
}

/// Every backend this host provides: in-process and worker always; the GPU compute backend when a
/// hardware adapter exists (`MVF_DISABLE_GPU=1` turns it off); the daemonless OCI backend on Linux
/// when `runc`/`crun` is installed (images from `MVF_STORE`, state under `~/.mvf/state`).
fn host_backends() -> BackendRegistry {
    let mut registry = default_backends().with(WorkerBackend::from_env());
    if let Some(gpu) = gpu_context() {
        registry = registry.with(mvf_compute::ComputeBackend::new(gpu));
    }
    let store_root = image::default_store();
    // The rootfs cache must be traversable by the user-namespace root, which `/root` is not.
    let state = std::env::var_os("MVF_OCI_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let root = std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find(|l| l.starts_with("Uid:"))
                        .map(|l| l.split_whitespace().nth(2) == Some("0"))
                })
                .unwrap_or(false);
            if root {
                PathBuf::from("/var/lib/mvf/oci")
            } else {
                store_root.parent().unwrap_or(Path::new(".")).join("state")
            }
        });
    if let Ok(store) = mvf_oci::ContentStore::open(&store_root)
        && let Some(oci) = mvf_container::OciBackend::detect(store, state)
    {
        registry = registry.with(oci);
    }
    registry
}

fn fail(code: u8, message: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(code)
}

fn contract(file: &Path) -> ExitCode {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => return fail(2, format!("{}: {e}", file.display())),
    };
    let contract = match mvf_contract::parse_contract(&text) {
        Ok(c) => c,
        Err(e) => return fail(2, e),
    };
    let placement = host_backends().negotiate(&contract);
    let out = serde_json::json!({
        "id": contract.metadata.id,
        "version": contract.metadata.version,
        "placement": match &placement {
            Ok(p) => serde_json::to_value(p).unwrap_or_default(),
            Err(e) => serde_json::to_value(e).unwrap_or_default(),
        },
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    if placement.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

fn resolver(args: &PipelineArgs) -> HostResolver {
    let base = args
        .pipeline
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut resolver = HostResolver::new(base);
    let mut roots = args.modules.clone();
    if let Some(env) = std::env::var_os("MVF_MODULE_PATH") {
        roots.extend(std::env::split_paths(&env));
    }
    if roots.is_empty()
        && let Some(found) = HostResolver::discover_module_root(base, 6)
    {
        roots.push(found);
    }
    for root in roots {
        resolver = resolver.with_module_root(root);
    }
    resolver
}

/// Host services for a pipeline: package directory and arena sizing from the arguments.
fn services_for(args: &PipelineArgs, base: HostServices) -> HostServices {
    let mut services = base;
    services.package_dir = args
        .pipeline
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                Path::new(".")
            } else {
                p
            }
        })
        .and_then(|p| std::fs::canonicalize(p).ok());
    if let Some(size) = args.arena_slot_size {
        services.arena_config.slot_size = size;
    }
    if let Some(n) = args.arena_slots {
        services.arena_config.slot_count = n;
    }
    let store = args.bindings.clone().unwrap_or_else(|| {
        services
            .package_dir
            .as_deref()
            .unwrap_or(Path::new("."))
            .join(".mvf/bindings.json")
    });
    services.bindings = std::sync::Arc::new(mvf_engine::Bindings::file(store));
    services
}

fn load_engine(args: &PipelineArgs, services: &HostServices) -> Result<Engine, ExitCode> {
    load_engine_for(args, services, None)
}

/// With `pipelined_queue`, the arena is sized from the graph (unless `--arena-slots` was given):
/// stage-parallel runs keep a queue's worth of frames in flight per edge.
fn load_engine_for(
    args: &PipelineArgs,
    services: &HostServices,
    pipelined_queue: Option<usize>,
) -> Result<Engine, ExitCode> {
    let text = std::fs::read_to_string(&args.pipeline)
        .map_err(|e| fail(2, format!("{}: {e}", args.pipeline.display())))?;
    let graph = mvf_graph::build_graph(&text, &resolver(args)).map_err(|e| fail(2, e))?;
    let mut services = services.clone();
    if let Some(queue) = pipelined_queue
        && args.arena_slots.is_none()
    {
        services.arena_config.slot_count =
            mvf_engine::pipelined::arena_slots_for(&graph, queue, services.arena_config.slot_count);
    }
    Engine::build(graph, &host_backends(), &services).map_err(|e| fail(2, e))
}

/// Execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Mode {
    /// One cycle at a time.
    Serial,
    /// Stage-parallel with bounded inter-stage queues.
    Pipelined,
}

fn validate(args: &PipelineArgs) -> ExitCode {
    let engine = match load_engine(args, &services_for(args, HostServices::default())) {
        Ok(e) => e,
        Err(code) => return code,
    };
    let graph = engine.graph();
    println!(
        "pipeline `{}`: {} nodes, {} data edges, {} control edges — valid",
        graph.name,
        graph.nodes.len(),
        graph.data_edges.len(),
        graph.control_edges.len()
    );
    for (node, stats) in graph.nodes.iter().zip(engine.stats()) {
        println!(
            "  {:<16} {:<32} backend={}",
            node.id,
            format!(
                "{}@{}",
                node.contract.metadata.id, node.contract.metadata.version
            ),
            stats.backend
        );
    }
    ExitCode::SUCCESS
}

fn print_record(record: &Record, json: bool) {
    if json {
        println!("{}", serde_json::json!({ "record": record }));
    } else {
        println!(
            "cycle={} node={} gen={} port={} {}",
            record.cycle,
            record.node,
            record.generation,
            record.port,
            serde_json::to_string(&record.value).unwrap_or_default()
        );
    }
}

fn print_log(event: &LogEvent, json: bool) {
    if json {
        eprintln!("{}", serde_json::json!({ "log": event }));
    } else {
        eprintln!(
            "[{} gen={} {}] {}",
            event.node, event.generation, event.level, event.message
        );
    }
}

fn print_report(report: &RunReport, json: bool) {
    if json {
        println!("{}", serde_json::json!({ "report": report }));
        return;
    }
    eprintln!(
        "run `{}` ended: {} after {} cycles in {:.1} ms (startup {:.1} ms, cycles {:.1} ms)",
        report.pipeline,
        serde_json::to_string(&report.end).unwrap_or_default(),
        report.cycles_completed,
        report.wall_time_ms,
        report.startup_ms,
        report.cycles_ms
    );
    for n in &report.nodes {
        eprintln!(
            "  {:<16} backend={:<10} gen={} executed={} skipped={} total={}us max={}us",
            n.id, n.backend, n.generation, n.executed, n.skipped, n.total_us, n.max_us
        );
    }
    if let Some(it) = &report.iteration {
        eprintln!(
            "  loop mode={} rewinds={} pauses={} paused={:.1}ms",
            serde_json::to_string(&it.mode)
                .unwrap_or_default()
                .trim_matches('"'),
            it.rewinds,
            it.pauses,
            it.paused_ms
        );
    }
    for e in &report.live {
        eprintln!(
            "  live {:<12} {:<14} at cycle {:<4} {}{}{}",
            serde_json::to_string(&e.kind)
                .unwrap_or_default()
                .trim_matches('"'),
            e.node.as_deref().unwrap_or("-"),
            e.cycle,
            e.field
                .as_deref()
                .map(|f| format!("field={f} "))
                .unwrap_or_default(),
            if e.persisted { "persisted " } else { "" },
            e.message.as_deref().unwrap_or_default()
        );
    }
    for e in &report.replacements {
        eprintln!(
            "  replace {:<10} {:<12} at cycle {:<4} gen {} -> {} {}{}{}",
            e.node,
            serde_json::to_string(&e.kind)
                .unwrap_or_default()
                .trim_matches('"'),
            e.cycle,
            e.from_generation,
            e.to_generation
                .map(|g| g.to_string())
                .unwrap_or_else(|| "-".into()),
            e.code.map(|c| format!("[{c}] ")).unwrap_or_default(),
            e.preparation_ms
                .map(|p| format!("prep={p:.1}ms "))
                .unwrap_or_default(),
            e.cutover_us
                .map(|c| format!("cutover={c}us"))
                .unwrap_or_default(),
        );
    }
    if let Some(a) = &report.data_plane.arena {
        eprintln!(
            "  data plane: copiesIn={} copiesOut={} transformWrites={} zeroCopyHandoffs={} inUse={}",
            a.copies_in, a.copies_out, a.transform_writes, a.zero_copy_handoffs, a.in_use
        );
    }
}

/// Submits scripted update steps at the boundary before their cycle.
struct PlanDriver {
    control: ControlHandle,
    steps: Vec<(u64, PlannedCommand)>,
    last: std::time::Instant,
    cycle_ms: Vec<f64>,
}

impl Observer for PlanDriver {
    fn cycle_completed(&mut self, cycle: u64) {
        let now = std::time::Instant::now();
        self.cycle_ms.push((now - self.last).as_secs_f64() * 1e3);
        self.last = now;
        for (at, command) in &self.steps {
            if *at == cycle + 1 {
                command.submit(&self.control);
            }
        }
    }
}

fn load_plan(path: &Path, args: &PipelineArgs) -> Result<Vec<(u64, PlannedCommand)>, ExitCode> {
    let text =
        std::fs::read_to_string(path).map_err(|e| fail(2, format!("{}: {e}", path.display())))?;
    let base = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut plan_resolver = HostResolver::new(base);
    for root in resolver(args).module_roots() {
        plan_resolver = plan_resolver.with_module_root(root);
    }
    parse_update_plan(&text, &plan_resolver)
        .map_err(|e| fail(2, format!("{}: {e}", path.display())))
}

/// What `--verify` requires besides the always-on checks.
#[derive(Debug, Clone, Copy)]
struct Expectations {
    promotions: Option<usize>,
    rollbacks: Option<usize>,
    max_cutover_us: Option<u64>,
}

fn verify(report: &RunReport, services: &HostServices, expect: Expectations, json: bool) -> bool {
    use mvf_engine::{RecordedValue, ReplacementEventKind};
    let mut checks: Vec<(&str, bool, String)> = Vec::new();
    let normal = matches!(
        report.end,
        EndReason::CycleLimit { .. } | EndReason::SourceExhausted { .. }
    );
    checks.push((
        "normal-end",
        normal,
        serde_json::to_string(&report.end).unwrap_or_default(),
    ));

    let mut per_sink: std::collections::BTreeMap<String, Vec<(u64, u64)>> = Default::default();
    for r in services.recorder.records() {
        if let RecordedValue::Frame { sequence, .. } = r.value {
            per_sink
                .entry(format!("{}.{}", r.node, r.port))
                .or_default()
                .push((r.cycle, sequence));
        }
    }
    let expected: Vec<(u64, u64)> = (1..=report.cycles_completed).map(|c| (c, c)).collect();
    checks.push((
        "recorded-sink",
        !per_sink.is_empty(),
        format!("{} recording sink port(s)", per_sink.len()),
    ));
    for (sink, seen) in &per_sink {
        checks.push((
            "every-cycle-exactly-once-in-order",
            *seen == expected,
            format!(
                "{sink}: {} records for {} cycles",
                seen.len(),
                report.cycles_completed
            ),
        ));
    }
    let in_use = report.data_plane.arena.map(|a| a.in_use).unwrap_or(0);
    checks.push((
        "no-leaked-arena-slot",
        in_use == 0,
        format!("inUse={in_use}"),
    ));
    let count =
        |k: ReplacementEventKind| report.replacements.iter().filter(|e| e.kind == k).count();
    if let Some(n) = expect.promotions {
        let got = count(ReplacementEventKind::Promoted);
        checks.push(("promotions", got == n, format!("expected {n}, got {got}")));
    }
    if let Some(n) = expect.rollbacks {
        let got = count(ReplacementEventKind::RolledBack);
        checks.push(("rollbacks", got == n, format!("expected {n}, got {got}")));
    }
    if let Some(limit) = expect.max_cutover_us {
        let worst = report
            .replacements
            .iter()
            .filter_map(|e| e.cutover_us)
            .max()
            .unwrap_or(0);
        checks.push((
            "cutover-pause",
            worst <= limit,
            format!("max {worst} us (limit {limit} us)"),
        ));
    }
    let plc: Vec<u64> = services
        .recorder
        .records()
        .iter()
        .filter(|r| r.port == "plc")
        .map(|r| r.cycle)
        .collect();
    if !plc.is_empty() {
        let unique: std::collections::BTreeSet<u64> = plc.iter().copied().collect();
        checks.push((
            "plc-no-duplicate-commit",
            unique.len() == plc.len() && plc.len() as u64 == report.cycles_completed,
            format!(
                "{} commits for {} cycles",
                plc.len(),
                report.cycles_completed
            ),
        ));
    }
    if let Some(g) = gpu_context().filter(|_| report.nodes.iter().any(|n| n.backend == "compute")) {
        let m = g.metrics();
        checks.push((
            "no-leaked-device-memory",
            m.allocated == 0,
            format!("allocated={}", m.allocated),
        ));
    }
    let pass = checks.iter().all(|(_, ok, _)| *ok);
    if json {
        let list: Vec<_> = checks
            .iter()
            .map(|(name, ok, detail)| serde_json::json!({"check": name, "pass": ok, "detail": detail}))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "verify": { "pass": pass, "checks": list } })
        );
    } else {
        for (name, ok, detail) in &checks {
            eprintln!(
                "  verify {:<36} {} {detail}",
                name,
                if *ok { "PASS" } else { "FAIL" }
            );
        }
        eprintln!("VERIFY {}", if pass { "PASS" } else { "FAIL" });
    }
    pass
}

/// What `run --manage` serves.
#[derive(Debug, Clone)]
struct ManageOptions {
    audit: Option<PathBuf>,
    preview: Vec<String>,
    uploads: bool,
}

fn run(
    args: &PipelineArgs,
    mut options: RunOptions,
    json: bool,
    updates: Option<&Path>,
    expect: Option<Expectations>,
    manage: Option<ManageOptions>,
) -> ExitCode {
    let feed = manage.is_some().then(|| {
        mvf_management::Feed::new(
            mvf_management::RuntimeInfo {
                pipeline: String::new(),
                version: env!("CARGO_PKG_VERSION").into(),
                mode: if options.pipelined {
                    "pipelined"
                } else {
                    "serial"
                }
                .into(),
            },
            mvf_management::FeedConfig::default(),
        )
    });
    let log_feed = feed.clone();
    let services = services_for(
        args,
        HostServices {
            logs: Logs::streaming(move |e| {
                print_log(e, json);
                if let Some(f) = &log_feed {
                    f.log(e);
                }
            }),
            ..HostServices::with_recorder(Recorder::streaming(move |r| print_record(r, json)))
        },
    );
    let queue = options.pipelined.then(|| {
        options
            .queue
            .unwrap_or(mvf_engine::pipelined::DEFAULT_QUEUE)
    });
    let mut engine = match load_engine_for(args, &services, queue) {
        Ok(e) => e,
        Err(code) => return code,
    };
    let steps = match updates.map(|p| load_plan(p, args)).transpose() {
        Ok(s) => s.unwrap_or_default(),
        Err(code) => return code,
    };
    let cancel = CancellationToken::new();
    let handler_token = cancel.clone();
    // Ctrl+C requests a clean stop at the next cycle boundary.
    let _ = ctrlc::set_handler(move || handler_token.cancel());
    let mut driver = PlanDriver {
        control: engine.control(),
        steps,
        last: std::time::Instant::now(),
        cycle_ms: Vec::new(),
    };
    options.cancel = cancel.clone();
    // Management (#33): the endpoint opens before the run and closes after the final status.
    let mut managed = match &feed {
        Some(feed) => {
            let backends: Vec<(String, String)> = engine
                .stats()
                .into_iter()
                .map(|s| (s.id, s.backend))
                .collect();
            feed.set_topology(&mvf_management::Topology::of(engine.graph(), &backends));
            let m = manage.clone().unwrap_or(ManageOptions {
                audit: None,
                preview: Vec::new(),
                uploads: false,
            });
            let package = services
                .package_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("."));
            let audit = m
                .audit
                .clone()
                .unwrap_or_else(|| package.join(".mvf/audit.jsonl"));
            if let Some(unknown) = m
                .preview
                .iter()
                .find(|n| !engine.graph().nodes.iter().any(|g| &g.id == *n))
            {
                return fail(
                    2,
                    format!("--preview: no node `{unknown}` in this pipeline"),
                );
            }
            if !m.preview.is_empty() {
                feed.enable_previews(mvf_management::PreviewConfig {
                    nodes: m.preview.clone(),
                    ..mvf_management::PreviewConfig::default()
                });
            }
            let control = mvf_management::Control {
                handle: engine.control(),
                cancel: cancel.clone(),
                resolver: std::sync::Arc::new(resolver(args)),
            };
            match mvf_management::Server::start(
                feed.clone(),
                mvf_management::ServerOptions {
                    control: Some(control),
                    audit: Some(audit.clone()),
                    uploads: m.uploads.then(|| {
                        mvf_management::UploadOptions::new(
                            package.join(".mvf/staged"),
                            ".mvf/staged",
                        )
                    }),
                    ..mvf_management::ServerOptions::default()
                },
            ) {
                Ok(server) => {
                    eprintln!(
                        "  management endpoint: {} (audit: {}; previews: {}; uploads: {})",
                        server.endpoint(),
                        audit.display(),
                        if m.preview.is_empty() {
                            "none".to_string()
                        } else {
                            m.preview.join(", ")
                        },
                        if m.uploads { "allowed" } else { "off" }
                    );
                    let tap = mvf_management::Tap::new(
                        feed.clone(),
                        engine.graph().nodes.iter().map(|n| n.id.clone()),
                    );
                    Some((server, tap))
                }
                Err(e) => return fail(2, format!("--manage: {e}")),
            }
        }
        None => None,
    };
    let report = match &mut managed {
        Some((_, tap)) => {
            let report = engine.run(&options, &mut mvf_management::Tee::new(&mut driver, tap));
            tap.finish(&report);
            report
        }
        None => engine.run(&options, &mut driver),
    };
    drop(managed);
    print_report(&report, json);
    print_metrics(&report, &mut driver.cycle_ms, json);
    if let Some(expect) = expect
        && !verify(&report, &services, expect, json)
    {
        return ExitCode::from(3);
    }
    match report.end {
        EndReason::NodeFailed { .. } | EndReason::StartFailed { .. } => ExitCode::from(1),
        _ => ExitCode::SUCCESS,
    }
}

/// Cycle latency percentiles, throughput and GPU counters of a run.
fn print_metrics(report: &RunReport, cycle_ms: &mut [f64], json: bool) {
    // The first cycle includes lazy start-up (pipeline compilation, arena creation).
    let steady = if cycle_ms.len() > 1 {
        &mut cycle_ms[1..]
    } else {
        cycle_ms
    };
    steady.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| {
        steady
            .get(((steady.len().max(1) - 1) as f64 * p).round() as usize)
            .copied()
            .unwrap_or(0.0)
    };
    let fps = if report.cycles_ms > 0.0 {
        report.cycles_completed as f64 / (report.cycles_ms / 1e3)
    } else {
        0.0
    };
    let gpu = gpu_context()
        .filter(|_| report.nodes.iter().any(|n| n.backend == "compute"))
        .map(|g| (g.info().name.clone(), g.metrics()));
    let metrics = serde_json::json!({
        "cycleMs": { "p50": pct(0.5), "p95": pct(0.95), "p99": pct(0.99), "max": pct(1.0) },
        "fps": fps,
        "gpu": gpu.as_ref().map(|(name, m)| serde_json::json!({ "adapter": name, "counters": m })),
    });
    if json {
        println!("{}", serde_json::json!({ "metrics": metrics }));
    } else {
        eprintln!(
            "  cycle ms p50={:.3} p95={:.3} p99={:.3} max={:.3}  fps={fps:.1}",
            pct(0.5),
            pct(0.95),
            pct(0.99),
            pct(1.0)
        );
        if let Some((name, m)) = gpu {
            eprintln!("  gpu {name}: {m:?}");
        }
    }
}

fn host() -> ExitCode {
    let out = serde_json::json!({
        "builtins": mvf_engine::builtins::builtin_names(),
        "backends": host_backends().capabilities(),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    ExitCode::SUCCESS
}
