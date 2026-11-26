//! `mvf-rs` — native CLI for the MVF Rust runtime.
//!
//! Exit codes: 0 = run ended normally (cycle limit, source exhausted, cancelled) or validation passed;
//! 1 = a node failed or could not start; 2 = invalid input (YAML, contract, graph, negotiation).
//!
//! Output: sink records go to stdout (text or `--json` lines); node logs and the run summary go to
//! stderr, so stdout stays machine-readable.

mod image;

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
        } => run(
            &args,
            max_cycles,
            json,
            updates.as_deref(),
            verify.then_some(Expectations {
                promotions: expect_promotions,
                rollbacks: expect_rollbacks,
            }),
        ),
        Command::Host => host(),
        Command::Image { store, command } => {
            image::run(&store.unwrap_or_else(image::default_store), command)
        }
    }
}

/// Every backend this binary provides.
fn host_backends() -> BackendRegistry {
    default_backends().with(WorkerBackend::from_env())
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
    services
}

fn load_engine(args: &PipelineArgs, services: &HostServices) -> Result<Engine, ExitCode> {
    let text = std::fs::read_to_string(&args.pipeline)
        .map_err(|e| fail(2, format!("{}: {e}", args.pipeline.display())))?;
    let graph = mvf_graph::build_graph(&text, &resolver(args)).map_err(|e| fail(2, e))?;
    Engine::build(graph, &host_backends(), services).map_err(|e| fail(2, e))
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
}

impl Observer for PlanDriver {
    fn cycle_completed(&mut self, cycle: u64) {
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

fn run(
    args: &PipelineArgs,
    max_cycles: Option<u64>,
    json: bool,
    updates: Option<&Path>,
    expect: Option<Expectations>,
) -> ExitCode {
    let services = services_for(
        args,
        HostServices {
            logs: Logs::streaming(move |e| print_log(e, json)),
            ..HostServices::with_recorder(Recorder::streaming(move |r| print_record(r, json)))
        },
    );
    let mut engine = match load_engine(args, &services) {
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
    };
    let report = engine.run(&RunOptions { max_cycles, cancel }, &mut driver);
    print_report(&report, json);
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

fn host() -> ExitCode {
    let out = serde_json::json!({
        "builtins": mvf_engine::builtins::builtin_names(),
        "backends": host_backends().capabilities(),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    ExitCode::SUCCESS
}
